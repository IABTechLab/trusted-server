use std::path::PathBuf;
use std::time::Duration;

use chromiumoxide::ArcHttpRequest;
use chromiumoxide::browser::Browser;
use chromiumoxide::handler::viewport::Viewport;
use futures::StreamExt as _;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::runtime::Builder;
use tokio::time::{sleep, timeout};
use url::Url;

use crate::commands::audit::browser::{
    BrowserLaunchOptions, CONSENT_STUB_SCRIPT as SHARED_CONSENT_STUB_SCRIPT, build_browser_config,
    resolve_chrome, set_browser_cookies,
};
use crate::commands::audit::browser_scroll::{self, CDP_OPERATION_TIMEOUT};
use crate::commands::audit::collector::{
    GENERATE_SETTLE_MAX_MS, GENERATE_SETTLE_QUIET_MS, GenerateBrowserOpts,
};
use crate::commands::audit::generate::collector::{
    AuditCollector, CONSENT_STUB_WARNING, CollectedGptSlot, CollectedLink, CollectedPage,
    CollectedRequest, CollectedScriptTag, CollectionProgress, ControlFlow, PageSink, ProgressSink,
    RootPlanner,
};
use crate::error::{CliResult, report_error};

const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long to wait for the navigation `load` event (and, separately, the main
/// document response) before falling through to the settle loop. Ad-heavy pages
/// (video players, continuous ad refresh) may never fire `load`, so this is a
/// soft bound: the settle loop is the real readiness signal and the scrape reads
/// whatever rendered by then.
const NAVIGATION_LOAD_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Size the page's resource-timing buffer is raised to before navigation, and
/// therefore also the count at which the buffer is full and entries were lost.
/// One constant so the script and the warning threshold cannot drift apart.
const RESOURCE_TIMING_BUFFER_SIZE: usize = 100_000;
const RESOURCE_TIMING_BUFFER_WARNING: &str = "browser resource timing buffer reached its configured size; some network assets may be missing";

/// A device the crawl can emulate.
///
/// Publishers routinely serve different GAM ad units per device
/// (`/network/desktop/news` vs `/network/mobile/news`). A single-profile crawl
/// cannot see that, so it would infer a template that is right for the profile
/// it used and silently wrong for every other impression. Crawling twice makes
/// the disagreement visible: the two profiles produce two ad-unit paths for the
/// same page, which template inference already treats as unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceProfile {
    /// A desktop viewport with Chrome's own user agent.
    Desktop,
    /// A phone viewport with touch and a mobile user agent.
    Mobile,
}

impl DeviceProfile {
    /// The operator-facing name, matching the `--profiles` value.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
        }
    }

    /// Parses a `--profiles` value.
    ///
    /// # Errors
    ///
    /// Returns an error naming the accepted values when `raw` is not one.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "desktop" => Ok(Self::Desktop),
            "mobile" => Ok(Self::Mobile),
            other => Err(format!(
                "unknown device profile `{other}` (expected desktop or mobile)"
            )),
        }
    }

    /// The viewport to emulate.
    fn viewport(self) -> Viewport {
        match self {
            Self::Desktop => Viewport {
                width: 1280,
                height: 800,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                is_landscape: true,
                has_touch: false,
            },
            Self::Mobile => Viewport {
                width: 390,
                height: 844,
                device_scale_factor: Some(3.0),
                emulating_mobile: true,
                is_landscape: false,
                has_touch: true,
            },
        }
    }

    /// The user agent override, or `None` to keep Chrome's own.
    ///
    /// Ad stacks branch on the user agent as well as the viewport, so emulating
    /// the viewport alone can still yield desktop ad units on a phone-sized page.
    fn user_agent(self) -> Option<&'static str> {
        match self {
            Self::Desktop => None,
            Self::Mobile => Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
                 AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
            ),
        }
    }
}

/// Collects pages through a local Chrome, emulating one device profile.
#[derive(Debug, Clone)]
pub(crate) struct BrowserAuditCollector {
    profile: Option<DeviceProfile>,
    /// Pause between page loads during a crawl.
    page_delay: Duration,
    /// Perform a deterministic scroll pass after the initial settle.
    scroll: bool,
    /// Run a visible browser instead of a headless one.
    headful: bool,
    /// Answer the consent APIs as a consenting reader.
    assume_consent: bool,
    /// Route the browser through this proxy, as `host:port`.
    proxy: Option<String>,
    /// Accept TLS certificates that do not validate.
    accept_invalid_certs: bool,
    /// Explicit Chrome executable, ahead of `$CHROME` and discovery.
    chrome: Option<PathBuf>,
    /// Quiet interval and hard settle cap shared with verification.
    settle_quiet: Duration,
    settle_max: Duration,
}

impl Default for BrowserAuditCollector {
    fn default() -> Self {
        Self {
            profile: None,
            page_delay: Duration::ZERO,
            scroll: false,
            headful: false,
            assume_consent: true,
            proxy: None,
            accept_invalid_certs: false,
            chrome: None,
            settle_quiet: Duration::from_millis(GENERATE_SETTLE_QUIET_MS),
            settle_max: Duration::from_millis(GENERATE_SETTLE_MAX_MS),
        }
    }
}

impl BrowserAuditCollector {
    /// Applies the browser options shared by generate, verify, and page audit.
    #[must_use]
    pub(crate) fn with_browser_options(mut self, options: &GenerateBrowserOpts) -> Self {
        self.chrome.clone_from(&options.chrome);
        self.headful = options.headful;
        self.assume_consent = !options.no_assume_consent;
        self.proxy.clone_from(&options.browser_proxy);
        self.accept_invalid_certs = options.danger_accept_invalid_certs;
        self.settle_quiet = Duration::from_millis(options.settle_quiet_ms);
        self.settle_max = Duration::from_millis(options.settle_max_ms);
        self
    }

    /// A collector emulating `profile`.
    #[must_use]
    pub(crate) fn with_profile(profile: DeviceProfile) -> Self {
        Self {
            profile: Some(profile),
            ..Self::default()
        }
    }

    /// Sets the pause between page loads.
    ///
    /// A crawl issues a dozen navigations in a row. Firing them back to back is
    /// both discourteous to the origin and self-defeating: request pacing is one
    /// of the signals bot protection scores, so an unpaced crawl invites the
    /// challenge that empties the rest of the run.
    #[must_use]
    pub(crate) fn with_page_delay(mut self, delay: Duration) -> Self {
        self.page_delay = delay;
        self
    }

    /// Enables or disables the deterministic scroll pass for every page.
    #[must_use]
    pub(crate) fn with_scroll(mut self, scroll: bool) -> Self {
        self.scroll = scroll;
        self
    }
}

/// The browser-session knobs one crawl runs under.
#[derive(Debug, Clone)]
struct SessionSettings {
    profile: Option<DeviceProfile>,
    page_delay: Duration,
    scroll: bool,
    headful: bool,
    assume_consent: bool,
    proxy: Option<String>,
    accept_invalid_certs: bool,
    chrome: Option<PathBuf>,
    settle_quiet: Duration,
    settle_max: Duration,
}

/// Per-page behavior shared by every tab in one browser session.
#[derive(Debug, Clone, Copy)]
struct PageCollectionSettings {
    assume_consent: bool,
    scroll: bool,
    settle_quiet: Duration,
    settle_max: Duration,
}

impl BrowserAuditCollector {
    fn session(&self) -> SessionSettings {
        SessionSettings {
            profile: self.profile,
            page_delay: self.page_delay,
            scroll: self.scroll,
            headful: self.headful,
            assume_consent: self.assume_consent,
            proxy: self.proxy.clone(),
            accept_invalid_certs: self.accept_invalid_certs,
            chrome: self.chrome.clone(),
            settle_quiet: self.settle_quiet,
            settle_max: self.settle_max,
        }
    }
}

fn ignore_collection_progress(_: CollectionProgress<'_>) -> CliResult<()> {
    Ok(())
}

impl AuditCollector for BrowserAuditCollector {
    fn collect_page(
        &self,
        target_url: &Url,
        cookies: &[(String, String)],
    ) -> CliResult<CollectedPage> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                report_error(format!(
                    "failed to build Tokio runtime for browser audit: {error}"
                ))
            })?;

        let settings = self.session();
        runtime.block_on(async {
            let mut collected = None;
            let mut ignore_progress = ignore_collection_progress;
            with_browser(
                vec![target_url.clone()],
                cookies,
                settings,
                None,
                &mut ignore_progress,
                &mut |_, result| {
                    collected = Some(result);
                    Ok(ControlFlow::Stop)
                },
            )
            .await?;
            collected.unwrap_or_else(|| Err(report_error("browser session produced no page")))
        })
    }

    fn collect_pages(
        &self,
        targets: &[Url],
        cookies: &[(String, String)],
        on_progress: ProgressSink<'_>,
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        if targets.is_empty() {
            return Ok(());
        }
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                report_error(format!(
                    "failed to build Tokio runtime for browser audit: {error}"
                ))
            })?;

        // Keep synchronous HTML analysis out of the current-thread runtime that
        // drives Chromium's websocket event pump. Collect browser results first,
        // close the session, then hand them to the caller for parsing.
        let mut collected_pages = Vec::with_capacity(targets.len());
        runtime.block_on(with_browser(
            targets.to_vec(),
            cookies,
            self.session(),
            None,
            on_progress,
            &mut |url, result| {
                collected_pages.push((url.clone(), result));
                Ok(ControlFlow::Continue)
            },
        ))?;
        for (url, collected) in collected_pages {
            if on_page(&url, collected)? == ControlFlow::Stop {
                break;
            }
        }
        Ok(())
    }

    fn collect_site(
        &self,
        root: &Url,
        cookies: &[(String, String)],
        on_progress: ProgressSink<'_>,
        planner: RootPlanner<'_>,
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                report_error(format!(
                    "failed to build Tokio runtime for browser audit: {error}"
                ))
            })?;
        let mut collected_pages = Vec::new();
        runtime.block_on(with_browser(
            vec![root.clone()],
            cookies,
            self.session(),
            Some(planner),
            on_progress,
            &mut |url, result| {
                collected_pages.push((url.clone(), result));
                Ok(ControlFlow::Continue)
            },
        ))?;
        for (url, collected) in collected_pages {
            if on_page(&url, collected)? == ControlFlow::Stop {
                break;
            }
        }
        Ok(())
    }
}

/// Launches one browser, walks `targets` on it, and hands each result to `sink`.
///
/// One launch for the whole crawl rather than one per page: a cold Chrome start
/// plus a fresh profile dominates the cost of a multi-page run. The shared
/// profile is also load-bearing — a bot-protection clearance cookie earned on
/// the first page carries to the rest of the crawl, which is what makes a
/// multi-section walk of a protected site viable at all. The tradeoff is that
/// paywall meters and personalization also accumulate across the run.
async fn with_browser(
    mut targets: Vec<Url>,
    cookies: &[(String, String)],
    settings: SessionSettings,
    mut root_planner: Option<RootPlanner<'_>>,
    on_progress: ProgressSink<'_>,
    sink: PageSink<'_>,
) -> CliResult<()> {
    let SessionSettings {
        profile,
        page_delay,
        scroll,
        headful,
        assume_consent,
        proxy,
        accept_invalid_certs,
        chrome,
        settle_quiet,
        settle_max,
    } = settings;
    let chrome_executable = resolve_chrome(chrome.as_deref()).map_err(report_error)?;
    let user_data_dir = TempDir::new().map_err(|error| {
        report_error(format!(
            "failed to create temporary browser profile for audit: {error}"
        ))
    })?;
    let profile = profile.unwrap_or(DeviceProfile::Desktop);
    let config = build_browser_config(BrowserLaunchOptions {
        chrome: &chrome_executable,
        profile_dir: user_data_dir.path(),
        headful,
        proxy: proxy.as_deref(),
        accept_invalid_certs,
        viewport: profile.viewport(),
        user_agent: profile.user_agent(),
    })
    .map_err(report_error)?;

    on_progress(CollectionProgress::Launching)?;
    let (mut browser, mut handler) = Browser::launch(config).await.map_err(|error| {
        report_error(format!(
            "failed to launch Chrome/Chromium for audit: {error}"
        ))
    })?;

    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page_settings = PageCollectionSettings {
        assume_consent,
        scroll,
        settle_quiet,
        settle_max,
    };

    // Sitemap discovery is a whole-site fact, so only the first target pays for it.
    let mut result: CliResult<()> = Ok(());
    let mut index = 0;
    while index < targets.len() {
        let target = targets[index].clone();
        let total = if index == 0 && root_planner.is_some() {
            None
        } else {
            Some(targets.len())
        };
        // Pace the crawl before announcing the page, so the progress line marks
        // the navigation rather than the start of the wait. Back-to-back
        // navigations are both discourteous to the origin and a signal bot
        // protection scores against the session.
        if index > 0 && !page_delay.is_zero() {
            sleep(page_delay).await;
        }
        if let Err(error) = on_progress(CollectionProgress::Loading {
            current: index + 1,
            total,
            url: &target,
        }) {
            result = Err(error);
            break;
        }
        let collected =
            collect_page_from_browser(&mut browser, &target, cookies, index == 0, page_settings)
                .await;
        if index == 0
            && let Some(planner) = root_planner.as_deref_mut()
            && let Ok(root_page) = &collected
        {
            if let Err(error) = on_progress(CollectionProgress::Planning) {
                result = Err(error);
                break;
            }
            match planner(&target, root_page) {
                Ok(planned) => targets.extend(planned),
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        match sink(&target, collected) {
            Ok(ControlFlow::Continue) => {}
            Ok(ControlFlow::Stop) => break,
            Err(error) => {
                result = Err(error);
                break;
            }
        }
        index += 1;
    }

    let finalization_result = on_progress(CollectionProgress::Finalizing);
    let close_result = timeout(BROWSER_CLOSE_TIMEOUT, browser.close())
        .await
        .map_err(|_| report_error("timed out closing browser after audit"))
        .and_then(|closed| {
            closed.map(|_| ()).map_err(|error| {
                report_error(format!("failed to close browser after audit: {error}"))
            })
        });
    // Reap the child even when the CDP close request failed or timed out. Give
    // waiting its own budget so a slow close cannot consume the entire teardown
    // window and leave chromiumoxide's drop handler to kill the process.
    let wait_result = timeout(BROWSER_CLOSE_TIMEOUT, browser.wait())
        .await
        .map_err(|_| report_error("timed out waiting for browser process to exit after audit"))
        .and_then(|waited| {
            waited.map(|_| ()).map_err(|error| {
                report_error(format!(
                    "failed waiting for browser process to exit after audit: {error}"
                ))
            })
        });
    handler_task.abort();
    let _ = handler_task.await;

    combine_browser_run_results(result, finalization_result, close_result, wait_result)
}

/// Combines already-attempted browser phases, preserving the first error.
fn combine_browser_run_results(
    run_result: CliResult<()>,
    finalization_result: CliResult<()>,
    close_result: CliResult<()>,
    wait_result: CliResult<()>,
) -> CliResult<()> {
    run_result
        .and(finalization_result)
        .and(close_result)
        .and(wait_result)
}

/// Collects one page on an already-launched browser.
///
/// `discover_sitemap` runs the `robots.txt`/sitemap fetch from inside this
/// page's context. It is meaningful only once per crawl (the site's sitemap does
/// not change per page), so callers pass `true` for the root page only.
async fn collect_page_from_browser(
    browser: &mut Browser,
    target_url: &Url,
    cookies: &[(String, String)],
    discover_sitemap: bool,
    settings: PageCollectionSettings,
) -> CliResult<CollectedPage> {
    // Per-page failures below return the message unlogged: the crawl attributes
    // each one to its page once, and `report_error` would also log an unscoped
    // duplicate in the middle of progress output.
    set_browser_cookies(browser, cookies, target_url).await?;

    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|error| format!("failed to create browser page for audit: {error}"))?;

    let result = collect_open_page(&page, target_url, discover_sitemap, settings).await;
    let close_result = timeout(BROWSER_CLOSE_TIMEOUT, page.close()).await;

    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(mut collected), Err(_)) => {
            collected.warnings.push(
                "page_close_timeout: timed out closing browser tab after page collection"
                    .to_string(),
            );
            Ok(collected)
        }
        (Ok(mut collected), Ok(Err(error))) => {
            collected.warnings.push(format!(
                "page_close_failed: failed to close browser tab after page collection: {error}"
            ));
            Ok(collected)
        }
        (Ok(collected), Ok(Ok(_))) => Ok(collected),
    }
}

/// Collects one open tab; its caller closes the tab on every return path.
async fn collect_open_page(
    page: &chromiumoxide::Page,
    target_url: &Url,
    discover_sitemap: bool,
    settings: PageCollectionSettings,
) -> CliResult<CollectedPage> {
    let mut warnings = Vec::new();

    // Must run before any page script, so the consent platform finds the APIs
    // already answered rather than installing its own gate.
    if settings.assume_consent {
        page.evaluate_on_new_document(SHARED_CONSENT_STUB_SCRIPT)
            .await
            .map_err(|error| format!("failed to install the consent stub: {error}"))?;
        warnings.push(CONSENT_STUB_WARNING.to_string());
    }
    page.evaluate_on_new_document(format!(
        "performance.setResourceTimingBufferSize({RESOURCE_TIMING_BUFFER_SIZE})"
    ))
    .await
    .map_err(|error| format!("failed to increase the resource timing buffer: {error}"))?;

    // Navigate, but don't hard-fail when the `load` event never fires. Ad-heavy
    // pages (video players, continuous ad refresh, anti-bot scripts) can keep
    // the frame "loading" indefinitely, so a load-wait timeout is downgraded to
    // a warning: the settle loop below is the real readiness signal and the
    // scrape reads whatever rendered by then.
    match timeout(NAVIGATION_LOAD_TIMEOUT, page.goto(target_url.as_str())).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warnings.push(format!(
            "navigation to `{target_url}` did not complete cleanly ({error}); results may be partial"
        )),
        Err(_) => warnings.push(format!(
            "navigation to `{target_url}` did not fire `load` within {}s; results may be partial",
            NAVIGATION_LOAD_TIMEOUT.as_secs()
        )),
    }

    // Best-effort read of the main-document response for status validation. When
    // the load wait above times out the response is usually already buffered, so
    // this returns promptly; tolerate a miss rather than failing the audit.
    match timeout(NAVIGATION_LOAD_TIMEOUT, page.wait_for_navigation_response()).await {
        Ok(Ok(navigation_response)) => {
            if let Some(warning) = validate_navigation_response(navigation_response)? {
                warnings.push(warning);
            }
        }
        Ok(Err(error)) => warnings.push(format!(
            "could not read the main document response from `{target_url}` ({error}); results may be partial"
        )),
        Err(_) => warnings.push(format!(
            "timed out reading the main document response from `{target_url}`; results may be partial"
        )),
    }

    // Settle phases share one clock, with a quiet-window floor for GPT polling.
    // Navigation has its own timeout; scrolling consumes the settle budget.
    // Read GPT before metadata so extraction cannot starve its polling. DOM and
    // network evidence consequently reflect the page after the GPT wait.
    let settle_start = std::time::Instant::now();
    if !wait_for_page_settle(page, settings.settle_quiet, settings.settle_max).await? {
        warnings.push(
            "browser audit timed out while waiting for the page to settle; results may be partial"
                .to_string(),
        );
    }

    if settings.scroll {
        warnings.extend(
            browser_scroll::scroll_page(page)
                .await
                .into_iter()
                .map(|failure| failure.to_string()),
        );
        let remaining = settings.settle_max.saturating_sub(settle_start.elapsed());
        if remaining.is_zero() {
            warnings.push(
                "settle budget was exhausted before the post-scroll settle; \
                 post-scroll evidence may be missing; raise `--settle-max-ms`"
                    .to_string(),
            );
        } else if !wait_for_page_settle(page, settings.settle_quiet, remaining).await? {
            warnings.push(
                "browser audit timed out while waiting for the page to settle after scroll; \
                 results may be partial"
                    .to_string(),
            );
        }
    }

    // GPT can finish registering slots after the document and resource stream
    // are otherwise quiet. Wait for a non-empty registry to stabilize instead
    // of treating the first empty read as authoritative.
    //
    // The ad-template verifier needs no counterpart wait: its evidence
    // collector wraps `googletag.defineSlot` before publisher scripts run and
    // accumulates every slot defined before the read. This command reads a
    // `getSlots()` snapshot instead and can observe a half-registered registry.
    // `ts audit page` also takes a snapshot, but reports what it saw rather
    // than generating config from it.
    // Initial settling and scrolling can exhaust the shared budget. Allow at
    // least one quiet window of GPT polling to capture later batches, while
    // retaining the partial-evidence warning if a full dwell cannot finish.
    let gpt_budget = settings
        .settle_max
        .saturating_sub(settle_start.elapsed())
        .max(settings.settle_quiet);
    let gpt_slots =
        collect_stable_gpt_slots(page, settings.settle_quiet, gpt_budget, &mut warnings).await;

    match timeout(CDP_OPERATION_TIMEOUT, page.frames()).await {
        Ok(Ok(frames)) if frames.len() > 1 => warnings.push(format!(
            "browser evidence inspects only the main frame; {} child frame(s) were present",
            frames.len() - 1
        )),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warnings.push(format!("failed to inspect browser frames: {error}")),
        Err(_) => warnings.push("timed out inspecting browser frames".to_string()),
    }

    let final_url = timeout(CDP_OPERATION_TIMEOUT, page.url())
        .await
        .map_err(|_| "timed out reading final page URL".to_string())?
        .map_err(|error| format!("failed to read final page URL: {error}"))?
        .ok_or("browser page URL was empty after navigation")?;
    let page_title = timeout(CDP_OPERATION_TIMEOUT, page.get_title())
        .await
        .map_err(|_| "timed out reading page title".to_string())?
        .map_err(|error| format!("failed to read page title: {error}"))?;
    let html = timeout(CDP_OPERATION_TIMEOUT, page.content())
        .await
        .map_err(|_| "timed out reading rendered page HTML".to_string())?
        .map_err(|error| format!("failed to read rendered page HTML: {error}"))?;

    let script_tags: Vec<BrowserScriptTag> = timeout(
        CDP_OPERATION_TIMEOUT,
        page.evaluate(
            r#"() => Array.from(document.scripts).map((script) => ({
                src: script.src || null,
                inline_text: script.src ? null : (script.textContent || null),
            }))"#,
        ),
    )
    .await
    .map_err(|_| "timed out reading rendered script tags".to_string())?
    .map_err(|error| format!("failed to read rendered script tags: {error}"))?
    .into_value()
    .map_err(|error| format!("failed to decode rendered script tag data: {error}"))?;

    let network_requests: Vec<BrowserPerformanceEntry> = timeout(
        CDP_OPERATION_TIMEOUT,
        page.evaluate(
            r#"() => performance.getEntriesByType('resource').map((entry) => ({
                url: entry.name,
                initiator_type: entry.initiatorType || null,
            }))"#,
        ),
    )
    .await
    .map_err(|_| "timed out reading browser performance entries".to_string())?
    .map_err(|error| format!("failed to read browser performance resource entries: {error}"))?
    .into_value()
    .map_err(|error| format!("failed to decode browser performance resource data: {error}"))?;

    if let Some(warning) = resource_timing_buffer_warning(network_requests.len()) {
        warnings.push(warning.to_string());
    }

    // Links come from the hydrated DOM, not the served markup: an app-router
    // page keeps its link graph in the framework payload, so parsing the raw
    // HTML finds only a fraction of the site's sections. Best-effort — an empty
    // list just means crawl planning falls back to other sources.
    // When the registry is empty, report what GPT actually looked like. An
    // empty registry has several very different causes — the library never
    // loaded, it loaded but the command queue never drained, or slots really
    // are absent — and the operator's next move differs for each.
    if gpt_slots.is_empty() {
        match timeout(CDP_OPERATION_TIMEOUT, page.evaluate(GPT_DIAGNOSTIC_SCRIPT)).await {
            Ok(Ok(result)) => match result.into_value::<serde_json::Value>() {
                Ok(state) => warnings.push(format!(
                    "no GPT slots in the registry; googletag state: {state}"
                )),
                Err(error) => warnings.push(format!("failed to decode GPT diagnostics: {error}")),
            },
            Ok(Err(error)) => warnings.push(format!("failed to evaluate GPT diagnostics: {error}")),
            Err(_) => warnings.push("timed out evaluating GPT diagnostics".to_string()),
        }
    }

    let links: Vec<CollectedLink> =
        match timeout(CDP_OPERATION_TIMEOUT, page.evaluate(LINKS_SCRIPT)).await {
            Ok(Ok(result)) => match result.into_value() {
                Ok(links) => links,
                Err(error) => {
                    warnings.push(format!("failed to decode page links: {error}"));
                    Vec::new()
                }
            },
            Ok(Err(error)) => {
                warnings.push(format!("failed to evaluate page links: {error}"));
                Vec::new()
            }
            Err(_) => {
                warnings.push("timed out evaluating page links".to_string());
                Vec::new()
            }
        };

    // Sitemap discovery is a whole-site fact, so it runs once per crawl. A miss
    // is normal (no sitemap, robots 404, fetch blocked) and leaves planning to
    // the link graph alone.
    let mut sitemap_locs: Vec<String> = if discover_sitemap {
        let evaluation = chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams::builder()
            .function_declaration(SITEMAP_SCRIPT)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .map_err(|error| format!("failed to build sitemap evaluation: {error}"))?;
        match timeout(CDP_OPERATION_TIMEOUT, page.evaluate(evaluation)).await {
            Ok(Ok(result)) => match result.into_value() {
                Ok(locations) => locations,
                Err(error) => {
                    warnings.push(format!("failed to decode sitemap locations: {error}"));
                    Vec::new()
                }
            },
            Ok(Err(error)) => {
                warnings.push(format!("failed to evaluate sitemap discovery: {error}"));
                Vec::new()
            }
            Err(_) => {
                warnings.push("timed out discovering sitemap locations".to_string());
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    sitemap_locs.truncate(MAX_SITEMAP_LOCS);
    if discover_sitemap && sitemap_locs.is_empty() {
        warnings.push(
            "no sitemap was reachable; site sections were inferred from page links only"
                .to_string(),
        );
    }

    Ok(CollectedPage {
        requested_url: target_url.to_string(),
        final_url,
        page_title: page_title.filter(|title| !title.trim().is_empty()),
        html,
        script_tags: script_tags
            .into_iter()
            .map(|script| CollectedScriptTag {
                src: script.src,
                inline_text: script.inline_text.filter(|text| !text.trim().is_empty()),
            })
            .collect(),
        network_requests: network_requests
            .into_iter()
            .map(|entry| CollectedRequest {
                url: entry.url,
                resource_type: entry.initiator_type,
            })
            .collect(),
        gpt_slots,
        links,
        sitemap_locs,
        warnings,
    })
}

/// Maximum sitemap `<loc>` entries kept. Section discovery needs one page per
/// section, so a 50,000-URL catalog sitemap is truncated hard.
const MAX_SITEMAP_LOCS: usize = 5000;

/// Reads same-origin `a[href]` targets from the hydrated DOM.
///
/// `anchor.href` is absolutized by the DOM already, and `in_nav` records whether
/// the anchor sits inside site navigation — navigation is the publisher's own
/// declaration of its taxonomy, so those links rank higher when picking sections.
///
/// Reading the DOM rather than the served markup is deliberate: an app-router
/// page keeps its link graph in the framework payload, so parsing raw HTML finds
/// only a fraction of a site's sections.
const LINKS_SCRIPT: &str = r#"() => {
    try {
        const navAnchors = new Set(
            Array.from(document.querySelectorAll(
                'nav a[href], header a[href], [role="navigation"] a[href]'
            ))
        );
        const out = [];
        const seen = new Set();
        for (const anchor of document.querySelectorAll('a[href]')) {
            if (out.length >= 2000) break;
            const href = anchor.href;
            if (!href || seen.has(href)) continue;
            if (!href.startsWith(location.origin)) continue;
            seen.add(href);
            out.push({ url: href, in_nav: navAnchors.has(anchor) });
        }
        return out;
    } catch (error) {
        return [];
    }
}"#;

/// Discovers sitemap page URLs from inside the page, starting at `robots.txt`.
///
/// Runs in the browser rather than through a Rust HTTP client on purpose: the
/// in-page `fetch` carries the session's cookies and Chrome's TLS fingerprint,
/// so a bot-protection layer that would answer a bare client with a challenge
/// serves the real document instead. It also gets transparent gzip and an XML
/// parser for free, which is why sitemap support needs no new Rust dependency.
///
/// Same-origin is enforced here *and* again in Rust: a `Sitemap:` directive can
/// name any host, and this crawl carries operator-supplied cookies.
const SITEMAP_SCRIPT: &str = r#"async () => {
    const sameOrigin = (raw) => {
        try {
            return new URL(raw, location.origin).origin === location.origin;
        } catch (error) {
            return false;
        }
    };
    const fetchText = async (url) => {
        try {
            const response = await fetch(url, { credentials: 'same-origin' });
            if (!response.ok) return null;
            return await response.text();
        } catch (error) {
            return null;
        }
    };
    const parseLocs = (text) => {
        try {
            const doc = new DOMParser().parseFromString(text, 'application/xml');
            if (doc.querySelector('parsererror')) return { pages: [], indexes: [] };
            const indexes = Array.from(doc.querySelectorAll('sitemapindex > sitemap > loc'))
                .map((node) => (node.textContent || '').trim()).filter(sameOrigin);
            const pages = Array.from(doc.querySelectorAll('urlset > url > loc'))
                .map((node) => (node.textContent || '').trim()).filter(sameOrigin);
            return { pages, indexes };
        } catch (error) {
            return { pages: [], indexes: [] };
        }
    };

    const roots = [];
    const robots = await fetchText('/robots.txt');
    if (robots) {
        for (const line of robots.split(/\r?\n/)) {
            const match = /^\s*sitemap\s*:\s*(\S+)/i.exec(line);
            if (match && sameOrigin(match[1])) roots.push(match[1]);
        }
    }
    if (roots.length === 0) roots.push('/sitemap.xml', '/sitemap_index.xml');

    const pages = [];
    let childrenFollowed = 0;
    for (const root of roots) {
        if (pages.length >= 5000) break;
        const text = await fetchText(root);
        if (!text) continue;
        const parsed = parseLocs(text);
        pages.push(...parsed.pages);
        for (const child of parsed.indexes) {
            if (childrenFollowed >= 10 || pages.length >= 5000) break;
            childrenFollowed += 1;
            const childText = await fetchText(child);
            if (!childText) continue;
            pages.push(...parseLocs(childText).pages);
        }
    }
    return pages.slice(0, 5000);
}"#;

/// Reports the observable state of GPT, for pages whose registry came back empty.
const GPT_DIAGNOSTIC_SCRIPT: &str = r#"() => {
    const tag = window.googletag;
    const count = (() => {
        try { return tag.pubads().getSlots().length } catch (error) { return -1 }
    })();
    return {
        googletag: typeof tag,
        api_ready: !!(tag && tag.apiReady),
        cmd_pending: tag && tag.cmd && typeof tag.cmd.length === 'number' ? tag.cmd.length : -1,
        has_pubads: !!(tag && typeof tag.pubads === 'function'),
        slots: count,
        tcfapi: typeof window.__tcfapi,
        scripts: document.scripts.length,
        ts_ad_slots: (() => {
            try { return (window.tsjs && window.tsjs.adSlots || []).length } catch (error) { return -1 }
        })(),
    };
}"#;

/// Reads the live GPT slot registry into `{gam_unit_path, div_id, sizes}` rows.
///
/// Mirrors the ad-template verifier's `getSlots()` scrape: it defends against a
/// missing or partially-initialized `googletag`, keeps only numeric sizes, and
/// drops slots without a path or div id.
const GPT_SLOTS_SCRIPT: &str = r#"() => {
    try {
        if (!window.googletag || typeof googletag.pubads !== 'function') return [];
        const pubads = googletag.pubads();
        if (typeof pubads.getSlots !== 'function') return [];
        return pubads.getSlots().map((slot) => {
            const path = typeof slot.getAdUnitPath === 'function' ? slot.getAdUnitPath() : '';
            const div = typeof slot.getSlotElementId === 'function' ? slot.getSlotElementId() : '';
            const rawSizes = typeof slot.getSizes === 'function' ? (slot.getSizes() || []) : [];
            const sizes = rawSizes.map((size) =>
                (size && typeof size.getWidth === 'function' && typeof size.getHeight === 'function')
                    ? [size.getWidth(), size.getHeight()]
                    : null
            ).filter(Boolean);
            return { gam_unit_path: path, div_id: div, sizes };
        }).filter((slot) => slot.gam_unit_path && slot.div_id);
    } catch (error) {
        return [];
    }
}"#;

/// How one GPT registry reading relates to the previous non-empty reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GptRegistryReading {
    /// GPT reported no slots, so any stability streak so far is void.
    Empty,
    /// The registry differs from the last non-empty reading.
    Changed,
    /// The registry matches the last non-empty reading, element for element.
    Repeated,
}

/// Classifies one GPT registry reading against the previous non-empty reading.
///
/// The comparison is element-wise and order-sensitive because registry order is
/// load-bearing downstream: slot discovery takes the GAM network id from the
/// first usable entry and emits slots in registry order, so a reading whose
/// order still churns has not stabilized even when its slot set has.
fn gpt_registry_reading(
    previous_nonempty: Option<&[CollectedGptSlot]>,
    current: &[CollectedGptSlot],
) -> GptRegistryReading {
    if current.is_empty() {
        GptRegistryReading::Empty
    } else if previous_nonempty == Some(current) {
        GptRegistryReading::Repeated
    } else {
        GptRegistryReading::Changed
    }
}

/// Reads GPT until a non-empty registry holds still for `dwell_target`, or
/// until `budget` expires.
///
/// A single pair of matching reads is not enough: GPT registers in bursts (SRA
/// batching, consent-gated definitions, lazy slots), so two consecutive reads
/// can both observe the same burst and miss the next. Requiring the reading to
/// repeat for a dwell window mirrors [`wait_for_page_settle`], and taking both
/// the dwell from the operator's quiet flag and the budget from the remaining
/// shared allowance, floored at that quiet window, bounds this phase. An
/// in-flight read may overrun the budget by its own bound. Even an exhausted
/// budget takes one snapshot; two consecutive empty polls end the wait early.
///
/// The latest non-empty snapshot is retained, with a warning, if registration
/// keeps changing through the budget. An empty result stays silent and
/// best-effort so the caller can report the more useful GPT state diagnostic.
async fn collect_stable_gpt_slots(
    page: &chromiumoxide::Page,
    dwell_target: Duration,
    budget: Duration,
    warnings: &mut Vec<String>,
) -> Vec<CollectedGptSlot> {
    poll_gpt_registry(
        || async {
            match timeout(CDP_OPERATION_TIMEOUT, page.evaluate(GPT_SLOTS_SCRIPT)).await {
                Ok(Ok(result)) => result
                    .into_value()
                    .map_err(|error| format!("failed to decode live GPT slots: {error}")),
                Ok(Err(error)) => Err(format!("failed to evaluate live GPT slots: {error}")),
                Err(_) => Err(format!(
                    "timed out evaluating live GPT slots after {}s; results may be partial",
                    CDP_OPERATION_TIMEOUT.as_secs()
                )),
            }
        },
        dwell_target,
        budget,
        warnings,
    )
    .await
}

/// Polls registry snapshots separately from their browser transport.
async fn poll_gpt_registry<F, R>(
    mut read: F,
    dwell_target: Duration,
    budget: Duration,
    warnings: &mut Vec<String>,
) -> Vec<CollectedGptSlot>
where
    F: FnMut() -> R,
    R: std::future::Future<Output = Result<Vec<CollectedGptSlot>, String>>,
{
    let start = tokio::time::Instant::now();
    let mut previous_nonempty: Option<Vec<CollectedGptSlot>> = None;
    let mut latest_nonempty = Vec::new();
    let mut stable_since = None;
    let mut previous_empty = false;
    let mut first_read = true;

    loop {
        if !first_read && start.elapsed() >= budget {
            return expired_gpt_registry(latest_nonempty, dwell_target, budget, warnings);
        }

        first_read = false;
        let slots = match read().await {
            Ok(slots) => slots,
            Err(error) => {
                warnings.push(error);
                return latest_nonempty;
            }
        };

        match gpt_registry_reading(previous_nonempty.as_deref(), &slots) {
            GptRegistryReading::Empty => {
                // Two empty snapshots end this best-effort wait; they do not
                // prove that a publisher can never register a later slot.
                if previous_empty {
                    return latest_nonempty;
                }
                previous_empty = true;
                previous_nonempty = None;
                stable_since = None;
            }
            GptRegistryReading::Changed => {
                previous_empty = false;
                latest_nonempty.clone_from(&slots);
                previous_nonempty = Some(slots);
                stable_since = None;
            }
            GptRegistryReading::Repeated => {
                previous_empty = false;
                let stable_start = stable_since.get_or_insert_with(tokio::time::Instant::now);
                if stable_start.elapsed() >= dwell_target {
                    return slots;
                }
            }
        }

        let remaining_budget = budget.saturating_sub(start.elapsed());
        if remaining_budget.is_zero() {
            return expired_gpt_registry(latest_nonempty, dwell_target, budget, warnings);
        }
        let remaining_dwell = stable_since
            .map(|stable_start| dwell_target.saturating_sub(stable_start.elapsed()))
            .unwrap_or(dwell_target);
        sleep(
            SETTLE_POLL_INTERVAL
                .min(remaining_budget)
                .min(remaining_dwell.max(Duration::from_millis(1))),
        )
        .await;
    }
}

/// Reports a stability budget that expired before the registry held still, and
/// hands back the latest snapshot.
///
/// The message names both the dwell and the budget rather than asserting that
/// registration churned: a registry that stopped changing late can also run out
/// of budget mid-dwell, and the operator's next move — raising the cap — is the
/// same either way.
///
/// An empty registry is left to the caller's GPT state diagnostic, which names
/// the actual cause; only a partial non-empty snapshot needs its own warning,
/// because otherwise it is indistinguishable in the output from a cleanly
/// stabilized read.
fn expired_gpt_registry(
    latest_nonempty: Vec<CollectedGptSlot>,
    dwell_target: Duration,
    budget: Duration,
    warnings: &mut Vec<String>,
) -> Vec<CollectedGptSlot> {
    if !latest_nonempty.is_empty() {
        warnings.push(format!(
            "GPT slot registration did not hold still for {}ms within the {}ms budget; using the \
             latest snapshot of {} slot(s), so results may be partial; raise `--settle-max-ms` \
             to wait longer",
            dwell_target.as_millis(),
            budget.as_millis(),
            latest_nonempty.len()
        ));
    }
    latest_nonempty
}

async fn wait_for_page_settle(
    page: &chromiumoxide::Page,
    quiet_target: Duration,
    max_wait: Duration,
) -> CliResult<bool> {
    let start = std::time::Instant::now();
    let mut previous_count = None;
    let mut quiet_since = None;

    while start.elapsed() < max_wait {
        let ready_state: String =
            timeout(CDP_OPERATION_TIMEOUT, page.evaluate("document.readyState"))
                .await
                .map_err(|_| "timed out reading document ready state".to_string())?
                .map_err(|error| format!("failed to read document ready state: {error}"))?
                .into_value()
                .map_err(|error| format!("failed to decode document ready state: {error}"))?;
        let resource_count: usize = timeout(
            CDP_OPERATION_TIMEOUT,
            page.evaluate("performance.getEntriesByType('resource').length"),
        )
        .await
        .map_err(|_| "timed out reading resource count".to_string())?
        .map_err(|error| format!("failed to read resource count: {error}"))?
        .into_value()
        .map_err(|error| format!("failed to decode resource count: {error}"))?;

        // Accept `interactive` as well as `complete`: ad-heavy pages often never
        // reach `complete` (the `load` event never fires), but their GPT slots
        // are defined once the DOM is interactive, so a quiet network period at
        // `interactive` is a valid settle signal for the slot scrape.
        if ready_state == "complete" || ready_state == "interactive" {
            if previous_count == Some(resource_count) {
                let quiet_start = quiet_since.get_or_insert_with(std::time::Instant::now);
                if quiet_start.elapsed() >= quiet_target {
                    return Ok(true);
                }
            } else {
                quiet_since = None;
            }
        } else {
            quiet_since = None;
        }

        previous_count = Some(resource_count);
        let remaining_max = max_wait.saturating_sub(start.elapsed());
        let remaining_quiet = quiet_since
            .map(|quiet_start| quiet_target.saturating_sub(quiet_start.elapsed()))
            .unwrap_or(quiet_target);
        sleep(
            SETTLE_POLL_INTERVAL
                .min(remaining_max)
                .min(remaining_quiet.max(Duration::from_millis(1))),
        )
        .await;
    }

    Ok(false)
}

fn validate_navigation_response(navigation_response: ArcHttpRequest) -> CliResult<Option<String>> {
    // These failures are recoverable during a multi-page crawl. The caller
    // records them once as a profile-aware `note:`; using `report_error` here
    // would also log an unscoped duplicate in the middle of progress output.
    let request = navigation_response
        .ok_or_else(|| "browser audit did not capture the main document response".to_string())?;

    if let Some(failure_text) = &request.failure_text {
        return Err(format!("main document request failed: {failure_text}"));
    }

    let response = request.response.as_ref().ok_or_else(|| {
        "browser audit did not capture the main document HTTP response".to_string()
    })?;

    if is_successful_navigation_status(response.status) {
        return Ok(None);
    }

    Ok(Some(format!(
        "audit request returned HTTP {} {} for `{}`; results may be partial",
        response.status, response.status_text, response.url
    )))
}

fn is_successful_navigation_status(status: i64) -> bool {
    (200..400).contains(&status)
}

fn resource_timing_buffer_warning(resource_count: usize) -> Option<&'static str> {
    (resource_count >= RESOURCE_TIMING_BUFFER_SIZE).then_some(RESOURCE_TIMING_BUFFER_WARNING)
}

#[derive(Debug, Deserialize)]
struct BrowserScriptTag {
    src: Option<String>,
    inline_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BrowserPerformanceEntry {
    url: String,
    initiator_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use chromiumoxide::cdp::browser_protocol::network::{Headers, RequestId, Response};
    use chromiumoxide::cdp::browser_protocol::security::SecurityState;
    use chromiumoxide::handler::http::HttpRequest;

    use super::*;
    use crate::commands::audit::browser::browser_fixture_available;

    const LAZY_GPT_FIXTURE: &str = r#"<!doctype html>
<html>
  <body style="height: 4000px">
    <div id="ad-lazy-0"></div>
    <script>
      window.addEventListener('scroll', function installLazySlot() {
        if (window.scrollY <= 0 || window.lazySlotScheduled) return
        window.lazySlotScheduled = true
        setTimeout(function () {
          var slot = {
            getAdUnitPath: function () { return '/123/lazy' },
            getSlotElementId: function () { return 'ad-lazy-0' },
            getSizes: function () {
              return [{
                getWidth: function () { return 300 },
                getHeight: function () { return 250 },
              }]
            },
          }
          window.googletag = {
            pubads: function () {
              return { getSlots: function () { return [slot] } }
            },
          }
        }, 900)
      })
    </script>
  </body>
</html>"#;

    const DELAYED_GPT_FIXTURE: &str = r#"<!doctype html>
<html>
  <body>
    <div id="ad-z-delayed-0"></div>
    <div id="ad-a-delayed-0"></div>
    <script>
      var zSlot = {
        getAdUnitPath: function () { return '/123/z-delayed' },
        getSlotElementId: function () { return 'ad-z-delayed-0' },
        getSizes: function () {
          return [{
            getWidth: function () { return 300 },
            getHeight: function () { return 250 },
          }]
        },
      }
      var aSlot = {
        getAdUnitPath: function () { return '/123/a-delayed' },
        getSlotElementId: function () { return 'ad-a-delayed-0' },
        getSizes: function () {
          return [{
            getWidth: function () { return 728 },
            getHeight: function () { return 90 },
          }]
        },
      }
      var slots = []
      var registrationStage = 0
      window.googletag = {
        pubads: function () {
          return {
            getSlots: function () {
              if (registrationStage === 0) {
                registrationStage = 1
                Promise.resolve().then(function () { slots = [zSlot] })
              } else if (registrationStage === 1 && slots.length === 1) {
                registrationStage = 2
                Promise.resolve().then(function () { slots = [zSlot, aSlot] })
              }
              return slots
            },
          }
        },
      }
    </script>
  </body>
</html>"#;

    /// A registry whose second burst lands on a real timer rather than on
    /// observation, so two consecutive identical reads can straddle the gap.
    ///
    /// The first burst is gated on the first `getSlots()` call — otherwise it
    /// would complete during the settle wait, long before polling starts — but
    /// the gap that follows is genuine wall-clock time, which is what makes
    /// this fixture able to detect a criterion that exits on a single matching
    /// pair.
    const BATCHED_GPT_FIXTURE: &str = r#"<!doctype html>
<html>
  <body>
    <div id="ad-first-batch-0"></div>
    <div id="ad-second-batch-0"></div>
    <script>
      var firstSlot = {
        getAdUnitPath: function () { return '/123/first-batch' },
        getSlotElementId: function () { return 'ad-first-batch-0' },
        getSizes: function () {
          return [{
            getWidth: function () { return 300 },
            getHeight: function () { return 250 },
          }]
        },
      }
      var secondSlot = {
        getAdUnitPath: function () { return '/123/second-batch' },
        getSlotElementId: function () { return 'ad-second-batch-0' },
        getSizes: function () {
          return [{
            getWidth: function () { return 728 },
            getHeight: function () { return 90 },
          }]
        },
      }
      var slots = []
      var armed = false
      // Model a metadata read slower than the shared settle budget, while
      // staying below the individual CDP operation timeout.
      if (location.hash === '#slow-title') {
        Object.defineProperty(document, 'title', {
          get: function () {
            var start = performance.now()
            while (performance.now() - start < 4000) {}
            return 'Slow metadata fixture'
          },
        })
      }
      window.googletag = {
        pubads: function () {
          return {
            getSlots: function () {
              // The budget-floor test observes read count, independently of
              // the real timer exercised by the other fixture users.
              if (location.hash === '#poll-driven') {
                if (!armed) {
                  armed = true
                  return [firstSlot]
                }
                return [firstSlot, secondSlot]
              }
              if (!armed) {
                armed = true
                slots = [firstSlot]
                setTimeout(function () {
                  slots = [firstSlot, secondSlot]
                  document.body.dataset.gptBatch = 'second'
                  var script = document.createElement('script')
                  script.src = '/late-evidence.js'
                  document.head.appendChild(script)
                }, 400)
              }
              return slots
            },
          }
        },
      }
    </script>
  </body>
</html>"#;

    /// Local HTTP server that serves one fixture document for a browser test.
    ///
    /// Chrome opens several sockets per navigation: the document request, socket
    /// pool preconnects that close without sending anything, and speculative
    /// `/favicon.ico`, `/robots.txt`, and `/sitemap.xml` fetches. The server must
    /// therefore keep accepting connections for as long as the test runs, must
    /// serve them concurrently so a silent preconnect cannot stall the document
    /// request, and must treat a connection that carries no request as normal.
    /// Serving a single connection instead loses the accept race and fails the
    /// navigation with `net::ERR_CONNECTION_REFUSED`.
    struct GptFixtureServer {
        url: Url,
        address: SocketAddr,
        shutdown: Arc<AtomicBool>,
        acceptor: Option<JoinHandle<()>>,
    }

    impl GptFixtureServer {
        fn url(&self) -> &Url {
            &self.url
        }

        fn address(&self) -> SocketAddr {
            self.address
        }
    }

    impl Drop for GptFixtureServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            // Wake the blocking accept so shutdown can join the listener thread.
            let _ = TcpStream::connect(self.address);
            if let Some(acceptor) = self.acceptor.take() {
                let _ = acceptor.join();
            }
        }
    }

    /// Serves `html` for `GET /` until the returned server is dropped.
    fn gpt_fixture_server(html: &'static str) -> GptFixtureServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind fixture server");
        let address = listener.local_addr().expect("should read fixture address");
        let shutdown = Arc::new(AtomicBool::new(false));
        let acceptor_shutdown = Arc::clone(&shutdown);
        let acceptor = std::thread::spawn(move || {
            while !acceptor_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if acceptor_shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::spawn(move || serve_gpt_fixture_connection(stream, html));
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return,
                }
            }
        });

        GptFixtureServer {
            url: Url::parse(&format!("http://{address}/")).expect("should parse fixture URL"),
            address,
            shutdown,
            acceptor: Some(acceptor),
        }
    }

    /// Answers one fixture connection, ignoring sockets that carry no request.
    fn serve_gpt_fixture_connection(mut stream: TcpStream, html: &'static str) {
        if stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .is_err()
        {
            return;
        }

        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut chunk = [0_u8; 1024];
            match stream.read(&mut chunk) {
                // A preconnect socket closes without a request; that is not a failure.
                Ok(0) => return,
                Ok(chunk_len) => request.extend_from_slice(&chunk[..chunk_len]),
                Err(_) => return,
            }
            if request.len() > 16 * 1024 {
                return;
            }
        }

        if !request.starts_with(b"GET / HTTP") {
            let _ = write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            return;
        }

        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html,
        );
    }

    /// Requests a path from a fixture server, failing if no response arrives in time.
    fn fixture_response(address: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).expect("should connect to the fixture server");
        // A fixture that stalls behind another connection must fail this read
        // rather than deliver the document late.
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("should bound the fixture client read");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: fixture.example.com\r\n\r\n"
        )
        .expect("should send the fixture request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("should read the fixture response before the client timeout");
        response
    }

    #[test]
    fn fixture_server_serves_the_document_after_a_socket_that_sends_no_request() {
        let fixture = gpt_fixture_server(DELAYED_GPT_FIXTURE);

        // Chrome's socket-pool preconnect opens a socket and closes it without
        // sending anything, and it can win the accept race with the navigation.
        drop(TcpStream::connect(fixture.address()).expect("should open a preconnect socket"));

        let response = fixture_response(fixture.address(), "/");
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "should still answer the document request: {response}"
        );
        assert!(
            response.contains("ad-z-delayed-0"),
            "should serve the fixture document body: {response}"
        );
    }

    #[test]
    fn fixture_server_serves_the_document_while_a_silent_socket_stays_open() {
        let fixture = gpt_fixture_server(DELAYED_GPT_FIXTURE);

        // Held open, sending nothing: serving connections sequentially would
        // block the document request behind this socket's read timeout.
        let _silent = TcpStream::connect(fixture.address()).expect("should open a silent socket");

        let response = fixture_response(fixture.address(), "/");
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "should answer the document request without waiting for the silent socket: {response}"
        );
    }

    #[test]
    fn fixture_server_answers_every_speculative_browser_request() {
        let fixture = gpt_fixture_server(LAZY_GPT_FIXTURE);

        // Chrome follows the document with /favicon.ico, /robots.txt and
        // /sitemap.xml probes on separate connections.
        for path in ["/favicon.ico", "/robots.txt", "/sitemap.xml"] {
            let response = fixture_response(fixture.address(), path);
            assert_eq!(
                response,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "should return an empty 404 for {path}"
            );
        }
        let response = fixture_response(fixture.address(), "/");
        assert!(
            response.starts_with("HTTP/1.1 200 OK") && response.ends_with(LAZY_GPT_FIXTURE),
            "should still serve the document after speculative requests"
        );
    }

    fn gpt_slot(unit_path: &str, div_id: &str) -> CollectedGptSlot {
        CollectedGptSlot {
            gam_unit_path: unit_path.to_string(),
            div_id: div_id.to_string(),
            sizes: vec![(300, 250)],
        }
    }

    #[test]
    fn empty_gpt_reading_voids_the_stability_streak() {
        let previous = vec![gpt_slot("/123/a", "ad-a-0")];

        assert_eq!(
            gpt_registry_reading(Some(&previous), &[]),
            GptRegistryReading::Empty,
            "an empty read should discard the earlier non-empty reading"
        );
        assert_eq!(
            gpt_registry_reading(None, &[]),
            GptRegistryReading::Empty,
            "an empty read with no history should still classify as empty"
        );
    }

    #[test]
    fn first_nonempty_gpt_reading_is_a_change() {
        let current = vec![gpt_slot("/123/a", "ad-a-0")];

        assert_eq!(
            gpt_registry_reading(None, &current),
            GptRegistryReading::Changed,
            "the first non-empty read has nothing to repeat"
        );
    }

    #[test]
    fn identical_gpt_reading_repeats() {
        let slots = vec![gpt_slot("/123/a", "ad-a-0"), gpt_slot("/123/b", "ad-b-0")];

        assert_eq!(
            gpt_registry_reading(Some(&slots), &slots),
            GptRegistryReading::Repeated,
            "an unchanged registry should classify as repeated"
        );
    }

    #[test]
    fn gpt_reading_stability_is_order_sensitive() {
        let previous = vec![gpt_slot("/123/a", "ad-a-0"), gpt_slot("/123/b", "ad-b-0")];
        let reordered = vec![gpt_slot("/123/b", "ad-b-0"), gpt_slot("/123/a", "ad-a-0")];
        let grown = vec![
            gpt_slot("/123/a", "ad-a-0"),
            gpt_slot("/123/b", "ad-b-0"),
            gpt_slot("/123/c", "ad-c-0"),
        ];

        assert_eq!(
            gpt_registry_reading(Some(&previous), &reordered),
            GptRegistryReading::Changed,
            "the same slot set in a different order is not yet stable"
        );
        assert_eq!(
            gpt_registry_reading(Some(&previous), &grown),
            GptRegistryReading::Changed,
            "a later registration burst should reset the streak"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_stops_after_two_empty_readings() {
        let mut reads = 0;
        let mut warnings = Vec::new();
        let start = tokio::time::Instant::now();
        let slots = poll_gpt_registry(
            || {
                reads += 1;
                std::future::ready(Ok(Vec::new()))
            },
            Duration::from_millis(750),
            Duration::from_secs(12),
            &mut warnings,
        )
        .await;
        assert!(slots.is_empty(), "should retain an empty registry");
        assert_eq!(reads, 2, "should stop after consecutive empty snapshots");
        assert_eq!(
            start.elapsed(),
            SETTLE_POLL_INTERVAL,
            "should avoid spending the full budget"
        );
        assert!(
            warnings.is_empty(),
            "should leave the empty-state diagnostic to the caller"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_quiet_window_budget_captures_late_batch_as_partial() {
        let first = vec![gpt_slot("/123/first", "ad-first")];
        let mut latest = first.clone();
        latest.push(gpt_slot("/123/second", "ad-second"));
        let start = tokio::time::Instant::now();
        let mut reads = 0;
        let mut warnings = Vec::new();

        let slots = poll_gpt_registry(
            || {
                reads += 1;
                let slots = if start.elapsed() >= Duration::from_millis(400) {
                    latest.clone()
                } else {
                    first.clone()
                };
                std::future::ready(Ok(slots))
            },
            Duration::from_millis(600),
            Duration::from_millis(600),
            &mut warnings,
        )
        .await;

        assert_eq!(slots, latest, "should capture the second batch");
        assert_eq!(reads, 3, "should poll beyond the first snapshot");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(600),
            "should respect the polling budget"
        );
        assert_eq!(warnings.len(), 1, "should report incomplete stabilization");
        assert!(
            warnings[0].contains("within the 600ms budget"),
            "should name the exhausted budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_reads_once_with_zero_budget() {
        let expected = vec![gpt_slot("/123/a", "ad-a")];
        let mut reads = 0;
        let mut warnings = Vec::new();
        let slots = poll_gpt_registry(
            || {
                reads += 1;
                std::future::ready(Ok(expected.clone()))
            },
            Duration::ZERO,
            Duration::ZERO,
            &mut warnings,
        )
        .await;
        assert_eq!(reads, 1, "should always collect an initial snapshot");
        assert_eq!(
            slots, expected,
            "should retain actual evidence at zero budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_resets_empty_streak_and_preserves_latest_evidence() {
        let first = vec![gpt_slot("/123/a", "ad-a")];
        let latest = vec![gpt_slot("/123/b", "ad-b")];
        let mut readings = [
            Vec::new(),
            first,
            Vec::new(),
            latest.clone(),
            Vec::new(),
            Vec::new(),
        ]
        .into_iter();
        let mut warnings = Vec::new();
        let result = poll_gpt_registry(
            || {
                std::future::ready(Ok(readings
                    .next()
                    .expect("should stop at consecutive empties")))
            },
            Duration::from_millis(750),
            Duration::from_secs(12),
            &mut warnings,
        )
        .await;
        assert_eq!(
            result, latest,
            "should retain the latest snapshot through empty readings"
        );
        assert!(
            readings.next().is_none(),
            "should reset each empty streak on a nonempty reading"
        );
        assert!(warnings.is_empty(), "should not report budget exhaustion");
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_requires_dwell_after_a_later_registration_burst() {
        let first = vec![gpt_slot("/123/a", "ad-a")];
        let latest = vec![gpt_slot("/123/a", "ad-a"), gpt_slot("/123/b", "ad-b")];
        let start = tokio::time::Instant::now();
        let mut warnings = Vec::new();
        let result = poll_gpt_registry(
            || {
                std::future::ready(Ok(if start.elapsed() < Duration::from_millis(500) {
                    first.clone()
                } else {
                    latest.clone()
                }))
            },
            Duration::from_millis(750),
            Duration::from_secs(12),
            &mut warnings,
        )
        .await;
        assert_eq!(
            result, latest,
            "should include the second registration burst"
        );
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(1500),
            "should reset the dwell after registration changes"
        );
        assert!(warnings.is_empty(), "should stabilize within the budget");
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_expiry_preserves_changing_evidence() {
        let mut reads = 0;
        let mut warnings = Vec::new();
        let result = poll_gpt_registry(
            || {
                reads += 1;
                std::future::ready(Ok(vec![gpt_slot("/123/a", &format!("ad-{reads}"))]))
            },
            Duration::from_millis(750),
            Duration::from_millis(600),
            &mut warnings,
        )
        .await;
        assert_eq!(reads, 3, "should stop at the budget without an extra read");
        assert_eq!(
            result,
            [gpt_slot("/123/a", "ad-3")],
            "should retain the most recent evidence"
        );
        assert_eq!(warnings.len(), 1, "should report partial evidence once");
        assert!(
            warnings[0].contains("600ms budget"),
            "should identify budget exhaustion"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn registry_poll_read_failure_preserves_evidence_and_reports_the_cause() {
        let expected = vec![gpt_slot("/123/a", "ad-a")];
        let mut readings = [Ok(expected.clone()), Err("read unavailable".to_string())].into_iter();
        let mut warnings = Vec::new();
        let result = poll_gpt_registry(
            || std::future::ready(readings.next().expect("should stop on read failure")),
            Duration::from_millis(750),
            Duration::from_secs(12),
            &mut warnings,
        )
        .await;
        assert_eq!(
            result, expected,
            "should preserve evidence on a later read failure"
        );
        assert_eq!(
            warnings,
            ["read unavailable"],
            "should distinguish transport failure from expiry"
        );
    }

    #[test]
    fn expired_stability_budget_warns_only_for_partial_evidence() {
        let mut partial_warnings = Vec::new();
        let partial = expired_gpt_registry(
            vec![gpt_slot("/123/a", "ad-a-0")],
            Duration::from_millis(750),
            Duration::from_millis(12_000),
            &mut partial_warnings,
        );

        assert_eq!(partial.len(), 1, "the latest snapshot should be returned");
        assert_eq!(
            partial_warnings,
            [
                "GPT slot registration did not hold still for 750ms within the 12000ms budget; \
                 using the latest snapshot of 1 slot(s), so results may be partial; raise \
                 `--settle-max-ms` to wait longer"
            ],
            "a partial snapshot should name the slot count, the dwell, and the budget"
        );

        let mut empty_warnings = Vec::new();
        let empty = expired_gpt_registry(
            Vec::new(),
            Duration::from_millis(750),
            Duration::from_millis(12_000),
            &mut empty_warnings,
        );

        assert!(empty.is_empty(), "an empty registry should stay empty");
        assert!(
            empty_warnings.is_empty(),
            "an empty registry is reported by the GPT state diagnostic instead"
        );
    }

    #[test]
    fn successful_navigation_status_allows_redirects_but_rejects_errors() {
        assert!(is_successful_navigation_status(200));
        assert!(is_successful_navigation_status(302));
        assert!(is_successful_navigation_status(399));
        assert!(!is_successful_navigation_status(199));
        assert!(!is_successful_navigation_status(400));
        assert!(!is_successful_navigation_status(500));
    }

    #[test]
    fn navigation_response_returns_warning_for_http_error_status() {
        let warning =
            validate_navigation_response(navigation_response_with_status(403, "Forbidden"))
                .expect("should validate navigation response")
                .expect("should return warning for HTTP error status");

        assert_eq!(
            warning,
            "audit request returned HTTP 403 Forbidden for `https://example.com/`; results may be partial",
            "should warn and continue when the main document returns an HTTP error"
        );
    }

    #[test]
    fn navigation_response_reports_chromium_request_failure() {
        let mut request =
            HttpRequest::new(RequestId::new("request-1"), None, None, false, Vec::new());
        request.failure_text = Some("net::ERR_BLOCKED_BY_ORB".to_string());

        let error = validate_navigation_response(Some(Arc::new(request)))
            .expect_err("should reject Chromium request failures");

        assert_eq!(
            error, "main document request failed: net::ERR_BLOCKED_BY_ORB",
            "the crawl should retain the browser failure for its final skipped-page note"
        );
    }

    #[test]
    fn resource_timing_buffer_warning_starts_at_threshold() {
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_SIZE - 1),
            None,
            "should not warn before the resource timing buffer threshold"
        );
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_SIZE),
            Some(RESOURCE_TIMING_BUFFER_WARNING),
            "should warn when the resource timing buffer reaches the threshold"
        );
    }

    #[test]
    fn browser_path_candidates_include_common_names() {
        let candidates = crate::commands::audit::browser::CHROME_NAMES;

        assert!(candidates.contains(&"google-chrome"));
        assert!(candidates.contains(&"chromium"));
        assert!(candidates.contains(&"Google Chrome for Testing"));
    }

    #[test]
    fn browser_run_reports_close_error_before_wait_error() {
        let result = combine_browser_run_results(
            Ok(()),
            Ok(()),
            Err("close failed".to_string()),
            Err("wait failed".to_string()),
        );

        assert_eq!(
            result.expect_err("should preserve teardown error"),
            "close failed",
            "the close failure is the first teardown failure"
        );
    }

    #[test]
    fn browser_run_reports_wait_error_when_close_succeeds() {
        let result =
            combine_browser_run_results(Ok(()), Ok(()), Ok(()), Err("wait failed".to_string()));

        assert_eq!(
            result.expect_err("should preserve wait error"),
            "wait failed",
            "a wait failure must not be mislabeled as a close failure"
        );
    }

    #[test]
    fn browser_run_preserves_collection_error_over_later_failures() {
        let result = combine_browser_run_results(
            Err("collection failed".to_string()),
            Err("finalization progress failed".to_string()),
            Err("close failed".to_string()),
            Err("wait failed".to_string()),
        );

        assert_eq!(
            result.expect_err("should preserve first browser run error"),
            "collection failed"
        );
    }

    #[test]
    fn browser_run_reports_finalization_progress_before_teardown_errors() {
        let result = combine_browser_run_results(
            Ok(()),
            Err("finalization progress failed".to_string()),
            Err("close failed".to_string()),
            Err("wait failed".to_string()),
        );

        assert_eq!(
            result.expect_err("should preserve finalization progress error"),
            "finalization progress failed"
        );
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn progress_failure_still_finalizes_browser_session() {
        if !browser_fixture_available() {
            return;
        }

        let collector = BrowserAuditCollector::default();
        let target = Url::parse("http://127.0.0.1:9/").expect("should parse fixture URL");
        let mut phases = Vec::new();
        let error = collector
            .collect_pages(
                &[target],
                &[],
                &mut |progress| match progress {
                    CollectionProgress::Launching => {
                        phases.push("launching");
                        Ok(())
                    }
                    CollectionProgress::Loading { .. } => {
                        phases.push("loading");
                        Err(report_error("simulated progress failure"))
                    }
                    CollectionProgress::Planning => {
                        phases.push("planning");
                        Ok(())
                    }
                    CollectionProgress::Finalizing => {
                        phases.push("finalizing");
                        Ok(())
                    }
                },
                &mut |_, _| panic!("page sink should not run after progress failure"),
            )
            .expect_err("should return progress failure after browser teardown");

        let rendered_error = format!("{error:?}");
        assert!(
            rendered_error.contains("simulated progress failure"),
            "should preserve progress failure, got {rendered_error}"
        );
        assert_eq!(phases, ["launching", "loading", "finalizing"]);
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn collects_lazy_gpt_slot_only_when_scroll_is_enabled() {
        if !browser_fixture_available() {
            return;
        }

        let unscrolled_fixture = gpt_fixture_server(LAZY_GPT_FIXTURE);
        let without_scroll = BrowserAuditCollector::default()
            .collect_page(unscrolled_fixture.url(), &[])
            .expect("should collect without scrolling");
        let scrolled_fixture = gpt_fixture_server(LAZY_GPT_FIXTURE);
        let with_scroll = BrowserAuditCollector::default()
            .with_scroll(true)
            .collect_page(scrolled_fixture.url(), &[])
            .expect("should collect with scrolling");

        assert!(
            without_scroll.gpt_slots.is_empty(),
            "lazy GPT slot should not exist before scrolling"
        );
        assert!(
            with_scroll
                .gpt_slots
                .iter()
                .any(|slot| { slot.gam_unit_path == "/123/lazy" && slot.div_id == "ad-lazy-0" }),
            "scrolling should trigger and collect the lazy GPT slot"
        );
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn waits_for_delayed_gpt_registry_to_stabilize_in_definition_order() {
        if !browser_fixture_available() {
            return;
        }

        let delayed_fixture = gpt_fixture_server(DELAYED_GPT_FIXTURE);
        let collected = BrowserAuditCollector::default()
            .collect_page(delayed_fixture.url(), &[])
            .expect("should collect delayed GPT registry");

        assert_eq!(
            collected.gpt_slots,
            vec![
                CollectedGptSlot {
                    gam_unit_path: "/123/z-delayed".to_string(),
                    div_id: "ad-z-delayed-0".to_string(),
                    sizes: vec![(300, 250)],
                },
                CollectedGptSlot {
                    gam_unit_path: "/123/a-delayed".to_string(),
                    div_id: "ad-a-delayed-0".to_string(),
                    sizes: vec![(728, 90)],
                },
            ],
            "collector should wait for stable registration without reordering slots"
        );
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn exhausted_page_settle_budget_still_polls_gpt_registry() {
        if !browser_fixture_available() {
            return;
        }

        for scroll in [false, true] {
            let fixture = gpt_fixture_server(BATCHED_GPT_FIXTURE);
            let mut url = fixture.url().clone();
            // A second read exposes the second batch without racing the
            // browser's 400ms timer against the 600ms budget on busy runners.
            url.set_fragment(Some("poll-driven"));
            // A quiet window equal to the maximum cannot finish inside that
            // maximum, so the initial settle spends the entire shared budget.
            let options = GenerateBrowserOpts {
                settle_quiet_ms: 600,
                settle_max_ms: 600,
                ..GenerateBrowserOpts::default()
            };
            let collected = BrowserAuditCollector::default()
                .with_browser_options(&options)
                .with_scroll(scroll)
                .collect_page(&url, &[])
                .expect("should poll the registry after the shared budget expires");

            assert_eq!(
                collected.gpt_slots,
                [
                    gpt_slot("/123/first-batch", "ad-first-batch-0"),
                    CollectedGptSlot {
                        gam_unit_path: "/123/second-batch".to_string(),
                        div_id: "ad-second-batch-0".to_string(),
                        sizes: vec![(728, 90)],
                    },
                ],
                "should collect the later batch within the quiet-window floor (scroll={scroll})"
            );
            assert!(
                collected
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("within the 600ms budget")),
                "should report partial evidence when the later batch cannot finish its dwell (scroll={scroll})"
            );
            assert_eq!(
                collected.warnings.iter().any(|warning| {
                    warning.contains("settle budget was exhausted before the post-scroll settle")
                }),
                scroll,
                "should report skipped post-scroll settling only when scrolling (scroll={scroll})"
            );
            assert!(
                !collected.warnings.iter().any(|warning| {
                    warning.contains("timed out while waiting for the page to settle after scroll")
                }),
                "should not report a timeout for a wait that never ran (scroll={scroll})"
            );
        }
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn slow_metadata_does_not_consume_gpt_settle_budget() {
        if !browser_fixture_available() {
            return;
        }

        let fixture = gpt_fixture_server(BATCHED_GPT_FIXTURE);
        let mut url = fixture.url().clone();
        url.set_fragment(Some("slow-title"));
        // The 4s title read exceeds this budget on its own. GPT's second
        // batch appears only after polling starts, so a single late snapshot
        // cannot substitute for giving the registry its remaining dwell time.
        let options = GenerateBrowserOpts {
            settle_quiet_ms: 750,
            settle_max_ms: 3500,
            ..GenerateBrowserOpts::default()
        };
        let collected = BrowserAuditCollector::default()
            .with_browser_options(&options)
            .collect_page(&url, &[])
            .expect("should collect GPT slots and slow metadata");

        assert_eq!(
            collected.page_title.as_deref(),
            Some("Slow metadata fixture"),
            "should still extract metadata after stabilizing the registry"
        );
        assert_eq!(
            collected.gpt_slots.len(),
            2,
            "should retain both registration batches despite slow metadata extraction"
        );
        assert!(
            !collected
                .warnings
                .iter()
                .any(|warning| warning.contains("GPT slot registration did not hold still")),
            "should let the registry stabilize before reading slow metadata: {:?}",
            collected.warnings
        );
    }

    #[test]
    #[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
    fn waits_through_a_timer_driven_gpt_registration_gap() {
        if !browser_fixture_available() {
            return;
        }

        let batched_fixture = gpt_fixture_server(BATCHED_GPT_FIXTURE);
        let collected = BrowserAuditCollector::default()
            .collect_page(batched_fixture.url(), &[])
            .expect("should collect batched GPT registry");

        assert_eq!(
            collected.gpt_slots,
            vec![
                CollectedGptSlot {
                    gam_unit_path: "/123/first-batch".to_string(),
                    div_id: "ad-first-batch-0".to_string(),
                    sizes: vec![(300, 250)],
                },
                CollectedGptSlot {
                    gam_unit_path: "/123/second-batch".to_string(),
                    div_id: "ad-second-batch-0".to_string(),
                    sizes: vec![(728, 90)],
                },
            ],
            "the dwell window should outlast a gap between registration bursts"
        );
        assert!(
            collected.html.contains("data-gpt-batch=\"second\""),
            "should capture DOM changes made during GPT polling"
        );
        assert!(
            collected.script_tags.iter().any(|script| {
                script
                    .src
                    .as_deref()
                    .is_some_and(|src| src.ends_with("/late-evidence.js"))
            }),
            "should capture scripts added during GPT polling"
        );
        assert!(
            collected
                .network_requests
                .iter()
                .any(|request| request.url.ends_with("/late-evidence.js")),
            "should capture network evidence from during GPT polling"
        );
    }

    fn navigation_response_with_status(status: i64, status_text: &str) -> ArcHttpRequest {
        let mut request =
            HttpRequest::new(RequestId::new("request-1"), None, None, false, Vec::new());
        request.response = Some(
            Response::builder()
                .url("https://example.com/")
                .status(status)
                .status_text(status_text)
                .headers(Headers::default())
                .mime_type("text/html")
                .charset("utf-8")
                .connection_reused(false)
                .connection_id(1.0)
                .encoded_data_length(0.0)
                .security_state(SecurityState::Secure)
                .build()
                .expect("should build navigation response"),
        );

        Some(Arc::new(request))
    }
}
