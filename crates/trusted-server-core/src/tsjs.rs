use trusted_server_js::{concatenated_hash, single_module_hash};

use crate::integrations::IntegrationRegistry;

/// Publisher runtime tags, preserving synchronous delivery for early dependencies.
///
/// The cacheable unified bundle installs core and GPT APIs before publisher
/// scripts. Separately deferred integrations retain their established order.
/// Supported GPT profiles have no inline initializer if this critical request fails.
#[must_use]
pub(crate) fn publisher_script_tags(integrations: &IntegrationRegistry) -> String {
    tsjs_script_tag_with_attributes(
        &integrations.js_module_ids_immediate(),
        &integrations.tsjs_script_tag_attributes(),
    )
}

/// GPT profiles whose unified runtime replaces the provisional initializer.
/// Other integrations retain their existing head snippets pending caller coverage.
pub(crate) fn supports_unified_gpt(module_ids: &[&str]) -> bool {
    module_ids.contains(&"gpt")
        && module_ids
            .iter()
            .all(|id| matches!(*id, "creative" | "gpt" | "prebid"))
}

fn trusted_script_attributes(attributes: &[(&'static str, &'static str)]) -> String {
    attributes
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
                    .any(|byte| matches!(byte, b'\"' | b'&' | b'<' | b'>')),
                "attribute value should not contain HTML-sensitive characters"
            );
            format!(" {name}=\"{value}\"")
        })
        .collect()
}

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
    tsjs_script_tag_with_attributes(module_ids, &[])
}

/// Publisher `<script>` tag for the tsjs bundle with trusted static attributes.
#[must_use]
pub fn tsjs_script_tag_with_attributes(
    module_ids: &[&str],
    attributes: &[(&'static str, &'static str)],
) -> String {
    let attributes = trusted_script_attributes(attributes);

    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"{attributes}></script>",
        tsjs_script_src(module_ids),
    )
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

/// `<script>` tag for the unified bundle when exact module IDs are unavailable.
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
    use trusted_server_js::module_bundle;

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
    fn unified_gpt_initializer_replacement_requires_a_supported_module_profile() {
        assert!(supports_unified_gpt(&["creative", "gpt"]));
        assert!(supports_unified_gpt(&["creative", "gpt", "prebid"]));
        assert!(!supports_unified_gpt(&["creative"]));
        for blocked in [
            "datadome",
            "google_tag_manager",
            "lockr",
            "permutive",
            "didomi",
            "sourcepoint",
            "osano",
            "testlight",
            "future_integration",
        ] {
            assert!(
                !supports_unified_gpt(&["creative", "gpt", blocked]),
                "should retain provisional initialization for {blocked}",
            );
        }
    }

    #[test]
    fn publisher_runtime_preserves_the_existing_synchronous_tag() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &serde_json::json!({"enabled": true}))
            .expect("should enable GPT");
        let registry = IntegrationRegistry::new(&settings).expect("should build GPT registry");
        assert_eq!(
            publisher_script_tags(&registry),
            tsjs_script_tag_with_attributes(
                &registry.js_module_ids_immediate(),
                &registry.tsjs_script_tag_attributes(),
            ),
            "should retain the exact synchronous tag instead of introducing a CSP hash requirement",
        );
    }

    #[test]
    fn supported_gpt_profiles_use_one_versioned_synchronous_unified_runtime() {
        for prebid_enabled in [false, true] {
            let mut settings = crate::test_support::tests::create_test_settings();
            settings
                .integrations
                .insert_config(
                    "gpt",
                    &serde_json::json!({"enabled": true, "gam_attribution_enabled": true}),
                )
                .expect("should enable GPT attribution");
            settings
                .integrations
                .insert_config(
                    "prebid",
                    &serde_json::json!({
                        "enabled": prebid_enabled,
                        "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js"
                    }),
                )
                .expect("should configure separately deferred Prebid");
            let registry = IntegrationRegistry::new(&settings).expect("should build GPT registry");
            let output = publisher_script_tags(&registry);
            assert_eq!(
                output,
                tsjs_script_tag_with_attributes(
                    &registry.js_module_ids_immediate(),
                    &[("data-ts-gam-attribution", "true")],
                ),
                "should synchronously load precisely the modules served by the unified route"
            );
            assert_eq!(output.matches("<script ").count(), 1);
            assert!(!output.contains(" defer") && !output.contains("trustedserver-bootstrap"));
            assert!(!output.contains("tsjs-gpt.min.js"));
            assert!(!registry.js_module_ids_immediate().contains(&"prebid"));
            assert_eq!(
                registry.js_module_ids_deferred().contains(&"prebid"),
                prebid_enabled
            );
        }
    }

    #[test]
    fn unified_gpt_runtime_preserves_aps_and_attribution_attributes() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt",
                &serde_json::json!({
                    "enabled": true, "gam_attribution_enabled": true
                }),
            )
            .expect("should enable GPT attribution");
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({
                    "enabled": true, "rendering_mode": "publisher_native"
                }),
            )
            .expect("should enable publisher-native APS rendering");
        settings.auction.providers.insert(
            "aps-main".parse().expect("should parse APS provider ID"),
            serde_json::from_value(serde_json::json!({
                "protocol": "openrtb-2.6", "profile": "aps",
                "endpoint": "https://aps.example/e/pb/bid", "routing": "all_eligible",
                "profile_config": {"account_id": "example-account"}
            }))
            .expect("should parse APS provider configuration"),
        );
        let registry =
            IntegrationRegistry::new(&settings).expect("should build GPT and APS registry");
        let output = publisher_script_tags(&registry);
        assert!(output.contains("data-ts-gam-attribution=\"true\""));
        assert!(output.contains("data-ts-aps-rendering-mode=\"publisher_native\""));
        assert!(supports_unified_gpt(&registry.js_module_ids()));
        assert_eq!(output.matches("<script ").count(), 1);
    }

    #[test]
    fn compiled_inline_bootstrap_cannot_terminate_its_script_element() {
        let script =
            module_bundle("bootstrap").expect("should embed the early publisher bootstrap");
        assert!(
            !script.to_ascii_lowercase().contains("</script"),
            "should compile script-text-safe inline JavaScript",
        );
        assert!(
            !script.contains("<!--"),
            "should escape HTML comment openers"
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
    fn publisher_tsjs_script_tag_renders_static_attributes() {
        let module_ids = ["gpt"];
        let src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag_with_attributes(&module_ids, &[("data-ts-gam-attribution", "true")]),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-gam-attribution=\"true\"></script>"
            ),
            "should render trusted static attributes on the publisher bundle tag"
        );
        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should keep the generic tag byte-for-byte unmarked"
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_invalid_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-bad_name", "true")]);
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_empty_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("", "true")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_double_quote_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad\"value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_ampersand_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad&value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_less_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad<value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_greater_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad>value")]);
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
