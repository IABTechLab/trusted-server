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
use crate::settings::vec_from_seq_or_map;

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
    /// Slot templates. Empty vec = feature disabled (no auction fired, no globals injected).
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub slot: Vec<CreativeOpportunitySlot>,
}

impl CreativeOpportunitiesConfig {
    /// Pre-compile glob patterns for all slots. Call once after deserialization.
    pub fn compile_slots(&mut self) {
        for slot in &mut self.slot {
            slot.compile_patterns();
        }
    }
}

/// A single ad placement opportunity on the publisher's site.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    /// Pre-compiled [`page_patterns`](Self::page_patterns) for hot-path matching.
    ///
    /// Populated by [`compile_patterns`](Self::compile_patterns) once at startup
    /// via [`CreativeOpportunitiesConfig::compile_slots`]. When this is
    /// empty, [`matches_path`](Self::matches_path) falls back to compiling on
    /// every call so callers that build slots by hand in tests
    /// still work.
    ///
    /// `pub(crate)` rather than private so cross-module test helpers in this
    /// crate can construct slots via struct-literal syntax with an empty cache.
    #[serde(skip, default)]
    pub(crate) compiled_patterns: Vec<Pattern>,
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
        // Fast path: use the pre-compiled patterns when available so we don't
        // re-run `Pattern::new` on every request. The vec is non-empty iff
        // [`compile_patterns`](Self::compile_patterns) succeeded at load time
        // and the slot has at least one pattern.
        if !self.compiled_patterns.is_empty() {
            return self.compiled_patterns.iter().any(|p| p.matches(path));
        }

        // Fallback for slots constructed by hand (tests, legacy callers that
        // skip `compile_patterns`). Re-compiles on every call.
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

    /// Compile [`page_patterns`](Self::page_patterns) into the
    /// [`compiled_patterns`](Self::compiled_patterns) cache.
    ///
    /// Patterns that fail to compile (either directly or after the `**`→`*`
    /// normalisation that [`matches_path`](Self::matches_path) does) are
    /// silently skipped — the slot just becomes un-matchable, matching the
    /// fallback behaviour.
    ///
    /// Idempotent: calling twice replaces the cache, so a slot list reloaded
    /// at runtime won't accumulate stale patterns.
    pub fn compile_patterns(&mut self) {
        self.compiled_patterns = self
            .page_patterns
            .iter()
            .filter_map(|pattern| {
                Pattern::new(pattern)
                    .or_else(|_| Pattern::new(&pattern.replace("**", "*")))
                    .ok()
            })
            .collect();
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
    pub fn to_ad_slot(&self) -> AdSlot {
        let mut bidders: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(ref aps) = self.providers.aps {
            bidders.insert(
                "aps".to_string(),
                serde_json::json!({ "slotID": aps.slot_id }),
            );
        }
        if let Some(ref prebid) = self.providers.prebid {
            for (name, params) in &prebid.bidders {
                bidders.insert(name.clone(), params.clone());
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SlotProviders {
    /// Amazon Publisher Services (APS/TAM) slot parameters.
    pub aps: Option<ApsSlotParams>,
    /// Prebid Server inline bidder parameters.
    ///
    /// When present, these are forwarded directly as `ext.prebid.bidder.*`
    /// in the OpenRTB request, bypassing PBS stored-request lookup for this slot.
    /// Useful in development environments where stored requests are not available.
    pub prebid: Option<PrebidSlotParams>,
}

/// APS-specific parameters for a slot.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApsSlotParams {
    /// The APS slot ID string used when making TAM bid requests.
    pub slot_id: String,
}

/// Inline Prebid Server bidder parameters for a slot.
///
/// Keyed by bidder name (e.g., `"mocktioneer"`). Each value is the
/// bidder-specific params object forwarded verbatim to PBS.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrebidSlotParams {
    /// Per-bidder inline params map. Bidder name → params object.
    pub bidders: HashMap<String, serde_json::Value>,
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
            compiled_patterns: Vec::new(),
        }
    }

    #[test]
    fn compile_patterns_populates_cache_and_match_uses_it() {
        let mut slot = make_slot("atf", vec!["/20**", "/about"]);
        assert!(
            slot.compiled_patterns.is_empty(),
            "freshly-built slot should have no compiled patterns"
        );
        slot.compile_patterns();
        assert_eq!(
            slot.compiled_patterns.len(),
            2,
            "compile_patterns should populate one entry per page pattern"
        );
        assert!(
            slot.matches_path("/2024/01/my-article/"),
            "matches_path should hit the compiled-pattern fast path"
        );
        assert!(
            slot.matches_path("/about"),
            "matches_path should hit /about via the compiled cache"
        );
        assert!(
            !slot.matches_path("/contact"),
            "matches_path should reject paths that match nothing in the cache"
        );
    }

    #[test]
    fn compile_slots_populates_every_slot() {
        let mut slots = vec![make_slot("a", vec!["/a/*"]), make_slot("b", vec!["/b/*"])];
        for slot in &mut slots {
            slot.compile_patterns();
        }
        for slot in &slots {
            assert_eq!(
                slot.compiled_patterns.len(),
                1,
                "every slot's patterns should be pre-compiled after compile_patterns()"
            );
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
        let ad_slot = slot.to_ad_slot();
        let aps_params = ad_slot.bidders.get("aps").expect("should have aps bidder");
        assert_eq!(
            aps_params.get("slotID").and_then(|v| v.as_str()),
            Some("aps-slot-atf"),
        );
    }

    #[test]
    fn to_ad_slot_sets_floor_price_and_formats() {
        let slot = make_slot("atf", vec!["/"]);
        let ad_slot = slot.to_ad_slot();
        assert_eq!(ad_slot.id, "atf");
        assert_eq!(ad_slot.floor_price, Some(0.50));
        assert_eq!(ad_slot.formats.len(), 1);
    }
}
