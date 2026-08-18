use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CliResult;

/// Sink invoked once per collected page during a batch crawl.
///
/// Receives the per-page outcome so a failed page can be folded into the run as
/// a warning rather than aborting it; returning `Err` stops the crawl.
pub(crate) type PageSink<'a> =
    &'a mut dyn FnMut(&Url, CliResult<CollectedPage>) -> CliResult<ControlFlow>;

/// Plans follow-up URLs from the successfully collected root page.
pub(crate) type RootPlanner<'a> = &'a mut dyn FnMut(&Url, &CollectedPage) -> CliResult<Vec<Url>>;

/// Whether a batch crawl should keep going after a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlFlow {
    /// Collect the next target.
    Continue,
    /// Stop the crawl without an error (budget reached, challenge rate exceeded).
    Stop,
}

pub(crate) trait AuditCollector {
    /// Collects a live page. `cookies` are `(name, value)` pairs set on the
    /// browser context before navigation (scoped to `target_url`) so an existing
    /// session — e.g. a valid bot-protection clearance cookie — can carry the
    /// audit past an origin challenge.
    fn collect_page(
        &self,
        target_url: &Url,
        cookies: &[(String, String)],
    ) -> CliResult<CollectedPage>;

    /// Collects several pages in one session, handing each result to `on_page`.
    ///
    /// The default implementation loops over [`collect_page`](Self::collect_page),
    /// which keeps every existing implementor working unchanged. The browser
    /// collector overrides it to reuse one Chrome instance and profile across the
    /// crawl — a fresh launch per page dominates the cost of a multi-page run,
    /// and a shared profile carries bot-protection clearance cookies site-wide.
    ///
    /// Results are streamed rather than returned as a `Vec` so the caller can
    /// fold each page into its evidence and drop the page's HTML immediately,
    /// instead of holding every DOM serialization at once.
    ///
    /// # Errors
    ///
    /// Returns an error when `on_page` does, or when the session itself cannot
    /// be established. Individual page failures are delivered to `on_page`.
    fn collect_pages(
        &self,
        targets: &[Url],
        cookies: &[(String, String)],
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        for target in targets {
            let collected = self.collect_page(target, cookies);
            if on_page(target, collected)? == ControlFlow::Stop {
                break;
            }
        }
        Ok(())
    }

    /// Collects a root and follow-up URLs planned from it in one logical crawl.
    ///
    /// The browser implementation overrides this so planning happens while the
    /// root's browser/profile remains open. Simple collectors retain equivalent
    /// behavior through the default implementation.
    fn collect_site(
        &self,
        root: &Url,
        cookies: &[(String, String)],
        planner: RootPlanner<'_>,
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        let root_page = self.collect_page(root, cookies)?;
        let targets = planner(root, &root_page)?;
        if on_page(root, Ok(root_page))? == ControlFlow::Stop {
            return Ok(());
        }
        self.collect_pages(&targets, cookies, on_page)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedPage {
    pub(crate) requested_url: String,
    pub(crate) final_url: String,
    pub(crate) page_title: Option<String>,
    pub(crate) html: String,
    pub(crate) script_tags: Vec<CollectedScriptTag>,
    pub(crate) network_requests: Vec<CollectedRequest>,
    /// Slots read from the live GPT registry (`googletag.pubads().getSlots()`).
    ///
    /// Populated at `defineSlot` time, so this captures configured slots even
    /// when the ad request never fires (consent-gated or iframe-issued).
    #[serde(default)]
    pub(crate) gpt_slots: Vec<CollectedGptSlot>,
    /// Same-origin `a[href]` targets read from the hydrated DOM, absolutized.
    ///
    /// Read from the live DOM rather than the served HTML on purpose: an
    /// app-router page keeps its link graph in the framework payload, so parsing
    /// the raw markup finds only a fraction of the site's sections.
    #[serde(default)]
    pub(crate) links: Vec<CollectedLink>,
    /// Sitemap `<loc>` entries discovered from `robots.txt`, when fetched.
    ///
    /// Empty unless sitemap discovery ran (root page only).
    #[serde(default)]
    pub(crate) sitemap_locs: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

/// A same-origin link observed in the hydrated DOM.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedLink {
    /// Absolute URL of the link target.
    pub(crate) url: String,
    /// Whether the anchor sits inside site navigation (`nav`, `header`,
    /// `[role="navigation"]`). Nav links are the publisher's own declaration of
    /// its taxonomy, so they rank above body links when choosing sections.
    pub(crate) in_nav: bool,
}

/// A single slot read from the page's live GPT registry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedGptSlot {
    /// The GAM ad-unit path (`slot.getAdUnitPath()`).
    pub(crate) gam_unit_path: String,
    /// The slot's div element id (`slot.getSlotElementId()`).
    pub(crate) div_id: String,
    /// Numeric `[width, height]` sizes (`slot.getSizes()`, fluid entries dropped).
    pub(crate) sizes: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedScriptTag {
    pub(crate) src: Option<String>,
    pub(crate) inline_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedRequest {
    pub(crate) url: String,
    pub(crate) resource_type: Option<String>,
}

impl CollectedPage {
    pub(crate) fn requested_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.requested_url)
    }

    pub(crate) fn final_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.final_url)
    }
}
