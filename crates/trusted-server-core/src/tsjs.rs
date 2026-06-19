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
/// IDs when `IntegrationRegistry` is available.
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

/// `/static` URL for a single split module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(module_id: &str) -> String {
    let hash = single_module_hash(module_id).unwrap_or_default();
    format!("/static/tsjs=tsjs-{module_id}.min.js?v={hash}")
}

/// Return true when a split module must execute synchronously after the main bundle.
fn tsjs_split_module_loads_synchronously(module_id: &str) -> bool {
    module_id == "prebid"
}

/// `<script>` tag for a single split module.
///
/// Most split modules use `defer`, but Prebid must execute synchronously after
/// the main TSJS bundle so publisher ad code observes the TSJS-owned `pbjs`
/// before attempting legacy Prebid bundle loading.
#[must_use]
pub fn tsjs_deferred_script_tag(module_id: &str) -> String {
    let defer_attr = if tsjs_split_module_loads_synchronously(module_id) {
        ""
    } else {
        " defer"
    };
    format!(
        "<script src=\"{}\"{defer_attr}></script>",
        tsjs_deferred_script_src(module_id)
    )
}

/// Generate all split module `<script>` tags for the given module IDs.
///
/// Returns an empty string when no split modules are present.
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
        let creative_prebid_src = tsjs_script_src(&["creative", "prebid"]);

        assert_ne!(
            creative_src, creative_prebid_src,
            "should include requested modules in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_hash_depends_on_module_order() {
        assert_ne!(
            tsjs_script_src(&["creative", "prebid"]),
            tsjs_script_src(&["prebid", "creative"]),
            "should include module order in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_deduplicates_core_module() {
        assert_eq!(
            tsjs_script_src(&["core", "prebid"]),
            tsjs_script_src(&["prebid"]),
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
    fn tsjs_script_tag_wraps_source_in_single_trustedserver_tag() {
        let module_ids = ["creative"];
        let src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should generate exactly one trusted server script tag"
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_all_module_ids() {
        let ids = all_module_ids();

        assert_eq!(
            tsjs_unified_script_src(),
            tsjs_script_src(&ids),
            "should hash all module IDs for the unified script source"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            tsjs_script_tag(&ids),
            "should wrap the all-module unified script source"
        );
    }

    #[test]
    fn tsjs_deferred_script_src_formats_known_module_url_with_hash() {
        let src = tsjs_deferred_script_src("prebid");

        assert!(
            src.starts_with("/static/tsjs=tsjs-prebid.min.js?v="),
            "should use per-module static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_deferred_script_src_uses_empty_hash_for_unknown_module() {
        assert_eq!(
            tsjs_deferred_script_src("unknown-module"),
            "/static/tsjs=tsjs-unknown-module.min.js?v=",
            "should document current unknown-module hash behavior"
        );
    }

    #[test]
    fn tsjs_deferred_script_tag_loads_prebid_synchronously() {
        let src = tsjs_deferred_script_src("prebid");

        assert_eq!(
            tsjs_deferred_script_tag("prebid"),
            format!("<script src=\"{src}\"></script>"),
            "should generate a synchronous prebid script tag"
        );
    }

    #[test]
    fn tsjs_deferred_script_tag_marks_other_split_modules_defer() {
        let src = tsjs_deferred_script_src("creative");

        assert_eq!(
            tsjs_deferred_script_tag("creative"),
            format!("<script src=\"{src}\" defer></script>"),
            "should defer non-prebid split module script tags"
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
    fn tsjs_unified_script_src_and_tag_include_cache_busting_hash() {
        let src = tsjs_unified_script_src();

        assert!(
            src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should include unified script URL prefix"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the unified source in a trusted server script tag"
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
