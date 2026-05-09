//! Configuration types and URL matching for creative opportunity slots.
//!
//! A [`CreativeOpportunitySlot`] describes a single ad placement: which pages
//! it appears on (via glob patterns), what ad formats it supports, and how it
//! maps to provider-specific identifiers such as GAM unit paths and APS slot IDs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use glob::Pattern;

use crate::auction::types::{AdFormat, AdSlot, MediaType};
use crate::price_bucket::PriceGranularity;

/// Top-level configuration for the creative opportunities system.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreativeOpportunitiesConfig {
    /// GAM network ID used to build default unit paths.
    pub gam_network_id: String,
    /// Maximum time in milliseconds to wait for the server-side auction before
    /// closing the response body.
    ///
    /// The auction runs concurrently with HTML body streaming. Body content
    /// above `</body>` has already been delivered and painted before the hold
    /// begins, so **FCP is not affected**. What this timeout bounds is the slip
    /// on `DOMContentLoaded` and `window.load`: third-party scripts that hook
    /// those events fire later by at most this duration.
    ///
    /// The worst case is a cache-hit page where the origin drains in <50 ms
    /// but the auction takes the full timeout — the browser sits idle waiting
    /// for `</body>`. 500 ms is the recommended default and the hard upper
    /// bound on DCL slip the publisher is willing to accept.
    ///
    /// When absent, falls back to `[auction].timeout_ms` from global config.
    #[serde(default)]
    pub auction_timeout_ms: Option<u32>,
    /// Price granularity for header-bidding price bucketing.
    #[serde(default = "PriceGranularity::dense")]
    pub price_granularity: PriceGranularity,
}

/// A single ad placement opportunity on the publisher's site.
#[derive(Debug, Clone, Deserialize)]
pub struct CreativeOpportunitySlot {
    /// Unique identifier for the slot (e.g., `"atf"`, `"below-fold-sidebar"`).
    pub id: String,
    /// Override for the GAM ad unit path.
    ///
    /// When absent, the path is derived as `/<gam_network_id>/<id>`.
    pub gam_unit_path: Option<String>,
    /// Override for the HTML `div` element ID that will hold the creative.
    ///
    /// Defaults to [`id`](Self::id) when absent.
    pub div_id: Option<String>,
    /// Glob patterns for page paths this slot should appear on.
    pub page_patterns: Vec<String>,
    /// Supported ad formats (size + media type combinations).
    pub formats: Vec<CreativeOpportunityFormat>,
    /// Optional floor price in CPM (USD).
    pub floor_price: Option<f64>,
    /// Slot-level targeting key–value pairs forwarded to the auction.
    #[serde(default)]
    pub targeting: HashMap<String, String>,
    /// Provider-specific slot identifiers.
    #[serde(default)]
    pub providers: SlotProviders,
}

impl CreativeOpportunitySlot {
    /// Returns `true` if `path` matches any of this slot's [`page_patterns`](Self::page_patterns).
    ///
    /// Patterns use glob syntax (e.g., `"/20**"` matches any path starting with `/20`,
    /// `"/"` matches only the root). When a pattern contains `**` in a position that the
    /// glob crate considers invalid (e.g., `b**`), the `**` is normalised to `*` before
    /// matching. A single `*` matches any sequence of characters including path separators
    /// because `require_literal_separator` is `false`.
    ///
    /// Patterns that cannot be compiled even after normalisation are silently skipped.
    #[must_use]
    pub fn matches_path(&self, path: &str) -> bool {
        self.page_patterns
            .iter()
            .any(|pattern| match Pattern::new(pattern) {
                Ok(p) => p.matches(path),
                Err(_) => {
                    let normalised = pattern.replace("**", "*");
                    Pattern::new(&normalised)
                        .map(|p| p.matches(path))
                        .unwrap_or(false)
                }
            })
    }

    /// Returns the GAM ad unit path for this slot.
    ///
    /// Uses the explicit [`gam_unit_path`](Self::gam_unit_path) override when set,
    /// otherwise constructs `/<gam_network_id>/<id>`.
    #[must_use]
    pub fn resolved_gam_unit_path(&self, gam_network_id: &str) -> String {
        self.gam_unit_path
            .clone()
            .unwrap_or_else(|| format!("/{}/{}", gam_network_id, self.id))
    }

    /// Returns the div element ID for this slot.
    ///
    /// Returns the [`div_id`](Self::div_id) override when set, otherwise returns [`id`](Self::id).
    #[must_use]
    pub fn resolved_div_id(&self) -> &str {
        self.div_id.as_deref().unwrap_or(&self.id)
    }

    /// Converts this slot into an [`AdSlot`] ready for use in an auction request.
    ///
    /// Provider-specific params (e.g., APS `slotID`, PBS bidder params) are wired
    /// into the `bidders` map keyed by provider/bidder name.
    #[must_use]
    pub fn to_ad_slot(&self, gam_network_id: &str) -> AdSlot {
        let _ = gam_network_id;
        let mut bidders: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(ref aps) = self.providers.aps {
            bidders.insert(
                "aps".to_string(),
                serde_json::json!({ "slotID": aps.slot_id }),
            );
        }
        if let Some(ref pbs) = self.providers.pbs {
            for (bidder_name, params) in &pbs.bidders {
                bidders.insert(bidder_name.clone(), params.clone());
            }
        }
        AdSlot {
            id: self.id.clone(),
            formats: self
                .formats
                .iter()
                .map(CreativeOpportunityFormat::to_ad_format)
                .collect(),
            floor_price: self.floor_price,
            targeting: self
                .targeting
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
            bidders,
        }
    }
}

/// An ad format combining a media type with pixel dimensions.
#[derive(Debug, Clone, Deserialize)]
pub struct CreativeOpportunityFormat {
    /// Creative width in pixels.
    pub width: u32,
    /// Creative height in pixels.
    pub height: u32,
    /// Media type for this format.
    #[serde(default = "MediaType::banner")]
    pub media_type: MediaType,
}

impl CreativeOpportunityFormat {
    fn to_ad_format(&self) -> AdFormat {
        AdFormat {
            media_type: self.media_type.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

/// Provider-specific slot identifiers for a [`CreativeOpportunitySlot`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SlotProviders {
    /// Amazon Publisher Services (APS/TAM) slot parameters.
    pub aps: Option<ApsSlotParams>,
    /// Prebid Server (PBS) slot parameters.
    pub pbs: Option<PbsSlotParams>,
}

/// APS-specific parameters for a slot.
#[derive(Debug, Clone, Deserialize)]
pub struct ApsSlotParams {
    /// The APS slot ID string used when making TAM bid requests.
    pub slot_id: String,
}

/// PBS-specific parameters for a slot.
///
/// Bidder params are sent inline to Prebid Server so bidder credentials
/// stay in `creative-opportunities.toml` rather than in PBS stored requests.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PbsSlotParams {
    /// Per-bidder params keyed by bidder name (must match PBS adapter name).
    ///
    /// Example in TOML:
    /// ```toml
    /// [slot.providers.pbs.bidders]
    /// mocktioneer = { bid = 2.00 }
    /// criteo = { networkId = 123456, pubid = "123456" }
    /// ```
    #[serde(default)]
    pub bidders: HashMap<String, serde_json::Value>,
}

/// TOML file structure for creative opportunity slot definitions.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CreativeOpportunitiesFile {
    /// All slot definitions in the file (mapped from `[[slot]]` TOML arrays).
    #[serde(rename = "slot", default)]
    pub slots: Vec<CreativeOpportunitySlot>,
}

/// Validates that a slot ID contains only safe characters.
///
/// Allowed characters: ASCII alphanumerics, underscores (`_`), and hyphens (`-`).
///
/// # Errors
///
/// Returns an error string when the ID is empty or contains disallowed characters.
pub fn validate_slot_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("slot id must not be empty".to_string());
    }
    if id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err(format!(
            "slot id '{id}' contains invalid characters; only [A-Za-z0-9_-] allowed"
        ))
    }
}

/// Returns all slots whose [`page_patterns`](CreativeOpportunitySlot::page_patterns) match `path`.
#[must_use]
pub fn match_slots<'a>(
    slots: &'a [CreativeOpportunitySlot],
    path: &str,
) -> Vec<&'a CreativeOpportunitySlot> {
    slots.iter().filter(|s| s.matches_path(path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_slot(id: &str, patterns: Vec<&str>) -> CreativeOpportunitySlot {
        CreativeOpportunitySlot {
            id: id.to_string(),
            gam_unit_path: None,
            div_id: None,
            page_patterns: patterns.into_iter().map(String::from).collect(),
            formats: vec![CreativeOpportunityFormat {
                width: 300,
                height: 250,
                media_type: crate::auction::types::MediaType::Banner,
            }],
            floor_price: Some(0.50),
            targeting: Default::default(),
            providers: Default::default(),
        }
    }

    #[test]
    fn glob_matches_article_path() {
        let slot = make_slot("atf", vec!["/20**"]);
        assert!(
            slot.matches_path("/2024/01/my-article/"),
            "should match article path"
        );
        assert!(!slot.matches_path("/"), "should not match root");
    }

    #[test]
    fn exact_match_homepage() {
        let slot = make_slot("home", vec!["/"]);
        assert!(slot.matches_path("/"), "should match root");
        assert!(!slot.matches_path("/about"), "should not match /about");
    }

    #[test]
    fn slot_id_validates_alphanumeric() {
        assert!(validate_slot_id("atf_sidebar_ad").is_ok());
        assert!(validate_slot_id("below-content-0").is_ok());
        assert!(validate_slot_id("").is_err(), "empty id should fail");
        assert!(
            validate_slot_id("xss<script>").is_err(),
            "html in id should fail"
        );
        assert!(validate_slot_id("has space").is_err(), "spaces should fail");
    }

    #[test]
    fn resolved_gam_unit_path_uses_default_when_absent() {
        let slot = make_slot("atf", vec!["/"]);
        assert_eq!(
            slot.resolved_gam_unit_path("21765378893"),
            "/21765378893/atf"
        );
    }

    #[test]
    fn resolved_gam_unit_path_uses_override_when_set() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some("/21765378893/publisher/atf-sidebar".to_string());
        assert_eq!(
            slot.resolved_gam_unit_path("21765378893"),
            "/21765378893/publisher/atf-sidebar"
        );
    }

    #[test]
    fn resolved_div_id_defaults_to_slot_id() {
        let slot = make_slot("atf", vec!["/"]);
        assert_eq!(slot.resolved_div_id(), "atf");
    }

    #[test]
    fn to_ad_slot_wires_aps_params_into_bidders() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.providers.aps = Some(ApsSlotParams {
            slot_id: "aps-slot-atf".to_string(),
        });
        let ad_slot = slot.to_ad_slot("21765378893");
        let aps_params = ad_slot.bidders.get("aps").expect("should have aps bidder");
        assert_eq!(
            aps_params.get("slotID").and_then(|v| v.as_str()),
            Some("aps-slot-atf"),
        );
    }

    #[test]
    fn to_ad_slot_wires_pbs_bidder_params_into_bidders() {
        let mut slot = make_slot("atf_sidebar_ad", vec!["/"]);
        slot.providers.pbs = Some(PbsSlotParams {
            bidders: [
                (
                    "mocktioneer".to_string(),
                    serde_json::json!({ "bid": 2.00 }),
                ),
                (
                    "criteo".to_string(),
                    serde_json::json!({ "networkId": 123456, "pubid": "123456" }),
                ),
            ]
            .into_iter()
            .collect(),
        });
        let ad_slot = slot.to_ad_slot("88059007");
        let mock_params = ad_slot
            .bidders
            .get("mocktioneer")
            .expect("should have mocktioneer bidder");
        assert_eq!(
            mock_params.get("bid").and_then(serde_json::Value::as_f64),
            Some(2.0),
            "should wire mocktioneer bid param"
        );
        let criteo_params = ad_slot
            .bidders
            .get("criteo")
            .expect("should have criteo bidder");
        assert_eq!(
            criteo_params
                .get("networkId")
                .and_then(serde_json::Value::as_i64),
            Some(123456),
            "should wire criteo networkId param"
        );
    }

    #[test]
    fn to_ad_slot_sets_floor_price_and_formats() {
        let slot = make_slot("atf", vec!["/"]);
        let ad_slot = slot.to_ad_slot("21765378893");
        assert_eq!(ad_slot.id, "atf");
        assert_eq!(ad_slot.floor_price, Some(0.50));
        assert_eq!(ad_slot.formats.len(), 1);
    }
}
