//! Keep the checked-in Cloudflare configuration aligned with the runtime defaults.

use edgezero_core::app::Hooks as _;
use edgezero_core::env_config::EnvConfig;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;

/// The binding of the key-value store's own namespace.
const KV_STORE_BINDING: &str = "TRUSTED_SERVER_KV";

const WRANGLER_MANIFESTS: [(&str, &str); 2] = [
    ("wrangler.toml", include_str!("../wrangler.toml")),
    ("wrangler.ci.toml", include_str!("../wrangler.ci.toml")),
];

/// The KV namespaces a Wrangler manifest declares, as binding and id.
fn kv_namespaces(manifest: &str) -> Vec<(String, String)> {
    let manifest: toml::Value =
        toml::from_str(manifest).expect("should parse the Wrangler manifest");
    manifest["kv_namespaces"]
        .as_array()
        .expect("should declare KV namespaces")
        .iter()
        .map(|namespace| {
            (
                namespace["binding"]
                    .as_str()
                    .expect("should name a binding")
                    .to_owned(),
                namespace["id"]
                    .as_str()
                    .expect("should name an id")
                    .to_owned(),
            )
        })
        .collect()
}

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
    assert_eq!(
        manifest["stores"]["config"]["adapters"]["cloudflare"]["name"].as_str(),
        Some(DEFAULT_CONFIG_STORE_ID),
        "the config store's binding should be its default platform name"
    );
}

#[test]
fn each_wrangler_manifest_binds_the_config_store_as_a_namespace_of_its_own() {
    for (name, manifest) in WRANGLER_MANIFESTS {
        let namespaces = kv_namespaces(manifest);
        let id_of = |binding: &str| {
            namespaces
                .iter()
                .find(|(bound, _)| bound == binding)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| panic!("{name} should bind `{binding}`: {namespaces:?}"))
        };

        assert_ne!(
            id_of(DEFAULT_CONFIG_STORE_ID),
            id_of(KV_STORE_BINDING),
            "{name} should keep the config store apart from the key-value store"
        );
    }
}

#[test]
fn no_wrangler_manifest_carries_settings_in_a_variable() {
    for (name, manifest) in WRANGLER_MANIFESTS {
        let manifest: toml::Value =
            toml::from_str(manifest).expect("should parse the Wrangler manifest");

        assert!(
            manifest
                .get("vars")
                .and_then(|vars| vars.get("TRUSTED_SERVER_CONFIG"))
                .is_none(),
            "{name} should leave the settings to the config store"
        );
    }
}
