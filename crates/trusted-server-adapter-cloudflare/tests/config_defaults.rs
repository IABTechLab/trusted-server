//! Keep the checked-in Cloudflare configuration aligned with the runtime defaults.

use edgezero_core::app::Hooks as _;
use edgezero_core::env_config::EnvConfig;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

/// `EdgeZero` treats a declared store as required, so a manifest that binds
/// no namespace under the name it opens for the declared key-value store
/// would fail every request.
#[test]
fn every_wrangler_manifest_binds_the_declared_key_value_store() {
    let declared = TrustedServerApp::stores()
        .kv
        .expect("the adapter should declare its key-value store");

    for (manifest_name, text) in [
        ("wrangler.toml", include_str!("../wrangler.toml")),
        ("wrangler.ci.toml", include_str!("../wrangler.ci.toml")),
    ] {
        let manifest: toml::Value =
            toml::from_str(text).expect("should parse the Wrangler manifest");
        // `EdgeZero` reads a store's binding name from the Worker's variables
        // and falls back to the store's id.
        let vars = manifest
            .get("vars")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())));
        let binding = EnvConfig::from_vars(vars).store_name("kv", declared.default);
        let bound: Vec<&str> = manifest
            .get("kv_namespaces")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|namespace| namespace.get("binding")?.as_str())
            .collect();

        assert!(
            bound.contains(&binding.as_str()),
            "{manifest_name} should bind `{binding}`, which the declared store opens, \
             and binds {bound:?}"
        );
    }
}

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
