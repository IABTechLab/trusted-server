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
#[serde(deny_unknown_fields)]
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
    /// Price granularity for header-bidding price bucketing. Defaults to `Dense`.
    #[serde(default)]
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

    /// Validate all slot definitions after runtime preparation.
    ///
    /// # Errors
    ///
    /// Returns an error string when a slot has an invalid identifier, page
    /// pattern set, format list, dimensions, or resolved GAM unit path.
    pub fn validate_runtime(&self) -> Result<(), String> {
        for slot in &self.slot {
            slot.validate_runtime(&self.gam_network_id)?;
        }

        Ok(())
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
    /// Validate the slot shape after [`compile_patterns`](Self::compile_patterns) has run.
    ///
    /// # Errors
    ///
    /// Returns an error string when required slot fields are empty, invalid,
    /// or semantically unusable at runtime.
    pub fn validate_runtime(&self, gam_network_id: &str) -> Result<(), String> {
        validate_slot_id(&self.id)?;

        if self.page_patterns.is_empty() {
            return Err(format!(
                "slot `{}` must include at least one page pattern",
                self.id
            ));
        }

        if self.compiled_patterns.is_empty() {
            return Err(format!(
                "slot `{}` must include at least one valid page pattern",
                self.id
            ));
        }

        if self.formats.is_empty() {
            return Err(format!(
                "slot `{}` must include at least one format",
                self.id
            ));
        }

        for format in &self.formats {
            format.validate_runtime(&self.id)?;
        }

        // A negative floor silently disables minimum-price enforcement, and a
        // non-finite floor (NaN/infinity) produces surprising all-pass/all-drop
        // comparisons and an invalid OpenRTB `bidfloor`.
        if let Some(floor_price) = self.floor_price
            && (!floor_price.is_finite() || floor_price < 0.0)
        {
            return Err(format!(
                "slot `{}` floor_price must be a finite value >= 0.0, got {floor_price}",
                self.id
            ));
        }

        // An explicit empty/whitespace `div_id` override is rejected: the
        // injected JS resolves slots with `candidate.id.startsWith(slot.div_id)`,
        // and every element id starts with the empty string, so an empty override
        // would bind the slot to the first id-bearing element in the document.
        if self
            .div_id
            .as_deref()
            .is_some_and(|div_id| div_id.trim().is_empty())
        {
            return Err(format!(
                "slot `{}` div_id override must not be empty",
                self.id
            ));
        }

        if self
            .resolved_gam_unit_path(gam_network_id)
            .trim()
            .is_empty()
        {
            return Err(format!(
                "slot `{}` resolved GAM unit path must not be empty",
                self.id
            ));
        }

        Ok(())
    }

    /// Returns `true` if `path` matches any of this slot's [`page_patterns`](Self::page_patterns).
    ///
    /// Patterns use glob syntax (e.g., `"/2024/*"` matches any path under `/2024/`,
    /// `"/"` matches only the root). A single `*` matches any sequence of characters
    /// including path separators because `require_literal_separator` is `false`.
    /// When a pattern contains `**` in a position the glob crate considers invalid
    /// (e.g., `"/20**"` or `"b**"`), the `**` is normalised to `*` before matching —
    /// prefer a valid single-`*` pattern over relying on this fallback.
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
                match Pattern::new(pattern).or_else(|_| Pattern::new(&pattern.replace("**", "*"))) {
                    Ok(compiled) => Some(compiled),
                    Err(_) => {
                        // Build-time validation only requires *one* valid pattern
                        // per slot, so a mixed valid/invalid set passes the build
                        // with the bad pattern silently dropped here. Warn so the
                        // operator can see the slot matches fewer pages than
                        // configured.
                        log::warn!(
                            "slot `{}`: dropping page pattern '{}' — it does not compile as a glob",
                            self.id,
                            pattern
                        );
                        None
                    }
                }
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
    ///
    /// When [`PrebidSlotParams::bidders`] is empty, a `trustedServer` entry is
    /// injected so [`PrebidAuctionProvider`] expands all `config.bidders`
    /// automatically. The slot's `targeting.zone` value is forwarded as
    /// `trustedServer.zone` so zone-aware bid-param override rules fire correctly.
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
            if prebid.bidders.is_empty() {
                // No explicit per-bidder override: let the Prebid provider expand
                // all config.bidders. The "trustedServer" key triggers
                // expand_trusted_server_bidders in PrebidAuctionProvider, giving
                // each bidder an empty params object that the override engine then
                // fills with zone-aware rules.
                let mut ts = serde_json::json!({ "bidderParams": {} });
                if let Some(zone) = self.targeting.get("zone") {
                    ts["zone"] = serde_json::Value::String(zone.clone());
                }
                bidders.insert("trustedServer".to_string(), ts);
            } else {
                for (name, params) in &prebid.bidders {
                    bidders.insert(name.clone(), params.clone());
                }
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
#[serde(deny_unknown_fields)]
pub struct CreativeOpportunityFormat {
    /// Creative width in pixels.
    pub width: u32,
    /// Creative height in pixels.
    pub height: u32,
    /// Media type for this format. Defaults to `Banner`.
    #[serde(default)]
    pub media_type: MediaType,
}

impl CreativeOpportunityFormat {
    fn validate_runtime(&self, slot_id: &str) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "slot `{slot_id}` format must have positive width and height"
            ));
        }

        Ok(())
    }

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
#[serde(deny_unknown_fields)]
pub struct SlotProviders {
    /// Amazon Publisher Services (APS/TAM) slot parameters.
    pub aps: Option<ApsSlotParams>,
    /// Prebid Server inline bidder parameters.
    ///
    /// When present, these are forwarded directly as `ext.prebid.bidder.*`
    /// in the `OpenRTB` request, bypassing PBS stored request lookup for this slot.
    /// Useful in development environments where stored requests are not available.
    pub prebid: Option<PrebidSlotParams>,
}

/// APS-specific parameters for a slot.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApsSlotParams {
    /// The APS slot ID string used when making TAM bid requests.
    pub slot_id: String,
}

/// Inline Prebid Server bidder parameters for a slot.
///
/// When `bidders` is empty, `to_ad_slot` injects a `trustedServer` entry so
/// [`PrebidAuctionProvider`] expands all `config.bidders` automatically.
/// When `bidders` is non-empty the map is forwarded verbatim, bypassing
/// automatic expansion (useful for slots that need explicit per-bidder params).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrebidSlotParams {
    /// Per-bidder inline params map. Bidder name → params object.
    ///
    /// Leave empty (or omit `bidders` in config) to auto-expand all
    /// `config.bidders` with zone-aware param overrides.
    ///
    /// Note: when this map is non-empty it is forwarded verbatim, so a slot's
    /// `targeting.zone` is **not** injected for these bidders (the `trustedServer`
    /// expansion key that carries it is only added when `bidders` is empty). Set
    /// explicit per-bidder params only when you do not need zone-aware overrides.
    #[serde(default)]
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

/// Three-state outcome of the server-side ad-stack gate.
///
/// [`Yes`](RuntimeAdStackExpected::Yes) and [`No`](RuntimeAdStackExpected::No)
/// are decided purely from known inputs; [`Unknown`](RuntimeAdStackExpected::Unknown)
/// is reserved for callers (such as the operator CLI) that cannot prove the live
/// consent state and pass `None` for `consent_allows_auction`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RuntimeAdStackExpected {
    /// All known gates pass and consent is known to allow the auction.
    Yes,
    /// At least one known gate blocks the server-side ad stack.
    No,
    /// All known gates pass but consent is unproven.
    Unknown,
}

/// Identifies a single gate evaluated by [`evaluate_ad_stack_gate`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AdStackGateName {
    /// Request method is `GET`.
    MethodGet,
    /// Request is a top-level navigation.
    Navigation,
    /// Request is not a prefetch.
    NotPrefetch,
    /// Request is not from a known bot.
    NotBot,
    /// At least one configured slot matches the request path.
    MatchedSlots,
    /// Consent is known to allow the auction.
    ConsentAllowsAuction,
    /// The global `[auction].enabled` kill switch is on.
    AuctionEnabled,
}

/// Inputs to [`evaluate_ad_stack_gate`].
///
/// `consent_allows_auction` is tri-state: `Some(true)` allows, `Some(false)`
/// blocks, and `None` means the caller cannot prove the consent state.
#[derive(Debug, Clone, Copy)]
pub struct AdStackGateInput {
    /// Request method is `GET`.
    pub method_get: bool,
    /// Request is a top-level navigation.
    pub navigation: bool,
    /// Request advertises itself as a prefetch.
    pub prefetch: bool,
    /// Request is from a known bot.
    pub bot: bool,
    /// At least one configured slot matches the request path.
    pub matched_slots: bool,
    /// Whether consent allows the auction; `None` when unprovable.
    pub consent_allows_auction: Option<bool>,
    /// The global `[auction].enabled` kill switch.
    pub auction_enabled: bool,
}

/// Result of [`evaluate_ad_stack_gate`]: the three-state expectation plus the
/// list of gates that blocked the stack (empty unless `expected` is
/// [`No`](RuntimeAdStackExpected::No)).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdStackGateResult {
    /// The three-state ad-stack expectation.
    pub expected: RuntimeAdStackExpected,
    blocking_gates: Vec<AdStackGateName>,
}

impl AdStackGateResult {
    /// Returns the gates that blocked the server-side ad stack.
    #[must_use]
    pub fn blocking_gates(&self) -> &[AdStackGateName] {
        &self.blocking_gates
    }
}

/// Evaluates whether the server-side ad stack should run for a request.
///
/// Any known gate that fails sets [`No`](RuntimeAdStackExpected::No) and is
/// recorded in [`AdStackGateResult::blocking_gates`]. When no known gate blocks,
/// the result is [`Yes`](RuntimeAdStackExpected::Yes) if consent is known to
/// allow the auction, or [`Unknown`](RuntimeAdStackExpected::Unknown) when
/// `consent_allows_auction` is `None`.
///
/// Gate polarity mirrors the runtime publisher path: `method_get`, `navigation`,
/// `matched_slots`, and `auction_enabled` block when `false`; `prefetch` and
/// `bot` block when `true`.
#[must_use]
pub fn evaluate_ad_stack_gate(input: AdStackGateInput) -> AdStackGateResult {
    let mut blocking_gates = Vec::new();
    if !input.method_get {
        blocking_gates.push(AdStackGateName::MethodGet);
    }
    if !input.navigation {
        blocking_gates.push(AdStackGateName::Navigation);
    }
    if input.prefetch {
        blocking_gates.push(AdStackGateName::NotPrefetch);
    }
    if input.bot {
        blocking_gates.push(AdStackGateName::NotBot);
    }
    if !input.matched_slots {
        blocking_gates.push(AdStackGateName::MatchedSlots);
    }
    if input.consent_allows_auction == Some(false) {
        blocking_gates.push(AdStackGateName::ConsentAllowsAuction);
    }
    if !input.auction_enabled {
        blocking_gates.push(AdStackGateName::AuctionEnabled);
    }

    let expected = if !blocking_gates.is_empty() {
        RuntimeAdStackExpected::No
    } else if input.consent_allows_auction.is_none() {
        RuntimeAdStackExpected::Unknown
    } else {
        RuntimeAdStackExpected::Yes
    };

    AdStackGateResult {
        expected,
        blocking_gates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_stack_gate_passes_for_eligible_navigation() {
        let result = evaluate_ad_stack_gate(AdStackGateInput {
            method_get: true,
            navigation: true,
            prefetch: false,
            bot: false,
            matched_slots: true,
            consent_allows_auction: Some(true),
            auction_enabled: true,
        });

        assert_eq!(result.expected, RuntimeAdStackExpected::Yes);
        assert!(result.blocking_gates().is_empty());
    }

    #[test]
    fn ad_stack_gate_blocks_known_kill_switch() {
        let result = evaluate_ad_stack_gate(AdStackGateInput {
            method_get: true,
            navigation: true,
            prefetch: false,
            bot: false,
            matched_slots: true,
            consent_allows_auction: Some(true),
            auction_enabled: false,
        });

        assert_eq!(result.expected, RuntimeAdStackExpected::No);
        assert!(
            result
                .blocking_gates()
                .contains(&AdStackGateName::AuctionEnabled)
        );
    }

    #[test]
    fn ad_stack_gate_is_unknown_when_consent_is_unknown() {
        let result = evaluate_ad_stack_gate(AdStackGateInput {
            method_get: true,
            navigation: true,
            prefetch: false,
            bot: false,
            matched_slots: true,
            consent_allows_auction: None,
            auction_enabled: true,
        });

        assert_eq!(result.expected, RuntimeAdStackExpected::Unknown);
    }

    // Locks the spec §5.2 mirror invariant: with Some(consent) supplied for every
    // input combination, `expected == Yes` must equal the legacy all-AND boolean.
    #[test]
    fn ad_stack_gate_with_known_consent_matches_legacy_boolean() {
        for bits in 0u8..128 {
            let input = AdStackGateInput {
                method_get: bits & 1 != 0,
                navigation: bits & 2 != 0,
                prefetch: bits & 4 != 0,
                bot: bits & 8 != 0,
                matched_slots: bits & 16 != 0,
                consent_allows_auction: Some(bits & 32 != 0),
                auction_enabled: bits & 64 != 0,
            };
            // Legacy semantics: all positive gates true, both negative gates false.
            let legacy = input.method_get
                && input.navigation
                && !input.prefetch
                && !input.bot
                && input.matched_slots
                && input.consent_allows_auction == Some(true)
                && input.auction_enabled;
            let got = evaluate_ad_stack_gate(input).expected == RuntimeAdStackExpected::Yes;
            assert_eq!(got, legacy, "gate mismatch for bits={bits}");
        }
    }

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
    fn validate_runtime_rejects_empty_div_id_override() {
        // An empty/whitespace div_id would resolve every slot to the first
        // id-bearing element via `candidate.id.startsWith(slot.div_id)`.
        let mut slot = make_slot("atf", vec!["/"]);
        slot.compile_patterns();

        slot.div_id = Some(String::new());
        assert!(
            slot.validate_runtime("1234").is_err(),
            "empty div_id override should fail validation"
        );

        slot.div_id = Some("   ".to_string());
        assert!(
            slot.validate_runtime("1234").is_err(),
            "whitespace-only div_id override should fail validation"
        );

        slot.div_id = Some("div-ad-x".to_string());
        assert!(
            slot.validate_runtime("1234").is_ok(),
            "a concrete div_id override should pass validation"
        );
    }

    #[test]
    fn validate_runtime_rejects_invalid_floor_prices() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.compile_patterns();

        slot.floor_price = Some(-0.01);
        assert!(
            slot.validate_runtime("1234").is_err(),
            "negative floor_price should fail validation"
        );

        slot.floor_price = Some(f64::NAN);
        assert!(
            slot.validate_runtime("1234").is_err(),
            "NaN floor_price should fail validation"
        );

        slot.floor_price = Some(f64::INFINITY);
        assert!(
            slot.validate_runtime("1234").is_err(),
            "infinite floor_price should fail validation"
        );

        slot.floor_price = Some(0.0);
        assert!(
            slot.validate_runtime("1234").is_ok(),
            "zero floor_price should pass validation"
        );

        slot.floor_price = None;
        assert!(
            slot.validate_runtime("1234").is_ok(),
            "absent floor_price should pass validation"
        );
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

    #[test]
    fn to_ad_slot_injects_trusted_server_when_prebid_bidders_empty() {
        let mut slot = make_slot("header", vec!["/"]);
        slot.targeting
            .insert("zone".to_string(), "header".to_string());
        slot.providers.prebid = Some(PrebidSlotParams {
            bidders: HashMap::new(),
        });
        let ad_slot = slot.to_ad_slot();

        let ts = ad_slot
            .bidders
            .get("trustedServer")
            .expect("should have trustedServer bidder");
        assert_eq!(
            ts.get("zone").and_then(|v| v.as_str()),
            Some("header"),
            "should forward zone from targeting"
        );
        assert!(
            ts.get("bidderParams").is_some(),
            "should include bidderParams key for expand_trusted_server_bidders"
        );
    }

    #[test]
    fn to_ad_slot_injects_trusted_server_without_zone_when_targeting_absent() {
        let mut slot = make_slot("no-zone", vec!["/"]);
        slot.providers.prebid = Some(PrebidSlotParams {
            bidders: HashMap::new(),
        });
        let ad_slot = slot.to_ad_slot();

        let ts = ad_slot
            .bidders
            .get("trustedServer")
            .expect("should have trustedServer bidder");
        assert!(
            ts.get("zone").is_none(),
            "should not inject zone when targeting has no zone key"
        );
    }

    #[test]
    fn to_ad_slot_uses_explicit_bidders_when_nonempty() {
        let mut slot = make_slot("explicit", vec!["/"]);
        slot.providers.prebid = Some(PrebidSlotParams {
            bidders: HashMap::from([(
                "mocktioneer".to_string(),
                serde_json::json!({"custom": true}),
            )]),
        });
        let ad_slot = slot.to_ad_slot();

        assert!(
            !ad_slot.bidders.contains_key("trustedServer"),
            "should not inject trustedServer when explicit bidders are set"
        );
        let params = ad_slot
            .bidders
            .get("mocktioneer")
            .expect("should have mocktioneer bidder");
        assert_eq!(
            params.get("custom").and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn config_rejects_unknown_top_level_key() {
        // A typo such as `slots` instead of `slot` must surface as a config
        // error rather than silently deserializing to an empty (disabled) stack.
        let typo = serde_json::json!({ "gam_network_id": "12345", "slots": [] });
        assert!(
            serde_json::from_value::<CreativeOpportunitiesConfig>(typo).is_err(),
            "unknown top-level key should be rejected by deny_unknown_fields"
        );

        let correct = serde_json::json!({ "gam_network_id": "12345", "slot": [] });
        assert!(
            serde_json::from_value::<CreativeOpportunitiesConfig>(correct).is_ok(),
            "the correct `slot` key should still deserialize"
        );
    }

    #[test]
    fn config_rejects_unknown_nested_keys() {
        // Format typo: `med.a_type` instead of `media_type`.
        let format_typo = serde_json::json!({ "width": 300, "height": 250, "meda_type": "banner" });
        assert!(
            serde_json::from_value::<CreativeOpportunityFormat>(format_typo).is_err(),
            "unknown format key should be rejected"
        );

        // Provider typo: `prebd` instead of `prebid`.
        let providers_typo = serde_json::json!({ "prebd": {} });
        assert!(
            serde_json::from_value::<SlotProviders>(providers_typo).is_err(),
            "unknown provider key should be rejected"
        );

        // APS typo: `slotId` instead of `slot_id`.
        let aps_typo = serde_json::json!({ "slotId": "x" });
        assert!(
            serde_json::from_value::<ApsSlotParams>(aps_typo).is_err(),
            "unknown APS key should be rejected"
        );
    }

    #[test]
    fn prebid_slot_params_deserializes_without_bidders_field() {
        let json = r#"{"bidders": {}}"#;
        let params: PrebidSlotParams =
            serde_json::from_str(json).expect("should deserialize with empty bidders");
        assert!(params.bidders.is_empty(), "should have empty bidders map");

        let json_no_field = r#"{}"#;
        let params2: PrebidSlotParams =
            serde_json::from_str(json_no_field).expect("should deserialize without bidders field");
        assert!(
            params2.bidders.is_empty(),
            "should default to empty when bidders field absent"
        );
    }
}
