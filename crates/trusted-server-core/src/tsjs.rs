use trusted_server_js::{all_module_ids, concatenated_hash, single_module_hash};

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the concatenated content of the given module set.
#[must_use]
pub fn tsjs_script_src(module_ids: &[&str]) -> String {
    let hash = concatenated_hash(module_ids);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// `<script>` tag for injecting the tsjs bundle.
#[must_use]
pub fn tsjs_script_tag(module_ids: &[&str]) -> String {
    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"></script>",
        tsjs_script_src(module_ids)
    )
}

/// `/static` URL for the unified bundle with a conservative cache-busting hash.
///
/// Hashes all compiled module IDs so the cache invalidates whenever any module
/// changes. Over-invalidates slightly (includes deferred modules in the hash)
/// but never serves stale content. Use [`tsjs_script_src`] with exact module
/// IDs when the [`IntegrationRegistry`] is available.
#[must_use]
pub fn tsjs_unified_script_src() -> String {
    let ids = all_module_ids();
    tsjs_script_src(&ids)
}

/// `<script>` tag for the unified bundle with a conservative cache-busting hash.
///
/// See [`tsjs_unified_script_src`] for details.
#[must_use]
pub fn tsjs_unified_script_tag() -> String {
    let ids = all_module_ids();
    tsjs_script_tag(&ids)
}

/// `/static` URL for a single deferred module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(module_id: &str) -> String {
    let hash = single_module_hash(module_id).unwrap_or_default();
    format!("/static/tsjs=tsjs-{module_id}.min.js?v={hash}")
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

    fn assert_sha256_hex_hash(src: &str) {
        let hash = src
            .rsplit_once("?v=")
            .expect("should have cache-busting query parameter")
            .1;

        assert_eq!(hash.len(), 64, "should use a SHA-256 hex hash");
        assert!(
            hash.chars().all(|character| character.is_ascii_hexdigit()),
            "should use only ASCII hex digits",
        );
    }

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let creative_src = tsjs_script_src(&["creative"]);
        let creative_prebid_src = tsjs_script_src(&["creative", "prebid"]);

        assert!(
            creative_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should include the unified bundle path and cache-busting parameter",
        );
        assert_sha256_hex_hash(&creative_src);
        assert_ne!(
            creative_src, creative_prebid_src,
            "should generate different cache-busting URLs for different module sets",
        );
    }

    #[test]
    fn tsjs_script_tag_wraps_source_in_a_single_tag() {
        let module_ids = ["creative"];
        let expected_src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{expected_src}\" id=\"trustedserver-js\"></script>"),
            "should render the injected trustedserver script tag",
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_all_module_ids() {
        let unified = tsjs_unified_script_src();
        let manual = tsjs_script_src(&all_module_ids());

        assert_eq!(
            unified, manual,
            "unified helpers must hash every registered module",
        );
        assert!(
            unified.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should include the unified bundle path and cache-busting parameter",
        );
        assert_sha256_hex_hash(&unified);
        assert_eq!(
            tsjs_unified_script_tag(),
            format!("<script src=\"{unified}\" id=\"trustedserver-js\"></script>"),
            "should wrap the unified bundle source in the standard script tag",
        );
    }

    #[test]
    fn tsjs_deferred_helpers_format_single_module_urls_and_tags() {
        let module_id = "prebid";
        let src = tsjs_deferred_script_src(module_id);

        assert!(
            src.starts_with("/static/tsjs=tsjs-prebid.min.js?v="),
            "should include the deferred module path and cache-busting parameter",
        );
        assert_sha256_hex_hash(&src);
        assert_eq!(
            tsjs_deferred_script_tag(module_id),
            format!("<script src=\"{src}\" defer></script>"),
            "should render a deferred script tag for the module",
        );
    }

    #[test]
    fn tsjs_deferred_script_src_documents_unregistered_module_fallback() {
        assert_eq!(
            tsjs_deferred_script_src("unknown-module"),
            "/static/tsjs=tsjs-unknown-module.min.js?v=",
            "should preserve the current fallback until callers validate registered modules",
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_concatenates_tags_in_input_order() {
        let result = tsjs_deferred_script_tags(&["prebid", "creative"]);
        let prebid_pos = result
            .find("tsjs-prebid")
            .expect("should contain prebid script");
        let creative_pos = result
            .find("tsjs-creative")
            .expect("should contain creative script");

        assert!(prebid_pos < creative_pos, "should preserve input order");
        assert_eq!(
            result.matches("<script ").count(),
            2,
            "should emit one tag per module",
        );
        assert_eq!(
            result.matches(" defer></script>").count(),
            2,
            "should mark each deferred script tag with defer",
        );
    }
}
