//! Browser-backed `ts audit ad-templates verify` orchestration.
//!
//! For each URL: collect live evidence through an [`AuditCollector`], match
//! configured slots against the **final** (post-redirect) path, evaluate the
//! runtime gate, compare evidence, and assemble the stable §8 wire result. The
//! orchestration is collector-agnostic so it is fully tested with an in-memory
//! fake collector, with no Chrome dependency.

use std::io::{self, Write};

use trusted_server_core::creative_opportunities::{
    AdStackGateInput, CreativeOpportunitiesConfig, evaluate_ad_stack_gate,
};

use crate::ad_templates::compare::{
    BrowserAdEvidence, EvidencePhase, ExtraEvidence, RuntimeGateSummary, SlotEvidence, SlotResult,
    SlotStatus as CompareStatus, compare_page_evidence,
};
use crate::ad_templates::expected::{ExpectedSlot, expected_slots_for_path, normalize_path_or_url};
use crate::ad_templates::output::{
    ConfiguredJson, EvidencePhaseJson, ExtraEvidenceJson, FormatJson, GateState, Gates,
    GptEvidenceJson, PageJson, RuntimeAdStackExpectedJson, SlotEvidenceJson, SlotJson, SlotStatus,
    VerificationReport, Warning,
};
use crate::commands::audit::AuditAdTemplatesVerifyArgs;
use crate::commands::audit::collector::{
    AdTemplateCollectorConfig, AuditCollector, BrowserCollectRequest, build_ad_template_init_script,
};

/// Verifies configured ad-template slots against live page evidence.
///
/// # Errors
///
/// Returns a user-facing string when config loading fails, or when verification
/// surfaces a page-level error or a `--strict` failure (after writing output).
pub(crate) fn run_verify(args: &AuditAdTemplatesVerifyArgs) -> Result<(), String> {
    let loaded = crate::app_config::load_settings(&args.config)?;
    let collector = crate::commands::audit::browser::BrowserCollector::from_opts(&args.browser);
    let report = build_report(
        &collector,
        loaded.settings.creative_opportunities.as_ref(),
        loaded.settings.auction.enabled,
        &args.urls,
        args.strict,
        args.scroll,
        &args.cookies,
    );

    let stdout = io::stdout();
    let mut out = stdout.lock();
    if args.json {
        write_json(&mut out, &report)?;
    } else {
        write_human(&mut out, &report)?;
    }

    if report.ok {
        Ok(())
    } else {
        Err("ad-template verification reported problems".to_string())
    }
}

/// Builds the verification report for `urls` using `collector`.
///
/// `creative` is the effective `[creative_opportunities]` config (if any) and
/// `auction_enabled` is the `[auction].enabled` kill switch.
fn build_report(
    collector: &dyn AuditCollector,
    creative: Option<&CreativeOpportunitiesConfig>,
    auction_enabled: bool,
    urls: &[url::Url],
    strict: bool,
    scroll: bool,
    cookies: &[(String, String)],
) -> VerificationReport {
    let init_script = build_init_script(creative);

    let mut pages = Vec::with_capacity(urls.len());
    let mut any_error = false;
    let mut any_strict_fail = false;

    for url in urls {
        let request = BrowserCollectRequest {
            url: url.clone(),
            init_scripts: init_script.clone().into_iter().collect(),
            scroll,
            collect_ad_evidence: true,
            cookies: cookies.to_vec(),
        };

        match collector.collect_page(request) {
            Err(message) => {
                any_error = true;
                pages.push(error_page(url, &message));
            }
            Ok(collected) => {
                let (page, strict_failed) = build_page(url, &collected, creative, auction_enabled);
                if strict && strict_failed {
                    any_strict_fail = true;
                }
                pages.push(page);
            }
        }
    }

    let ok = !(any_error || (strict && any_strict_fail));
    VerificationReport {
        ok,
        strict,
        pages,
        warnings: Vec::new(),
    }
}

/// Builds the read-only collector init script from the configured slots.
fn build_init_script(creative: Option<&CreativeOpportunitiesConfig>) -> Option<String> {
    let config = AdTemplateCollectorConfig {
        div_prefixes: creative
            .map(|creative| {
                creative
                    .slot
                    .iter()
                    .map(|slot| slot.resolved_div_id().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        aps_slot_ids: creative
            .map(|creative| {
                creative
                    .slot
                    .iter()
                    .filter_map(|slot| slot.providers.aps.as_ref().map(|aps| aps.slot_id.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    };
    build_ad_template_init_script(&config).ok()
}

/// Assembles a successful page result, returning the wire `PageJson` and whether
/// the page would fail `--strict`.
fn build_page(
    requested: &url::Url,
    collected: &crate::commands::audit::collector::CollectedPage,
    creative: Option<&CreativeOpportunitiesConfig>,
    auction_enabled: bool,
) -> (PageJson, bool) {
    let requested_path = normalize_path_or_url(requested.as_str()).unwrap_or_else(|_| "/".into());
    let final_url = &collected.final_url;
    let final_path = normalize_path_or_url(final_url.as_str()).unwrap_or_else(|_| "/".into());

    let expected = creative
        .map(|creative| expected_slots_for_path(&final_path, creative).slots)
        .unwrap_or_default();
    let matched = !expected.is_empty();

    let gate = evaluate_ad_stack_gate(AdStackGateInput {
        method_get: true,
        navigation: true,
        prefetch: false,
        bot: false,
        matched_slots: matched,
        consent_allows_auction: None,
        auction_enabled,
    });

    let evidence = collected.ad_evidence.clone().unwrap_or_else(empty_evidence);
    let result = compare_page_evidence(
        &expected,
        &evidence,
        RuntimeGateSummary::from_expected(gate.expected),
    );
    let strict_failed = result.strict_failed();

    let mut warnings: Vec<Warning> = collected.warnings.to_vec();
    if requested_path != final_path {
        warnings.push(Warning {
            code: "redirected".to_string(),
            message: format!("navigation redirected from {requested_path} to {final_path}"),
        });
    }

    let slots = expected
        .iter()
        .zip(result.slots.iter())
        .map(|(expected_slot, slot_result)| to_slot_json(expected_slot, slot_result))
        .collect();
    let extra_evidence = result.extra_evidence.iter().map(to_extra_json).collect();

    let page = PageJson {
        url: requested.to_string(),
        final_url: Some(final_url.to_string()),
        requested_path,
        path: Some(final_path),
        error: None,
        runtime_ad_stack_expected: Some(RuntimeAdStackExpectedJson::from(
            result.runtime_ad_stack_expected,
        )),
        gates: Some(to_gates(matched, auction_enabled)),
        matched_slot_count: Some(expected.len()),
        slots,
        extra_evidence,
        warnings,
    };
    (page, strict_failed)
}

/// Builds a page-level navigation-failure result (spec §8 `navigation_failed`).
fn error_page(requested: &url::Url, message: &str) -> PageJson {
    let requested_path = normalize_path_or_url(requested.as_str()).unwrap_or_else(|_| "/".into());
    PageJson {
        url: requested.to_string(),
        final_url: None,
        requested_path,
        path: None,
        error: Some(Warning {
            code: "navigation_failed".to_string(),
            message: message.to_string(),
        }),
        runtime_ad_stack_expected: None,
        gates: None,
        matched_slot_count: None,
        slots: Vec::new(),
        extra_evidence: Vec::new(),
        warnings: Vec::new(),
    }
}

fn empty_evidence() -> BrowserAdEvidence {
    BrowserAdEvidence {
        dom_ids: Vec::new(),
        gpt_slots: Vec::new(),
        aps_calls: Vec::new(),
        page_bids: Vec::new(),
        warnings: Vec::new(),
    }
}

fn to_gates(matched: bool, auction_enabled: bool) -> Gates {
    let pass_if = |cond: bool| {
        if cond {
            GateState::Pass
        } else {
            GateState::Fail
        }
    };
    Gates {
        method_get: GateState::Pass,
        navigation: GateState::Pass,
        not_prefetch: GateState::Pass,
        not_bot: GateState::Pass,
        matched_slots: pass_if(matched),
        auction_enabled: pass_if(auction_enabled),
        // Live consent is not provable from a browser navigation in Phase 1.
        consent_allows_auction: GateState::Unknown,
    }
}

fn to_slot_json(expected: &ExpectedSlot, result: &SlotResult) -> SlotJson {
    SlotJson {
        id: result.id.clone(),
        status: to_status(result.status),
        phase: to_phase(result.phase),
        configured: ConfiguredJson {
            div_id: expected.div_id.clone(),
            gam_unit_path: expected.gam_unit_path.clone(),
            formats: expected
                .formats
                .iter()
                .map(|format| FormatJson {
                    width: format.width,
                    height: format.height,
                    media_type: format.media_type.clone(),
                })
                .collect(),
            providers: expected.providers.clone(),
        },
        evidence: to_slot_evidence(&result.evidence),
        warnings: result.warnings.clone(),
    }
}

fn to_slot_evidence(evidence: &SlotEvidence) -> SlotEvidenceJson {
    SlotEvidenceJson {
        dom_id: evidence.dom_id.clone(),
        gpt: evidence.gpt.as_ref().map(|gpt| GptEvidenceJson {
            gam_unit_path: gpt.gam_unit_path.clone(),
            div_id: gpt.div_id.clone(),
            sizes: gpt.sizes.iter().map(|&(w, h)| [w, h]).collect(),
        }),
    }
}

fn to_extra_json(extra: &ExtraEvidence) -> ExtraEvidenceJson {
    ExtraEvidenceJson {
        kind: extra.kind.clone(),
        phase: to_phase(extra.phase),
        dom_id: extra.dom_id.clone(),
        gam_unit_path: extra.gam_unit_path.clone(),
        sizes: extra.sizes.iter().map(|&(w, h)| [w, h]).collect(),
        reason: extra.reason.clone(),
    }
}

fn to_status(status: CompareStatus) -> SlotStatus {
    match status {
        CompareStatus::Confirmed => SlotStatus::Confirmed,
        CompareStatus::Partial => SlotStatus::Partial,
        CompareStatus::Missing => SlotStatus::Missing,
    }
}

fn to_phase(phase: EvidencePhase) -> EvidencePhaseJson {
    match phase {
        EvidencePhase::InitialLoad => EvidencePhaseJson::InitialLoad,
        EvidencePhase::Scroll => EvidencePhaseJson::Scroll,
    }
}

fn write_json(out: &mut dyn Write, report: &VerificationReport) -> Result<(), String> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize verification report: {error}"))?;
    writeln!(out, "{json}").map_err(write_err)
}

fn write_human(out: &mut dyn Write, report: &VerificationReport) -> Result<(), String> {
    for page in &report.pages {
        writeln!(out, "url: {}", page.url).map_err(write_err)?;
        if let Some(error) = &page.error {
            writeln!(out, "  error [{}]: {}", error.code, error.message).map_err(write_err)?;
            continue;
        }
        if let Some(path) = &page.path {
            writeln!(out, "  path: {path}").map_err(write_err)?;
        }
        for slot in &page.slots {
            writeln!(out, "  slot {}: {}", slot.id, status_label(slot.status))
                .map_err(write_err)?;
            for warning in &slot.warnings {
                writeln!(out, "    warning [{}]: {}", warning.code, warning.message)
                    .map_err(write_err)?;
            }
        }
        for warning in &page.warnings {
            writeln!(out, "  warning [{}]: {}", warning.code, warning.message)
                .map_err(write_err)?;
        }
    }
    writeln!(out, "ok: {}", report.ok).map_err(write_err)
}

fn status_label(status: SlotStatus) -> &'static str {
    match status {
        SlotStatus::Confirmed => "confirmed",
        SlotStatus::Partial => "partial",
        SlotStatus::Missing => "missing",
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "used as a map_err fn that receives io::Error by value"
)]
fn write_err(error: io::Error) -> String {
    format!("failed to write command output: {error}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ad_templates::compare::{DomEvidence, GptSlotEvidence};
    use crate::commands::audit::collector::CollectedPage;

    struct FakeCollector {
        pages: HashMap<String, Result<CollectedPage, String>>,
    }

    impl FakeCollector {
        fn page(requested: &str, final_url: &str, evidence: BrowserAdEvidence) -> Self {
            let mut pages = HashMap::new();
            pages.insert(
                requested.to_string(),
                Ok(CollectedPage {
                    final_url: url::Url::parse(final_url).expect("valid final url"),
                    title: String::new(),
                    script_count: 0,
                    resource_count: 0,
                    warnings: Vec::new(),
                    ad_evidence: Some(evidence),
                }),
            );
            Self { pages }
        }

        fn with_error(mut self, requested: &str, message: &str) -> Self {
            self.pages
                .insert(requested.to_string(), Err(message.to_string()));
            self
        }
    }

    impl AuditCollector for FakeCollector {
        fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String> {
            self.pages
                .get(request.url.as_str())
                .cloned()
                .unwrap_or_else(|| Err(format!("no fake page for {}", request.url)))
        }
    }

    fn news_config() -> CreativeOpportunitiesConfig {
        let toml = "gam_network_id = \"123\"\n\
             \n\
             [[slot]]\n\
             id = \"atf\"\n\
             gam_unit_path = \"/123/news/atf\"\n\
             div_id = \"ad-atf-\"\n\
             page_patterns = [\"/news/*\"]\n\
             formats = [{ width = 300, height = 250 }]\n";
        let mut config =
            toml::from_str::<CreativeOpportunitiesConfig>(toml).expect("should deserialize");
        config.compile_slots();
        config
    }

    fn confirmed_news_evidence() -> BrowserAdEvidence {
        BrowserAdEvidence {
            dom_ids: vec![DomEvidence {
                dom_id: "ad-atf-0".to_string(),
                phase: EvidencePhase::InitialLoad,
            }],
            gpt_slots: vec![GptSlotEvidence {
                gam_unit_path: "/123/news/atf".to_string(),
                div_id: "ad-atf-0".to_string(),
                sizes: vec![(300, 250)],
                phase: EvidencePhase::InitialLoad,
            }],
            aps_calls: Vec::new(),
            page_bids: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn report_for(
        collector: &dyn AuditCollector,
        auction_enabled: bool,
        strict: bool,
        urls: &[&str],
    ) -> VerificationReport {
        let config = news_config();
        let parsed: Vec<url::Url> = urls
            .iter()
            .map(|url| url::Url::parse(url).expect("valid url"))
            .collect();
        build_report(
            collector,
            Some(&config),
            auction_enabled,
            &parsed,
            strict,
            false,
            &[],
        )
    }

    #[test]
    fn verify_uses_final_url_for_matching_after_redirect() {
        let collector = FakeCollector::page(
            "https://www.example.com/",
            "https://www.example.com/news/story",
            confirmed_news_evidence(),
        );
        let report = report_for(&collector, true, false, &["https://www.example.com/"]);
        let json = serde_json::to_value(&report).expect("should serialize");

        assert_eq!(json["pages"][0]["path"], "/news/story");
        assert_eq!(json["pages"][0]["slots"][0]["status"], "confirmed");
        let warnings = json["pages"][0]["warnings"]
            .as_array()
            .expect("warnings array");
        assert!(
            warnings.iter().any(|w| w["code"] == "redirected"),
            "redirect should emit a `redirected` warning"
        );
    }

    #[test]
    fn confirmed_page_is_ok_in_default_mode() {
        let collector = FakeCollector::page(
            "https://www.example.com/news/story",
            "https://www.example.com/news/story",
            confirmed_news_evidence(),
        );
        let report = report_for(
            &collector,
            true,
            false,
            &["https://www.example.com/news/story"],
        );

        assert!(report.ok, "confirmed page should be ok");
        assert_eq!(report.pages[0].matched_slot_count, Some(1));
    }

    #[test]
    fn strict_missing_slot_fails() {
        let collector = FakeCollector::page(
            "https://www.example.com/news/story",
            "https://www.example.com/news/story",
            empty_evidence(),
        );
        let report = report_for(
            &collector,
            true,
            true,
            &["https://www.example.com/news/story"],
        );

        assert!(
            !report.ok,
            "strict mode with a missing slot should not be ok"
        );
    }

    #[test]
    fn auction_disabled_skips_strict_missing_failure() {
        let collector = FakeCollector::page(
            "https://www.example.com/news/story",
            "https://www.example.com/news/story",
            empty_evidence(),
        );
        // auction disabled -> runtime expected No -> strict does not fail on missing.
        let report = report_for(
            &collector,
            false,
            true,
            &["https://www.example.com/news/story"],
        );

        assert!(
            report.ok,
            "missing slot must not fail strict when auction is disabled"
        );
        assert_eq!(
            report.pages[0].runtime_ad_stack_expected,
            Some(RuntimeAdStackExpectedJson::No)
        );
    }

    #[test]
    fn multi_url_page_error_sets_ok_false() {
        let collector = FakeCollector::page(
            "https://www.example.com/news/story",
            "https://www.example.com/news/story",
            confirmed_news_evidence(),
        )
        .with_error("https://www.example.com/broken", "navigation failed");
        let report = report_for(
            &collector,
            true,
            false,
            &[
                "https://www.example.com/news/story",
                "https://www.example.com/broken",
            ],
        );

        assert!(!report.ok, "a page-level error sets ok=false");
        let json = serde_json::to_value(&report).expect("should serialize");
        assert_eq!(json["pages"][1]["error"]["code"], "navigation_failed");
        assert!(json["pages"][1]["final_url"].is_null());
    }
}
