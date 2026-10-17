//! URLs and `<script>` tags for the served tsjs script.
//!
//! Every helper here takes [`JsModulePart`]s rather than module ids, so a
//! module a vendor crate carries on its registration is tagged with the same
//! content hash the serving path computes for it.

use crate::tsjs_bundle::{JsModulePart, compose_hash};

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the composed content of the given parts.
#[must_use]
pub fn tsjs_script_src(parts: &[JsModulePart]) -> String {
    let hash = compose_hash(parts);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// `<script>` tag for injecting the tsjs bundle.
#[must_use]
pub fn tsjs_script_tag(parts: &[JsModulePart]) -> String {
    tsjs_script_tag_with_attributes(parts, &[])
}

/// Publisher `<script>` tag for the tsjs bundle with trusted static attributes.
#[must_use]
pub fn tsjs_script_tag_with_attributes(
    parts: &[JsModulePart],
    attributes: &[(&'static str, &'static str)],
) -> String {
    let attributes = attributes
        .iter()
        .map(|(name, value)| {
            debug_assert!(
                !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    }),
                "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
            );
            debug_assert!(
                !value
                    .bytes()
                    .any(|byte| matches!(byte, b'"' | b'&' | b'<' | b'>')),
                "attribute value should not contain HTML-sensitive characters"
            );
            format!(" {name}=\"{value}\"")
        })
        .collect::<String>();

    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"{attributes}></script>",
        tsjs_script_src(parts),
    )
}

/// `/static` URL for the unified bundle when the exact parts are unavailable.
///
/// This intentionally omits `?v=` because the serving path can only mark a URL
/// immutable when the hash matches the exact enabled module set. Use
/// [`tsjs_script_src`] with the registry's parts when [`IntegrationRegistry`]
/// is available.
///
/// [`IntegrationRegistry`]: crate::integrations::IntegrationRegistry
#[must_use]
pub fn tsjs_unified_script_src() -> String {
    "/static/tsjs=tsjs-unified.min.js".to_string()
}

/// `<script>` tag for the unified bundle when the exact parts are unavailable.
///
/// See [`tsjs_unified_script_src`] for details.
#[must_use]
pub fn tsjs_unified_script_tag() -> String {
    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"></script>",
        tsjs_unified_script_src()
    )
}

/// `/static` URL for one module with its own cache-busting hash.
#[must_use]
pub fn tsjs_single_module_script_src(part: &JsModulePart) -> String {
    format!("/static/tsjs=tsjs-{}.min.js?v={}", part.id, part.sha256)
}

/// `/static` URL for a single deferred module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(part: &JsModulePart) -> String {
    tsjs_single_module_script_src(part)
}

/// `<script defer>` tag for a single deferred module.
#[must_use]
pub fn tsjs_deferred_script_tag(part: &JsModulePart) -> String {
    format!(
        "<script src=\"{}\" defer></script>",
        tsjs_deferred_script_src(part)
    )
}

/// Generate all deferred `<script defer>` tags for the given parts.
///
/// Returns an empty string when no deferred modules are present.
#[must_use]
pub fn tsjs_deferred_script_tags(parts: &[JsModulePart]) -> String {
    parts
        .iter()
        .map(tsjs_deferred_script_tag)
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::tsjs_bundle::compile_time_parts;

    fn hash_query_value(src: &str) -> &str {
        src.split_once("?v=")
            .map(|(_, hash)| hash)
            .expect("should contain cache-busting hash query")
    }

    fn assert_sha256_hex_hash(value: &str) {
        assert_eq!(value.len(), 64, "should be a SHA-256 hex digest");
        assert!(
            value.chars().all(|ch| ch.is_ascii_hexdigit()),
            "should contain only ASCII hex digits"
        );
    }

    /// Builds a part whose `sha256` really is the hash of `source`.
    fn carried_part(id: &'static str, source: &'static str) -> JsModulePart {
        let sha256 = Box::leak(hex::encode(Sha256::digest(source)).into_boxed_str());
        JsModulePart { id, source, sha256 }
    }

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let src = tsjs_script_src(&compile_time_parts(&["creative"]));

        assert!(
            src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_script_src_matches_the_compile_time_hash_for_the_same_ids() {
        let ids = ["core", "lockr", "permutive"];

        assert_eq!(
            tsjs_script_src(&compile_time_parts(&ids)),
            format!(
                "/static/tsjs=tsjs-unified.min.js?v={}",
                trusted_server_js::concatenated_hash(&ids)
            ),
            "should keep today's URL for a compile-time module set"
        );
    }

    #[test]
    fn tsjs_script_src_empty_part_list_matches_core_only_bundle() {
        let empty_src = tsjs_script_src(&[]);

        assert!(
            empty_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&empty_src));
        assert_eq!(
            empty_src,
            tsjs_script_src(&compile_time_parts(&["core"])),
            "should include core exactly once for an empty part list"
        );
    }

    #[test]
    fn tsjs_script_src_hash_changes_with_module_set() {
        let creative_src = tsjs_script_src(&compile_time_parts(&["creative"]));
        let creative_datadome_src = tsjs_script_src(&compile_time_parts(&["creative", "datadome"]));

        assert_ne!(
            creative_src, creative_datadome_src,
            "should include requested modules in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_hash_depends_on_module_order() {
        assert_ne!(
            tsjs_script_src(&compile_time_parts(&["creative", "datadome"])),
            tsjs_script_src(&compile_time_parts(&["datadome", "creative"])),
            "should include module order in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_deduplicates_core_module() {
        assert_eq!(
            tsjs_script_src(&compile_time_parts(&["core", "datadome"])),
            tsjs_script_src(&compile_time_parts(&["datadome"])),
            "should not hash core twice when requested explicitly"
        );
    }

    #[test]
    fn tsjs_script_src_is_stable_for_identical_parts() {
        let parts = compile_time_parts(&["core", "lockr", "permutive"]);
        let src = tsjs_script_src(&parts);

        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            src,
            tsjs_script_src(&parts),
            "should produce a stable URL for identical parts"
        );
    }

    #[test]
    fn tsjs_script_src_hashes_a_carried_part_by_content() {
        let core = compile_time_parts(&["core"]);
        let before = [
            core[0],
            carried_part("probe", "(() => { window.probe = 1; })()"),
        ];
        let after = [
            core[0],
            carried_part("probe", "(() => { window.probe = 2; })()"),
        ];

        assert_ne!(
            tsjs_script_src(&before),
            tsjs_script_src(&after),
            "should change the bundle URL when a carried module's source changes"
        );
    }

    #[test]
    fn tsjs_script_tag_wraps_source_in_single_trustedserver_tag() {
        let parts = compile_time_parts(&["creative"]);
        let src = tsjs_script_src(&parts);

        assert_eq!(
            tsjs_script_tag(&parts),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should generate exactly one trusted server script tag"
        );
    }

    #[test]
    fn publisher_tsjs_script_tag_renders_static_attributes() {
        let parts = compile_time_parts(&["gpt"]);
        let src = tsjs_script_src(&parts);

        assert_eq!(
            tsjs_script_tag_with_attributes(&parts, &[("data-ts-gam-attribution", "true")]),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-gam-attribution=\"true\"></script>"
            ),
            "should render trusted static attributes on the publisher bundle tag"
        );
        assert_eq!(
            tsjs_script_tag(&parts),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should keep the generic tag byte-for-byte unmarked"
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_invalid_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(
            &compile_time_parts(&["gpt"]),
            &[("data-bad_name", "true")],
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_empty_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(&compile_time_parts(&["gpt"]), &[("", "true")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_double_quote_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(
            &compile_time_parts(&["gpt"]),
            &[("data-safe-name", "bad\"value")],
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_ampersand_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(
            &compile_time_parts(&["gpt"]),
            &[("data-safe-name", "bad&value")],
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_less_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(
            &compile_time_parts(&["gpt"]),
            &[("data-safe-name", "bad<value")],
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_greater_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(
            &compile_time_parts(&["gpt"]),
            &[("data-safe-name", "bad>value")],
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_unversioned_fallback_without_registry() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "registry-free unified helper should not emit an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the registry-free unified source"
        );
    }

    #[test]
    fn tsjs_single_module_script_src_formats_known_module_url_with_hash() {
        let parts = compile_time_parts(&["creative"]);
        let src = tsjs_single_module_script_src(&parts[0]);

        assert!(
            src.starts_with("/static/tsjs=tsjs-creative.min.js?v="),
            "should use per-module static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            src,
            format!(
                "/static/tsjs=tsjs-creative.min.js?v={}",
                trusted_server_js::single_module_hash("creative")
                    .expect("should have compiled creative in")
            ),
            "should keep today's URL for a compile-time module"
        );
    }

    #[test]
    fn tsjs_deferred_script_src_hashes_prebid_shim() {
        let parts = compile_time_parts(&["prebid"]);
        let prebid_src = tsjs_deferred_script_src(&parts[0]);

        assert!(
            prebid_src.starts_with("/static/tsjs=tsjs-prebid.min.js?v="),
            "prebid shim should be served from the deferred tsjs route"
        );
        assert_sha256_hex_hash(hash_query_value(&prebid_src));
    }

    #[test]
    fn tsjs_deferred_script_tag_carries_a_carried_parts_own_hash() {
        let part = carried_part("probe", "(() => { window.probe = 1; })()");

        assert_eq!(
            tsjs_deferred_script_tag(&part),
            format!(
                "<script src=\"/static/tsjs=tsjs-probe.min.js?v={}\" defer></script>",
                part.sha256
            ),
            "should version a carried deferred module by its own content hash"
        );
    }

    #[test]
    fn tsjs_deferred_script_tag_marks_script_defer() {
        let parts = compile_time_parts(&["prebid"]);
        let src = tsjs_deferred_script_src(&parts[0]);

        assert_eq!(
            tsjs_deferred_script_tag(&parts[0]),
            format!("<script src=\"{src}\" defer></script>"),
            "should generate a deferred script tag"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_returns_empty_for_empty_input() {
        assert_eq!(
            tsjs_deferred_script_tags(&[]),
            "",
            "should not emit tags when no deferred modules exist"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_preserves_input_order() {
        let parts = compile_time_parts(&["prebid", "creative"]);

        assert_eq!(
            tsjs_deferred_script_tags(&parts),
            format!(
                "{}{}",
                tsjs_deferred_script_tag(&parts[0]),
                tsjs_deferred_script_tag(&parts[1])
            ),
            "should preserve caller-provided deferred module order"
        );
    }

    #[test]
    fn tsjs_unified_script_src_and_tag_omit_unverifiable_cache_busting_hash() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "should use the unified script URL without an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the unified source in a trusted server script tag"
        );
    }

    #[test]
    fn tsjs_script_src_differs_for_different_module_sets() {
        assert_ne!(
            tsjs_script_src(&compile_time_parts(&["lockr"])),
            tsjs_script_src(&compile_time_parts(&["lockr", "permutive"])),
            "should bust the cache when the module set content changes"
        );
    }
}
