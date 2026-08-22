//! Collector abstraction shared by the generic page audit and the ad-template
//! verifier.
//!
//! Decoupling collection behind [`AuditCollector`] lets the verifier orchestration
//! (Task 9) be tested with an in-memory fake collector, with no Chrome dependency.

use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::ad_templates::compare::BrowserAdEvidence;

/// Default quiet window for generation's browser collector.
pub(crate) const GENERATE_SETTLE_QUIET_MS: u64 = 750;
/// Default maximum settle wait for generation's browser collector.
pub(crate) const GENERATE_SETTLE_MAX_MS: u64 = 12_000;

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
    /// Browser device profile used for viewport and user-agent emulation.
    #[arg(long = "browser-profile", value_enum, default_value_t = BrowserProfile::Desktop)]
    pub profile: BrowserProfile,
    /// Run a visible browser instead of Chrome's new headless mode.
    #[arg(long)]
    pub headful: bool,
    /// Do not answer the standard IAB consent APIs for the fresh audit profile.
    #[arg(long)]
    pub no_assume_consent: bool,
    /// Route the browser through this proxy, as `host:port` or a full URL.
    #[arg(long, value_name = "HOST:PORT")]
    pub browser_proxy: Option<String>,
    /// Quiet window in milliseconds (no new network resources) that marks the
    /// page settled.
    #[arg(long, default_value_t = 750)]
    pub settle_quiet_ms: u64,
    /// Hard cap in milliseconds on waiting for the page to settle.
    #[arg(long, default_value_t = 10_000)]
    pub settle_max_ms: u64,
    /// Navigate to origins whose TLS certificate does not validate.
    ///
    /// DANGEROUS: the audit sends any `--cookie` session to the origin and
    /// treats what it reads back as verification evidence, so an invalid
    /// certificate could mean an impersonator is harvesting the session and
    /// fabricating the evidence. Use only against a host you control with a
    /// known self-signed certificate.
    #[arg(long)]
    pub danger_accept_invalid_certs: bool,
}

/// Browser options for generation, whose device selection is controlled by
/// `--profiles` rather than the verifier's singular `--browser-profile`.
#[derive(Debug, Clone, Args)]
pub struct GenerateBrowserOpts {
    /// Path to the Chrome/Chromium executable. Falls back to `$CHROME`, then auto-detection.
    #[arg(long)]
    pub chrome: Option<PathBuf>,
    /// Run a visible browser instead of Chrome's new headless mode.
    #[arg(long)]
    pub headful: bool,
    /// Do not answer the standard IAB consent APIs for the fresh audit profile.
    #[arg(long)]
    pub no_assume_consent: bool,
    /// Route the browser through this proxy, as `host:port` or a full URL.
    #[arg(long, value_name = "HOST:PORT")]
    pub browser_proxy: Option<String>,
    /// Quiet window in milliseconds that marks the page settled.
    #[arg(long, default_value_t = GENERATE_SETTLE_QUIET_MS)]
    pub settle_quiet_ms: u64,
    /// Hard cap in milliseconds on waiting for the page to settle.
    #[arg(long, default_value_t = GENERATE_SETTLE_MAX_MS)]
    pub settle_max_ms: u64,
    /// Navigate to origins whose TLS certificate does not validate.
    ///
    /// DANGEROUS: the audit sends any `--cookie` session to the origin and
    /// treats what it reads back as the evidence it writes config from, so an
    /// invalid certificate could mean an impersonator is harvesting the session
    /// and fabricating the evidence. Use only against a host you control with a
    /// known self-signed certificate.
    #[arg(long)]
    pub danger_accept_invalid_certs: bool,
}

/// Defaults mirroring the `#[arg(default_value_t)]` values above, so a path that
/// builds these options in code (the legacy `ts audit <url>` form) behaves like
/// the parsed command.
impl Default for GenerateBrowserOpts {
    fn default() -> Self {
        Self {
            chrome: None,
            headful: false,
            no_assume_consent: false,
            browser_proxy: None,
            settle_quiet_ms: GENERATE_SETTLE_QUIET_MS,
            settle_max_ms: GENERATE_SETTLE_MAX_MS,
            danger_accept_invalid_certs: false,
        }
    }
}

impl GenerateBrowserOpts {
    /// Validates relationships between independently parsed browser flags.
    pub fn validate(&self) -> Result<(), String> {
        validate_settle_window(self.settle_quiet_ms, self.settle_max_ms)
    }
}

/// Browser device profile shared by page audits and ad-template verification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum BrowserProfile {
    /// Desktop Chrome at 1280×800.
    #[default]
    Desktop,
    /// Mobile-sized viewport with a mobile user agent.
    Mobile,
}

impl BrowserOpts {
    /// Validates relationships between independently parsed browser flags.
    pub fn validate(&self) -> Result<(), String> {
        validate_settle_window(self.settle_quiet_ms, self.settle_max_ms)
    }
}

fn validate_settle_window(quiet_ms: u64, max_ms: u64) -> Result<(), String> {
    if quiet_ms > max_ms {
        return Err(format!(
            "--settle-quiet-ms ({quiet_ms}) cannot exceed --settle-max-ms ({max_ms})"
        ));
    }
    Ok(())
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

    /// Collects several pages, preserving request order.
    ///
    /// Browser-backed implementations override this to reuse one runtime,
    /// browser, and profile. The default keeps in-memory test collectors and
    /// other simple implementations source-compatible.
    fn collect_pages(
        &self,
        requests: &[BrowserCollectRequest],
    ) -> Vec<Result<CollectedPage, String>> {
        requests
            .iter()
            .cloned()
            .map(|request| self.collect_page(request))
            .collect()
    }
}

/// Configuration handed to the read-only ad-template collector script.
///
/// Only configured div prefixes are embedded — no page data is requested.
// Assembled by the ad-template verifier from the configured slots.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AdTemplateCollectorConfig {
    /// Configured slot div ID prefixes to match in the DOM.
    pub div_prefixes: Vec<String>,
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
    fn generation_browser_defaults_preserve_the_established_settle_window() {
        let options = GenerateBrowserOpts::default();

        assert_eq!(options.settle_quiet_ms, 750);
        assert_eq!(options.settle_max_ms, 12_000);
    }

    #[test]
    fn init_script_embeds_config_and_read_only_hooks() {
        let config = AdTemplateCollectorConfig {
            div_prefixes: vec!["ad-atf-".to_string()],
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
        // Bounded GPT instrumentation plus on-demand scrape.
        assert!(
            script.contains("__ts_install(\"googletag\""),
            "should install googletag hook"
        );
        assert!(
            !script.contains("googletag.cmd.push = function"),
            "must not replace the publisher's variadic cmd.push"
        );
        assert!(script.contains("defineSlot"), "should record defineSlot");
        assert!(!script.contains("fetchBids"), "APS should not be mutated");
        assert!(
            script.contains("new WeakSet()"),
            "should track wrapped objects without publisher-visible markers"
        );
        assert!(
            script.contains("Object.getOwnPropertyDescriptor(")
                && script.contains("\"defineSlot\"")
                && script.contains("enumerable: descriptor ? descriptor.enumerable : true"),
            "wrapped methods should preserve the publisher's enumerability"
        );
        assert!(
            script.contains("4294967295"),
            "sizes above Rust's u32 range must be rejected in-page"
        );
        assert!(
            script.contains("slice(0, __ts_max_string_length)"),
            "publisher-controlled strings should be capped in-page"
        );
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
