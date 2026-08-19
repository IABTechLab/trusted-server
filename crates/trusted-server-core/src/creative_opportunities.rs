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

const MAX_DYNAMIC_GAM_UNIT_PATH_BYTES: usize = 100;
const MAX_SECTION_BYTES: usize = 100;
const DEFAULT_TEMPLATE_CACHE_MAX_AGE_SECONDS: u32 = 60;
const MAX_TEMPLATE_CACHE_MAX_AGE_SECONDS: u32 = 86_400;

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

impl UnitTemplatePart {
    fn is_placeholder(&self) -> bool {
        !matches!(self, Self::Literal(_))
    }
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

fn resolved_unit_template_part<'a>(
    part: &'a UnitTemplatePart,
    gam_network_id: &'a str,
    section: &'a str,
    slot_id: &'a str,
) -> &'a str {
    match part {
        UnitTemplatePart::Literal(value) => value,
        UnitTemplatePart::NetworkId => gam_network_id,
        UnitTemplatePart::Section => section,
        UnitTemplatePart::SlotId => slot_id,
    }
}

fn render_dynamic_unit_path(
    parts: &[UnitTemplatePart],
    gam_network_id: &str,
    section: &str,
    slot_id: &str,
) -> Option<String> {
    let rendered_len = parts.iter().try_fold(0usize, |len, part| {
        let value = resolved_unit_template_part(part, gam_network_id, section, slot_id);
        len.checked_add(value.len())
    })?;
    if rendered_len > MAX_DYNAMIC_GAM_UNIT_PATH_BYTES {
        return None;
    }

    let mut rendered = String::with_capacity(rendered_len);
    for part in parts {
        rendered.push_str(resolved_unit_template_part(
            part,
            gam_network_id,
            section,
            slot_id,
        ));
    }
    Some(rendered)
}

fn is_section_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

/// Collapses each run of characters outside `[A-Za-z0-9_-]` to a single `_`.
///
/// Returns a non-empty, request-derived ASCII string for any non-empty input,
/// capped at 100 ASCII (and therefore UTF-8) bytes.
fn sanitize_section(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len().min(MAX_SECTION_BYTES));
    let mut in_bad_run = false;
    let mut chars = segment.chars();
    while out.len() < MAX_SECTION_BYTES {
        let Some(ch) = chars.next() else {
            break;
        };
        if is_section_char(ch) {
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
/// Takes the non-empty path segment at `section_segment` (0-based, counting
/// only non-empty segments), sanitizes it to `[A-Za-z0-9_-]`, and caps the
/// request-derived result at 100 ASCII/UTF-8 bytes. Falls back to `section_root`
/// when the path has no such segment — the site root (`/`), repeated slashes,
/// or a path shorter than the configured index.
///
/// `section_segment` exists because the URL→section convention is
/// publisher-specific: a site that prefixes a locale (`/en/news/article`) sets
/// `section_segment = 1` to get `news` rather than `en`.
///
/// The path is used **raw** (not percent-decoded) so this stays consistent with
/// how [`page_patterns`](CreativeOpportunitySlot::page_patterns) glob-match the
/// same path — e.g. `/new%20s` yields `new_20s`, never the decoded `new_s`.
#[must_use]
fn derive_section(path: &str, section_root: &str, section_segment: usize) -> String {
    match path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .nth(section_segment)
    {
        Some(segment) => sanitize_section(segment),
        None => section_root.to_string(),
    }
}

/// How per-user ad state reaches the page.
///
/// `Inline` is the shipped behaviour: the auction result is injected before
/// `</body>` and the root document is therefore uncacheable. `Esi` stores a
/// request-neutral shared template and fills its per-request byte seam at the edge.
///
/// Spike-only, for the #1009 ESI validation. Remove with the spike.
///
/// # Why the template must be request-neutral
///
/// Under `Esi` the template is shared across visitors, so
/// nothing whose *presence* depends on the request may appear in it — not merely
/// nothing whose *value* does. `tsjs.adSlots` is the trap: its content is derived
/// from config and path, but whether it is emitted at all is gated on consent,
/// bot classification, prefetch status and the auction kill switch. A template
/// filled by the first request would freeze that request's decision for every
/// later reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyMode {
    /// Inject bids inline before `</body>`. Root uncacheable. Shipped behaviour.
    #[default]
    Inline,
    /// Serve a shared template; assemble its inert marker with an exact byte split.
    ///
    /// The operator-facing spelling remains `esi` for continuity, but no general
    /// purpose ESI parser executes on this path.
    Esi,
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
    /// Value substituted for `{section}` when the request path has no segment
    /// at [`section_segment`](Self::section_segment), such as `/` or a path
    /// shorter than that configured index.
    ///
    /// Required when any slot's [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path)
    /// template contains `{section}`. No default — a home-section name is
    /// publisher-specific, so it stays in config, not core.
    ///
    /// Static and absent [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path)
    /// configurations remain compatible with the legacy schema only when both
    /// this key and [`section_segment`](Self::section_segment) are omitted.
    /// These structs use `deny_unknown_fields`, so any pushed new key makes an
    /// older binary fail configuration load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_root: Option<String>,
    /// Index of the path segment `{section}` is taken from, 0-based over
    /// non-empty segments. Defaults to `0` (the first segment).
    ///
    /// The URL→section convention is publisher-specific: a site that prefixes a
    /// locale (`/en/news/article`) sets `section_segment = 1` to select `news`
    /// instead of `en`. Paths with no segment at this index fall back to
    /// [`section_root`](Self::section_root), so on `/en` a config with
    /// `section_segment = 1` renders the root section.
    ///
    /// During typed/startup finalization, after successfully parsing any
    /// placeholder-bearing template,
    /// [`compile_unit_templates`](Self::compile_unit_templates) materializes
    /// `Some(0)` when this is unset as an automatic compatibility marker: an
    /// older `deny_unknown_fields` binary then fails loudly rather than silently
    /// accepting a dynamic configuration it does not understand. Static and
    /// absent [`gam_unit_path`](CreativeOpportunitySlot::gam_unit_path)
    /// configurations remain legacy-compatible only when both this key and
    /// [`section_root`](Self::section_root) are omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_segment: Option<usize>,
    /// How per-user ad state reaches the page. Absent means
    /// [`AssemblyMode::Inline`], the shipped behaviour.
    ///
    /// `Option` rather than a bare enum, and `skip_serializing_if`, deliberately:
    /// these structs use `deny_unknown_fields`, so a pushed key makes an older
    /// binary fail configuration load. Keeping it absent when unset means a
    /// deployment that never sets it stays rollback-compatible.
    ///
    /// Spike-only. See [`AssemblyMode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly_mode: Option<AssemblyMode>,
    /// Request headers the origin varies on, which the shared-template cache key must
    /// cover.
    ///
    /// Operator-stated because a cache **lookup happens before the fetch**, so on a cold
    /// key the origin's `Vary` is not yet known. See `VarySpec` for why the alternatives
    /// (two-phase lookup, or storing the list and re-keying) were not taken.
    ///
    /// **Unset or empty means no operator-stated header is covered, so any origin
    /// `Vary` other than structurally covered `Accept-Encoding` disqualifies the
    /// response.** `Cookie` may never be configured: a per-cookie object violates the
    /// reader-neutral template contract. This fail-closed default prevents a deployment
    /// that has not stated what its origin varies on from gaining a shared cache by
    /// omission.
    ///
    /// Spike-only. Same `Option` + `skip_serializing_if` reasoning as `assembly_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_cache_vary: Option<Vec<String>>,
    /// Maximum time a reader-neutral transformed template may remain in the shared template cache.
    ///
    /// This is a safety ceiling, not freshness authorization. The origin must still
    /// provide positive shared freshness, and the stored lifetime is the smaller of
    /// the origin's remaining edge freshness and this value. Defaults to 60 seconds
    /// and may be configured from 1 second through 1 day.
    ///
    /// Spike-only. Same `Option` + `skip_serializing_if` reasoning as `assembly_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_cache_max_age_seconds: Option<u32>,
    /// Operator assertion that the origin's HTML does not depend on request cookies.
    ///
    /// Unset or `false` disqualifies **every cookie-bearing request** from the shared
    /// template cache, in both directions. That is safe and it is also very nearly a
    /// disable switch: Trusted Server sets its own identity cookie, so essentially every
    /// repeat visitor carries one. Left at the default, the cache can only ever serve
    /// first-ever page views and cookie-less clients.
    ///
    /// Setting `true` asserts the origin serves the same HTML with or without cookies.
    /// It is not taken on trust alone — if the origin ever declares `Vary: Cookie`, the
    /// response is refused regardless of this flag or the configured key. So a wrong
    /// assertion is caught whenever the origin is honest about it, and this only widens
    /// the window where the origin personalizes *silently*.
    ///
    /// Spike-only. Same `Option` + `skip_serializing_if` reasoning as `assembly_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_is_cookie_independent: Option<bool>,
    /// Slot templates. Empty vec = feature disabled (no auction fired, no globals injected).
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub slot: Vec<CreativeOpportunitySlot>,
}

impl CreativeOpportunitiesConfig {
    /// Resolved assembly mode, defaulting to [`AssemblyMode::Inline`] when unset.
    #[must_use]
    pub fn assembly_mode(&self) -> AssemblyMode {
        self.assembly_mode.unwrap_or_default()
    }

    /// Whether a cookie-bearing request may participate in the shared cache.
    ///
    /// Defaults to `false`, which is the conservative reading and also the one that
    /// makes the cache almost inert on real traffic. See
    /// [`Self::origin_is_cookie_independent`].
    #[must_use]
    pub fn origin_is_cookie_independent(&self) -> bool {
        self.origin_is_cookie_independent.unwrap_or(false)
    }

    /// Headers the cache key covers, per operator config.
    ///
    /// Unset yields an empty operator spec, so any origin `Vary` other than the
    /// structurally covered `Accept-Encoding` reads as a gap and the response is never
    /// cached. Failing closed is deliberate: an unconfigured deployment should not
    /// acquire a shared cache silently.
    #[must_use]
    pub fn template_cache_vary(&self) -> crate::platform::VarySpec {
        crate::platform::VarySpec::new(self.template_cache_vary.clone().unwrap_or_default())
    }

    /// Safety ceiling for one shared transformed-template cache entry.
    #[must_use]
    pub fn template_cache_max_age(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(
            self.template_cache_max_age_seconds
                .unwrap_or(DEFAULT_TEMPLATE_CACHE_MAX_AGE_SECONDS),
        ))
    }
    /// Derives the `{section}` value for `path` under this config's section
    /// policy ([`section_root`](Self::section_root) and
    /// [`section_segment`](Self::section_segment)).
    ///
    /// This keeps both policy knobs together so callers consistently apply the
    /// configured section-selection and fallback rules.
    ///
    /// An unset [`section_root`](Self::section_root) yields an empty section for
    /// a path with no matching segment. [`validate_runtime`](Self::validate_runtime)
    /// rejects that combination for any template that uses `{section}`, so it
    /// cannot reach a rendered unit path.
    #[must_use]
    pub fn section_for_path(&self, path: &str) -> String {
        derive_section(
            path,
            self.section_root.as_deref().unwrap_or_default(),
            self.section_segment.unwrap_or(0),
        )
    }

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
    /// Returns an error string when any slot's template is malformed. During
    /// typed/startup finalization, after all templates parse successfully,
    /// materializes `section_segment = Some(0)` for a placeholder-bearing
    /// template that omitted it, so rollback to an older `deny_unknown_fields`
    /// binary fails loudly.
    pub fn compile_unit_templates(&mut self) -> Result<(), String> {
        for slot in &mut self.slot {
            slot.compile_unit_template()?;
        }
        if self.section_segment.is_none()
            && self
                .slot
                .iter()
                .any(CreativeOpportunitySlot::template_is_dynamic)
        {
            self.section_segment = Some(0);
        }
        Ok(())
    }

    /// Validate all slot definitions after runtime preparation.
    ///
    /// Call [`compile_unit_templates`](Self::compile_unit_templates) first so
    /// malformed templates fail at startup. When the cache is absent, validation
    /// also reads a valid raw template so placeholder-dependent requirements are
    /// still enforced; compilation remains required to reject malformed raw
    /// templates. [`Settings::prepare_runtime`](crate::settings::Settings::prepare_runtime)
    /// enforces this order.
    ///
    /// # Errors
    ///
    /// Returns an error string when [`gam_network_id`](Self::gam_network_id) is
    /// blank but consumed by a default path or `{network_id}` template; when a
    /// slot has an invalid identifier, page pattern set, format list, or
    /// dimensions; when `template_cache_max_age_seconds` falls outside 1–86,400;
    /// when a `{section}` template lacks a valid
    /// [`section_root`](Self::section_root); or when configured values make a
    /// dynamic path exceed 100 UTF-8 bytes.
    pub fn validate_runtime(&self) -> Result<(), String> {
        if self
            .template_cache_max_age_seconds
            .is_some_and(|seconds| !(1..=MAX_TEMPLATE_CACHE_MAX_AGE_SECONDS).contains(&seconds))
        {
            return Err(format!(
                "template_cache_max_age_seconds must be between 1 and {MAX_TEMPLATE_CACHE_MAX_AGE_SECONDS}"
            ));
        }

        if let Some(names) = &self.template_cache_vary {
            crate::platform::VarySpec::try_new(names.clone()).map_err(|name| {
                format!("template_cache_vary contains invalid HTTP header name `{name}`")
            })?;
            if names.iter().any(|name| name.eq_ignore_ascii_case("cookie")) {
                return Err(
                    "template_cache_vary must not include Cookie; shared templates are reader-neutral"
                        .to_string(),
                );
            }
        }

        // A network ID is required only when a slot renders the default
        // `/<network_id>/<slot_id>` path or substitutes `{network_id}`. Static
        // and `{slot_id}`/`{section}`-only templates leave it inert.
        let network_id_consumed = self
            .slot
            .iter()
            .any(|slot| slot.gam_unit_path.is_none() || slot.template_uses_network_id());
        if network_id_consumed && self.gam_network_id.trim().is_empty() {
            return Err("gam_network_id must not be empty".to_string());
        }

        for slot in &self.slot {
            slot.validate_runtime()?;
        }

        if self
            .slot
            .iter()
            .any(CreativeOpportunitySlot::template_uses_section)
        {
            match self.section_root.as_deref() {
                Some(root) if !root.is_empty() && root.chars().all(is_section_char) => {}
                _ => {
                    return Err("section_root is required and must match [A-Za-z0-9_-]+ \
                                when a gam_unit_path template uses {section}"
                        .to_string());
                }
            }
        }

        let configured_section = self.section_root.as_deref().unwrap_or_default();
        for slot in &self.slot {
            if slot.template_is_dynamic()
                && slot
                    .render_gam_unit_path(&self.gam_network_id, configured_section)
                    .is_none()
            {
                return Err(format!(
                    "slot `{}` dynamic gam_unit_path must render to at most \
                     {MAX_DYNAMIC_GAM_UNIT_PATH_BYTES} UTF-8 bytes using configured values",
                    slot.id
                ));
            }
            if slot.providers.aps.is_some() {
                log::warn!(
                    "creative opportunity slot '{}': providers.aps is retained only for configuration compatibility and is ignored by APS OpenRTB",
                    slot.id
                );
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
    /// `None` means *not compiled* — either the slot has no explicit
    /// `gam_unit_path`, or it was deserialized/built without running
    /// [`CreativeOpportunitiesConfig::compile_unit_templates`]. Callers must
    /// therefore fall back to [`gam_unit_path`](Self::gam_unit_path) rather than
    /// treating `None` as "no template"; see
    /// [`render_gam_unit_path`](Self::render_gam_unit_path).
    ///
    /// `pub(crate)` so cross-module test helpers can build slots via
    /// struct-literal syntax with an empty cache.
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
            return Err(format!(
                "slot `{}` gam_unit_path must not be empty",
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

    /// Parses [`gam_unit_path`](Self::gam_unit_path) into
    /// [`compiled_unit`](Self::compiled_unit). Call once at startup via
    /// [`CreativeOpportunitiesConfig::compile_unit_templates`].
    ///
    /// # Errors
    ///
    /// Returns an error string (prefixed with the slot id) when the template is
    /// malformed. See [`parse_unit_template`].
    pub(crate) fn compile_unit_template(&mut self) -> Result<(), String> {
        self.compiled_unit = match &self.gam_unit_path {
            Some(raw) => {
                Some(parse_unit_template(raw).map_err(|e| format!("slot `{}`: {e}", self.id))?)
            }
            None => None,
        };
        Ok(())
    }

    fn template_is_dynamic(&self) -> bool {
        let is_dynamic =
            |parts: &[UnitTemplatePart]| parts.iter().any(UnitTemplatePart::is_placeholder);
        match (&self.compiled_unit, &self.gam_unit_path) {
            (Some(parts), _) => is_dynamic(parts),
            (None, Some(raw)) => parse_unit_template(raw).is_ok_and(|parts| is_dynamic(&parts)),
            (None, None) => false,
        }
    }

    /// Renders the resolved GAM unit path for a given network id and section.
    ///
    /// Substitutes `{network_id}`, `{section}`, and `{slot_id}` in the parsed
    /// template. Falls back to `/<network_id>/<id>` only when the slot has no
    /// [`gam_unit_path`](Self::gam_unit_path) at all.
    ///
    /// Returns `None` when a dynamic template would render beyond the 100-byte
    /// GAM unit-path limit. Explicit static paths and the default path retain
    /// their pre-template behavior and are not subject to this dynamic limit.
    ///
    /// This is the path-aware replacement for the pre-templating
    /// `resolved_gam_unit_path(&self, gam_network_id)`.
    ///
    /// # Performance
    ///
    /// The hot path reads the [`compiled_unit`](Self::compiled_unit) cache. A
    /// slot with an explicit `gam_unit_path` but no cache (built by hand, or
    /// deserialized without [`CreativeOpportunitiesConfig::compile_unit_templates`])
    /// re-parses its template on every call — same fallback shape as
    /// [`matches_path`](Self::matches_path). It must never silently degrade to
    /// the default path, which would bid against the wrong inventory.
    /// Dynamic templates compute their exact UTF-8 byte length with checked
    /// arithmetic before allocating the final string, then allocate once at
    /// the exact capacity.
    #[must_use]
    pub fn render_gam_unit_path(&self, gam_network_id: &str, section: &str) -> Option<String> {
        let is_dynamic =
            |parts: &[UnitTemplatePart]| parts.iter().any(UnitTemplatePart::is_placeholder);
        match (&self.compiled_unit, &self.gam_unit_path) {
            (Some(parts), _) if is_dynamic(parts) => {
                render_dynamic_unit_path(parts, gam_network_id, section, &self.id)
            }
            (Some(_), Some(raw)) => Some(raw.clone()),
            (Some(parts), None) => Some(
                parts
                    .iter()
                    .map(|part| {
                        resolved_unit_template_part(part, gam_network_id, section, &self.id)
                    })
                    .collect(),
            ),
            // A malformed template cannot reach a compiled config (startup
            // rejects it), so on this path use the raw string verbatim — the
            // pre-templating behaviour — instead of dropping to the default.
            (None, Some(raw)) => match parse_unit_template(raw) {
                Ok(parts) if is_dynamic(&parts) => {
                    render_dynamic_unit_path(&parts, gam_network_id, section, &self.id)
                }
                Ok(_) | Err(_) => Some(raw.clone()),
            },
            (None, None) => Some(format!("/{}/{}", gam_network_id, self.id)),
        }
    }

    /// Returns `true` if this slot's `gam_unit_path` template contains `{section}`.
    ///
    /// Reads the raw template when [`compiled_unit`](Self::compiled_unit) is
    /// empty so validation cannot silently skip the
    /// [`section_root`](CreativeOpportunitiesConfig::section_root) requirement
    /// for an uncompiled config.
    #[must_use]
    pub(crate) fn template_uses_section(&self) -> bool {
        let uses_section = |parts: &[UnitTemplatePart]| {
            parts.iter().any(|p| matches!(p, UnitTemplatePart::Section))
        };
        match (&self.compiled_unit, &self.gam_unit_path) {
            (Some(parts), _) => uses_section(parts),
            (None, Some(raw)) => parse_unit_template(raw).is_ok_and(|parts| uses_section(&parts)),
            (None, None) => false,
        }
    }

    /// Returns `true` if this slot's `gam_unit_path` template contains `{network_id}`.
    ///
    /// Reads the raw template when [`compiled_unit`](Self::compiled_unit) is
    /// empty so validation cannot silently skip the network ID requirement for
    /// an uncompiled config.
    fn template_uses_network_id(&self) -> bool {
        let uses_network_id = |parts: &[UnitTemplatePart]| {
            parts
                .iter()
                .any(|part| matches!(part, UnitTemplatePart::NetworkId))
        };
        match (&self.compiled_unit, &self.gam_unit_path) {
            (Some(parts), _) => uses_network_id(parts),
            (None, Some(raw)) => {
                parse_unit_template(raw).is_ok_and(|parts| uses_network_id(&parts))
            }
            (None, None) => false,
        }
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
    /// Prebid Server bidder params are wired into the `bidders` map keyed by
    /// bidder name. Legacy APS slot params are accepted in configuration but
    /// intentionally ignored by the APS `OpenRTB` provider.
    ///
    /// When [`PrebidSlotParams::bidders`] is empty, a `trustedServer` entry is
    /// injected so [`PrebidAuctionProvider`] expands all `config.bidders`
    /// automatically. The slot's `targeting.zone` value is forwarded as
    /// `trustedServer.zone` so zone-aware bid-param override rules fire correctly.
    #[must_use]
    pub fn to_ad_slot(&self) -> AdSlot {
        let mut bidders: HashMap<String, serde_json::Value> = HashMap::new();
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
    /// Legacy APS slot parameters, retained only for configuration compatibility.
    ///
    /// APS `OpenRTB` uses the canonical creative-opportunity slot ID and does not
    /// forward this value to APS or Prebid Server.
    pub aps: Option<ApsSlotParams>,
    /// Prebid Server inline bidder parameters.
    ///
    /// When present, these are forwarded directly as `ext.prebid.bidder.*`
    /// in the `OpenRTB` request, bypassing PBS stored request lookup for this slot.
    /// Useful in development environments where stored requests are not available.
    pub prebid: Option<PrebidSlotParams>,
}

/// Legacy APS-specific parameters retained for configuration compatibility.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApsSlotParams {
    /// Deprecated legacy slot ID. APS `OpenRTB` ignores this value.
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
    fn resolved_div_id_defaults_to_slot_id() {
        let slot = make_slot("atf", vec!["/"]);
        assert_eq!(slot.resolved_div_id(), "atf");
    }

    #[test]
    fn parse_unit_template_accepts_known_placeholders() {
        let parts = parse_unit_template("/{network_id}/example/{section}")
            .expect("should parse valid template");
        assert_eq!(parts.len(), 4, "should split into literal+ph+literal+ph");
    }

    #[test]
    fn parse_unit_template_accepts_static_path() {
        let parts = parse_unit_template("/99999/example/homepage")
            .expect("should parse a static path as a single literal");
        assert!(
            matches!(parts.as_slice(), [UnitTemplatePart::Literal(s)] if s == "/99999/example/homepage"),
            "should be one literal part"
        );
    }

    #[test]
    fn parse_unit_template_rejects_unknown_placeholder() {
        let err = parse_unit_template("/{network_id}/{oops}")
            .expect_err("should reject unknown placeholder");
        assert!(
            err.contains("oops"),
            "error should name the bad placeholder"
        );
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
        assert_eq!(derive_section("/news", "home", 0), "news");
        assert_eq!(derive_section("/news/article-123", "home", 0), "news");
        assert_eq!(derive_section("/my-section/x", "home", 0), "my-section");
    }

    #[test]
    fn derive_section_uses_configured_segment_index() {
        // A locale-prefixed site sets section_segment = 1.
        assert_eq!(derive_section("/en/news/article", "home", 1), "news");
        assert_eq!(derive_section("/en/news", "home", 1), "news");
        // Repeated separators are not counted as segments.
        assert_eq!(derive_section("//en//news//x", "home", 1), "news");
    }

    #[test]
    fn derive_section_uses_root_when_segment_index_out_of_range() {
        // Section landing page of a locale-prefixed site: no segment 1 exists,
        // so the root value stands in rather than reusing the locale.
        assert_eq!(derive_section("/en", "home", 1), "home");
        assert_eq!(derive_section("/", "home", 1), "home");
    }

    #[test]
    fn derive_section_uses_root_when_no_segment() {
        assert_eq!(derive_section("/", "homepage", 0), "homepage");
        assert_eq!(derive_section("///", "homepage", 0), "homepage");
    }

    #[test]
    fn derive_section_sanitizes_unsafe_runs_to_single_underscore() {
        // Not decoded: in "new%20s" only '%' is disallowed ('2' and '0' are
        // alphanumeric), so it collapses to a single '_' -> "new_20s". This is
        // exactly the no-decode contract: had we decoded, %20 would be a space
        // and yield "new_s"; we do NOT decode.
        assert_eq!(derive_section("/new%20s", "home", 0), "new_20s");
        // A run of disallowed chars collapses to one '_'.
        assert_eq!(derive_section("/a..b", "home", 0), "a_b");
    }

    #[test]
    fn derive_section_caps_safe_segment_at_one_hundred_ascii_bytes() {
        let path = format!("/{}", "a".repeat(150));

        let section = derive_section(&path, "home", 0);

        assert_eq!(
            section,
            "a".repeat(100),
            "should cap a safe request segment at 100 ASCII bytes"
        );
        assert!(section.is_ascii(), "section output should remain ASCII");
        assert_eq!(
            section.len(),
            100,
            "section should contain exactly 100 bytes"
        );
    }

    #[test]
    fn derive_section_caps_disallowed_run_to_one_underscore() {
        let path = format!("/{}%!?z", "a".repeat(99));

        let section = derive_section(&path, "home", 0);

        assert_eq!(
            section,
            format!("{}_", "a".repeat(99)),
            "a disallowed run at the cap should emit one underscore and stop"
        );
        assert_eq!(section.len(), 100, "section should stop at the byte cap");
        assert!(
            !section.contains('z'),
            "a safe character beyond the cap should not leak into the section"
        );
    }

    #[test]
    fn derive_section_stops_before_disallowed_run_when_cap_is_full() {
        let path = format!("/{}%!?z", "a".repeat(100));

        let section = derive_section(&path, "home", 0);

        assert_eq!(
            section,
            "a".repeat(100),
            "a full safe prefix should prevent scanning or emitting the later run"
        );
        assert!(
            !section.contains('_') && !section.contains('z'),
            "nothing beyond the full safe prefix should be emitted"
        );
    }

    #[test]
    fn section_for_path_applies_both_policy_knobs() {
        let mut config = make_config_with_section_template(Some("home"));
        assert_eq!(
            config.section_for_path("/en/news/article"),
            "en",
            "should default to the first segment when section_segment is unset"
        );

        config.section_segment = Some(1);
        assert_eq!(
            config.section_for_path("/en/news/article"),
            "news",
            "should honour the configured segment index"
        );
        assert_eq!(
            config.section_for_path("/en"),
            "home",
            "should fall back to section_root when the index is out of range"
        );
    }

    #[test]
    fn section_segment_is_omitted_from_serialized_config_when_unset() {
        // Same rollback contract as section_root: `deny_unknown_fields` on the
        // previous binary rejects a blob carrying keys it does not know.
        let config = make_config_with_section_template(None);
        let value = serde_json::to_value(&config).expect("should serialize config");
        assert!(
            value.get("section_segment").is_none(),
            "unset section_segment should not be serialized, got {value}"
        );
    }

    #[test]
    fn derive_section_is_non_empty_for_all_disallowed_segment() {
        assert_eq!(derive_section("/%%%/x", "home", 0), "_");
    }

    fn make_config_with_section_template(
        section_root: Option<&str>,
    ) -> CreativeOpportunitiesConfig {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
        CreativeOpportunitiesConfig {
            gam_network_id: "99999".to_string(),
            auction_timeout_ms: None,
            price_granularity: PriceGranularity::default(),
            section_root: section_root.map(str::to_string),
            assembly_mode: None,
            template_cache_vary: None,
            template_cache_max_age_seconds: None,
            origin_is_cookie_independent: None,
            section_segment: None,
            slot: vec![slot],
        }
    }

    #[test]
    fn render_gam_unit_path_substitutes_placeholders() {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
        slot.compile_unit_template()
            .expect("should compile template");
        assert_eq!(
            slot.render_gam_unit_path("99999", "news"),
            Some("/99999/example/news".to_string())
        );
    }

    #[test]
    fn render_gam_unit_path_omits_over_limit_compiled_dynamic_template() {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{section}/{section}".to_string());
        slot.compile_unit_template()
            .expect("should compile template");

        let rendered = slot.render_gam_unit_path("99999", &"a".repeat(60));

        assert_eq!(
            rendered, None,
            "a compiled dynamic path over 100 bytes should be omitted"
        );
    }

    #[test]
    fn render_gam_unit_path_omits_over_limit_raw_dynamic_template() {
        let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
        slot.gam_unit_path = Some("/{section}/{section}".to_string());
        assert!(
            slot.compiled_unit.is_none(),
            "test should exercise the raw parsing fallback"
        );

        let rendered = slot.render_gam_unit_path("99999", &"a".repeat(60));

        assert_eq!(
            rendered, None,
            "a raw dynamic path over 100 bytes should be omitted"
        );
    }

    #[test]
    fn render_gam_unit_path_accepts_exact_multibyte_byte_limit() {
        let expected = format!("{}ax", "é".repeat(49));
        assert_eq!(
            expected.len(),
            100,
            "test fixture should render to exactly 100 UTF-8 bytes"
        );

        for compile_template in [true, false] {
            let cache_kind = if compile_template { "compiled" } else { "raw" };
            let mut slot = make_slot("x", vec!["/"]);
            slot.gam_unit_path = Some(format!("{}a{{slot_id}}", "é".repeat(49)));
            if compile_template {
                slot.compile_unit_template()
                    .expect("should compile multibyte template");
            }
            assert_eq!(
                slot.compiled_unit.is_some(),
                compile_template,
                "test should exercise the {cache_kind} template path"
            );

            let rendered = slot.render_gam_unit_path("unused", "unused");

            assert_eq!(
                rendered,
                Some(expected.clone()),
                "{cache_kind} dynamic template should accept exactly 100 UTF-8 bytes"
            );
        }
    }

    #[test]
    fn render_gam_unit_path_rejects_multibyte_byte_limit_plus_one() {
        let hypothetical_render = format!("{}abx", "é".repeat(49));
        assert_eq!(
            hypothetical_render.len(),
            101,
            "test fixture should render to 101 UTF-8 bytes"
        );
        assert!(
            hypothetical_render.chars().count() < 100,
            "fixture should fail if rendering counts characters instead of UTF-8 bytes"
        );

        for compile_template in [true, false] {
            let cache_kind = if compile_template { "compiled" } else { "raw" };
            let mut slot = make_slot("x", vec!["/"]);
            slot.gam_unit_path = Some(format!("{}ab{{slot_id}}", "é".repeat(49)));
            if compile_template {
                slot.compile_unit_template()
                    .expect("should compile multibyte template");
            }
            assert_eq!(
                slot.compiled_unit.is_some(),
                compile_template,
                "test should exercise the {cache_kind} template path"
            );

            let rendered = slot.render_gam_unit_path("unused", "unused");

            assert_eq!(
                rendered, None,
                "{cache_kind} dynamic template should reject 101 UTF-8 bytes"
            );
        }
    }

    #[test]
    fn render_gam_unit_path_defaults_when_no_template() {
        let mut slot = make_slot("sidebar", vec!["/*"]);
        slot.gam_unit_path = None;
        slot.compile_unit_template()
            .expect("should compile (no template)");
        assert_eq!(
            slot.render_gam_unit_path("99999", "ignored"),
            Some("/99999/sidebar".to_string()),
            "an absent template should retain the default path behavior"
        );
    }

    #[test]
    fn render_gam_unit_path_uses_static_template_verbatim() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some("/99999/example/homepage".to_string());
        slot.compile_unit_template()
            .expect("should compile static template");
        assert_eq!(
            slot.render_gam_unit_path("99999", "news"),
            Some("/99999/example/homepage".to_string())
        );
    }

    #[test]
    fn render_gam_unit_path_preserves_over_limit_static_template() {
        let static_path = format!("/{}", "a".repeat(100));
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some(static_path.clone());
        slot.compile_unit_template()
            .expect("should compile static template");

        let rendered = slot.render_gam_unit_path("99999", "news");

        assert_eq!(
            rendered,
            Some(static_path),
            "an explicit static path should retain pre-template behavior"
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
        assert!(
            err.contains("section_root"),
            "error should mention section_root"
        );
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
    fn validate_runtime_rejects_dynamic_template_over_limit_with_configured_root() {
        let root = "a".repeat(60);
        let mut config = make_config_with_section_template(Some(&root));
        config.slot[0].gam_unit_path = Some("/{section}/{section}".to_string());
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("templates should compile");

        let err = config
            .validate_runtime()
            .expect_err("should reject a configured dynamic path over 100 bytes");

        assert!(
            err.contains("ad-header-0"),
            "error should identify the over-limit slot, got: {err}"
        );
        assert!(
            err.contains("100"),
            "error should identify the dynamic path byte limit, got: {err}"
        );
    }

    #[test]
    fn render_gam_unit_path_honours_raw_template_without_compiled_cache() {
        // A slot deserialized straight from JSON (or built by a test helper)
        // never ran `compile_unit_templates`. It must still render its explicit
        // path — dropping to `/<network>/<id>` would bid the wrong inventory.
        let slot: CreativeOpportunitySlot = serde_json::from_value(serde_json::json!({
            "id": "ad-header-0",
            "gam_unit_path": "/{network_id}/example/{section}",
            "page_patterns": ["/news/*"],
            "formats": [{ "width": 728, "height": 90 }],
        }))
        .expect("should deserialize slot");
        assert!(
            slot.compiled_unit.is_none(),
            "direct deserialization should leave the template cache empty"
        );
        assert_eq!(
            slot.render_gam_unit_path("99999", "news"),
            Some("/99999/example/news".to_string()),
            "uncompiled slot should still substitute placeholders"
        );
    }

    #[test]
    fn render_gam_unit_path_honours_static_path_without_compiled_cache() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some("/99999/example/homepage".to_string());
        assert_eq!(
            slot.render_gam_unit_path("99999", "news"),
            Some("/99999/example/homepage".to_string()),
            "uncompiled static path should render verbatim, not the default"
        );
    }

    #[test]
    fn render_gam_unit_path_preserves_malformed_raw_template() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.gam_unit_path = Some("/{unknown}".to_string());

        let rendered = slot.render_gam_unit_path("99999", "news");

        assert_eq!(
            rendered,
            Some("/{unknown}".to_string()),
            "a malformed raw template should retain direct-caller compatibility"
        );
    }

    #[test]
    fn validate_runtime_requires_section_root_for_uncompiled_template() {
        // `template_uses_section` must read the raw template, otherwise an
        // uncompiled config silently skips the section_root requirement.
        let mut config = make_config_with_section_template(None);
        config.compile_slots();
        assert!(
            config.slot[0].compiled_unit.is_none(),
            "test precondition: template cache is empty"
        );
        let err = config
            .validate_runtime()
            .expect_err("should require section_root even without compiled templates");
        assert!(
            err.contains("section_root"),
            "error should mention section_root"
        );
    }

    #[test]
    fn validate_runtime_allows_blank_network_id_with_static_paths() {
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = Some("/12345/example/homepage".to_string());
        config.gam_network_id = "   ".to_string();
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("should compile static template");
        config
            .validate_runtime()
            .expect("should allow a blank unused network id");
    }

    #[test]
    fn validate_runtime_allows_blank_network_id_with_slot_id_template() {
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = Some("/example/{slot_id}".to_string());
        config.gam_network_id = String::new();
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("should compile slot-id template");
        config
            .validate_runtime()
            .expect("should allow a blank unused network id");
    }

    #[test]
    fn validate_runtime_rejects_blank_network_id_when_default_path_uses_it() {
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = None;
        config.gam_network_id = String::new();
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("should compile default path");
        let err = config
            .validate_runtime()
            .expect_err("blank network id should fail when the default path uses it");
        assert_eq!(
            err, "gam_network_id must not be empty",
            "should report the blank network id"
        );
    }

    #[test]
    fn validate_runtime_rejects_blank_network_id_when_compiled_template_uses_it() {
        // `gam_unit_path = "{network_id}"` renders to an empty string with a
        // blank network id, which reaches googletag.defineSlot as an invalid path.
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = Some("{network_id}".to_string());
        config.gam_network_id = String::new();
        config.compile_slots();
        config
            .compile_unit_templates()
            .expect("templates should compile");
        let err = config
            .validate_runtime()
            .expect_err("blank gam_network_id should fail startup validation");
        assert!(
            err.contains("gam_network_id"),
            "error should name gam_network_id, got: {err}"
        );
    }

    #[test]
    fn validate_runtime_rejects_blank_network_id_when_raw_template_uses_it() {
        let mut config = make_config_with_section_template(Some("home"));
        config.slot[0].gam_unit_path = Some("/{network_id}/example".to_string());
        config.gam_network_id = String::new();
        config.compile_slots();
        assert!(
            config.slot[0].compiled_unit.is_none(),
            "test precondition: template cache is empty"
        );
        let err = config
            .validate_runtime()
            .expect_err("blank network id should fail when a raw template uses it");
        assert_eq!(
            err, "gam_network_id must not be empty",
            "should report the blank network id"
        );
    }

    #[test]
    fn validate_runtime_allows_blank_network_id_when_no_slots_configured() {
        // An empty slot list disables the feature, so the id is never rendered.
        // Failing startup there would break a deploy over an unused value.
        let mut config = make_config_with_section_template(Some("home"));
        config.gam_network_id = String::new();
        config.slot.clear();
        config
            .validate_runtime()
            .expect("a disabled creative_opportunities stack should not fail on a blank id");
    }

    #[test]
    fn section_root_is_omitted_from_serialized_config_when_unset() {
        // Older binaries deserialize this struct with `deny_unknown_fields`, so
        // a pushed config blob must not carry `"section_root": null`.
        let config = CreativeOpportunitiesConfig {
            gam_network_id: "99999".to_string(),
            auction_timeout_ms: None,
            price_granularity: PriceGranularity::default(),
            section_root: None,
            assembly_mode: None,
            template_cache_vary: None,
            template_cache_max_age_seconds: None,
            origin_is_cookie_independent: None,
            section_segment: None,
            slot: Vec::new(),
        };
        let value = serde_json::to_value(&config).expect("should serialize config");
        assert!(
            value.get("section_root").is_none(),
            "unset section_root should not be serialized, got {value}"
        );

        let with_root = CreativeOpportunitiesConfig {
            section_root: Some("home".to_string()),
            ..config
        };
        assert_eq!(
            serde_json::to_value(&with_root)
                .expect("should serialize config")
                .get("section_root")
                .and_then(serde_json::Value::as_str),
            Some("home"),
            "a set section_root should still round-trip"
        );
    }

    #[test]
    fn documented_page_patterns_match_and_render_their_documented_paths() {
        // Mirrors the example in docs/guide/configuration.md. `/news/*` alone
        // does NOT match `/news` (the glob needs the trailing separator), so the
        // documented config must list the section landing pages explicitly.
        let mut slot = make_slot(
            "ad-header",
            vec!["/", "/news", "/news/*", "/reviews", "/reviews/*"],
        );
        slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
        slot.compile_patterns();
        slot.compile_unit_template()
            .expect("should compile template");
        let slots = vec![slot];

        for (path, expected) in [
            ("/", "/123456789/example/home"),
            ("/news", "/123456789/example/news"),
            ("/news/article", "/123456789/example/news"),
            ("/reviews/x", "/123456789/example/reviews"),
        ] {
            let matched = match_slots(&slots, path);
            assert_eq!(
                matched.len(),
                1,
                "`{path}` should match the documented slot"
            );
            assert_eq!(
                matched[0].render_gam_unit_path("123456789", &derive_section(path, "home", 0)),
                Some(expected.to_string()),
                "`{path}` should render the documented unit path"
            );
        }
    }

    #[test]
    fn bare_section_pattern_does_not_match_without_trailing_separator() {
        // Guards the docs fix above: a `"/news/*"`-only config loses the section
        // landing page entirely.
        let mut slot = make_slot("ad-header", vec!["/news/*"]);
        slot.compile_patterns();
        assert!(
            !slot.matches_path("/news"),
            "`/news/*` must not match `/news`"
        );
        assert!(
            slot.matches_path("/news/article"),
            "`/news/*` should match descendants"
        );
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
    fn to_ad_slot_ignores_legacy_aps_params() {
        let mut slot = make_slot("atf", vec!["/"]);
        slot.providers.aps = Some(ApsSlotParams {
            slot_id: "legacy-aps-slot-atf".to_string(),
        });
        let ad_slot = slot.to_ad_slot();
        assert!(
            !ad_slot.bidders.contains_key("aps"),
            "legacy APS params must not enable APS through Prebid Server"
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
    fn assembly_mode_defaults_to_inline_when_absent() {
        // Arrange: the minimal config an existing deployment would have.
        let toml = r#"
            gam_network_id = "99999"
        "#;

        // Act
        let config: CreativeOpportunitiesConfig =
            toml::from_str(toml).expect("should deserialize without assembly_mode");

        // Assert
        assert_eq!(
            config.assembly_mode, None,
            "an absent key should stay absent rather than materializing a value"
        );
        assert_eq!(
            config.assembly_mode(),
            AssemblyMode::Inline,
            "should resolve to the shipped inline behaviour"
        );
    }

    #[test]
    fn assembly_mode_deserializes_each_variant() {
        for (raw, expected) in [("inline", AssemblyMode::Inline), ("esi", AssemblyMode::Esi)] {
            let toml = format!(
                r#"
                    gam_network_id = "99999"
                    assembly_mode = "{raw}"
                "#
            );
            let config: CreativeOpportunitiesConfig =
                toml::from_str(&toml).unwrap_or_else(|e| panic!("should parse {raw}: {e}"));
            assert_eq!(
                config.assembly_mode(),
                expected,
                "should resolve `{raw}` to {expected:?}"
            );
        }

        let removed_mode = r#"
            gam_network_id = "99999"
            assembly_mode = "client_fill"
        "#;
        assert!(
            toml::from_str::<CreativeOpportunitiesConfig>(removed_mode).is_err(),
            "client_fill is outside #1009's ESI byte-seam design and must be rejected"
        );
    }

    #[test]
    fn template_cache_vary_rejects_invalid_header_names() {
        let config: CreativeOpportunitiesConfig = toml::from_str(
            r#"
                gam_network_id = "99999"
                template_cache_vary = ["rsc", "not a header"]
            "#,
        )
        .expect("shape should deserialize before runtime validation");
        let err = config
            .validate_runtime()
            .expect_err("invalid field names must fail configuration validation");
        assert!(err.contains("not a header"), "unexpected error: {err}");

        let cookie_key: CreativeOpportunitiesConfig = toml::from_str(
            r#"
                gam_network_id = "99999"
                template_cache_vary = ["Cookie"]
            "#,
        )
        .expect("shape should deserialize before runtime validation");
        let err = cookie_key
            .validate_runtime()
            .expect_err("per-cookie templates violate the reader-neutral shared-template contract");
        assert!(err.contains("Cookie"), "unexpected error: {err}");
    }

    #[test]
    fn template_cache_max_age_accepts_a_positive_value_up_to_one_day() {
        for seconds in [1_u32, 1_200, 86_400] {
            let config: CreativeOpportunitiesConfig = toml::from_str(&format!(
                r#"
                    gam_network_id = "99999"
                    template_cache_max_age_seconds = {seconds}
                "#
            ))
            .unwrap_or_else(|error| panic!("{seconds}s should deserialize: {error}"));

            config
                .validate_runtime()
                .unwrap_or_else(|error| panic!("{seconds}s should validate: {error}"));
            let serialized = serde_json::to_value(config).expect("should serialize config");
            assert_eq!(
                serialized
                    .get("template_cache_max_age_seconds")
                    .and_then(serde_json::Value::as_u64),
                Some(u64::from(seconds)),
                "the configured ceiling must survive typed configuration"
            );
        }
    }

    #[test]
    fn template_cache_max_age_rejects_zero_and_more_than_one_day() {
        for seconds in [0_u32, 86_401] {
            let config: CreativeOpportunitiesConfig = toml::from_str(&format!(
                r#"
                    gam_network_id = "99999"
                    template_cache_max_age_seconds = {seconds}
                "#
            ))
            .unwrap_or_else(|error| panic!("shape should deserialize before validation: {error}"));

            let error = config
                .validate_runtime()
                .expect_err("an unsafe template-cache ceiling must fail startup validation");
            assert!(
                error.contains("template_cache_max_age_seconds"),
                "unexpected validation error: {error}"
            );
        }
    }

    #[test]
    fn unset_template_cache_max_age_is_omitted_for_rollback_compatibility() {
        let config: CreativeOpportunitiesConfig =
            toml::from_str("gam_network_id = \"99999\"").expect("should deserialize");

        assert_eq!(
            config.template_cache_max_age(),
            std::time::Duration::from_secs(60),
            "an absent ceiling must preserve the spike's existing lifetime"
        );
        let serialized = toml::to_string(&config).expect("should serialize");

        assert!(
            !serialized.contains("template_cache_max_age_seconds"),
            "an unset new key must not break rollback to an older binary: {serialized}"
        );
    }

    #[test]
    fn unset_assembly_mode_is_omitted_from_serialized_config() {
        // `deny_unknown_fields` means a pushed key breaks config load on an older
        // binary. A deployment that never sets this must not gain the key just by
        // round-tripping through a newer one.
        let config: CreativeOpportunitiesConfig =
            toml::from_str("gam_network_id = \"99999\"").expect("should deserialize");

        let serialized = toml::to_string(&config).expect("should serialize");

        assert!(
            !serialized.contains("assembly_mode"),
            "unset assembly_mode must not be serialized, got:\n{serialized}"
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
