//! Pure comparison of configured expected slots against browser ad evidence.
//!
//! This module is collector-independent and Chrome-free: it takes decoded
//! [`BrowserAdEvidence`] plus the [`ExpectedSlot`] set and produces a
//! [`PageVerificationResult`] with per-slot statuses, warnings, and unmatched
//! extra evidence, mirroring spec §5.3–§5.6.
//!
//! Consumed by the browser collector decode (Task 8) and the audit verifier
//! (Task 9); exercised by tests until then, hence the module-scoped allow.
#![allow(
    dead_code,
    reason = "consumed by the browser collector and audit verifier in later tasks"
)]

use serde::Deserialize;

use trusted_server_core::creative_opportunities::RuntimeAdStackExpected;

use crate::ad_templates::expected::ExpectedSlot;
use crate::ad_templates::output::Warning;

/// The phase in which a piece of evidence was observed.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePhase {
    /// Observed during the initial load and settle.
    InitialLoad,
    /// Observed only after the deterministic scroll pass.
    Scroll,
}

/// A DOM element ID observed on the page.
#[derive(Debug, Clone, Deserialize)]
pub struct DomEvidence {
    /// The element ID.
    pub dom_id: String,
    /// The phase it was first observed in.
    pub phase: EvidencePhase,
}

/// A GPT slot observed on the page.
#[derive(Debug, Clone, Deserialize)]
pub struct GptSlotEvidence {
    /// The observed GAM ad unit path.
    pub gam_unit_path: String,
    /// The observed GPT slot element ID.
    pub div_id: String,
    /// Observed numeric sizes as `(width, height)` pairs (non-numeric dropped upstream).
    pub sizes: Vec<(u32, u32)>,
    /// The phase it was first observed in.
    pub phase: EvidencePhase,
}

/// An `apstag.fetchBids` call observed on the page (spec §5.5).
#[derive(Debug, Clone, Deserialize)]
pub struct ApsFetchBidsEvidence {
    /// The APS slot ID requested.
    pub slot_id: String,
    /// Sizes requested for the slot.
    pub sizes: Vec<(u32, u32)>,
    /// The phase it was observed in.
    pub phase: EvidencePhase,
}

/// A `/__ts/page-bids` observation for SPA routes (spec §5.2).
///
/// DEFERRED in Phase 1: kept as forward scaffolding so the decoded evidence shape
/// stays forward-compatible. Not populated by the collector or surfaced in JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct PageBidsEvidence {
    /// The slot ID present in the page-bids response.
    pub slot_id: String,
    /// The phase it was observed in.
    pub phase: EvidencePhase,
}

/// All read-only ad evidence decoded from a single browser page.
#[derive(Debug, Clone, Deserialize)]
pub struct BrowserAdEvidence {
    /// DOM element IDs matching configured prefixes.
    pub dom_ids: Vec<DomEvidence>,
    /// GPT slots observed via `defineSlot` and `getSlots()`.
    pub gpt_slots: Vec<GptSlotEvidence>,
    /// `apstag.fetchBids` calls observed.
    pub aps_calls: Vec<ApsFetchBidsEvidence>,
    /// `/__ts/page-bids` observations (deferred; default empty).
    #[serde(default)]
    pub page_bids: Vec<PageBidsEvidence>,
    /// Collector-level warnings (no page HTML/cookies/storage).
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// Summary of the runtime ad-stack gate for a page.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeGateSummary {
    /// The three-state ad-stack expectation.
    pub expected: RuntimeAdStackExpected,
}

impl RuntimeGateSummary {
    /// Builds a summary from a computed runtime expectation.
    #[must_use]
    pub fn from_expected(expected: RuntimeAdStackExpected) -> Self {
        Self { expected }
    }

    #[cfg(test)]
    fn unknown_allowed() -> Self {
        Self::from_expected(RuntimeAdStackExpected::Unknown)
    }

    #[cfg(test)]
    fn auction_disabled() -> Self {
        Self::from_expected(RuntimeAdStackExpected::No)
    }
}

/// Confirmation status for a single configured slot (compare-side mirror of the
/// output `SlotStatus`).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SlotStatus {
    /// GPT evidence matches GAM path, div, and a compatible size.
    Confirmed,
    /// Some evidence, but not enough to confirm.
    Partial,
    /// No DOM or GPT evidence confirms the slot.
    Missing,
}

/// The verification result for one audited page.
#[derive(Debug, Clone)]
pub struct PageVerificationResult {
    /// Whether the runtime ad stack was expected to run for this page.
    pub runtime_ad_stack_expected: RuntimeAdStackExpected,
    /// Per-slot results, in expected-slot order.
    pub slots: Vec<SlotResult>,
    /// Live evidence that matched no configured slot.
    pub extra_evidence: Vec<ExtraEvidence>,
}

impl PageVerificationResult {
    /// Whether `--strict` should fail for this page.
    ///
    /// False when the runtime ad stack is not expected to run (a known gate
    /// suppressed it); otherwise true if any slot is missing or partial. Provider
    /// warnings and extra evidence alone never fail strict.
    #[must_use]
    pub fn strict_failed(&self) -> bool {
        if self.runtime_ad_stack_expected == RuntimeAdStackExpected::No {
            return false;
        }
        self.slots
            .iter()
            .any(|slot| matches!(slot.status, SlotStatus::Missing | SlotStatus::Partial))
    }
}

/// Per-slot verification result.
#[derive(Debug, Clone)]
pub struct SlotResult {
    /// The configured slot id.
    pub id: String,
    /// The confirmation status.
    pub status: SlotStatus,
    /// The phase the confirming evidence was observed in.
    pub phase: EvidencePhase,
    /// The live evidence observed for this slot.
    pub evidence: SlotEvidence,
    /// Slot-level warnings (size, provider, etc.).
    pub warnings: Vec<Warning>,
}

/// Live evidence observed for a configured slot.
#[derive(Debug, Clone)]
pub struct SlotEvidence {
    /// The resolved DOM element ID, if any.
    pub dom_id: Option<String>,
    /// The matched GPT slot, if any.
    pub gpt: Option<GptSlotEvidence>,
}

/// Live ad-slot evidence with no matching configured slot.
#[derive(Debug, Clone)]
pub struct ExtraEvidence {
    /// Evidence kind: `dom`, `gpt`, or `aps`.
    pub kind: String,
    /// The phase it was observed in.
    pub phase: EvidencePhase,
    /// The DOM element ID, if any.
    pub dom_id: Option<String>,
    /// The GAM unit path, if any.
    pub gam_unit_path: Option<String>,
    /// Observed numeric sizes.
    pub sizes: Vec<(u32, u32)>,
    /// Why this evidence is reported as extra.
    pub reason: String,
}

fn warning(code: &str, message: String) -> Warning {
    Warning {
        code: code.to_string(),
        message,
    }
}

/// Resolves the slot root DOM element per spec §5.3.
///
/// Exact `div_id` match first, then the first element whose ID starts with
/// `div_id`, ignoring `-container` wrappers.
fn resolve_dom<'a>(dom_ids: &'a [DomEvidence], div_id: &str) -> Option<&'a DomEvidence> {
    if let Some(exact) = dom_ids.iter().find(|dom| dom.dom_id == div_id) {
        return Some(exact);
    }
    dom_ids
        .iter()
        .find(|dom| dom.dom_id.starts_with(div_id) && !dom.dom_id.ends_with("-container"))
}

/// Returns true when a GPT slot's element ID matches the resolved DOM id (or its
/// `-container`), per spec §5.4.
fn gpt_div_matches(gpt_div: &str, expected: &ExpectedSlot, resolved_dom_id: Option<&str>) -> bool {
    match resolved_dom_id {
        Some(dom_id) => gpt_div == dom_id || gpt_div == format!("{dom_id}-container"),
        None => {
            gpt_div == expected.div_id
                || (gpt_div.starts_with(&expected.div_id) && !gpt_div.ends_with("-container"))
        }
    }
}

fn banner_sizes(expected: &ExpectedSlot) -> Vec<(u32, u32)> {
    expected
        .formats
        .iter()
        .filter(|format| format.media_type == "banner")
        .map(|format| (format.width, format.height))
        .collect()
}

/// Compares configured expected slots against decoded browser evidence.
#[must_use]
pub fn compare_page_evidence(
    expected: &[ExpectedSlot],
    evidence: &BrowserAdEvidence,
    gate: RuntimeGateSummary,
) -> PageVerificationResult {
    let mut consumed_gpt = vec![false; evidence.gpt_slots.len()];
    let mut slots = Vec::with_capacity(expected.len());

    for slot in expected {
        let resolved = resolve_dom(&evidence.dom_ids, &slot.div_id);
        let resolved_id = resolved.map(|dom| dom.dom_id.clone());
        let gpt_idx = evidence.gpt_slots.iter().position(|gpt| {
            gpt.gam_unit_path == slot.gam_unit_path
                && gpt_div_matches(&gpt.div_id, slot, resolved_id.as_deref())
        });

        let banner = banner_sizes(slot);
        let mut warnings = Vec::new();

        let (status, dom_for_evidence, gpt_for_evidence, phase) = if let Some(idx) = gpt_idx {
            consumed_gpt[idx] = true;
            let gpt = &evidence.gpt_slots[idx];
            let dom_id = resolved_id.clone().or_else(|| Some(gpt.div_id.clone()));
            if banner.is_empty() {
                warnings.push(warning(
                    "unsupported_format",
                    format!(
                        "slot `{}` has only non-banner formats; not confirmable in Phase 1",
                        slot.id
                    ),
                ));
                (SlotStatus::Partial, dom_id, Some(gpt.clone()), gpt.phase)
            } else if gpt.sizes.is_empty() {
                warnings.push(warning(
                    "out_of_page_slot",
                    format!(
                        "slot `{}` matched an out-of-page GPT slot with no sizes",
                        slot.id
                    ),
                ));
                (SlotStatus::Partial, dom_id, Some(gpt.clone()), gpt.phase)
            } else if banner.iter().any(|size| gpt.sizes.contains(size)) {
                let extra: Vec<(u32, u32)> = gpt
                    .sizes
                    .iter()
                    .copied()
                    .filter(|size| !banner.contains(size))
                    .collect();
                if !extra.is_empty() {
                    warnings.push(warning(
                        "extra_observed_size",
                        format!("slot `{}` observed extra GPT sizes {extra:?}", slot.id),
                    ));
                }
                let missing: Vec<(u32, u32)> = banner
                    .iter()
                    .copied()
                    .filter(|size| !gpt.sizes.contains(size))
                    .collect();
                if !missing.is_empty() {
                    warnings.push(warning(
                        "configured_size_not_observed",
                        format!(
                            "slot `{}` configured sizes {missing:?} were not observed",
                            slot.id
                        ),
                    ));
                }
                (SlotStatus::Confirmed, dom_id, Some(gpt.clone()), gpt.phase)
            } else {
                warnings.push(warning(
                    "incompatible_sizes",
                    format!(
                        "slot `{}` GPT path and div matched but no configured size overlapped",
                        slot.id
                    ),
                ));
                (SlotStatus::Partial, dom_id, Some(gpt.clone()), gpt.phase)
            }
        } else if let Some(dom) = resolved {
            warnings.push(warning(
                "dom_without_gpt",
                "DOM element matched, but no GPT slot evidence was observed".to_string(),
            ));
            (
                SlotStatus::Partial,
                Some(dom.dom_id.clone()),
                None,
                dom.phase,
            )
        } else {
            (SlotStatus::Missing, None, None, EvidencePhase::InitialLoad)
        };

        if let Some(aps_slot_id) = &slot.aps_slot_id {
            let matched = evidence
                .aps_calls
                .iter()
                .any(|call| &call.slot_id == aps_slot_id);
            if !matched {
                warnings.push(warning(
                    "aps_evidence_missing",
                    format!("configured APS slot `{aps_slot_id}` had no fetchBids evidence"),
                ));
            }
        }

        slots.push(SlotResult {
            id: slot.id.clone(),
            status,
            phase,
            evidence: SlotEvidence {
                dom_id: dom_for_evidence,
                gpt: gpt_for_evidence,
            },
            warnings,
        });
    }

    let extra_evidence = evidence
        .gpt_slots
        .iter()
        .enumerate()
        .filter(|(idx, _)| !consumed_gpt[*idx])
        .map(|(_, gpt)| ExtraEvidence {
            kind: "gpt".to_string(),
            phase: gpt.phase,
            dom_id: Some(gpt.div_id.clone()),
            gam_unit_path: Some(gpt.gam_unit_path.clone()),
            sizes: gpt.sizes.clone(),
            reason: "no_configured_slot_matched".to_string(),
        })
        .collect();

    PageVerificationResult {
        runtime_ad_stack_expected: gate.expected,
        slots,
        extra_evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad_templates::expected::ExpectedFormat;

    fn dom(id: &str) -> DomEvidence {
        DomEvidence {
            dom_id: id.to_string(),
            phase: EvidencePhase::InitialLoad,
        }
    }

    fn gpt_slot(gam_unit_path: &str, div_id: &str, sizes: &[(u32, u32)]) -> GptSlotEvidence {
        GptSlotEvidence {
            gam_unit_path: gam_unit_path.to_string(),
            div_id: div_id.to_string(),
            sizes: sizes.to_vec(),
            phase: EvidencePhase::InitialLoad,
        }
    }

    fn aps(slot_id: &str, sizes: &[(u32, u32)]) -> ApsFetchBidsEvidence {
        ApsFetchBidsEvidence {
            slot_id: slot_id.to_string(),
            sizes: sizes.to_vec(),
            phase: EvidencePhase::InitialLoad,
        }
    }

    fn evidence(
        doms: Vec<DomEvidence>,
        gpts: Vec<GptSlotEvidence>,
        aps: Vec<ApsFetchBidsEvidence>,
    ) -> BrowserAdEvidence {
        BrowserAdEvidence {
            dom_ids: doms,
            gpt_slots: gpts,
            aps_calls: aps,
            page_bids: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn expected_slot(
        id: &str,
        div_id: &str,
        gam_unit_path: &str,
        sizes: &[(u32, u32)],
        providers: &[&str],
    ) -> ExpectedSlot {
        ExpectedSlot {
            id: id.to_string(),
            div_id: div_id.to_string(),
            gam_unit_path: gam_unit_path.to_string(),
            formats: sizes
                .iter()
                .map(|&(width, height)| ExpectedFormat {
                    width,
                    height,
                    media_type: "banner".to_string(),
                })
                .collect(),
            providers: providers.iter().copied().map(String::from).collect(),
            aps_slot_id: providers.contains(&"aps").then(|| id.to_string()),
            page_patterns: Vec::new(),
        }
    }

    fn expected_slot_video(id: &str, div_id: &str, gam_unit_path: &str) -> ExpectedSlot {
        ExpectedSlot {
            id: id.to_string(),
            div_id: div_id.to_string(),
            gam_unit_path: gam_unit_path.to_string(),
            formats: vec![ExpectedFormat {
                width: 0,
                height: 0,
                media_type: "video".to_string(),
            }],
            providers: Vec::new(),
            aps_slot_id: None,
            page_patterns: Vec::new(),
        }
    }

    #[test]
    fn gpt_path_div_and_size_overlap_confirms_slot() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(
            vec![dom("ad-atf-0")],
            vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Confirmed);
        assert!(
            result.slots[0].warnings.is_empty(),
            "confirmed slot should carry no warnings"
        );
    }

    #[test]
    fn dom_only_is_partial() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(vec![dom("ad-atf-0")], Vec::new(), Vec::new());

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Partial);
        assert!(
            result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code == "dom_without_gpt")
        );
    }

    #[test]
    fn no_dom_or_gpt_is_missing() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(Vec::new(), Vec::new(), Vec::new());

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Missing);
    }

    #[test]
    fn prefix_dom_resolution_ignores_container_suffix() {
        let expected = expected_slot(
            "header",
            "ad-header-0-",
            "/123/homepage/header",
            &[(728, 90)],
            &[],
        );
        let evidence = evidence(
            vec![dom("ad-header-0--container"), dom("ad-header-0-_R_abc123")],
            Vec::new(),
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(
            result.slots[0].evidence.dom_id.as_deref(),
            Some("ad-header-0-_R_abc123"),
            "prefix match should skip -container"
        );
        assert_eq!(result.slots[0].status, SlotStatus::Partial);
    }

    #[test]
    fn unmatched_gpt_slot_becomes_extra_evidence() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(
            vec![dom("ad-atf-0")],
            vec![
                gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)]),
                gpt_slot(
                    "/123/publisher/right-rail",
                    "ad-right-rail-0",
                    &[(300, 250)],
                ),
            ],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Confirmed);
        assert_eq!(result.extra_evidence.len(), 1);
        assert_eq!(result.extra_evidence[0].kind, "gpt");
        assert!(
            !result.strict_failed(),
            "extra evidence alone must not fail strict"
        );
    }

    #[test]
    fn auction_disabled_skips_strict_missing_failure() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(Vec::new(), Vec::new(), Vec::new());

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::auction_disabled(),
        );

        assert_eq!(result.runtime_ad_stack_expected, RuntimeAdStackExpected::No);
        assert_eq!(result.slots[0].status, SlotStatus::Missing);
        assert!(
            !result.strict_failed(),
            "missing slot must not fail strict when ad stack is No"
        );
    }

    #[test]
    fn gpt_incompatible_sizes_is_partial() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(
            vec![dom("ad-atf-0")],
            vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(728, 90)])],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Partial);
        assert!(
            result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code == "incompatible_sizes")
        );
    }

    #[test]
    fn non_banner_only_slot_is_partial() {
        let expected = expected_slot_video("video", "ad-video-", "/123/news/video");
        let evidence = evidence(
            vec![dom("ad-video-0")],
            vec![gpt_slot("/123/news/video", "ad-video-0", &[(640, 480)])],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Partial);
        assert!(
            result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code == "unsupported_format")
        );
    }

    #[test]
    fn gpt_container_element_id_confirms() {
        let expected = expected_slot("atf", "ad-atf-0", "/123/news/atf", &[(300, 250)], &[]);
        let evidence = evidence(
            vec![dom("ad-atf-0"), dom("ad-atf-0-container")],
            vec![gpt_slot(
                "/123/news/atf",
                "ad-atf-0-container",
                &[(300, 250)],
            )],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(
            result.slots[0].status,
            SlotStatus::Confirmed,
            "container element id is a valid GPT div match"
        );
    }

    #[test]
    fn out_of_page_gpt_slot_warns_and_does_not_confirm() {
        let expected = expected_slot(
            "interstitial",
            "ad-oop-",
            "/123/news/oop",
            &[(300, 250)],
            &[],
        );
        let evidence = evidence(
            vec![dom("ad-oop-0")],
            vec![gpt_slot("/123/news/oop", "ad-oop-0", &[])],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_ne!(result.slots[0].status, SlotStatus::Confirmed);
        assert!(
            result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code == "out_of_page_slot")
        );
    }

    #[test]
    fn aps_match_adds_no_warning() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &["aps"]);
        let evidence = evidence(
            vec![dom("ad-atf-0")],
            vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
            vec![aps("atf", &[(300, 250)])],
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(result.slots[0].status, SlotStatus::Confirmed);
        assert!(
            !result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code.starts_with("aps_")),
            "matching APS should not warn"
        );
    }

    #[test]
    fn aps_missing_warns_but_keeps_confirmed() {
        let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &["aps"]);
        let evidence = evidence(
            vec![dom("ad-atf-0")],
            vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
            Vec::new(),
        );

        let result = compare_page_evidence(
            &[expected],
            &evidence,
            RuntimeGateSummary::unknown_allowed(),
        );

        assert_eq!(
            result.slots[0].status,
            SlotStatus::Confirmed,
            "missing APS does not flip status"
        );
        assert!(
            result.slots[0]
                .warnings
                .iter()
                .any(|w| w.code == "aps_evidence_missing")
        );
        assert!(
            !result.strict_failed(),
            "provider warning alone must not fail strict"
        );
    }
}
