use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CliResult;

/// Warning recorded on a page collected with the audit consent stub installed.
///
/// A whole-run fact rather than a property of one page, so consumers report it
/// once and unscoped instead of once per page and per profile.
pub(crate) const CONSENT_STUB_WARNING: &str = "consent_stub_active: audit consent APIs were stubbed; re-run with --no-assume-consent to observe the publisher CMP without substitution";

/// A user-visible phase reached while collecting browser audit evidence.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CollectionProgress<'a> {
    /// The browser process is about to launch.
    Launching,
    /// A page navigation is about to begin.
    Loading {
        /// One-based position of this attempted page in the crawl.
        current: usize,
        /// Total pages when planning has completed, or `None` for the root.
        total: Option<usize>,
        /// Target page; renderers must omit credentials, query, and fragment.
        url: &'a Url,
    },
    /// Follow-up pages are being selected from the collected root page.
    Planning,
    /// The browser session is being closed and its process reaped.
    Finalizing,
}

/// Sink invoked synchronously when browser collection reaches a visible phase.
///
/// Returning an error stops new collection work. An already-launched browser
/// must still be finalized, closed, and waited on before that error is returned.
pub(crate) type ProgressSink<'a> =
    &'a mut dyn for<'event> FnMut(CollectionProgress<'event>) -> CliResult<()>;

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
    ///
    /// What this can prevent depends on the collector: a sequential one loads no
    /// further pages, while the browser collector has already finished
    /// navigating by the time it folds, so there it only stops the fold.
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
    /// Collectors may buffer results until the browser session closes so CPU-heavy
    /// HTML analysis cannot starve a single-threaded CDP event pump. The sink API
    /// keeps that buffering policy private and lets simple collectors stream.
    ///
    /// # Errors
    ///
    /// Returns an error when `on_page` does, or when the session itself cannot
    /// be established. Individual page failures are delivered to `on_page`.
    fn collect_pages(
        &self,
        targets: &[Url],
        cookies: &[(String, String)],
        on_progress: ProgressSink<'_>,
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        for (index, target) in targets.iter().enumerate() {
            on_progress(CollectionProgress::Loading {
                current: index + 1,
                total: Some(targets.len()),
                url: target,
            })?;
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
        on_progress: ProgressSink<'_>,
        planner: RootPlanner<'_>,
        on_page: PageSink<'_>,
    ) -> CliResult<()> {
        on_progress(CollectionProgress::Loading {
            current: 1,
            total: None,
            url: root,
        })?;
        // A root failure is reported through `on_page` rather than returned, so
        // the caller sees the reason as a per-page note exactly as it does from
        // the browser collector. With no root page there is nothing to plan
        // from, so the crawl ends here.
        let root_page = match self.collect_page(root, cookies) {
            Ok(page) => page,
            Err(error) => {
                on_page(root, Err(error))?;
                return Ok(());
            }
        };
        on_progress(CollectionProgress::Planning)?;
        let targets = planner(root, &root_page)?;
        if on_page(root, Ok(root_page))? == ControlFlow::Stop {
            return Ok(());
        }
        let total = targets.len() + 1;
        for (index, target) in targets.iter().enumerate() {
            on_progress(CollectionProgress::Loading {
                current: index + 2,
                total: Some(total),
                url: target,
            })?;
            let collected = self.collect_page(target, cookies);
            if on_page(target, collected)? == ControlFlow::Stop {
                break;
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{cli_error, report_error};

    struct ProgressCollector;

    impl AuditCollector for ProgressCollector {
        fn collect_page(
            &self,
            target_url: &Url,
            _cookies: &[(String, String)],
        ) -> CliResult<CollectedPage> {
            if target_url.path() == "/broken" {
                return cli_error("simulated page failure");
            }
            Ok(CollectedPage {
                requested_url: target_url.to_string(),
                final_url: target_url.to_string(),
                page_title: None,
                html: String::new(),
                script_tags: Vec::new(),
                network_requests: Vec::new(),
                gpt_slots: Vec::new(),
                links: Vec::new(),
                sitemap_locs: Vec::new(),
                warnings: Vec::new(),
            })
        }
    }

    fn record_progress(event: CollectionProgress<'_>) -> String {
        match event {
            CollectionProgress::Launching => "launching".to_string(),
            CollectionProgress::Loading {
                current,
                total,
                url,
            } => format!(
                "loading:{current}/{}:{}",
                total.map_or_else(|| "?".to_string(), |total| total.to_string()),
                url.path()
            ),
            CollectionProgress::Planning => "planning".to_string(),
            CollectionProgress::Finalizing => "finalizing".to_string(),
        }
    }

    #[test]
    fn default_collect_site_reports_root_planning_and_offset_followups() {
        let collector = ProgressCollector;
        let root = Url::parse("https://publisher.example/").expect("should parse root URL");
        let news = Url::parse("https://publisher.example/news").expect("should parse news URL");
        let broken =
            Url::parse("https://publisher.example/broken").expect("should parse broken URL");
        let mut events = Vec::new();
        let mut outcomes = Vec::new();

        collector
            .collect_site(
                &root,
                &[],
                &mut |event| {
                    events.push(record_progress(event));
                    Ok(())
                },
                &mut |_, _| Ok(vec![news.clone(), broken.clone()]),
                &mut |url, result| {
                    outcomes.push((url.path().to_string(), result.is_ok()));
                    Ok(ControlFlow::Continue)
                },
            )
            .expect("should collect site despite one page outcome failing");

        assert_eq!(
            events,
            [
                "loading:1/?:/",
                "planning",
                "loading:2/3:/news",
                "loading:3/3:/broken",
            ]
        );
        assert_eq!(
            outcomes,
            [
                ("/".to_string(), true),
                ("/news".to_string(), true),
                ("/broken".to_string(), false)
            ]
        );
    }

    #[test]
    fn default_collect_pages_reports_a_fixed_total() {
        let collector = ProgressCollector;
        let targets = [
            Url::parse("https://publisher.example/").expect("should parse root URL"),
            Url::parse("https://publisher.example/broken").expect("should parse broken URL"),
        ];
        let mut events = Vec::new();

        collector
            .collect_pages(
                &targets,
                &[],
                &mut |event| {
                    events.push(record_progress(event));
                    Ok(())
                },
                &mut |_, _| Ok(ControlFlow::Continue),
            )
            .expect("should deliver failed page as an outcome");

        assert_eq!(events, ["loading:1/2:/", "loading:2/2:/broken"]);
    }

    #[test]
    fn default_collection_stops_when_progress_fails() {
        let collector = ProgressCollector;
        let targets = [Url::parse("https://publisher.example/").expect("should parse root URL")];

        let error = collector
            .collect_pages(
                &targets,
                &[],
                &mut |_| Err(report_error("simulated progress failure")),
                &mut |_, _| panic!("page sink should not run after progress failure"),
            )
            .expect_err("should return progress failure");

        assert!(format!("{error:?}").contains("simulated progress failure"));
    }
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
