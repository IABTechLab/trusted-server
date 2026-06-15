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

/// Returns `true` when `id` is non-empty and only `[A-Za-z0-9_-]`.
fn is_valid_slot_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

    // At least one non-empty page pattern.
    let has_valid_pattern = slot
        .get("page_patterns")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|patterns| {
            patterns
                .iter()
                .any(|p| p.as_str().is_some_and(|s| !s.trim().is_empty()))
        });
    if !has_valid_pattern {
        return Err(format!(
            "slot `{id}` must include at least one non-empty page pattern"
        ));
    }

    // At least one format, each with positive width and height.
    match slot.get("formats").and_then(serde_json::Value::as_array) {
        Some(formats) if !formats.is_empty() => {
            for format in formats {
                let width = format.get("width").and_then(serde_json::Value::as_u64);
                let height = format.get("height").and_then(serde_json::Value::as_u64);
                if !matches!((width, height), (Some(w), Some(h)) if w > 0 && h > 0) {
                    return Err(format!(
                        "slot `{id}` format must have positive width and height"
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
    use super::validate_creative_slot;
    use serde_json::json;

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
