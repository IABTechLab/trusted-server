use trusted_server_js::{concatenated_hash, single_module_hash};

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the concatenated content of the given module set.
#[must_use]
pub fn tsjs_script_src(module_ids: &[&str]) -> String {
    let hash = concatenated_hash(module_ids);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// Runtime tag with the configured origin used to recognize existing proxy URLs.
///
/// Dynamic source and origin values are escaped independently of trusted static attributes.
#[must_use]
pub(crate) fn tsjs_script_tag_with_public_origin(
    src: &str,
    attributes: &[(&'static str, &'static str)],
    public_origin: &str,
) -> String {
    let mut attributes = attributes
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

    attributes.push_str(&format!(
        " data-ts-public-origin=\"{}\"",
        escape_attribute_value(public_origin)
    ));

    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"{attributes}></script>",
        escape_attribute_value(src),
    )
}

fn escape_attribute_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `/static` URL for the unified bundle when exact module IDs are unavailable.
///
/// This intentionally omits `?v=` because the serving path can only mark a URL
/// immutable when the hash matches the exact enabled module set. Use
/// [`tsjs_script_src`] with exact module IDs when [`IntegrationRegistry`] is
/// available.
///
/// [`IntegrationRegistry`]: crate::integrations::IntegrationRegistry
#[must_use]
pub fn tsjs_unified_script_src() -> String {
    "/static/tsjs=tsjs-unified.min.js".to_string()
}

/// `/static` URL for one module with its own cache-busting hash.
#[must_use]
pub fn tsjs_single_module_script_src(module_id: &str) -> String {
    let hash = single_module_hash(module_id).unwrap_or_default();
    format!("/static/tsjs=tsjs-{module_id}.min.js?v={hash}")
}

/// `/static` URL for a single deferred module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(module_id: &str) -> String {
    tsjs_single_module_script_src(module_id)
}

/// `<script defer>` tag for a single deferred module.
#[must_use]
pub fn tsjs_deferred_script_tag(module_id: &str) -> String {
    format!(
        "<script src=\"{}\" defer></script>",
        tsjs_deferred_script_src(module_id)
    )
}

/// Generate all deferred `<script defer>` tags for the given module IDs.
///
/// Returns an empty string when no deferred modules are present.
#[must_use]
pub fn tsjs_deferred_script_tags(module_ids: &[&str]) -> String {
    module_ids
        .iter()
        .map(|id| tsjs_deferred_script_tag(id))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let src = tsjs_script_src(&["creative"]);

        assert!(
            src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_script_src_empty_module_list_matches_core_only_bundle() {
        let empty_src = tsjs_script_src(&[]);

        assert!(
            empty_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&empty_src));
        assert_eq!(
            empty_src,
            tsjs_script_src(&["core"]),
            "should include core exactly once for an empty module list"
        );
    }

    #[test]
    fn tsjs_script_src_hash_changes_with_module_set() {
        let creative_src = tsjs_script_src(&["creative"]);
        let creative_datadome_src = tsjs_script_src(&["creative", "datadome"]);

        assert_ne!(
            creative_src, creative_datadome_src,
            "should include requested modules in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_hash_depends_on_module_order() {
        assert_ne!(
            tsjs_script_src(&["creative", "datadome"]),
            tsjs_script_src(&["datadome", "creative"]),
            "should include module order in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_deduplicates_core_module() {
        assert_eq!(
            tsjs_script_src(&["core", "datadome"]),
            tsjs_script_src(&["datadome"]),
            "should not hash core twice when requested explicitly"
        );
    }

    #[test]
    fn tsjs_script_src_is_stable_for_identical_module_ids() {
        let module_ids = ["core", "lockr", "permutive"];
        let src = tsjs_script_src(&module_ids);

        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            src,
            tsjs_script_src(&module_ids),
            "should produce a stable URL for identical module IDs"
        );
    }

    #[test]
    fn runtime_metadata_wraps_source_in_single_trustedserver_tag() {
        let src = tsjs_script_src(&["creative"]);
        let origin = "https://ads.publisher.example";

        assert_eq!(
            tsjs_script_tag_with_public_origin(&src, &[], origin),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-public-origin=\"{origin}\"></script>"
            ),
            "should generate exactly one trusted server script tag with origin metadata"
        );
    }

    #[test]
    fn publisher_tsjs_script_tag_renders_static_attributes() {
        let src = tsjs_script_src(&["gpt"]);
        let origin = "https://ads.publisher.example";

        assert_eq!(
            tsjs_script_tag_with_public_origin(
                &src,
                &[("data-ts-gam-attribution", "true")],
                origin,
            ),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-gam-attribution=\"true\" data-ts-public-origin=\"{origin}\"></script>"
            ),
            "should render trusted static attributes and origin metadata on the publisher bundle tag"
        );
    }

    #[test]
    fn runtime_metadata_preserves_source_hash_and_static_attributes() {
        let src = tsjs_script_src(&["gpt", "creative"]);
        let origin = String::from("https://ads.publisher.example:8443");
        assert_eq!(
            tsjs_script_tag_with_public_origin(
                &src,
                &[("data-ts-gam-attribution", "true")],
                &origin,
            ),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-gam-attribution=\"true\" data-ts-public-origin=\"{origin}\"></script>"
            ),
            "should add metadata without changing the source or static attributes"
        );
    }

    #[test]
    fn runtime_metadata_escapes_dynamic_values() {
        assert_eq!(
            tsjs_script_tag_with_public_origin(
                "/static/test?v=a&b=\"<x>\"",
                &[],
                "https://example.com/\"<&>"
            ),
            "<script src=\"/static/test?v=a&amp;b=&quot;&lt;x&gt;&quot;\" id=\"trustedserver-js\" data-ts-public-origin=\"https://example.com/&quot;&lt;&amp;&gt;\"></script>",
            "should escape dynamic source and origin without introducing attributes or markup"
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_invalid_attribute_name() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("data-bad_name", "true")],
            "https://example.com",
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_empty_attribute_name() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("", "true")],
            "https://example.com",
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_double_quote_in_attribute_value() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("data-safe-name", "bad\"value")],
            "https://example.com",
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_ampersand_in_attribute_value() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("data-safe-name", "bad&value")],
            "https://example.com",
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_less_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("data-safe-name", "bad<value")],
            "https://example.com",
        );
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_greater_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_public_origin(
            "/static/test",
            &[("data-safe-name", "bad>value")],
            "https://example.com",
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_unversioned_fallback_without_registry() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "registry-free unified helper should not emit an unverifiable hash"
        );
    }

    #[test]
    fn tsjs_single_module_script_src_formats_known_module_url_with_hash() {
        let src = tsjs_single_module_script_src("creative");

        assert!(
            src.starts_with("/static/tsjs=tsjs-creative.min.js?v="),
            "should use per-module static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_deferred_script_src_hashes_prebid_shim_and_empties_unknown_module() {
        let prebid_src = tsjs_deferred_script_src("prebid");
        assert!(
            prebid_src.starts_with("/static/tsjs=tsjs-prebid.min.js?v="),
            "prebid shim should be served from the deferred tsjs route"
        );
        assert_sha256_hex_hash(hash_query_value(&prebid_src));
        assert_eq!(
            tsjs_deferred_script_src("unknown-module"),
            "/static/tsjs=tsjs-unknown-module.min.js?v=",
            "should document current unknown-module hash behavior"
        );
    }

    #[test]
    fn tsjs_deferred_script_tag_marks_script_defer() {
        let src = tsjs_deferred_script_src("prebid");

        assert_eq!(
            tsjs_deferred_script_tag("prebid"),
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
        assert_eq!(
            tsjs_deferred_script_tags(&["prebid", "creative"]),
            format!(
                "{}{}",
                tsjs_deferred_script_tag("prebid"),
                tsjs_deferred_script_tag("creative")
            ),
            "should preserve caller-provided deferred module order"
        );
    }

    #[test]
    fn tsjs_unified_script_src_omits_unverifiable_cache_busting_hash() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "should use the unified script URL without an unverifiable hash"
        );
    }

    #[test]
    fn tsjs_script_src_differs_for_different_module_sets() {
        assert_ne!(
            tsjs_script_src(&["lockr"]),
            tsjs_script_src(&["lockr", "permutive"]),
            "should bust the cache when the module set content changes"
        );
    }

    #[test]
    fn tsjs_deferred_script_src_has_empty_hash_for_unknown_module() {
        assert_eq!(
            tsjs_deferred_script_src("does-not-exist"),
            "/static/tsjs=tsjs-does-not-exist.min.js?v=",
            "should fall back to an empty cache-busting hash for an unknown module"
        );
    }
}
