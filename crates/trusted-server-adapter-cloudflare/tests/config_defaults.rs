//! Keep the checked-in Cloudflare configuration aligned with the runtime defaults.

use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

#[test]
fn cloudflare_manifest_uses_the_runtime_config_store_id() {
    let manifest: toml::Value = toml::from_str(include_str!("../cloudflare.toml"))
        .expect("should parse the Cloudflare manifest");

    assert_eq!(
        manifest["stores"]["config"]["name"].as_str(),
        Some(DEFAULT_CONFIG_STORE_ID),
        "Cloudflare config store should match the manifest-derived runtime default"
    );
}

#[test]
fn wrangler_placeholder_uses_the_runtime_blob_key() {
    let manifest: toml::Value = toml::from_str(include_str!("../wrangler.toml"))
        .expect("should parse the Wrangler manifest");
    let raw_config = manifest["vars"]["TRUSTED_SERVER_CONFIG"]
        .as_str()
        .expect("should declare the config JSON binding");
    let config: serde_json::Value =
        serde_json::from_str(raw_config).expect("should parse the config JSON placeholder");
    let entries = config
        .as_object()
        .expect("should contain config properties");

    assert_eq!(entries.len(), 1, "should declare only the current blob key");
    assert_eq!(
        entries
            .get(CONFIG_BLOB_KEY)
            .and_then(serde_json::Value::as_str),
        Some(""),
        "placeholder should use the runtime key and remain invalid until seeded"
    );
}

#[test]
fn runtime_kv_binding_matches_both_wrangler_manifests() {
    for (name, raw) in [
        ("wrangler.toml", include_str!("../wrangler.toml")),
        ("wrangler.ci.toml", include_str!("../wrangler.ci.toml")),
    ] {
        let manifest: toml::Value =
            toml::from_str(raw).expect("should parse the Wrangler manifest");
        let bindings = manifest["kv_namespaces"]
            .as_array()
            .expect("should declare KV namespaces")
            .iter()
            .filter_map(|namespace| namespace["binding"].as_str())
            .collect::<Vec<_>>();

        assert!(
            bindings.contains(&trusted_server_adapter_cloudflare::platform::CLOUDFLARE_KV_BINDING),
            "{name} should bind the namespace the Worker opens as runtime KV, found {bindings:?}"
        );
    }
}
