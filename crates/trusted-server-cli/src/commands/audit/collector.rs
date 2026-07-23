//! Collector abstraction shared by the generic page audit and the ad-template
//! verifier.
//!
//! Decoupling collection behind [`AuditCollector`] lets the verifier orchestration
//! (Task 9) be tested with an in-memory fake collector, with no Chrome dependency.

use std::path::PathBuf;

use clap::Args;

use crate::ad_templates::compare::BrowserAdEvidence;

/// Operator-tunable browser options shared by `ts audit page` and
/// `ts audit ad-templates verify`.
///
/// These are audit-tool knobs, not publisher runtime config, so they live on the
/// CLI (flags / `CHROME` env) rather than in `trusted-server.toml`.
#[derive(Debug, Clone, Args)]
pub struct BrowserOpts {
    /// Path to the Chrome/Chromium executable. Falls back to `$CHROME`, then
    /// auto-detection on `PATH` and standard install locations.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
    /// Quiet window in milliseconds (no new network resources) that marks the
    /// page settled.
    #[arg(long, default_value_t = 750)]
    pub settle_quiet_ms: u64,
    /// Hard cap in milliseconds on waiting for the page to settle.
    #[arg(long, default_value_t = 10_000)]
    pub settle_max_ms: u64,
}

/// A request to collect a single page.
#[derive(Debug, Clone)]
pub struct BrowserCollectRequest {
    /// The URL to navigate to.
    pub url: url::Url,
    /// Pre-navigation init scripts (evaluate-on-new-document). Empty for a plain
    /// page audit; the ad-template verifier supplies the read-only collector here.
    pub init_scripts: Vec<String>,
    /// Whether to perform the deterministic scroll pass after settle.
    pub scroll: bool,
    /// Whether to extract `window.__tsAdTemplateEvidence` after settle/scroll.
    pub collect_ad_evidence: bool,
    /// Operator-supplied `(name, value)` cookies set on the browser context
    /// before navigation, scoped to the request URL. Used to carry an existing
    /// authenticated session (e.g. a valid bot-protection clearance cookie) so
    /// the origin serves the real page instead of a challenge. The collector
    /// only sends these; it never reads cookies back.
    pub cookies: Vec<(String, String)>,
}

/// The result of collecting a single page.
#[derive(Debug, Clone)]
pub struct CollectedPage {
    /// The final URL after redirects.
    pub final_url: url::Url,
    /// The page title.
    pub title: String,
    /// Number of `<script>` resources observed (counts only; no content).
    pub script_count: usize,
    /// Number of resource entries observed (counts only; no content).
    pub resource_count: usize,
    /// Collector-level warnings (no page HTML/cookies/storage).
    pub warnings: Vec<crate::ad_templates::output::Warning>,
    /// Ad-template evidence, present only when `collect_ad_evidence` was requested.
    pub ad_evidence: Option<BrowserAdEvidence>,
}

/// A source of collected page evidence.
pub trait AuditCollector {
    /// Collects a single page per `request`.
    ///
    /// # Errors
    ///
    /// Returns a user-facing string when the browser cannot be launched or the
    /// navigation fails before any result can be produced.
    fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String>;
}

/// Configuration handed to the read-only ad-template collector script.
///
/// Only the configured div prefixes and APS slot IDs are embedded — no page data
/// is requested. Serialized into the injected `__TS_CONFIG`.
// Assembled by the ad-template verifier from the configured slots.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AdTemplateCollectorConfig {
    /// Configured slot div ID prefixes to match in the DOM.
    pub div_prefixes: Vec<String>,
    /// Configured APS slot IDs (reserved for provider-scoped filtering).
    pub aps_slot_ids: Vec<String>,
}

/// Builds the read-only ad-template init script, embedding `config` as `__TS_CONFIG`.
///
/// The returned script is installed via evaluate-on-new-document before the
/// publisher's own scripts run.
///
/// # Errors
///
/// Returns a user-facing string when the collector config cannot be serialized.
pub fn build_ad_template_init_script(config: &AdTemplateCollectorConfig) -> Result<String, String> {
    let config_json = serde_json::to_string(config)
        .map_err(|error| format!("failed to serialize ad-template collector config: {error}"))?;
    Ok(format!(
        ";(() => {{ const __TS_CONFIG = {config_json};\n{}\n}})();",
        include_str!("ad_template_collector.js")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad_templates::compare::BrowserAdEvidence;

    #[test]
    fn init_script_embeds_config_and_read_only_hooks() {
        let config = AdTemplateCollectorConfig {
            div_prefixes: vec!["ad-atf-".to_string()],
            aps_slot_ids: vec!["atf".to_string()],
        };
        let script = build_ad_template_init_script(&config).expect("should build script");

        // Config is injected and only the configured prefix is embedded.
        assert!(
            script.contains("__TS_CONFIG"),
            "should inject config object"
        );
        assert!(
            script.contains("ad-atf-"),
            "should embed the configured div prefix"
        );
        assert!(
            !script.contains("ad-not-configured-"),
            "should not embed other prefixes"
        );
        // Read-only instrumentation markers (googletag/apstag wrapping + on-demand scrape).
        assert!(
            script.contains("__ts_install(\"googletag\""),
            "should install googletag hook"
        );
        assert!(script.contains("cmd.push"), "should wrap cmd.push");
        assert!(script.contains("defineSlot"), "should record defineSlot");
        assert!(script.contains("fetchBids"), "should wrap apstag.fetchBids");
        assert!(
            script.contains("window.__tsCollectAdTemplateEvidence"),
            "should expose the on-demand scrape function"
        );
        // Must never capture page data or spoof automation flags.
        assert!(!script.contains("document.cookie"), "must not read cookies");
        assert!(
            !script.contains("localStorage"),
            "must not read localStorage"
        );
        assert!(
            !script.contains("navigator.webdriver"),
            "must not override navigator.webdriver"
        );
    }

    #[test]
    fn collector_payload_decodes_into_browser_ad_evidence() {
        // Mirrors the JSON shape the collector writes to window.__tsAdTemplateEvidence.
        let payload = r#"{
            "dom_ids": [{ "dom_id": "ad-atf-0", "phase": "initial_load" }],
            "gpt_slots": [
                {
                    "gam_unit_path": "/123/news/atf",
                    "div_id": "ad-atf-0",
                    "sizes": [[300, 250]],
                    "phase": "initial_load"
                }
            ],
            "aps_calls": [{ "slot_id": "atf", "sizes": [[300, 250]], "phase": "scroll" }],
            "warnings": [{ "code": "fluid_size_ignored", "message": "non-numeric GPT size ignored" }]
        }"#;

        let evidence: BrowserAdEvidence =
            serde_json::from_str(payload).expect("collector payload should decode");

        assert_eq!(evidence.dom_ids.len(), 1);
        assert_eq!(evidence.dom_ids[0].dom_id, "ad-atf-0");
        assert_eq!(evidence.gpt_slots[0].gam_unit_path, "/123/news/atf");
        assert_eq!(evidence.gpt_slots[0].sizes, vec![(300, 250)]);
        assert_eq!(evidence.aps_calls[0].slot_id, "atf");
        // page_bids is absent in the payload and defaults to empty.
        assert!(evidence.page_bids.is_empty());
        assert_eq!(evidence.warnings[0].code, "fluid_size_ignored");
    }
}
