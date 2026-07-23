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

/// A single parsed segment of a [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path) template.
#[derive(Debug, Clone)]
pub(crate) enum UnitTemplatePart {
    /// Verbatim text between placeholders.
    Literal(String),
    /// `{network_id}` — replaced with the GAM network id.
    NetworkId,
    /// `{section}` — replaced with the request-derived section.
    Section,
    /// `{slot_id}` — replaced with the slot id.
    SlotId,
}

/// Parses a `gam_unit_path` template into an ordered list of parts.
///
/// Supported placeholders: `{network_id}`, `{section}`, `{slot_id}`. A template
/// with no placeholders is a single [`UnitTemplatePart::Literal`] and renders
/// verbatim.
///
/// # Errors
///
/// Returns an error string for an empty template, an unmatched or nested `{`,
/// a stray `}`, or an unknown placeholder name.
fn parse_unit_template(raw: &str) -> Result<Vec<UnitTemplatePart>, String> {
    if raw.is_empty() {
        return Err("gam_unit_path template must not be empty".to_string());
    }
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if !literal.is_empty() {
                    parts.push(UnitTemplatePart::Literal(std::mem::take(&mut literal)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') => return Err(format!("nested '{{' in template `{raw}`")),
                        Some(ch) => name.push(ch),
                        None => return Err(format!("unmatched '{{' in template `{raw}`")),
                    }
                }
                match name.as_str() {
                    "network_id" => parts.push(UnitTemplatePart::NetworkId),
                    "section" => parts.push(UnitTemplatePart::Section),
                    "slot_id" => parts.push(UnitTemplatePart::SlotId),
                    other => {
                        return Err(format!(
                            "unknown placeholder `{{{other}}}` in template `{raw}`"
                        ));
                    }
                }
            }
            '}' => return Err(format!("stray '}}' in template `{raw}`")),
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        parts.push(UnitTemplatePart::Literal(literal));
    }
    Ok(parts)
}

/// Collapses each run of characters outside `[A-Za-z0-9_-]` to a single `_`.
///
/// Returns a non-empty string for any non-empty input.
fn sanitize_section(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut in_bad_run = false;
    for ch in segment.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            in_bad_run = false;
        } else if !in_bad_run {
            out.push('_');
            in_bad_run = true;
        }
    }
    out
}

/// Derives the `{section}` value from a request path.
///
/// Uses the first non-empty path segment, sanitized to `[A-Za-z0-9_-]`. Falls
/// back to `section_root` when the path has no segment (`/`, repeated slashes).
///
/// The path is used **raw** (not percent-decoded) so this stays consistent with
/// how [`page_patterns`](CreativeOpportunitySlot::page_patterns) glob-match the
/// same path — e.g. `/new%20s` yields `new_20s`, never the decoded `new_s`.
pub(crate) fn derive_section(path: &str, section_root: &str) -> String {
    match path.split('/').find(|segment| !segment.is_empty()) {
        Some(segment) => sanitize_section(segment),
        None => section_root.to_string(),
    }
}

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
    /// Value substituted for `{section}` when the request path has no first
    /// segment (e.g. `/`).
    ///
    /// Required when any slot's [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path)
    /// template contains `{section}`. No default — a home-section name is
    /// publisher-specific, so the URL→section convention stays in config, not core.
    #[serde(default)]
    pub section_root: Option<String>,
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

    /// Parse every slot's [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path)
    /// template. Call once after deserialization, before [`validate_runtime`](Self::validate_runtime).
    ///
    /// # Errors
    ///
    /// Returns an error string when any slot's template is malformed.
    pub fn compile_unit_templates(&mut self) -> Result<(), String> {
        for slot in &mut self.slot {
            slot.compile_unit_template()?;
        }
        Ok(())
    }

    /// Validate all slot definitions after runtime preparation.
    ///
    /// # Errors
    ///
    /// Returns an error string when a slot has an invalid identifier, page
    /// pattern set, format list, or dimensions, or when a slot's `gam_unit_path`
    /// template uses `{section}` without a valid [`section_root`](Self::section_root).
    pub fn validate_runtime(&self) -> Result<(), String> {
        for slot in &self.slot {
            slot.validate_runtime()?;
        }

        if self
            .slot
            .iter()
            .any(CreativeOpportunitySlot::template_uses_section)
        {
            match self.section_root.as_deref() {
                Some(root)
                    if !root.is_empty()
                        && root
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => {}
                _ => {
                    return Err("section_root is required and must match [A-Za-z0-9_-]+ \
                                when a gam_unit_path template uses {section}"
                        .to_string());
                }
            }
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
    /// Pre-parsed [`gam_unit_path`](Self::gam_unit_path) template, populated by
    /// [`compile_unit_template`](Self::compile_unit_template) at startup.
    ///
    /// `None` when the slot has no explicit `gam_unit_path` (renders the default
    /// `/<network_id>/<id>`). `pub(crate)` so cross-module test helpers can build
    /// slots via struct-literal syntax with an empty cache.
    #[serde(skip, default)]
    pub(crate) compiled_unit: Option<Vec<UnitTemplatePart>>,
}

impl CreativeOpportunitySlot {
    /// Validate the slot shape after [`compile_patterns`](Self::compile_patterns) has run.
    ///
    /// # Errors
    ///
    /// Returns an error string when required slot fields are empty, invalid,
    /// or semantically unusable at runtime.
    pub fn validate_runtime(&self) -> Result<(), String> {
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

        // A present-but-blank `gam_unit_path` renders to an empty/whitespace
        // unit path. An empty string also fails template parsing at startup;
        // this keeps the slot-level check self-contained (tests call
        // `validate_runtime` without compiling templates first).
        if let Some(raw) = &self.gam_unit_path
            && raw.trim().is_empty()
        {
            return Err(format!("slot `{}` gam_unit_path must not be empty", self.id));
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

    /// Parses [`gam_unit_path`](Self::gam_unit_path) into
    /// [`compiled_unit`](Self::compiled_unit). Call once at startup via
    /// [`CreativeOpportunitiesConfig::compile_unit_templates`].
    ///
    /// # Errors
    ///
    /// Returns an error string (prefixed with the slot id) when the template is
    /// malformed. See [`parse_unit_template`].
    pub fn compile_unit_template(&mut self) -> Result<(), String> {
        self.compiled_unit = match &self.gam_unit_path {
            Some(raw) => {
                Some(parse_unit_template(raw).map_err(|e| format!("slot `{}`: {e}", self.id))?)
            }
            None => None,
        };
        Ok(())
    }

    /// Renders the resolved GAM unit path for a given network id and section.
    ///
    /// Substitutes `{network_id}`, `{section}`, and `{slot_id}` in the parsed
    /// template. Falls back to `/<network_id>/<id>` when the slot has no template.
    #[must_use]
    pub fn render_gam_unit_path(&self, gam_network_id: &str, section: &str) -> String {
        match &self.compiled_unit {
            Some(parts) => parts
                .iter()
                .map(|part| match part {
                    UnitTemplatePart::Literal(s) => s.as_str(),
                    UnitTemplatePart::NetworkId => gam_network_id,
                    UnitTemplatePart::Section => section,
                    UnitTemplatePart::SlotId => self.id.as_str(),
                })
                .collect(),
            None => format!("/{}/{}", gam_network_id, self.id),
        }
    }

    /// Returns `true` if this slot's compiled template contains `{section}`.
    #[must_use]
    pub(crate) fn template_uses_section(&self) -> bool {
        self.compiled_unit
            .as_ref()
            .is_some_and(|parts| parts.iter().any(|p| matches!(p, UnitTemplatePart::Section)))
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
            compiled_unit: None,
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
    fn parse_unit_template_accepts_known_placeholders() {
        let parts = parse_unit_template("/{network_id}/autoblog/{section}")
            .expect("should parse valid template");
        assert_eq!(parts.len(), 4, "should split into literal+ph+literal+ph");
    }

    #[test]
    fn parse_unit_template_accepts_static_path() {
        let parts = parse_unit_template("/88059007/autoblog/homepage")
            .expect("should parse a static path as a single literal");
        assert!(
            matches!(parts.as_slice(), [UnitTemplatePart::Literal(s)] if s == "/88059007/autoblog/homepage"),
            "should be one literal part"
        );
    }

    #[test]
    fn parse_unit_template_rejects_unknown_placeholder() {
        let err = parse_unit_template("/{network_id}/{oops}")
            .expect_err("should reject unknown placeholder");
        assert!(err.contains("oops"), "error should name the bad placeholder");
    }

    #[test]
    fn parse_unit_template_rejects_unmatched_brace() {
        parse_unit_template("/{network_id}/{section").expect_err("should reject unmatched '{'");
        parse_unit_template("/a}b").expect_err("should reject stray '}'");
    }

    #[test]
    fn parse_unit_template_rejects_nested_brace() {
        parse_unit_template("/{net{work}_id}").expect_err("should reject nested '{'");
    }

    #[test]
    fn parse_unit_template_rejects_empty() {
        parse_unit_template("").expect_err("should reject empty template");
    }

    #[test]
    fn derive_section_uses_first_segment() {
        assert_eq!(derive_section("/news", "home"), "news");
        assert_eq!(derive_section("/news/gm-cadillac", "home"), "news");
        assert_eq!(derive_section("/car-research/x", "home"), "car-research");
    }

    #[test]
    fn derive_section_uses_root_when_no_segment() {
        assert_eq!(derive_section("/", "homepage"), "homepage");
        assert_eq!(derive_section("///", "homepage"), "homepage");
    }

    #[test]
    fn derive_section_sanitizes_unsafe_runs_to_single_underscore() {
        // Not decoded: in "new%20s" only '%' is disallowed ('2' and '0' are
        // alphanumeric), so it collapses to a single '_' -> "new_20s". This is
        // exactly the no-decode contract: had we decoded, %20 would be a space
        // and yield "new_s"; we do NOT decode.
        assert_eq!(derive_section("/new%20s", "home"), "new_20s");
        // A run of disallowed chars collapses to one '_'.
        assert_eq!(derive_section("/a..b", "home"), "a_b");
    }

    #[test]
    fn derive_section_is_non_empty_for_all_disallowed_segment() {
        assert_eq!(derive_section("/%%%/x", "home"), "_");
    }

    fn make_config_with_section_template(section_root: Option<&str>) -> CreativeOpportunitiesConfig {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{network_id}/autoblog/{section}".to_string());
        CreativeOpportunitiesConfig {
            gam_network_id: "88059007".to_string(),
            auction_timeout_ms: None,
            price_granularity: PriceGranularity::default(),
            section_root: section_root.map(str::to_string),
            slot: vec![slot],
        }
    }

    #[test]
    fn render_gam_unit_path_substitutes_placeholders() {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{network_id}/autoblog/{section}".to_string());
        slot.compile_unit_template().expect("should compile template");
        assert_eq!(
            slot.render_gam_unit_path("88059007", "news"),
            "/88059007/autoblog/news"
        );
    }

    #[test]
    fn render_gam_unit_path_defaults_when_no_template() {
        let mut slot = make_slot("sidebar", vec!["/*"]);
        slot.gam_unit_path = None;
        slot.compile_unit_template().expect("should compile (no template)");
        assert_eq!(slot.render_gam_unit_path("99999", "ignored"), "/99999/sidebar");
    }

    #[test]
    fn render_gam_unit_path_uses_static_template_verbatim() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some("/99999/example/homepage".to_string());
        slot.compile_unit_template()
            .expect("should compile static template");
        assert_eq!(
            slot.render_gam_unit_path("99999", "news"),
            "/99999/example/homepage"
        );
    }

    #[test]
    fn validate_runtime_requires_section_root_when_template_uses_section() {
        let mut config = make_config_with_section_template(None);
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("templates should compile");
        let err = config
            .validate_runtime()
            .expect_err("should require section_root");
        assert!(err.contains("section_root"), "error should mention section_root");
    }

    #[test]
    fn validate_runtime_rejects_invalid_section_root() {
        let mut config = make_config_with_section_template(Some("has space"));
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("templates should compile");
        config
            .validate_runtime()
            .expect_err("should reject non [A-Za-z0-9_-] root");
    }

    #[test]
    fn validate_runtime_accepts_section_template_with_valid_root() {
        let mut config = make_config_with_section_template(Some("homepage"));
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("templates should compile");
        config
            .validate_runtime()
            .expect("should accept valid section_root");
    }

    #[test]
    fn compile_unit_templates_surfaces_parse_error() {
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = Some("/{bad}".to_string());
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect_err("should surface unknown-placeholder error");
    }

    #[test]
    fn validate_runtime_rejects_empty_div_id_override() {
        // An empty/whitespace div_id would resolve every slot to the first
        // id-bearing element via `candidate.id.startsWith(slot.div_id)`.
        let mut slot = make_slot("atf", vec!["/"]);
        slot.compile_patterns();

        slot.div_id = Some(String::new());
        assert!(
            slot.validate_runtime().is_err(),
            "empty div_id override should fail validation"
        );

        slot.div_id = Some("   ".to_string());
        assert!(
            slot.validate_runtime().is_err(),
            "whitespace-only div_id override should fail validation"
        );

        slot.div_id = Some("div-ad-x".to_string());
        assert!(
            slot.validate_runtime().is_ok(),
            "a concrete div_id override should pass validation"
        );
    }

    #[test]
    fn validate_runtime_rejects_invalid_floor_prices() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.compile_patterns();

        slot.floor_price = Some(-0.01);
        assert!(
            slot.validate_runtime().is_err(),
            "negative floor_price should fail validation"
        );

        slot.floor_price = Some(f64::NAN);
        assert!(
            slot.validate_runtime().is_err(),
            "NaN floor_price should fail validation"
        );

        slot.floor_price = Some(f64::INFINITY);
        assert!(
            slot.validate_runtime().is_err(),
            "infinite floor_price should fail validation"
        );

        slot.floor_price = Some(0.0);
        assert!(
            slot.validate_runtime().is_ok(),
            "zero floor_price should pass validation"
        );

        slot.floor_price = None;
        assert!(
            slot.validate_runtime().is_ok(),
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
