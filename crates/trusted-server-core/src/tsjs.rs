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
    use trusted_server_js::{all_module_ids, concatenated_hash, single_module_hash};

    use super::*;

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let module_ids = ["core", "creative"];
        let expected_hash = concatenated_hash(&module_ids);

        assert_eq!(
            tsjs_script_src(&module_ids),
            format!("/static/tsjs=tsjs-unified.min.js?v={expected_hash}"),
            "should include the unified bundle path and cache-busting hash",
        );
    }

    #[test]
    fn tsjs_script_tag_wraps_source_in_a_single_tag() {
        let module_ids = ["core", "creative"];
        let expected_src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{expected_src}\" id=\"trustedserver-js\"></script>"),
            "should render the injected trustedserver script tag",
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_all_module_ids() {
        let all_ids = all_module_ids();
        let expected_src = format!(
            "/static/tsjs=tsjs-unified.min.js?v={}",
            concatenated_hash(&all_ids)
        );

        assert_eq!(
            tsjs_unified_script_src(),
            expected_src,
            "should hash all compiled modules for the unified bundle",
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!("<script src=\"{expected_src}\" id=\"trustedserver-js\"></script>"),
            "should wrap the unified bundle source in the standard script tag",
        );
    }

    #[test]
    fn tsjs_deferred_helpers_format_single_module_urls_and_tags() {
        let module_id = "prebid";
        let expected_hash = single_module_hash(module_id).expect("should hash known module");
        let expected_src = format!("/static/tsjs=tsjs-{module_id}.min.js?v={expected_hash}");

        assert_eq!(
            tsjs_deferred_script_src(module_id),
            expected_src,
            "should include the deferred module path and hash",
        );
        assert_eq!(
            tsjs_deferred_script_tag(module_id),
            format!("<script src=\"{expected_src}\" defer></script>"),
            "should render a deferred script tag for the module",
        );
    }

    #[test]
    fn tsjs_deferred_script_src_uses_empty_hash_for_unknown_module() {
        assert_eq!(
            tsjs_deferred_script_src("unknown-module"),
            "/static/tsjs=tsjs-unknown-module.min.js?v=",
            "should fall back to an empty hash for unknown deferred modules",
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_concatenates_tags_in_input_order() {
        let module_ids = ["prebid", "creative"];
        let expected = module_ids
            .iter()
            .map(|id| tsjs_deferred_script_tag(id))
            .collect::<String>();

        assert_eq!(
            tsjs_deferred_script_tags(&module_ids),
            expected,
            "should concatenate one deferred script tag per module in order",
        );
    }
}
