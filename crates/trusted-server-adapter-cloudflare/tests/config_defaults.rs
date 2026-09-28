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
