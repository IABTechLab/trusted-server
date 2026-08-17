use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::ArcHttpRequest;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::handler::viewport::Viewport;
use futures::StreamExt as _;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::runtime::Builder;
use tokio::time::{sleep, timeout};
use url::Url;
use which::which;

use crate::commands::audit::generate::collector::{
    AuditCollector, CollectedGptSlot, CollectedLink, CollectedPage, CollectedRequest,
    CollectedScriptTag, ControlFlow, PageSink,
};
use crate::error::{CliResult, report_error};

const SETTLE_QUIET_PERIOD: Duration = Duration::from_millis(750);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SETTLE_MAX_WAIT: Duration = Duration::from_secs(12);
/// How long to wait for the navigation `load` event (and, separately, the main
/// document response) before falling through to the settle loop. Ad-heavy pages
/// (video players, continuous ad refresh) may never fire `load`, so this is a
/// soft bound: the settle loop is the real readiness signal and the scrape reads
/// whatever rendered by then.
const NAVIGATION_LOAD_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD: usize = 250;
const RESOURCE_TIMING_BUFFER_WARNING: &str =
    "browser resource timing buffer reached its default size; some network assets may be missing";

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
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BrowserAuditCollector {
    profile: Option<DeviceProfile>,
    /// Pause between page loads during a crawl.
    page_delay: Duration,
    /// Run a visible browser instead of a headless one.
    headful: bool,
}

impl BrowserAuditCollector {
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

    /// Runs a visible browser rather than a headless one.
    ///
    /// Headless Chrome is trivially detectable and is scored heavily by bot
    /// protection, so an origin that serves a real page to a normal browser may
    /// answer the same request headless with a challenge.
    #[must_use]
    pub(crate) fn headful(mut self, headful: bool) -> Self {
        self.headful = headful;
        self
    }
}

/// The browser-session knobs one crawl runs under.
#[derive(Debug, Clone, Copy)]
struct SessionSettings {
    profile: Option<DeviceProfile>,
    page_delay: Duration,
    headful: bool,
}

impl BrowserAuditCollector {
    fn session(self) -> SessionSettings {
        SessionSettings {
            profile: self.profile,
            page_delay: self.page_delay,
            headful: self.headful,
        }
    }
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
            with_browser(
                std::slice::from_ref(target_url),
                cookies,
                settings,
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

        runtime.block_on(with_browser(targets, cookies, self.session(), on_page))
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
    targets: &[Url],
    cookies: &[(String, String)],
    settings: SessionSettings,
    sink: PageSink<'_>,
) -> CliResult<()> {
    let SessionSettings {
        profile,
        page_delay,
        headful,
    } = settings;
    let chrome_executable = find_browser_executable()?;
    let user_data_dir = TempDir::new().map_err(|error| {
        report_error(format!(
            "failed to create temporary browser profile for audit: {error}"
        ))
    })?;
    // chromiumoxide ignores TLS errors by default. `generate` sends operator
    // cookies and writes what it scrapes into the operator's config, so a
    // certificate-invalid impersonator could both harvest the session and seed
    // the config with slots of its choosing. Validate certificates.
    let mut builder = BrowserConfig::builder()
        .chrome_executable(chrome_executable)
        .user_data_dir(user_data_dir.path())
        .respect_https_errors();
    // `BrowserConfig` defaults to the *old* headless mode, which is both more
    // detectable and less faithful than either alternative — so both branches
    // must be explicit. Omitting the call is not the same as running headful.
    builder = if headful {
        builder.with_head()
    } else {
        builder.new_headless_mode()
    };
    if let Some(profile) = profile {
        let viewport = profile.viewport();
        builder = builder
            .window_size(viewport.width, viewport.height)
            .viewport(viewport);
        if let Some(user_agent) = profile.user_agent() {
            // Ad stacks branch on the user agent as well as the viewport, so
            // emulating size alone can still return desktop ad units.
            builder = builder.arg(format!("--user-agent={user_agent}"));
        }
    }
    let config = builder.build().map_err(|error| {
        report_error(format!(
            "failed to build Chromium configuration for audit: {error}"
        ))
    })?;

    let (mut browser, mut handler) = Browser::launch(config).await.map_err(|error| {
        report_error(format!(
            "failed to launch Chrome/Chromium for audit: {error}"
        ))
    })?;

    let handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    // Sitemap discovery is a whole-site fact, so only the first target pays for it.
    let mut result = Ok(());
    for (index, target) in targets.iter().enumerate() {
        // Pace the crawl. Back-to-back navigations are both discourteous to the
        // origin and a signal bot protection scores against the session.
        if index > 0 && !page_delay.is_zero() {
            sleep(page_delay).await;
        }
        let collected = collect_page_from_browser(&mut browser, target, cookies, index == 0).await;
        match sink(target, collected) {
            Ok(ControlFlow::Continue) => {}
            Ok(ControlFlow::Stop) => break,
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }

    let close_result = timeout(BROWSER_CLOSE_TIMEOUT, browser.close())
        .await
        .map_err(|_| report_error("timed out closing browser after audit"))
        .and_then(|closed| {
            closed.map_err(|error| {
                report_error(format!("failed to close browser after audit: {error}"))
            })
        });
    if close_result.is_err() {
        handler_task.abort();
    }
    let _ = handler_task.await;

    match (result, close_result) {
        (Ok(()), Ok(_)) => Ok(()),
        (Ok(()), Err(error)) | (Err(error), _) => Err(error),
    }
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
) -> CliResult<CollectedPage> {
    let page = browser.new_page("about:blank").await.map_err(|error| {
        report_error(format!("failed to create browser page for audit: {error}"))
    })?;

    // Set operator-supplied cookies before navigating so the origin sees an
    // authenticated session on the first request. Scoping each to the target URL
    // lets Chrome infer domain/path.
    for (name, value) in cookies {
        let mut cookie = CookieParam::new(name.clone(), value.clone());
        cookie.url = Some(target_url.to_string());
        page.set_cookie(cookie)
            .await
            .map_err(|error| report_error(format!("failed to set cookie `{name}`: {error}")))?;
    }

    let mut warnings = Vec::new();

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

    if !wait_for_page_settle(&page).await? {
        warnings.push(
            "browser audit timed out while waiting for the page to settle; results may be partial"
                .to_string(),
        );
    }

    let final_url = page
        .url()
        .await
        .map_err(|error| report_error(format!("failed to read final page URL: {error}")))?
        .ok_or_else(|| report_error("browser page URL was empty after navigation"))?;
    let page_title = page
        .get_title()
        .await
        .map_err(|error| report_error(format!("failed to read page title: {error}")))?;
    let html = page
        .content()
        .await
        .map_err(|error| report_error(format!("failed to read rendered page HTML: {error}")))?;

    let script_tags: Vec<BrowserScriptTag> = page
        .evaluate(
            r#"() => Array.from(document.scripts).map((script) => ({
                src: script.src || null,
                inline_text: script.src ? null : (script.textContent || null),
            }))"#,
        )
        .await
        .map_err(|error| report_error(format!("failed to read rendered script tags: {error}")))?
        .into_value()
        .map_err(|error| {
            report_error(format!(
                "failed to decode rendered script tag data: {error}"
            ))
        })?;

    let network_requests: Vec<BrowserPerformanceEntry> = page
        .evaluate(
            r#"() => performance.getEntriesByType('resource').map((entry) => ({
                url: entry.name,
                initiator_type: entry.initiatorType || null,
            }))"#,
        )
        .await
        .map_err(|error| {
            report_error(format!(
                "failed to read browser performance resource entries: {error}"
            ))
        })?
        .into_value()
        .map_err(|error| {
            report_error(format!(
                "failed to decode browser performance resource data: {error}"
            ))
        })?;

    if let Some(warning) = resource_timing_buffer_warning(network_requests.len()) {
        warnings.push(warning.to_string());
    }

    // Best-effort read of the live GPT slot registry. This is the authoritative
    // source for slot path/div/size, so a failure here downgrades to empty
    // rather than failing the whole audit.
    let gpt_slots: Vec<CollectedGptSlot> = match page.evaluate(GPT_SLOTS_SCRIPT).await {
        Ok(result) => result.into_value().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // Links come from the hydrated DOM, not the served markup: an app-router
    // page keeps its link graph in the framework payload, so parsing the raw
    // HTML finds only a fraction of the site's sections. Best-effort — an empty
    // list just means crawl planning falls back to other sources.
    let links: Vec<CollectedLink> = match page.evaluate(LINKS_SCRIPT).await {
        Ok(result) => result.into_value().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // Sitemap discovery is a whole-site fact, so it runs once per crawl. A miss
    // is normal (no sitemap, robots 404, fetch blocked) and leaves planning to
    // the link graph alone.
    let mut sitemap_locs: Vec<String> = if discover_sitemap {
        match page.evaluate(SITEMAP_SCRIPT).await {
            Ok(result) => result.into_value().unwrap_or_default(),
            Err(_) => Vec::new(),
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

async fn wait_for_page_settle(page: &chromiumoxide::Page) -> CliResult<bool> {
    let mut elapsed = Duration::ZERO;
    let mut previous_count = None;
    let mut stable_for = Duration::ZERO;

    while elapsed < SETTLE_MAX_WAIT {
        let ready_state: String = page
            .evaluate("document.readyState")
            .await
            .map_err(|error| report_error(format!("failed to read document ready state: {error}")))?
            .into_value()
            .map_err(|error| {
                report_error(format!("failed to decode document ready state: {error}"))
            })?;
        let resource_count: usize = page
            .evaluate("performance.getEntriesByType('resource').length")
            .await
            .map_err(|error| report_error(format!("failed to read resource count: {error}")))?
            .into_value()
            .map_err(|error| report_error(format!("failed to decode resource count: {error}")))?;

        // Accept `interactive` as well as `complete`: ad-heavy pages often never
        // reach `complete` (the `load` event never fires), but their GPT slots
        // are defined once the DOM is interactive, so a quiet network period at
        // `interactive` is a valid settle signal for the slot scrape.
        if ready_state == "complete" || ready_state == "interactive" {
            if previous_count == Some(resource_count) {
                stable_for += SETTLE_POLL_INTERVAL;
            } else {
                stable_for = Duration::ZERO;
            }

            if stable_for >= SETTLE_QUIET_PERIOD {
                return Ok(true);
            }
        }

        previous_count = Some(resource_count);
        sleep(SETTLE_POLL_INTERVAL).await;
        elapsed += SETTLE_POLL_INTERVAL;
    }

    Ok(false)
}

fn validate_navigation_response(navigation_response: ArcHttpRequest) -> CliResult<Option<String>> {
    let request = navigation_response
        .ok_or_else(|| report_error("browser audit did not capture the main document response"))?;

    if let Some(failure_text) = &request.failure_text {
        return Err(report_error(format!(
            "main document request failed: {failure_text}"
        )));
    }

    let response = request.response.as_ref().ok_or_else(|| {
        report_error("browser audit did not capture the main document HTTP response")
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
    (resource_count >= RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD)
        .then_some(RESOURCE_TIMING_BUFFER_WARNING)
}

fn find_browser_executable() -> CliResult<PathBuf> {
    for candidate in browser_executable_path_candidates() {
        if let Ok(path) = which(candidate) {
            return Ok(path);
        }
    }

    for candidate in browser_executable_fallbacks() {
        let candidate_path = Path::new(candidate);
        if candidate_path.is_file() {
            return Ok(candidate_path.to_path_buf());
        }
    }

    Err(report_error(
        "Chrome/Chromium was not found on PATH or in the standard local install locations checked by `ts audit`. Install a local Chrome or Chromium binary before running `ts audit`.",
    ))
}

fn browser_executable_path_candidates() -> &'static [&'static str] {
    &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "Google Chrome",
        "Google Chrome for Testing",
    ]
}

fn browser_executable_fallbacks() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ]
    }

    #[cfg(target_os = "linux")]
    {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        &[]
    }
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
    use std::sync::Arc;

    use chromiumoxide::cdp::browser_protocol::network::{Headers, RequestId, Response};
    use chromiumoxide::cdp::browser_protocol::security::SecurityState;
    use chromiumoxide::handler::http::HttpRequest;

    use super::*;

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
    fn resource_timing_buffer_warning_starts_at_threshold() {
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD - 1),
            None,
            "should not warn before the resource timing buffer threshold"
        );
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD),
            Some(RESOURCE_TIMING_BUFFER_WARNING),
            "should warn when the resource timing buffer reaches the threshold"
        );
    }

    #[test]
    fn browser_path_candidates_include_common_names() {
        let candidates = browser_executable_path_candidates();

        assert!(candidates.contains(&"google-chrome"));
        assert!(candidates.contains(&"chromium"));
        assert!(candidates.contains(&"Google Chrome for Testing"));
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
