//! Keep the checked-in Cloudflare configuration aligned with the runtime defaults.

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
