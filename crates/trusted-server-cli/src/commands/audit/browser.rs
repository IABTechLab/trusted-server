//! Chrome/Chromium-backed implementation of [`AuditCollector`] using
//! `chromiumoxide` (CDP).
//!
//! The collector installs optional pre-navigation init scripts, sets any
//! operator-supplied cookies, navigates, waits for the page to settle, optionally
//! scrolls, and reads back a bounded set of evidence. It never *captures* page
//! HTML, cookies, or storage; supplied cookies are only *sent* to carry an
//! existing session past origin gates.

use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::Page;
use futures::StreamExt as _;

use crate::ad_templates::compare::BrowserAdEvidence;
use crate::ad_templates::output::Warning;
use crate::commands::audit::collector::{
    AuditCollector, BrowserCollectRequest, BrowserOpts, BrowserProfile, CollectedPage,
};

/// Candidate Chrome/Chromium executable names searched on `PATH`.
pub(crate) const CHROME_NAMES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
    "Google Chrome",
    "Google Chrome for Testing",
];

/// Poll interval while waiting for the page network to settle, in milliseconds.
const SETTLE_POLL_MS: u64 = 250;
/// Hard cap on page navigation so a stalled load cannot hang the audit.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for each CDP operation after navigation.
const CDP_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Hard cap per decoded evidence list, so a hostile page cannot inflate CLI
/// memory.
///
/// Must equal `__ts_max_entries` in `ad_template_collector.js`. The collector
/// already caps each list, but the evidence object lives on `window`, so a page
/// that appends to it directly is bounded here instead. Anything the collector
/// itself dropped is reported as an `evidence_truncated` warning.
const MAX_EVIDENCE_ENTRIES: usize = 128;
/// Hard cap on the UTF-8 JSON payload before CDP transfers it back to Rust.
const MAX_EVIDENCE_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Hard cap on browser teardown so a wedged Chrome cannot hang the audit.
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default quiet window (no new resources) marking the page settled.
const DEFAULT_SETTLE_QUIET_MS: u64 = 750;
/// Default hard cap on settling so slow/ad-heavy pages still terminate.
const DEFAULT_SETTLE_MAX_MS: u64 = 10_000;

/// Page-settle timing thresholds.
#[derive(Debug, Clone, Copy)]
struct SettleConfig {
    /// Quiet window with no new resources marking the page settled.
    quiet: Duration,
    /// Hard cap on total settle time.
    max: Duration,
}

/// Immutable browser/session settings shared by every URL in one audit batch.
struct BrowserSessionOptions<'a> {
    chrome: &'a std::path::Path,
    profile_dir: &'a std::path::Path,
    settle: SettleConfig,
    accept_invalid_certs: bool,
    headful: bool,
    assume_consent: bool,
    proxy: Option<&'a str>,
    profile: BrowserProfile,
}

/// A `chromiumoxide`-backed page collector launching a local Chrome/Chromium.
#[derive(Debug, Clone)]
pub struct BrowserCollector {
    /// Explicit Chrome/Chromium executable override (else `$CHROME`, else auto-detect).
    chrome: Option<std::path::PathBuf>,
    /// Quiet window marking the page settled.
    settle_quiet: Duration,
    /// Hard cap on settling.
    settle_max: Duration,
    /// Navigate to origins with invalid TLS certificates (dangerous opt-in).
    accept_invalid_certs: bool,
    /// Run visible Chrome rather than new headless Chrome.
    headful: bool,
    /// Install the standard consent API stub before publisher scripts.
    assume_consent: bool,
    /// Optional browser proxy endpoint.
    proxy: Option<String>,
    /// Device viewport/user-agent profile.
    profile: BrowserProfile,
}

impl Default for BrowserCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserCollector {
    /// Creates a collector with default tuning and auto-detected Chrome.
    #[must_use]
    pub fn new() -> Self {
        Self {
            chrome: None,
            settle_quiet: Duration::from_millis(DEFAULT_SETTLE_QUIET_MS),
            settle_max: Duration::from_millis(DEFAULT_SETTLE_MAX_MS),
            accept_invalid_certs: false,
            headful: false,
            assume_consent: true,
            proxy: None,
            profile: BrowserProfile::Desktop,
        }
    }

    /// Creates a collector from operator-supplied browser options.
    #[must_use]
    pub fn from_opts(opts: &BrowserOpts) -> Self {
        Self {
            chrome: opts.chrome.clone(),
            settle_quiet: Duration::from_millis(opts.settle_quiet_ms),
            settle_max: Duration::from_millis(opts.settle_max_ms),
            accept_invalid_certs: opts.danger_accept_invalid_certs,
            headful: opts.headful,
            assume_consent: !opts.no_assume_consent,
            proxy: opts.browser_proxy.clone(),
            profile: opts.profile,
        }
    }
}

/// Pre-document consent behavior shared with the generation crawler.
pub(crate) const CONSENT_STUB_SCRIPT: &str = include_str!("consent_stub.js");

/// Shared browser launch inputs used by both audit collectors.
pub(crate) struct BrowserLaunchOptions<'a> {
    pub(crate) chrome: &'a std::path::Path,
    pub(crate) profile_dir: &'a std::path::Path,
    pub(crate) headful: bool,
    pub(crate) proxy: Option<&'a str>,
    pub(crate) accept_invalid_certs: bool,
    pub(crate) viewport: Viewport,
    pub(crate) user_agent: Option<&'a str>,
}

/// Builds the common Chrome configuration for all browser-backed audits.
pub(crate) fn build_browser_config(
    options: BrowserLaunchOptions<'_>,
) -> Result<BrowserConfig, String> {
    let mut builder = BrowserConfig::builder()
        .chrome_executable(options.chrome)
        .user_data_dir(options.profile_dir);
    if !options.accept_invalid_certs {
        builder = builder.respect_https_errors();
    }
    if let Some(proxy) = options.proxy {
        let endpoint = if proxy.contains("://") {
            proxy.to_string()
        } else {
            format!("http://{proxy}")
        };
        builder = builder
            .arg(("proxy-server", endpoint.as_str()))
            .arg(("proxy-bypass-list", "<-loopback>"));
    }
    builder = if options.headful {
        builder.with_head()
    } else {
        builder.new_headless_mode()
    };
    builder = builder
        .window_size(options.viewport.width, options.viewport.height)
        .viewport(options.viewport);
    if let Some(user_agent) = options.user_agent {
        builder = builder.arg(("user-agent", user_agent));
    }
    builder
        .build()
        .map_err(|error| format!("failed to build browser config: {error}"))
}

fn browser_profile(profile: BrowserProfile) -> (Viewport, Option<&'static str>) {
    match profile {
        BrowserProfile::Desktop => (
            Viewport {
                width: 1280,
                height: 800,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                is_landscape: true,
                has_touch: false,
            },
            None,
        ),
        BrowserProfile::Mobile => (
            Viewport {
                width: 390,
                height: 844,
                device_scale_factor: Some(3.0),
                emulating_mobile: true,
                is_landscape: false,
                has_touch: true,
            },
            Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
                 AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
            ),
        ),
    }
}

/// Resolves the Chrome/Chromium executable to launch.
///
/// Precedence: explicit `--chrome` override, then the `CHROME` environment
/// variable, then auto-detection on `PATH` and standard install locations.
pub(crate) fn resolve_chrome(
    override_path: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    if let Some(path) = override_path {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!(
                "--chrome path does not point to a file: {}",
                path.display()
            ))
        };
    }
    if let Ok(env_path) = std::env::var("CHROME") {
        let path = std::path::PathBuf::from(&env_path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("CHROME={env_path} does not point to a file"))
        };
    }
    find_chrome()
}

/// Builds a host-only cookie that applies to every path on `url`'s host.
///
/// Scoped by origin rather than by the full URL: only the origin is load-bearing
/// for a host-only cookie, and a full URL would carry the path, query, and any
/// `user:password@` into CDP and into this function's error message.
pub(crate) fn host_cookie(name: &str, value: &str, url: &url::Url) -> Result<CookieParam, String> {
    let origin = url.origin();
    if !origin.is_tuple() {
        return Err(format!(
            "cannot scope cookie `{name}` because the audited URL has no host"
        ));
    }
    let mut cookie = CookieParam::new(name.to_string(), value.to_string());
    cookie.url = Some(origin.ascii_serialization());
    cookie.path = Some("/".to_string());
    cookie.secure = Some(url.scheme() == "https");
    Ok(cookie)
}

fn format_cookie_install_error(name: &str, _error: impl std::fmt::Display) -> String {
    // Do not forward the CDP error: a browser implementation may include the
    // rejected cookie value in its diagnostic.
    format!("failed to set cookie `{name}`")
}

/// Installs host-only, root-scoped cookies before a page has an origin.
pub(crate) async fn set_browser_cookies(
    browser: &Browser,
    cookies: &[(String, String)],
    url: &url::Url,
) -> Result<(), String> {
    for (name, value) in cookies {
        let cookie = host_cookie(name, value, url)?;
        browser
            .set_cookies(vec![cookie])
            .await
            .map_err(|error| format_cookie_install_error(name, error))?;
    }
    Ok(())
}

/// Auto-detects a Chrome/Chromium executable.
///
/// Searches `PATH` by common names first, then well-known per-OS install
/// locations (e.g. the macOS `.app` bundle, which is not on `PATH`).
fn find_chrome() -> Result<std::path::PathBuf, String> {
    if let Some(path) = CHROME_NAMES.iter().find_map(|name| which::which(name).ok()) {
        return Ok(path);
    }
    if let Some(path) = well_known_chrome_paths()
        .into_iter()
        .find(|path| path.is_file())
    {
        return Ok(path);
    }
    Err(format!(
        "could not find Chrome/Chromium on PATH or in standard install locations (looked for: {})",
        CHROME_NAMES.join(", ")
    ))
}

/// Well-known absolute Chrome/Chromium install locations for the host OS.
fn well_known_chrome_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        const APPS: &[&str] = &[
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "Chromium.app/Contents/MacOS/Chromium",
        ];
        for app in APPS {
            paths.push(std::path::PathBuf::from(format!("/Applications/{app}")));
            if let Ok(home) = std::env::var("HOME") {
                paths.push(std::path::PathBuf::from(format!(
                    "{home}/Applications/{app}"
                )));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for path in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ] {
            paths.push(std::path::PathBuf::from(path));
        }
    }

    #[cfg(target_os = "windows")]
    {
        for path in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ] {
            paths.push(std::path::PathBuf::from(path));
        }
    }

    paths
}

impl AuditCollector for BrowserCollector {
    fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String> {
        self.collect_pages(std::slice::from_ref(&request))
            .into_iter()
            .next()
            .expect("should return one result for one browser request")
    }

    fn collect_pages(
        &self,
        requests: &[BrowserCollectRequest],
    ) -> Vec<Result<CollectedPage, String>> {
        if requests.is_empty() {
            return Vec::new();
        }
        // HTTP(S) scheme is enforced by the CLI value parser before we get here.
        let chrome = match resolve_chrome(self.chrome.as_deref()) {
            Ok(chrome) => chrome,
            Err(error) => return vec![Err(error); requests.len()],
        };
        let profile = match tempfile::tempdir() {
            Ok(profile) => profile,
            Err(error) => {
                let error = format!("failed to create browser profile dir: {error}");
                return vec![Err(error); requests.len()];
            }
        };

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let error = format!("failed to build browser runtime: {error}");
                return vec![Err(error); requests.len()];
            }
        };

        let settle = SettleConfig {
            quiet: self.settle_quiet,
            max: self.settle_max,
        };

        let accept_invalid_certs = self.accept_invalid_certs;
        let headful = self.headful;
        let assume_consent = self.assume_consent;
        let proxy = self.proxy.clone();
        let browser_profile = self.profile;
        let request_count = requests.len();
        let requests = requests.to_vec();
        let result = runtime.block_on(async move {
            let options = BrowserSessionOptions {
                chrome: &chrome,
                profile_dir: profile.path(),
                settle,
                accept_invalid_certs,
                headful,
                assume_consent,
                proxy: proxy.as_deref(),
                profile: browser_profile,
            };
            collect(requests, &options).await
        });
        match result {
            Ok(results) => results,
            Err(error) => vec![Err(error); request_count],
        }
    }
}

/// Drives a single page collection on the current-thread runtime.
async fn collect(
    requests: Vec<BrowserCollectRequest>,
    options: &BrowserSessionOptions<'_>,
) -> Result<Vec<Result<CollectedPage, String>>, String> {
    // chromiumoxide defaults to ignoring TLS errors. The audit sends
    // operator-supplied session cookies and treats what it reads back as
    // verification evidence, so a certificate-invalid impersonator could both
    // harvest the session and fabricate the evidence. Validate certificates
    // unless the operator explicitly opts out.
    let (viewport, user_agent) = browser_profile(options.profile);
    let config = build_browser_config(BrowserLaunchOptions {
        chrome: options.chrome,
        profile_dir: options.profile_dir,
        headful: options.headful,
        proxy: options.proxy,
        accept_invalid_certs: options.accept_invalid_certs,
        viewport,
        user_agent,
    })?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|error| format!("failed to launch browser: {error}"))?;

    // Drive the CDP event loop for the duration of the session.
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let mut results = Vec::with_capacity(requests.len());
    for request in requests {
        results.push(
            collect_with_browser(&browser, request, options.settle, options.assume_consent).await,
        );
    }

    // Best-effort teardown; ignore errors since we already have a result, but
    // bound it so a Chrome that ignores `close` cannot hang the command.
    let _ = tokio::time::timeout(BROWSER_CLOSE_TIMEOUT, browser.close()).await;
    let _ = tokio::time::timeout(BROWSER_CLOSE_TIMEOUT, browser.wait()).await;
    handler_task.abort();

    Ok(results)
}

async fn collect_with_browser(
    browser: &Browser,
    request: BrowserCollectRequest,
    settle_config: SettleConfig,
    assume_consent: bool,
) -> Result<CollectedPage, String> {
    set_browser_cookies(browser, &request.cookies, &request.url).await?;

    // Open a blank page first so init scripts are installed before the real
    // document loads (evaluate-on-new-document applies to subsequent navigations).
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|error| format!("failed to open browser page: {error}"))?;

    let result = collect_open_page(&page, &request, settle_config, assume_consent).await;
    let close_result = tokio::time::timeout(BROWSER_CLOSE_TIMEOUT, page.close()).await;

    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(mut collected), Err(_)) => {
            collected.warnings.push(Warning {
                code: "page_close_timeout".to_string(),
                message: "timed out closing the browser tab after collection".to_string(),
            });
            Ok(collected)
        }
        (Ok(mut collected), Ok(Err(error))) => {
            collected.warnings.push(Warning {
                code: "page_close_failed".to_string(),
                message: format!("failed to close the browser tab after collection: {error}"),
            });
            Ok(collected)
        }
        (Ok(collected), Ok(Ok(_))) => Ok(collected),
    }
}

/// Collects from an open tab. The caller owns tab teardown so every return path,
/// including an error from this function, closes the page before continuing.
async fn collect_open_page(
    page: &Page,
    request: &BrowserCollectRequest,
    settle_config: SettleConfig,
    assume_consent: bool,
) -> Result<CollectedPage, String> {
    let mut warnings = Vec::new();

    if assume_consent {
        page.evaluate_on_new_document(CONSENT_STUB_SCRIPT)
            .await
            .map_err(|error| format!("failed to install consent init script: {error}"))?;
        warnings.push(Warning {
            code: "consent_stub_active".to_string(),
            message: "audit consent APIs were stubbed; re-run with --no-assume-consent to observe the publisher CMP without substitution".to_string(),
        });
    }
    page.evaluate_on_new_document("performance.setResourceTimingBufferSize(100000)")
        .await
        .map_err(|error| format!("failed to increase resource timing buffer: {error}"))?;

    for script in &request.init_scripts {
        page.evaluate_on_new_document(script.clone())
            .await
            .map_err(|error| format!("failed to install init script: {error}"))?;
    }

    tokio::time::timeout(NAVIGATION_TIMEOUT, page.goto(request.url.as_str()))
        .await
        .map_err(|_| format!("navigation to {} timed out", request.url))?
        .map_err(|error| format!("failed to navigate to {}: {error}", request.url))?;
    match tokio::time::timeout(NAVIGATION_TIMEOUT, page.wait_for_navigation()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warnings.push(Warning {
            code: "navigation_wait_failed".to_string(),
            message: format!(
                "navigation load event could not be read ({error}); continuing with settled page evidence"
            ),
        }),
        Err(_) => warnings.push(Warning {
            code: "navigation_wait_timeout".to_string(),
            message: format!(
                "navigation did not fire its load event within {} seconds; continuing with settled page evidence",
                NAVIGATION_TIMEOUT.as_secs()
            ),
        }),
    }

    settle(page, settle_config, &mut warnings).await;

    if request.scroll {
        if request.collect_ad_evidence {
            // Snapshot evidence before scrolling so entries already present at
            // initial load keep phase "load"; the store dedups first-seen, so
            // the post-scroll scrape only adds genuinely scroll-phase entries.
            if tokio::time::timeout(
                CDP_OPERATION_TIMEOUT,
                page.evaluate(
                    "(typeof window.__tsCollectAdTemplateEvidence === 'function' \
                     && window.__tsCollectAdTemplateEvidence(), null)",
                ),
            )
            .await
            .is_err()
            {
                warnings.push(Warning {
                    code: "ad_evidence_snapshot_timeout".to_string(),
                    message: "timed out snapshotting ad evidence before scroll".to_string(),
                });
            }
        }
        scroll_page(page, &mut warnings).await;
        settle(page, settle_config, &mut warnings).await;
    }

    let final_url_text = tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.url())
        .await
        .map_err(|_| "timed out reading final page URL".to_string())?
        .map_err(|error| format!("failed to read final page URL: {error}"))?
        .ok_or_else(|| "browser page URL was empty after navigation".to_string())?;
    let final_url = url::Url::parse(&final_url_text).map_err(|error| {
        format!("browser returned invalid final URL `{final_url_text}`: {error}")
    })?;
    let title = match tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.get_title()).await {
        Ok(Ok(title)) => title.unwrap_or_default(),
        Ok(Err(error)) => {
            warnings.push(Warning {
                code: "page_title_failed".to_string(),
                message: format!("failed to read page title: {error}"),
            });
            String::new()
        }
        Err(_) => {
            warnings.push(Warning {
                code: "page_title_timeout".to_string(),
                message: "timed out reading page title".to_string(),
            });
            String::new()
        }
    };
    let script_count = eval_usize(page, "document.querySelectorAll('script').length")
        .await
        .unwrap_or_else(|message| {
            warnings.push(Warning {
                code: "script_count_failed".to_string(),
                message,
            });
            0
        });
    let resource_count = resource_count(page).await.unwrap_or_else(|message| {
        warnings.push(Warning {
            code: "resource_count_failed".to_string(),
            message,
        });
        0
    });

    if resource_count >= 250 {
        warnings.push(Warning {
            code: "resource_timing_heavy".to_string(),
            message: format!("page recorded {resource_count} network resources"),
        });
    }

    if let Ok(Ok(frames)) = tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.frames()).await
        && frames.len() > 1
    {
        warnings.push(Warning {
            code: "child_frames_not_inspected".to_string(),
            message: format!(
                "ad-template evidence inspected only the main frame; {} child frame(s) were present",
                frames.len() - 1
            ),
        });
    }

    let ad_evidence = if request.collect_ad_evidence {
        extract_ad_evidence(page, &mut warnings).await
    } else {
        None
    };

    Ok(CollectedPage {
        final_url,
        title,
        script_count,
        resource_count,
        warnings,
        ad_evidence,
    })
}

/// Waits for the page network to go quiet after navigation or scroll.
///
/// Polls the resource-entry count and returns once it stays unchanged for a
/// quiet window, or when the hard cap elapses — so ad-heavy pages finish loading
/// before evidence is read, without hanging on pages that never go idle.
async fn settle(page: &Page, config: SettleConfig, warnings: &mut Vec<Warning>) {
    let start = std::time::Instant::now();
    let mut last = None;
    let mut quiet_since = None;

    loop {
        if start.elapsed() >= config.max {
            warnings.push(Warning {
                code: "settle_timeout".to_string(),
                message: "page did not settle before the configured maximum wait".to_string(),
            });
            return;
        }

        let ready_state = match eval_string(page, "document.readyState").await {
            Ok(state) => state,
            Err(message) => {
                warnings.push(Warning {
                    code: "settle_read_failed".to_string(),
                    message,
                });
                return;
            }
        };
        let current = match resource_count(page).await {
            Ok(count) => count,
            Err(message) => {
                warnings.push(Warning {
                    code: "settle_read_failed".to_string(),
                    message,
                });
                return;
            }
        };
        let ready = matches!(ready_state.as_str(), "interactive" | "complete");
        if ready && last == Some(current) {
            let quiet_start = quiet_since.get_or_insert_with(std::time::Instant::now);
            if quiet_start.elapsed() >= config.quiet {
                return;
            }
        } else {
            quiet_since = None;
        }
        last = Some(current);

        let remaining_max = config.max.saturating_sub(start.elapsed());
        let remaining_quiet = quiet_since
            .map(|quiet_start| config.quiet.saturating_sub(quiet_start.elapsed()))
            .unwrap_or(config.quiet);
        let sleep_for = Duration::from_millis(SETTLE_POLL_MS)
            .min(remaining_max)
            .min(remaining_quiet.max(Duration::from_millis(1)));
        tokio::time::sleep(sleep_for).await;
    }
}

/// Reads the number of resource timing entries observed so far.
async fn resource_count(page: &Page) -> Result<usize, String> {
    eval_usize(page, "performance.getEntriesByType('resource').length").await
}

/// Performs a deterministic stepped scroll to trigger lazy ad loading.
async fn scroll_page(page: &Page, warnings: &mut Vec<Warning>) {
    // Mark subsequent observations as scroll-phase for the collector.
    eval_discard(page, "window.__tsScrollPhase = true", warnings).await;
    for fraction in ["0.33", "0.66", "1"] {
        let script = format!(
            "window.scrollTo(0, Math.floor(Math.max(document.body.scrollHeight, \
             document.documentElement.scrollHeight) * {fraction}))"
        );
        eval_discard(page, script, warnings).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eval_discard(page, "window.scrollTo(0, 0)", warnings).await;
}

async fn eval_discard(page: &Page, expression: impl Into<String>, warnings: &mut Vec<Warning>) {
    match tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.evaluate(expression.into())).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warnings.push(Warning {
            code: "page_evaluation_failed".to_string(),
            message: format!("browser page evaluation failed: {error}"),
        }),
        Err(_) => warnings.push(Warning {
            code: "page_evaluation_timeout".to_string(),
            message: "browser page evaluation timed out".to_string(),
        }),
    }
}

async fn eval_usize(page: &Page, expression: &str) -> Result<usize, String> {
    tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.evaluate(expression))
        .await
        .map_err(|_| format!("timed out evaluating `{expression}`"))?
        .map_err(|error| format!("failed to evaluate `{expression}`: {error}"))?
        .into_value::<usize>()
        .map_err(|error| format!("failed to decode `{expression}`: {error}"))
}

async fn eval_string(page: &Page, expression: &str) -> Result<String, String> {
    tokio::time::timeout(CDP_OPERATION_TIMEOUT, page.evaluate(expression))
        .await
        .map_err(|_| format!("timed out evaluating `{expression}`"))?
        .map_err(|error| format!("failed to evaluate `{expression}`: {error}"))?
        .into_value::<String>()
        .map_err(|error| format!("failed to decode `{expression}`: {error}"))
}

/// Reads and decodes `window.__tsAdTemplateEvidence`, warning (not failing) on a
/// decode error.
async fn extract_ad_evidence(
    page: &Page,
    warnings: &mut Vec<Warning>,
) -> Option<BrowserAdEvidence> {
    // Serialize and size-check in the page so a hostile publisher-controlled
    // evidence object cannot force an unbounded CDP response and Rust decode.
    let evaluation = tokio::time::timeout(
        CDP_OPERATION_TIMEOUT,
        page.evaluate(format!(
            r#"(() => {{
                    const evidence = typeof window.__tsCollectAdTemplateEvidence === 'function'
                        ? window.__tsCollectAdTemplateEvidence()
                        : (window.__tsAdTemplateEvidence || null)
                    if (evidence === null) return {{ kind: 'absent' }}
                    try {{
                        const json = JSON.stringify(evidence)
                        const bytes = new TextEncoder().encode(json).byteLength
                        if (bytes > {MAX_EVIDENCE_PAYLOAD_BYTES}) return {{ kind: 'too_large' }}
                        return {{ kind: 'evidence', json }}
                    }} catch (error) {{
                        return {{
                            kind: 'serialization_failed',
                            message: String(error).slice(0, 512),
                        }}
                    }}
                }})()"#
        )),
    )
    .await;

    let envelope = match evaluation {
        Ok(Ok(result)) => match result.into_value::<EvidenceEnvelope>() {
            Ok(envelope) => Some(envelope),
            Err(error) => {
                warnings.push(Warning {
                    code: "ad_evidence_decode_failed".to_string(),
                    message: format!("failed to decode ad-template evidence envelope: {error}"),
                });
                return None;
            }
        },
        Ok(Err(error)) => {
            warnings.push(Warning {
                code: "ad_evidence_read_failed".to_string(),
                message: format!("failed to read ad-template evidence: {error}"),
            });
            return None;
        }
        Err(_) => {
            warnings.push(Warning {
                code: "ad_evidence_read_timeout".to_string(),
                message: "timed out reading ad-template evidence".to_string(),
            });
            return None;
        }
    };

    match envelope {
        Some(envelope) => decode_ad_evidence_envelope(envelope, warnings),
        None => {
            warnings.push(Warning {
                code: "ad_evidence_absent".to_string(),
                message: "no ad-template evidence was collected from the page".to_string(),
            });
            None
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EvidenceEnvelope {
    Absent,
    TooLarge,
    Evidence { json: String },
    SerializationFailed { message: String },
}

fn decode_ad_evidence_envelope(
    envelope: EvidenceEnvelope,
    warnings: &mut Vec<Warning>,
) -> Option<BrowserAdEvidence> {
    match envelope {
        EvidenceEnvelope::Absent => {
            warnings.push(Warning {
                code: "ad_evidence_absent".to_string(),
                message: "no ad-template evidence was collected from the page".to_string(),
            });
            None
        }
        EvidenceEnvelope::TooLarge => {
            warnings.push(Warning {
                code: "ad_evidence_too_large".to_string(),
                message: format!(
                    "ad-template evidence exceeded the {MAX_EVIDENCE_PAYLOAD_BYTES}-byte limit"
                ),
            });
            None
        }
        EvidenceEnvelope::SerializationFailed { message } => {
            warnings.push(Warning {
                code: "ad_evidence_encode_failed".to_string(),
                message: format!("failed to serialize ad-template evidence in the page: {message}"),
            });
            None
        }
        EvidenceEnvelope::Evidence { json } => {
            match serde_json::from_str::<BrowserAdEvidence>(&json) {
                Ok(mut evidence) => {
                    // Defense in depth: the injected script caps these lists, but the
                    // page owns that store, so re-cap after decode.
                    evidence.dom_ids.truncate(MAX_EVIDENCE_ENTRIES);
                    evidence.gpt_slots.truncate(MAX_EVIDENCE_ENTRIES);
                    evidence.aps_calls.truncate(MAX_EVIDENCE_ENTRIES);
                    evidence.warnings.truncate(MAX_EVIDENCE_ENTRIES);
                    Some(evidence)
                }
                Err(error) => {
                    warnings.push(Warning {
                        code: "ad_evidence_decode_failed".to_string(),
                        message: format!("failed to decode ad-template evidence: {error}"),
                    });
                    None
                }
            }
        }
    }
}

/// Whether a Chrome/Chromium fixture is available for browser-backed tests.
///
/// Skips optional local runs, but makes the scripted/CI contract fail loudly.
/// Shared with the generation collector's tests so the contract has one
/// definition.
#[cfg(test)]
pub(crate) fn browser_fixture_available() -> bool {
    if resolve_chrome(None).is_ok() {
        return true;
    }
    assert!(
        std::env::var_os("TS_AUDIT_BROWSER_TESTS").is_none(),
        "TS_AUDIT_BROWSER_TESTS requires Chrome/Chromium; set CHROME to its executable"
    );
    false
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;
    use crate::commands::audit::collector::{
        AdTemplateCollectorConfig, build_ad_template_init_script,
    };

    const AD_TEMPLATE_COLLECTOR_JS: &str = include_str!("ad_template_collector.js");

    #[test]
    fn rust_and_javascript_evidence_entry_caps_match() {
        let expected_declaration = format!("const __ts_max_entries = {MAX_EVIDENCE_ENTRIES}");
        assert!(
            AD_TEMPLATE_COLLECTOR_JS
                .lines()
                .any(|line| line.trim() == expected_declaration),
            "should keep the JS cap equal to MAX_EVIDENCE_ENTRIES"
        );
    }

    #[test]
    fn well_known_chrome_paths_are_known_for_this_os() {
        // macOS/Linux/Windows each have candidate paths; guards the cfg branches.
        assert!(
            !well_known_chrome_paths().is_empty(),
            "supported OSes should list candidate Chrome install paths"
        );
    }

    #[test]
    fn oversized_ad_evidence_is_an_explicit_warning() {
        let mut warnings = Vec::new();
        let evidence = decode_ad_evidence_envelope(EvidenceEnvelope::TooLarge, &mut warnings);

        assert!(evidence.is_none());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "ad_evidence_too_large");
    }

    #[test]
    fn supplied_cookie_is_host_only_and_root_scoped() {
        let url =
            url::Url::parse("https://publisher.example/news/story").expect("should parse test URL");
        let cookie = host_cookie("clearance", "token", &url).expect("should build cookie");

        assert!(cookie.domain.is_none(), "host-only cookies omit Domain");
        assert_eq!(cookie.path.as_deref(), Some("/"));
        assert_eq!(
            cookie.url.as_deref(),
            Some("https://publisher.example"),
            "the origin scopes a host-only cookie before first navigation"
        );
        assert_eq!(cookie.secure, Some(true), "HTTPS cookies must be Secure");
    }

    #[test]
    fn cookie_install_error_identifies_name_without_a_value() {
        let error = format_cookie_install_error(
            "datadome",
            "invalid cookie value operator-secret-cookie-value",
        );

        assert_eq!(error, "failed to set cookie `datadome`");
        assert!(!error.contains("operator-secret-cookie-value"));
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn supplied_cookie_reaches_first_navigation() {
        if !browser_fixture_available() {
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind fixture server");
        let address = listener.local_addr().expect("should read fixture address");
        let (request_tx, request_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("should accept browser request");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("should set fixture read timeout");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut chunk = [0_u8; 1024];
                let chunk_len = stream.read(&mut chunk).expect("should read HTTP request");
                assert!(chunk_len > 0, "request should contain complete headers");
                request.extend_from_slice(&chunk[..chunk_len]);
                assert!(
                    request.len() <= 16 * 1024,
                    "request headers should be bounded"
                );
            }
            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .expect("should send captured request");

            let body = b"<!doctype html><title>cookie fixture</title>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("should write fixture headers");
            stream.write_all(body).expect("should write fixture body");
        });

        let collector = BrowserCollector {
            settle_quiet: Duration::from_millis(100),
            settle_max: Duration::from_secs(1),
            ..BrowserCollector::new()
        };
        collector
            .collect_page(BrowserCollectRequest {
                url: url::Url::parse(&format!("http://{address}/"))
                    .expect("should parse fixture URL"),
                init_scripts: Vec::new(),
                scroll: false,
                collect_ad_evidence: false,
                cookies: vec![("clearance".to_string(), "token".to_string())],
            })
            .expect("cookie should be installed before first navigation");

        let request = request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fixture should receive the first navigation");
        assert!(
            request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("cookie")
                        && value
                            .trim()
                            .split(';')
                            .any(|cookie| cookie.trim() == "clearance=token")
                })
            }),
            "first navigation should carry the supplied cookie; request was {request:?}"
        );
    }

    /// A self-contained page that stubs just enough of GPT (no network) for the
    /// collector to observe a defined slot via the wrapped `defineSlot` and the
    /// `getSlots()` scrape.
    const GPT_FIXTURE: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <div id="ad-atf-0"></div>
    <script>
      (function () {
        // A malformed early assignment must not break the collector's setter or
        // prevent a later valid GPT object from being installed.
        window.googletag = { cmd: { malformed: true } }
        var slots = []
        var gt = { cmd: [] }
        gt.defineSlot = function (path, sizes, div) {
          var slot = {
            getAdUnitPath: function () { return path },
            getSlotElementId: function () { return div },
            getSizes: function () {
              return sizes.map(function (p) {
                return { getWidth: function () { return p[0] }, getHeight: function () { return p[1] } }
              })
            },
          }
          slots.push(slot)
          return slot
        }
        gt.pubads = function () { return { getSlots: function () { return slots } } }
        var originalPush = gt.cmd.push.bind(gt.cmd)
        gt.cmd.push = function (cb) { originalPush(cb); cb() }
        window.googletag = gt
        window.googletag.cmd.push(function () {
          // The out-of-u32 pair must be dropped without poisoning the valid slot.
          window.googletag.defineSlot('/123/news/oversized', [[4294967296, 250]], 'ad-atf-bad')
          window.googletag.defineSlot('/123/news/atf', [[300, 250]], 'ad-atf-0')
        })
      })()
    </script>
  </head>
  <body></body>
</html>"#;

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn collects_gpt_slot_from_local_fixture() {
        if !browser_fixture_available() {
            // Browser fixture test requires a local Chrome/Chromium; skipping.
            return;
        }
        let mut fixture = tempfile::Builder::new()
            .suffix(".html")
            .tempfile()
            .expect("should create fixture file");
        fixture
            .write_all(GPT_FIXTURE.as_bytes())
            .expect("should write fixture");
        let url = url::Url::from_file_path(fixture.path()).expect("should build file url");

        let script = build_ad_template_init_script(&AdTemplateCollectorConfig {
            div_prefixes: vec!["ad-atf-".to_string()],
        })
        .expect("should build init script");

        let collector = BrowserCollector::new();
        let page = collector
            .collect_page(BrowserCollectRequest {
                url,
                init_scripts: vec![script],
                scroll: false,
                collect_ad_evidence: true,
                cookies: Vec::new(),
            })
            .expect("should collect fixture page");

        let evidence = page.ad_evidence.expect("fixture should yield ad evidence");
        assert!(
            evidence
                .gpt_slots
                .iter()
                .any(|slot| slot.gam_unit_path == "/123/news/atf"),
            "should capture the defined GPT slot"
        );
        assert!(
            evidence.dom_ids.iter().any(|dom| dom.dom_id == "ad-atf-0"),
            "should capture the configured-prefix DOM id"
        );
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn scroll_pass_keeps_initial_load_phase_for_load_time_evidence() {
        if !browser_fixture_available() {
            // Browser fixture test requires a local Chrome/Chromium; skipping.
            return;
        }
        let mut fixture = tempfile::Builder::new()
            .suffix(".html")
            .tempfile()
            .expect("should create fixture file");
        fixture
            .write_all(GPT_FIXTURE.as_bytes())
            .expect("should write fixture");
        let url = url::Url::from_file_path(fixture.path()).expect("should build file url");

        let script = build_ad_template_init_script(&AdTemplateCollectorConfig {
            div_prefixes: vec!["ad-atf-".to_string()],
        })
        .expect("should build init script");

        let collector = BrowserCollector::new();
        let page = collector
            .collect_page(BrowserCollectRequest {
                url,
                init_scripts: vec![script],
                scroll: true,
                collect_ad_evidence: true,
                cookies: Vec::new(),
            })
            .expect("should collect fixture page");

        // The slot and DOM id exist at load time, so the pre-scroll snapshot
        // must record them as initial-load even though a scroll pass ran.
        let evidence = page.ad_evidence.expect("fixture should yield ad evidence");
        assert!(
            evidence.dom_ids.iter().any(|dom| dom.dom_id == "ad-atf-0"
                && dom.phase == crate::ad_templates::compare::EvidencePhase::InitialLoad),
            "load-time DOM id should keep phase initial_load under --scroll"
        );
        assert!(
            evidence.gpt_slots.iter().any(|slot| {
                slot.gam_unit_path == "/123/news/atf"
                    && slot.phase == crate::ad_templates::compare::EvidencePhase::InitialLoad
            }),
            "load-time GPT slot should keep phase initial_load under --scroll"
        );
    }
}
