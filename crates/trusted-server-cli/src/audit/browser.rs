//! Chrome/Chromium-backed implementation of [`AuditCollector`] using
//! `chromiumoxide` (CDP).
//!
//! The collector is read-only: it installs optional pre-navigation init scripts,
//! navigates, waits for the page to settle, optionally scrolls, and reads back a
//! bounded set of evidence. It never captures page HTML, cookies, or storage.

use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt as _;

use crate::ad_templates::compare::BrowserAdEvidence;
use crate::ad_templates::output::Warning;
use crate::audit::collector::{AuditCollector, BrowserCollectRequest, BrowserOpts, CollectedPage};

/// Candidate Chrome/Chromium executable names searched on `PATH`.
const CHROME_NAMES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
];

/// Poll interval while waiting for the page network to settle, in milliseconds.
const SETTLE_POLL_MS: u64 = 250;
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

/// A `chromiumoxide`-backed page collector launching a local Chrome/Chromium.
#[derive(Debug, Clone)]
pub struct BrowserCollector {
    /// Explicit Chrome/Chromium executable override (else `$CHROME`, else auto-detect).
    chrome: Option<std::path::PathBuf>,
    /// Quiet window marking the page settled.
    settle_quiet: Duration,
    /// Hard cap on settling.
    settle_max: Duration,
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
        }
    }

    /// Creates a collector from operator-supplied browser options.
    #[must_use]
    pub fn from_opts(opts: &BrowserOpts) -> Self {
        Self {
            chrome: opts.chrome.clone(),
            settle_quiet: Duration::from_millis(opts.settle_quiet_ms),
            settle_max: Duration::from_millis(opts.settle_max_ms),
        }
    }
}

/// Resolves the Chrome/Chromium executable to launch.
///
/// Precedence: explicit `--chrome` override, then the `CHROME` environment
/// variable, then auto-detection on `PATH` and standard install locations.
fn resolve_chrome(override_path: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
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
        // HTTP(S) scheme is enforced by the CLI value parser before we get here.
        let chrome = resolve_chrome(self.chrome.as_deref())?;
        let profile = tempfile::tempdir()
            .map_err(|error| format!("failed to create browser profile dir: {error}"))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to build browser runtime: {error}"))?;

        let settle = SettleConfig {
            quiet: self.settle_quiet,
            max: self.settle_max,
        };

        // chromiumoxide logs unparseable CDP events at WARN (its bundled CDP schema
        // lags newer Chrome). These are benign; quiet them for the browser session
        // so audit output stays clean, then restore the prior threshold.
        let previous_level = log::max_level();
        log::set_max_level(log::LevelFilter::Error);
        let result = runtime
            .block_on(async move { collect(&chrome, profile.path(), request, settle).await });
        log::set_max_level(previous_level);
        result
    }
}

/// Drives a single page collection on the current-thread runtime.
async fn collect(
    chrome: &std::path::Path,
    profile_dir: &std::path::Path,
    request: BrowserCollectRequest,
    settle_config: SettleConfig,
) -> Result<CollectedPage, String> {
    let config = BrowserConfig::builder()
        .chrome_executable(chrome)
        .user_data_dir(profile_dir)
        .build()
        .map_err(|error| format!("failed to build browser config: {error}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|error| format!("failed to launch browser: {error}"))?;

    // Drive the CDP event loop for the duration of the session.
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let result = collect_with_browser(&browser, request, settle_config).await;

    // Best-effort teardown; ignore errors since we already have a result.
    let _ = browser.close().await;
    let _ = browser.wait().await;
    handler_task.abort();

    result
}

async fn collect_with_browser(
    browser: &Browser,
    request: BrowserCollectRequest,
    settle_config: SettleConfig,
) -> Result<CollectedPage, String> {
    let mut warnings = Vec::new();

    // Open a blank page first so init scripts are installed before the real
    // document loads (evaluate-on-new-document applies to subsequent navigations).
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|error| format!("failed to open browser page: {error}"))?;

    for script in &request.init_scripts {
        page.evaluate_on_new_document(script.clone())
            .await
            .map_err(|error| format!("failed to install init script: {error}"))?;
    }

    page.goto(request.url.as_str())
        .await
        .map_err(|error| format!("failed to navigate to {}: {error}", request.url))?;
    page.wait_for_navigation()
        .await
        .map_err(|error| format!("failed to read main document navigation response: {error}"))?;

    settle(&page, settle_config).await;

    if request.scroll {
        scroll_page(&page).await;
        settle(&page, settle_config).await;
    }

    let final_url = match page.url().await {
        Ok(Some(url)) => url::Url::parse(&url).unwrap_or_else(|_| request.url.clone()),
        _ => request.url.clone(),
    };
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    let script_count = eval_usize(&page, "document.querySelectorAll('script').length").await;
    let resource_count = resource_count(&page).await;

    let ad_evidence = if request.collect_ad_evidence {
        extract_ad_evidence(&page, &mut warnings).await
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
async fn settle(page: &Page, config: SettleConfig) {
    let start = std::time::Instant::now();
    let quiet_target = config.quiet;
    let max = config.max;
    let mut last = resource_count(page).await;
    let mut quiet = Duration::ZERO;
    while start.elapsed() < max {
        tokio::time::sleep(Duration::from_millis(SETTLE_POLL_MS)).await;
        let current = resource_count(page).await;
        if current == last {
            quiet += Duration::from_millis(SETTLE_POLL_MS);
            if quiet >= quiet_target {
                break;
            }
        } else {
            quiet = Duration::ZERO;
            last = current;
        }
    }
}

/// Reads the number of resource timing entries observed so far.
async fn resource_count(page: &Page) -> usize {
    eval_usize(page, "performance.getEntriesByType('resource').length").await
}

/// Performs a deterministic stepped scroll to trigger lazy ad loading.
async fn scroll_page(page: &Page) {
    // Mark subsequent observations as scroll-phase for the collector.
    let _ = page.evaluate("window.__tsScrollPhase = true").await;
    for fraction in ["0.33", "0.66", "1"] {
        let script = format!(
            "window.scrollTo(0, Math.floor(Math.max(document.body.scrollHeight, \
             document.documentElement.scrollHeight) * {fraction}))"
        );
        let _ = page.evaluate(script).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = page.evaluate("window.scrollTo(0, 0)").await;
}

/// Evaluates a numeric expression, returning 0 on any failure.
async fn eval_usize(page: &Page, expression: &str) -> usize {
    page.evaluate(expression)
        .await
        .ok()
        .and_then(|result| result.into_value::<usize>().ok())
        .unwrap_or(0)
}

/// Reads and decodes `window.__tsAdTemplateEvidence`, warning (not failing) on a
/// decode error.
async fn extract_ad_evidence(
    page: &Page,
    warnings: &mut Vec<Warning>,
) -> Option<BrowserAdEvidence> {
    // Trigger the on-demand DOM + getSlots scrape, then read the evidence object.
    let value = page
        .evaluate(
            "(typeof window.__tsCollectAdTemplateEvidence === 'function' \
             ? window.__tsCollectAdTemplateEvidence() \
             : (window.__tsAdTemplateEvidence || null))",
        )
        .await
        .ok()
        .and_then(|result| result.into_value::<serde_json::Value>().ok());

    match value {
        Some(serde_json::Value::Null) | None => {
            warnings.push(Warning {
                code: "ad_evidence_absent".to_string(),
                message: "no ad-template evidence was collected from the page".to_string(),
            });
            None
        }
        Some(value) => match serde_json::from_value::<BrowserAdEvidence>(value) {
            Ok(evidence) => Some(evidence),
            Err(error) => {
                warnings.push(Warning {
                    code: "ad_evidence_decode_failed".to_string(),
                    message: format!("failed to decode ad-template evidence: {error}"),
                });
                None
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::audit::collector::{build_ad_template_init_script, AdTemplateCollectorConfig};

    /// Whether a local Chrome/Chromium is available to run browser fixture tests.
    fn chrome_available() -> bool {
        find_chrome().is_ok()
    }

    #[test]
    fn well_known_chrome_paths_are_known_for_this_os() {
        // macOS/Linux/Windows each have candidate paths; guards the cfg branches.
        assert!(
            !well_known_chrome_paths().is_empty(),
            "supported OSes should list candidate Chrome install paths"
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
          window.googletag.defineSlot('/123/news/atf', [[300, 250]], 'ad-atf-0')
        })
      })()
    </script>
  </head>
  <body></body>
</html>"#;

    #[test]
    fn collects_gpt_slot_from_local_fixture() {
        if !chrome_available() {
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
            aps_slot_ids: Vec::new(),
        })
        .expect("should build init script");

        let collector = BrowserCollector::new();
        let page = collector
            .collect_page(BrowserCollectRequest {
                url,
                init_scripts: vec![script],
                scroll: false,
                collect_ad_evidence: true,
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
}
