//! Stable, serializable output model for ad-template diagnostics.
//!
//! These types mirror the `--json` contract in
//! `docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md` §8.
//! Field names and declaration order are load-bearing: `serde` serializes struct
//! fields in declaration order, so the order here must match the spec examples.
//!
//! The model is consumed by the `ts audit ad-templates verify` orchestrator
//! (Task 9), which assembles these wire types from the URL/gate context and the
//! pure comparison result. Until that consumer lands, the types are exercised only
//! by tests, hence the module-scoped `dead_code` allow.
#![allow(
    dead_code,
    reason = "wire model assembled by the audit verifier in a later task"
)]

use serde::{Deserialize, Serialize};

use trusted_server_core::creative_opportunities::RuntimeAdStackExpected;

/// Confirmation status for a single configured slot.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    /// GPT evidence matches GAM path, div, and a compatible size.
    Confirmed,
    /// Some evidence, but not enough to confirm.
    Partial,
    /// No DOM or GPT evidence confirms the slot.
    Missing,
}

/// JSON rendering of the runtime ad-stack expectation.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAdStackExpectedJson {
    /// The server-side ad stack is expected to run.
    Yes,
    /// A known gate blocks the server-side ad stack.
    No,
    /// Consent or another gate is unprovable.
    Unknown,
}

impl From<RuntimeAdStackExpected> for RuntimeAdStackExpectedJson {
    fn from(value: RuntimeAdStackExpected) -> Self {
        match value {
            RuntimeAdStackExpected::Yes => Self::Yes,
            RuntimeAdStackExpected::No => Self::No,
            RuntimeAdStackExpected::Unknown => Self::Unknown,
        }
    }
}

/// State of a single runtime gate.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    /// The gate passed.
    Pass,
    /// The gate blocked the ad stack.
    Fail,
    /// The gate state could not be proven.
    Unknown,
}

/// Evidence-collection phase, rendered for JSON output.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePhaseJson {
    /// Observed during the initial page load and settle.
    InitialLoad,
    /// Observed only after the deterministic scroll pass.
    Scroll,
}

/// A structured warning with a stable machine code and human message.
///
/// `Serialize` for output; `Deserialize` because the browser collector payload
/// carries warning objects decoded into the comparison input.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable machine-readable code (e.g. `dom_without_gpt`).
    pub code: String,
    /// Human-readable message; JSON consumers must not parse this.
    pub message: String,
}

/// Top-level `--json` document for `ts audit ad-templates verify`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerificationReport {
    /// True when no strict failure and no page-level error occurred.
    pub ok: bool,
    /// Whether `--strict` was set.
    pub strict: bool,
    /// One entry per requested URL, in input order.
    pub pages: Vec<PageJson>,
    /// Run-level warnings not attributable to a single page.
    pub warnings: Vec<Warning>,
}

/// A single audited page result.
///
/// `error` is declared immediately after `path` so the serialized key order
/// matches the spec §8 `navigation_failed` shape; on normal pages it is `None`
/// and skipped, leaving the runtime/gates fields in §8 order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageJson {
    /// The requested URL.
    pub url: String,
    /// The final URL after redirects, or `null` on navigation failure.
    pub final_url: Option<String>,
    /// The requested URL's path.
    pub requested_path: String,
    /// The final path used for matching, or `null` on navigation failure.
    pub path: Option<String>,
    /// Present only on a page-level collection failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Warning>,
    /// Three-state runtime ad-stack expectation; absent on error pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_ad_stack_expected: Option<RuntimeAdStackExpectedJson>,
    /// Per-gate evidence; absent on error pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gates: Option<Gates>,
    /// Number of configured slots matched for the final path; absent on error pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_slot_count: Option<usize>,
    /// Per-slot verification results.
    pub slots: Vec<SlotJson>,
    /// Live ad-slot evidence with no matching configured slot.
    pub extra_evidence: Vec<ExtraEvidenceJson>,
    /// Page-level warnings.
    pub warnings: Vec<Warning>,
}

/// Runtime gate states for a page, one field per spec §5.2 gate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gates {
    /// Request method is `GET`.
    pub method_get: GateState,
    /// Request is a top-level navigation.
    pub navigation: GateState,
    /// Request is not a prefetch.
    pub not_prefetch: GateState,
    /// Request is not from a known bot.
    pub not_bot: GateState,
    /// At least one configured slot matched the final path.
    pub matched_slots: GateState,
    /// The `[auction].enabled` kill switch is on.
    pub auction_enabled: GateState,
    /// Consent allows the auction (often `unknown` for live requests).
    pub consent_allows_auction: GateState,
}

/// A single configured slot's verification result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotJson {
    /// The configured slot id.
    pub id: String,
    /// The slot's confirmation status.
    pub status: SlotStatus,
    /// The phase the confirming evidence was observed in.
    pub phase: EvidencePhaseJson,
    /// The configured shape of the slot (no `id`/`page_patterns` per §8).
    pub configured: ConfiguredJson,
    /// The live evidence observed for this slot.
    pub evidence: SlotEvidenceJson,
    /// Slot-level warnings (e.g. provider or size warnings).
    pub warnings: Vec<Warning>,
}

/// The configured shape of a slot, as rendered in §8 `configured`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfiguredJson {
    /// Resolved div element ID.
    pub div_id: String,
    /// Resolved GAM unit path.
    pub gam_unit_path: String,
    /// Configured formats.
    pub formats: Vec<FormatJson>,
    /// Configured provider names.
    pub providers: Vec<String>,
}

/// A configured format, as rendered in §8.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FormatJson {
    /// Creative width in pixels.
    pub width: u32,
    /// Creative height in pixels.
    pub height: u32,
    /// Media type string (`banner`, `video`, `native`).
    pub media_type: String,
}

/// Live evidence observed for a configured slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotEvidenceJson {
    /// The resolved DOM element ID observed, if any.
    pub dom_id: Option<String>,
    /// GPT slot evidence, if any (no `phase` key per §8).
    pub gpt: Option<GptEvidenceJson>,
}

/// GPT slot evidence, as rendered in §8 `evidence.gpt`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GptEvidenceJson {
    /// The observed GAM ad unit path.
    pub gam_unit_path: String,
    /// The observed GPT slot element ID.
    pub div_id: String,
    /// Observed numeric sizes as `[width, height]` pairs.
    pub sizes: Vec<[u32; 2]>,
}

/// Live ad-slot evidence with no matching configured slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtraEvidenceJson {
    /// Evidence kind: `dom`, `gpt`, or `aps`.
    pub kind: String,
    /// The phase the evidence was observed in.
    pub phase: EvidencePhaseJson,
    /// The DOM element ID, if any.
    pub dom_id: Option<String>,
    /// The GAM unit path, if any.
    pub gam_unit_path: Option<String>,
    /// Observed numeric sizes as `[width, height]` pairs.
    pub sizes: Vec<[u32; 2]>,
    /// Why this evidence is reported as extra.
    pub reason: String,
}

#[cfg(test)]
impl VerificationReport {
    fn example_confirmed_with_extra_evidence() -> Self {
        VerificationReport {
            ok: true,
            strict: false,
            pages: vec![PageJson {
                url: "https://www.example.com/news/story".to_string(),
                final_url: Some("https://www.example.com/news/story".to_string()),
                requested_path: "/news/story".to_string(),
                path: Some("/news/story".to_string()),
                error: None,
                runtime_ad_stack_expected: Some(RuntimeAdStackExpectedJson::Unknown),
                gates: Some(Gates {
                    method_get: GateState::Pass,
                    navigation: GateState::Pass,
                    not_prefetch: GateState::Pass,
                    not_bot: GateState::Pass,
                    matched_slots: GateState::Pass,
                    auction_enabled: GateState::Pass,
                    consent_allows_auction: GateState::Unknown,
                }),
                matched_slot_count: Some(1),
                slots: vec![SlotJson {
                    id: "atf".to_string(),
                    status: SlotStatus::Confirmed,
                    phase: EvidencePhaseJson::InitialLoad,
                    configured: ConfiguredJson {
                        div_id: "ad-atf-".to_string(),
                        gam_unit_path: "/123/news/atf".to_string(),
                        formats: vec![FormatJson {
                            width: 300,
                            height: 250,
                            media_type: "banner".to_string(),
                        }],
                        providers: vec!["aps".to_string()],
                    },
                    evidence: SlotEvidenceJson {
                        dom_id: Some("ad-atf-0".to_string()),
                        gpt: Some(GptEvidenceJson {
                            gam_unit_path: "/123/news/atf".to_string(),
                            div_id: "ad-atf-0".to_string(),
                            sizes: vec![[300, 250]],
                        }),
                    },
                    warnings: Vec::new(),
                }],
                extra_evidence: vec![ExtraEvidenceJson {
                    kind: "gpt".to_string(),
                    phase: EvidencePhaseJson::InitialLoad,
                    dom_id: Some("ad-right-rail-0".to_string()),
                    gam_unit_path: Some("/123/publisher/right-rail".to_string()),
                    sizes: vec![[300, 250]],
                    reason: "no_configured_slot_matched".to_string(),
                }],
                warnings: vec![Warning {
                    code: "redirected".to_string(),
                    message: "navigation redirected to the final path".to_string(),
                }],
            }],
            warnings: Vec::new(),
        }
    }

    fn example_navigation_failed() -> Self {
        VerificationReport {
            ok: false,
            strict: false,
            pages: vec![PageJson {
                url: "https://www.example.com/broken".to_string(),
                final_url: None,
                requested_path: "/broken".to_string(),
                path: None,
                error: Some(Warning {
                    code: "navigation_failed".to_string(),
                    message: "failed to read main document navigation response".to_string(),
                }),
                runtime_ad_stack_expected: None,
                gates: None,
                matched_slot_count: None,
                slots: Vec::new(),
                extra_evidence: Vec::new(),
                warnings: Vec::new(),
            }],
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_json_contains_gate_state_and_extra_evidence() {
        let result = VerificationReport::example_confirmed_with_extra_evidence();
        let value = serde_json::to_value(&result).expect("should serialize");

        assert_eq!(value["ok"], true);
        assert_eq!(value["pages"][0]["requested_path"], "/news/story");
        assert_eq!(value["pages"][0]["runtime_ad_stack_expected"], "unknown");
        assert_eq!(
            value["pages"][0]["gates"]["consent_allows_auction"],
            "unknown"
        );
        assert_eq!(value["pages"][0]["slots"][0]["status"], "confirmed");
        assert_eq!(
            value["pages"][0]["slots"][0]["evidence"]["gpt"]["sizes"][0][0],
            300
        );
        assert_eq!(value["pages"][0]["extra_evidence"][0]["kind"], "gpt");
        assert_eq!(value["pages"][0]["warnings"][0]["code"], "redirected");
        // `configured` excludes id/page_patterns per §8.
        assert!(value["pages"][0]["slots"][0]["configured"]["id"].is_null());
        assert!(value["pages"][0]["slots"][0]["configured"]["page_patterns"].is_null());
        // `evidence.gpt` has no `phase` key per §8.
        assert!(value["pages"][0]["slots"][0]["evidence"]["gpt"]["phase"].is_null());
    }

    #[test]
    fn page_error_json_matches_navigation_failed_shape() {
        let result = VerificationReport::example_navigation_failed();
        let value = serde_json::to_value(&result).expect("should serialize");
        let page = &value["pages"][0];

        assert_eq!(page["error"]["code"], "navigation_failed");
        assert!(page["final_url"].is_null(), "final_url should be null");
        assert!(page["path"].is_null(), "path should be null");
        assert!(
            page.get("runtime_ad_stack_expected").is_none(),
            "runtime field absent on error page"
        );
        assert!(page.get("gates").is_none(), "gates absent on error page");
        assert!(
            page.get("matched_slot_count").is_none(),
            "matched_slot_count absent on error page"
        );
        assert_eq!(value["ok"], false);
    }
}
