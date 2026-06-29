//! Build-time validation for creative-opportunity slot definitions.
//!
//! This module is compiled in two contexts:
//! - by `build.rs` (via `#[path]`), which runs it against the raw slot JSON
//!   merged from `trusted-server.toml` and `TRUSTED_SERVER__*` env overrides
//!   before the config is embedded into the binary;
//! - by the crate's test build (via `#[cfg(test)] mod`), so the rules below are
//!   exercised under `cargo test`.
//!
//! It mirrors the runtime validator
//! (`CreativeOpportunitySlot::validate_runtime`) so an invalid slot fails the
//! build instead of surfacing as a request-time configuration error. It reads
//! raw JSON (not the typed runtime struct) because the typed slot vec is
//! intentionally empty in the build context, keeping `build.rs` free of the
//! full runtime dependency graph.

/// Top-level slot fields the runtime [`CreativeOpportunitySlot`] accepts.
///
/// The runtime struct is `#[serde(deny_unknown_fields)]`, but the build context
/// deserializes slots as raw `serde_json::Value`, which silently keeps unknown
/// keys. Mirror the runtime field set here so an env-injected typo or stray key
/// fails the build instead of failing settings load on every request.
///
/// `compiled_patterns` is intentionally excluded: it is `#[serde(skip)]` on the
/// runtime struct and is never a valid input field.
///
/// [`CreativeOpportunitySlot`]: crate::creative_opportunities::CreativeOpportunitySlot
const ALLOWED_SLOT_FIELDS: &[&str] = &[
    "id",
    "gam_unit_path",
    "div_id",
    "page_patterns",
    "formats",
    "floor_price",
    "targeting",
    "providers",
];

/// Fields the runtime [`CreativeOpportunityFormat`] accepts.
///
/// Mirrors the struct's `#[serde(deny_unknown_fields)]`; the build path
/// deserializes formats as raw JSON, so a typo like `mediatype` (for
/// `media_type`) would otherwise embed and fail runtime settings load.
///
/// [`CreativeOpportunityFormat`]: crate::creative_opportunities::CreativeOpportunityFormat
const ALLOWED_FORMAT_FIELDS: &[&str] = &["width", "height", "media_type"];

/// Provider keys the runtime [`SlotProviders`] accepts.
///
/// [`SlotProviders`]: crate::creative_opportunities::SlotProviders
const ALLOWED_PROVIDER_FIELDS: &[&str] = &["aps", "prebid"];

/// Fields the runtime [`ApsSlotParams`] accepts.
///
/// [`ApsSlotParams`]: crate::creative_opportunities::ApsSlotParams
const ALLOWED_APS_FIELDS: &[&str] = &["slot_id"];

/// Fields the runtime [`PrebidSlotParams`] accepts.
///
/// [`PrebidSlotParams`]: crate::creative_opportunities::PrebidSlotParams
const ALLOWED_PREBID_FIELDS: &[&str] = &["bidders"];

/// Rejects any key in `object` that is not in `allowed`, mirroring the runtime
/// struct's `#[serde(deny_unknown_fields)]`.
///
/// `context` names the offending object in the error (e.g. `` slot `atf`
/// format ``) so a build failure points at the exact config location.
fn reject_unknown_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    context: &str,
) -> Result<(), String> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("{context} has unknown field '{key}'"));
        }
    }
    Ok(())
}

/// Validate that `value` is a `price_granularity` the runtime can deserialize.
///
/// The build context types `price_granularity` as a `String`, so an invalid
/// value such as `custom` would embed cleanly and then fail runtime settings
/// load — the real [`PriceGranularity`] enum cannot deserialize it — on every
/// non-health request. Delegating to that enum's `Deserialize` impl keeps the
/// accepted set in lockstep with the runtime, avoiding drift.
///
/// # Errors
///
/// Returns an error string when `value` is not one of the runtime
/// [`PriceGranularity`] variants.
///
/// [`PriceGranularity`]: crate::price_bucket::PriceGranularity
pub(crate) fn validate_price_granularity(value: &str) -> Result<(), String> {
    serde_json::from_value::<crate::price_bucket::PriceGranularity>(serde_json::Value::String(
        value.to_string(),
    ))
    .map(|_| ())
    .map_err(|_| {
        format!(
            "price_granularity '{value}' is invalid; expected one of: low, medium, dense, high, auto"
        )
    })
}

/// Accepted `media_type` values, mirroring the runtime [`MediaType`] enum's
/// `#[serde(rename_all = "lowercase")]` variants.
///
/// The build path types a format's `media_type` as raw JSON, so a value such as
/// `"bannerr"` would embed cleanly and then fail runtime settings load — the real
/// [`MediaType`] enum cannot deserialize it. A crate-context test
/// (`media_type_values_match_runtime_enum`) asserts this list stays in lockstep
/// with the enum's `Deserialize` impl, so the two cannot drift.
///
/// [`MediaType`]: crate::auction::types::MediaType
const MEDIA_TYPE_VALUES: &[&str] = &["banner", "video", "native"];

/// Validate a format's `media_type` value against the runtime [`MediaType`] enum.
///
/// # Errors
///
/// Returns an error string when `value` is not a JSON string naming one of the
/// runtime [`MediaType`] variants.
///
/// [`MediaType`]: crate::auction::types::MediaType
fn validate_media_type(value: &serde_json::Value, slot_id: &str) -> Result<(), String> {
    let media_type = value
        .as_str()
        .ok_or_else(|| format!("slot `{slot_id}` format media_type must be a string"))?;
    if !MEDIA_TYPE_VALUES.contains(&media_type) {
        return Err(format!(
            "slot `{slot_id}` format media_type '{media_type}' is invalid; expected one of: banner, video, native"
        ));
    }
    Ok(())
}

/// Validate that `value` is a string→string map, mirroring a runtime
/// `HashMap<String, String>` field.
///
/// # Errors
///
/// Returns an error string when `value` is not a JSON object or any of its values
/// is not a JSON string. `context` names the offending field in the error.
fn validate_string_map(value: &serde_json::Value, context: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} must be a map of string keys to string values"))?;
    for (key, entry) in object {
        if !entry.is_string() {
            return Err(format!("{context} value for '{key}' must be a string"));
        }
    }
    Ok(())
}

/// Returns `true` when `id` is non-empty and only `[A-Za-z0-9_-]`.
fn is_valid_slot_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Returns `true` when `pattern` compiles as a glob, mirroring the runtime
/// `CreativeOpportunitySlot::compile_patterns` contract: try `glob::Pattern::new`
/// directly, then fall back to the `**` -> `*` normalization. A pattern that
/// fails both is dropped at runtime, leaving the slot unmatchable, so the build
/// must reject it too.
fn pattern_compiles(pattern: &str) -> bool {
    glob::Pattern::new(pattern)
        .or_else(|_| glob::Pattern::new(&pattern.replace("**", "*")))
        .is_ok()
}

/// Validate a single raw creative-opportunity slot.
///
/// Mirrors the runtime checks in `CreativeOpportunitySlot::validate_runtime`:
/// a syntactically safe non-empty id, at least one non-empty page pattern, at
/// least one format with positive dimensions, and a non-empty resolved GAM unit
/// path. Returns an error string describing the first problem found.
///
/// # Errors
///
/// Returns an error string when the slot is missing required fields, has an
/// invalid id, has no usable page pattern or format, has a zero-dimension
/// format, or resolves to an empty GAM unit path.
pub(crate) fn validate_creative_slot(
    slot: &serde_json::Value,
    gam_network_id: &str,
) -> Result<(), String> {
    let id = match slot.get("id").and_then(serde_json::Value::as_str) {
        Some(id) => id,
        None => return Err("a slot entry is missing the required 'id' field".to_string()),
    };
    if id.is_empty() {
        return Err("slot id must not be empty".to_string());
    }
    if !is_valid_slot_id(id) {
        return Err(format!(
            "slot id '{id}' is invalid; only [A-Za-z0-9_-] allowed"
        ));
    }

    // Reject unknown top-level keys, mirroring the runtime slot's
    // `#[serde(deny_unknown_fields)]`. The raw-JSON build path would otherwise
    // accept env-injected typos that the runtime rejects at settings load.
    if let Some(object) = slot.as_object() {
        reject_unknown_keys(object, ALLOWED_SLOT_FIELDS, &format!("slot `{id}`"))?;
    }

    // Reject nested unknown/mistyped fields too. The runtime's typed structs are
    // all `#[serde(deny_unknown_fields)]`, but the raw-JSON build path bypasses
    // those checks, so a config like `formats=[{width,height,mediatype}]` or
    // `providers={aps={slotId}}` would otherwise pass the build and fail runtime
    // settings load.
    if let Some(formats) = slot.get("formats").and_then(serde_json::Value::as_array) {
        for format in formats {
            if let Some(object) = format.as_object() {
                reject_unknown_keys(
                    object,
                    ALLOWED_FORMAT_FIELDS,
                    &format!("slot `{id}` format"),
                )?;
                // Validate the nested `media_type` value, not just the field
                // name: a value like `"bannerr"` passes the key check but the
                // runtime `MediaType` enum cannot deserialize it.
                if let Some(media_type) = object.get("media_type") {
                    validate_media_type(media_type, id)?;
                }
            }
        }
    }
    if let Some(providers) = slot.get("providers").and_then(serde_json::Value::as_object) {
        reject_unknown_keys(
            providers,
            ALLOWED_PROVIDER_FIELDS,
            &format!("slot `{id}` providers"),
        )?;
        if let Some(aps) = providers.get("aps").and_then(serde_json::Value::as_object) {
            reject_unknown_keys(
                aps,
                ALLOWED_APS_FIELDS,
                &format!("slot `{id}` providers.aps"),
            )?;
            // `ApsSlotParams::slot_id` is a `String`; a non-string value embeds
            // cleanly but fails runtime deserialization.
            if let Some(slot_id_value) = aps.get("slot_id") {
                if !slot_id_value.is_string() {
                    return Err(format!(
                        "slot `{id}` providers.aps.slot_id must be a string"
                    ));
                }
            }
        }
        if let Some(prebid) = providers
            .get("prebid")
            .and_then(serde_json::Value::as_object)
        {
            reject_unknown_keys(
                prebid,
                ALLOWED_PREBID_FIELDS,
                &format!("slot `{id}` providers.prebid"),
            )?;
            // `PrebidSlotParams::bidders` is a map; a non-object value (e.g. a
            // bare string or array) fails runtime deserialization.
            if let Some(bidders) = prebid.get("bidders") {
                if !bidders.is_object() {
                    return Err(format!(
                        "slot `{id}` providers.prebid.bidders must be a map of bidder names to params"
                    ));
                }
            }
        }
    }

    // `targeting` is a runtime `HashMap<String, String>`; a non-string value
    // (e.g. `targeting = { pos = 1 }`) embeds cleanly but fails settings load.
    if let Some(targeting) = slot.get("targeting") {
        validate_string_map(targeting, &format!("slot `{id}` targeting"))?;
    }

    // `floor_price` is an `Option<f64>`; a non-numeric value would fail the
    // runtime deserialization the build path otherwise bypasses.
    if let Some(floor_price) = slot.get("floor_price") {
        if !floor_price.is_null() && floor_price.as_f64().is_none() {
            return Err(format!("slot `{id}` floor_price must be a number"));
        }
    }

    // An explicit empty/whitespace `div_id` override is rejected, mirroring
    // `CreativeOpportunitySlot::validate_runtime`: the injected JS resolves slots
    // with `candidate.id.startsWith(slot.div_id)`, and every element id starts
    // with the empty string, so an empty override would bind the slot to the
    // first id-bearing element in the document.
    if let Some(div_id) = slot.get("div_id").and_then(serde_json::Value::as_str) {
        if div_id.trim().is_empty() {
            return Err(format!("slot `{id}` div_id override must not be empty"));
        }
    }

    // `page_patterns` is a runtime `Vec<String>`; a non-string entry (e.g.
    // `page_patterns = [123]`) fails deserialization. The validity check below
    // skips non-strings via `filter_map`, so reject them explicitly first.
    if let Some(patterns) = slot
        .get("page_patterns")
        .and_then(serde_json::Value::as_array)
    {
        if patterns.iter().any(|p| !p.is_string()) {
            return Err(format!("slot `{id}` page_patterns entries must be strings"));
        }
    }

    // At least one page pattern that is non-empty and compiles as a glob.
    // Runtime preparation drops uncompilable patterns and rejects the slot when
    // none remain, so a private/env config like `page_patterns = ["["]` would
    // otherwise pass the build and fail settings load on the deployed service.
    let has_valid_pattern = slot
        .get("page_patterns")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|patterns| {
            patterns
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|s| !s.trim().is_empty() && pattern_compiles(s))
        });
    if !has_valid_pattern {
        return Err(format!(
            "slot `{id}` must include at least one valid page pattern"
        ));
    }

    // At least one format, each with positive width and height.
    match slot.get("formats").and_then(serde_json::Value::as_array) {
        Some(formats) if !formats.is_empty() => {
            for format in formats {
                let width = format.get("width").and_then(serde_json::Value::as_u64);
                let height = format.get("height").and_then(serde_json::Value::as_u64);
                // Runtime dimensions are `u32`, so a value above `u32::MAX` passes
                // a bare `> 0` check here but fails `from_value::<u32>` at runtime
                // settings load on every request — the exact failure this build
                // check exists to prevent.
                let in_u32 =
                    |v: Option<u64>| matches!(v, Some(n) if n > 0 && n <= u64::from(u32::MAX));
                if !(in_u32(width) && in_u32(height)) {
                    return Err(format!(
                        "slot `{id}` format must have positive width and height within u32 range"
                    ));
                }
            }
        }
        _ => {
            return Err(format!("slot `{id}` must include at least one format"));
        }
    }

    // Resolved GAM unit path must not be empty. An explicit override is used
    // when present; otherwise it is derived as `/<gam_network_id>/<id>`.
    let resolved_gam_unit_path = match slot
        .get("gam_unit_path")
        .and_then(serde_json::Value::as_str)
    {
        Some(path) => path.to_string(),
        None => format!("/{gam_network_id}/{id}"),
    };
    if resolved_gam_unit_path.trim().is_empty() {
        return Err(format!(
            "slot `{id}` resolved GAM unit path must not be empty"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_creative_slot, validate_price_granularity};
    use serde_json::json;

    #[test]
    fn rejects_unknown_slot_field() {
        // The runtime slot is deny_unknown_fields, so an env-injected typo like
        // `floorprice` must fail the build, not pass it and break settings load.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "floorprice": 1.5
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("unknown slot field must fail at build time");
        assert!(err.contains("unknown field 'floorprice'"), "got: {err}");
    }

    #[test]
    fn accepts_all_known_slot_fields() {
        let slot = json!({
            "id": "atf",
            "gam_unit_path": "/123456789/publisher/atf",
            "div_id": "atf-div",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "floor_price": 1.5,
            "targeting": { "pos": "atf" },
            "providers": {}
        });
        assert!(
            validate_creative_slot(&slot, "123456789").is_ok(),
            "all documented slot fields must be accepted"
        );
    }

    #[test]
    fn accepts_valid_price_granularities() {
        for value in ["low", "medium", "dense", "high", "auto"] {
            assert!(
                validate_price_granularity(value).is_ok(),
                "'{value}' should be a valid price_granularity"
            );
        }
    }

    #[test]
    fn rejects_invalid_price_granularity() {
        // The runtime PriceGranularity enum has no `custom` variant, so a build
        // that embeds it would fail settings load on every request.
        let err = validate_price_granularity("custom")
            .expect_err("invalid price_granularity must fail at build time");
        assert!(
            err.contains("price_granularity 'custom' is invalid"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_a_well_formed_slot() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }]
        });
        assert!(validate_creative_slot(&slot, "123456789").is_ok());
    }

    #[test]
    fn accepts_explicit_gam_unit_path_override() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/"],
            "formats": [{ "width": 300, "height": 250 }],
            "gam_unit_path": "/123456789/publisher/atf"
        });
        assert!(validate_creative_slot(&slot, "123456789").is_ok());
    }

    #[test]
    fn rejects_empty_formats() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": []
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("empty formats must fail at build time");
        assert!(err.contains("at least one format"), "got: {err}");
    }

    #[test]
    fn rejects_zero_dimension_format() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 0, "height": 250 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("zero dimensions must fail at build time");
        assert!(err.contains("positive width and height"), "got: {err}");
    }

    #[test]
    fn rejects_empty_page_patterns() {
        let slot = json!({
            "id": "atf",
            "page_patterns": [],
            "formats": [{ "width": 300, "height": 250 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("empty page patterns must fail at build time");
        assert!(err.contains("page pattern"), "got: {err}");
    }

    #[test]
    fn rejects_blank_page_pattern_strings() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["   "],
            "formats": [{ "width": 300, "height": 250 }]
        });
        assert!(validate_creative_slot(&slot, "123456789").is_err());
    }

    #[test]
    fn rejects_uncompilable_glob_pattern() {
        // `[` is an unterminated character class; it fails to compile both
        // directly and after the ** -> * normalization, so the slot would be
        // unmatchable at runtime.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["["],
            "formats": [{ "width": 300, "height": 250 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("uncompilable glob pattern must fail at build time");
        assert!(err.contains("valid page pattern"), "got: {err}");
    }

    #[test]
    fn accepts_recursive_glob_pattern() {
        // `/20**` fails direct glob compilation but compiles after the
        // ** -> * normalization, matching runtime behavior.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }]
        });
        assert!(validate_creative_slot(&slot, "123456789").is_ok());
    }

    #[test]
    fn rejects_blank_gam_unit_path_override() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "gam_unit_path": "   "
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("blank GAM unit path must fail at build time");
        assert!(err.contains("GAM unit path"), "got: {err}");
    }

    #[test]
    fn rejects_blank_div_id_override() {
        // An empty div_id override binds the slot to the first id-bearing
        // element at runtime, so validate_runtime rejects it — the build must
        // too, or a CI-green config fails settings load on the deployed service.
        let slot = json!({
            "id": "atf",
            "div_id": "  ",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("blank div_id override must fail at build time");
        assert!(
            err.contains("div_id override must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_unknown_format_field() {
        // `mediatype` is a typo for `media_type`; the runtime format struct is
        // deny_unknown_fields, so the build must reject it.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250, "mediatype": "banner" }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("unknown format field must fail at build time");
        assert!(err.contains("unknown field 'mediatype'"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_provider_field() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "providers": { "appnexus": {} }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("unknown provider field must fail at build time");
        assert!(err.contains("unknown field 'appnexus'"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_aps_field() {
        // `slotId` is a typo for `slot_id`.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "providers": { "aps": { "slotId": "abc" } }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("unknown aps field must fail at build time");
        assert!(err.contains("unknown field 'slotId'"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_prebid_field() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "providers": { "prebid": { "bidder": {} } }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("unknown prebid field must fail at build time");
        assert!(err.contains("unknown field 'bidder'"), "got: {err}");
    }

    #[test]
    fn accepts_well_formed_nested_provider_config() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250, "media_type": "banner" }],
            "providers": {
                "aps": { "slot_id": "abc" },
                "prebid": { "bidders": {} }
            }
        });
        assert!(
            validate_creative_slot(&slot, "123456789").is_ok(),
            "well-formed nested provider config must be accepted"
        );
    }

    #[test]
    fn rejects_invalid_media_type() {
        // `bannerr` passes the field-name check but the runtime MediaType enum
        // cannot deserialize it, so settings load would fail on the service.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250, "media_type": "bannerr" }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("invalid media_type must fail at build time");
        assert!(
            err.contains("media_type 'bannerr' is invalid"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_non_string_media_type() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250, "media_type": 1 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-string media_type must fail at build time");
        assert!(err.contains("media_type must be a string"), "got: {err}");
    }

    #[test]
    fn accepts_all_media_types() {
        for media_type in ["banner", "video", "native"] {
            let slot = json!({
                "id": "atf",
                "page_patterns": ["/20**"],
                "formats": [{ "width": 300, "height": 250, "media_type": media_type }]
            });
            assert!(
                validate_creative_slot(&slot, "123456789").is_ok(),
                "'{media_type}' should be a valid media_type"
            );
        }
    }

    #[test]
    fn media_type_values_match_runtime_enum() {
        use crate::auction::types::MediaType;
        // Every listed value must deserialize into the runtime enum.
        for value in super::MEDIA_TYPE_VALUES {
            serde_json::from_value::<MediaType>(json!(value))
                .unwrap_or_else(|_| panic!("'{value}' should deserialize into MediaType"));
        }
        // Exhaustive match so a newly added MediaType variant forces this test
        // (and MEDIA_TYPE_VALUES) to be updated, preventing silent drift.
        for variant in [MediaType::Banner, MediaType::Video, MediaType::Native] {
            let covered = match variant {
                MediaType::Banner => "banner",
                MediaType::Video => "video",
                MediaType::Native => "native",
            };
            assert!(
                super::MEDIA_TYPE_VALUES.contains(&covered),
                "MEDIA_TYPE_VALUES is missing runtime variant '{covered}'"
            );
        }
    }

    #[test]
    fn rejects_non_string_targeting_value() {
        // `targeting` is a runtime HashMap<String, String>; a numeric value
        // embeds cleanly but fails settings load.
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "targeting": { "pos": 1 }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-string targeting value must fail at build time");
        assert!(
            err.contains("targeting value for 'pos' must be a string"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_non_string_aps_slot_id() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "providers": { "aps": { "slot_id": 123 } }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-string aps slot_id must fail at build time");
        assert!(
            err.contains("providers.aps.slot_id must be a string"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_non_object_prebid_bidders() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "providers": { "prebid": { "bidders": "appnexus" } }
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-object prebid bidders must fail at build time");
        assert!(
            err.contains("providers.prebid.bidders must be a map"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_non_numeric_floor_price() {
        let slot = json!({
            "id": "atf",
            "page_patterns": ["/20**"],
            "formats": [{ "width": 300, "height": 250 }],
            "floor_price": "high"
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-numeric floor_price must fail at build time");
        assert!(err.contains("floor_price must be a number"), "got: {err}");
    }

    #[test]
    fn rejects_non_string_page_pattern_entry() {
        let slot = json!({
            "id": "atf",
            "page_patterns": [123],
            "formats": [{ "width": 300, "height": 250 }]
        });
        let err = validate_creative_slot(&slot, "123456789")
            .expect_err("non-string page_patterns entry must fail at build time");
        assert!(
            err.contains("page_patterns entries must be strings"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_missing_id() {
        let slot = json!({ "page_patterns": ["/"], "formats": [{ "width": 1, "height": 1 }] });
        assert!(validate_creative_slot(&slot, "net").is_err());
    }

    #[test]
    fn rejects_invalid_id_characters() {
        let slot = json!({ "id": "a b", "page_patterns": ["/"], "formats": [{ "width": 1, "height": 1 }] });
        assert!(validate_creative_slot(&slot, "net").is_err());
    }
}
