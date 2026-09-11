//! Portable logical store metadata for the Trusted Server application.
//!
//! [`STORES_METADATA`] is the single source of truth for the logical store ids
//! the app declares, mirroring the `[stores.*]` tables in the workspace-root
//! `edgezero.toml`. Every adapter's `Hooks::stores()` returns this same const,
//! so the `EdgeZero` store registry is wired identically across the Fastly, Axum,
//! Cloudflare, and Spin runtimes. The anti-drift test in this module asserts the
//! const and the manifest never diverge.
//!
//! The `DataDome` IP-CIDR config store (`datadome_ip_bypass`) is declared here
//! as an underscore logical id. `EdgeZero`'s manifest validator requires store
//! ids to match `[A-Za-z0-9_]` (they become `EDGEZERO__STORES__…` env
//! segments), so the previously hyphenated names were converged onto
//! underscores. Under the D7 convention the logical id equals the physical
//! platform store name, so each maps to a same-named physical store; an
//! operator whose physical store keeps a different name overrides it out of
//! band with `EDGEZERO__STORES__<KIND>__<ID>__NAME`.

use edgezero_core::app::{StoreMetadata, StoresMetadata};

/// Secret store ids resolved at runtime but deliberately NOT declared in
/// `edgezero.toml` `[stores.secrets]`.
///
/// The `EdgeZero` v0.0.8 CLI capability matrix marks the Axum, Cloudflare, and
/// Spin adapters Single-capable for secrets (their secret backends are flat
/// namespaces), and `ts config push` runs that check across every adapter the
/// manifest declares — so `[stores.secrets].ids` may hold only the default id.
/// The request-signing private-key store is provisioned on the management path
/// (never by `ts config push`), so it is bound registry-locally instead: the
/// Fastly and Axum registry builders add these ids on top of
/// [`STORES_METADATA`]. On adapters whose registries come from `run_app`
/// (Cloudflare, Spin) these ids do not resolve, and reads fail closed.
pub const RUNTIME_ONLY_SECRET_IDS: &[&str] = &["signing_keys"];

/// Logical store metadata declared by Trusted Server, shared by every adapter's
/// [`edgezero_core::app::Hooks::stores`] implementation.
///
/// The `default` of each kind is the general-purpose registry slot; the named
/// ids carry the real reads (request signing, EC identity, consent, Tinybird).
/// Keep this in lockstep with `edgezero.toml` — the
/// `stores_metadata_matches_edgezero_manifest` test enforces it.
pub const STORES_METADATA: StoresMetadata = StoresMetadata {
    config: Some(StoreMetadata {
        default: "trusted_server_config",
        ids: &["trusted_server_config", "jwks_store", "datadome_ip_bypass"],
    }),
    kv: Some(StoreMetadata {
        default: "trusted_server_kv",
        ids: &[
            "trusted_server_kv",
            "ec_identity_store",
            "consent_store",
            "creative_store",
        ],
    }),
    // Only the default id is declared: the v0.0.8 CLI rejects multi-id
    // `[stores.secrets]` while any Single-capable adapter (axum, cloudflare,
    // spin) is declared in the manifest. The management-provisioned
    // request-signing store binds registry-locally via
    // [`RUNTIME_ONLY_SECRET_IDS`] instead. The retired `ts_secrets` and
    // `s3_auth` logical ids are gone: since mainline #1036, DataDome/S3/
    // Tinybird credentials resolve at startup from the default store, and
    // `trusted_server_secrets` maps to a differently named physical store
    // (e.g. Fastly `ts_secrets`) via `EDGEZERO__STORES__SECRETS__…__NAME`.
    secrets: Some(StoreMetadata {
        default: "trusted_server_secrets",
        ids: &["trusted_server_secrets"],
    }),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn read_manifest() -> toml::Value {
        let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../edgezero.toml"));
        toml::from_str(raw).expect("should parse edgezero.toml")
    }

    fn assert_kind(manifest: &toml::Value, kind: &str, meta: &StoreMetadata) {
        let decl = manifest
            .get("stores")
            .and_then(|stores| stores.get(kind))
            .unwrap_or_else(|| panic!("should declare [stores.{kind}] in edgezero.toml"));
        let ids: Vec<&str> = decl
            .get("ids")
            .and_then(toml::Value::as_array)
            .unwrap_or_else(|| panic!("should have ids array for [stores.{kind}]"))
            .iter()
            .map(|id| {
                id.as_str()
                    .unwrap_or_else(|| panic!("ids entry for [stores.{kind}] should be a string"))
            })
            .collect();
        let default = decl
            .get("default")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("should have default for [stores.{kind}]"));
        assert_eq!(
            ids.as_slice(),
            meta.ids,
            "ids for [stores.{kind}] should match STORES_METADATA"
        );
        assert_eq!(
            default, meta.default,
            "default for [stores.{kind}] should match STORES_METADATA"
        );
    }

    #[test]
    fn stores_metadata_matches_edgezero_manifest() {
        let manifest = read_manifest();
        assert_kind(
            &manifest,
            "config",
            &STORES_METADATA
                .config
                .expect("should declare config stores"),
        );
        assert_kind(
            &manifest,
            "kv",
            &STORES_METADATA.kv.expect("should declare kv stores"),
        );
        assert_kind(
            &manifest,
            "secrets",
            &STORES_METADATA
                .secrets
                .expect("should declare secrets stores"),
        );
    }

    /// Pins the push-to-boot contract: the key `ts config push` writes must be
    /// the key the runtime boot read looks up.
    ///
    /// `ts config push` defaults the blob key to the logical store id (only an
    /// explicit `--key` overrides it), and the `EdgeZero` config registry binds
    /// each store with `default_key` = its logical id. So the app-config blob
    /// key must equal the declared `[stores.config]` default. If these drift,
    /// `ts config push` writes one key while boot reads another and startup
    /// fails with "config key not found" — the exact end-to-end break this test
    /// exists to prevent.
    #[test]
    fn config_blob_key_matches_declared_config_store() {
        let config = STORES_METADATA
            .config
            .expect("should declare config stores");

        assert_eq!(
            crate::config_payload::CONFIG_BLOB_KEY,
            config.default,
            "app-config blob key must equal the declared config store id so `ts config push` \
             (which defaults the key to the logical store id) writes what boot reads"
        );
        assert!(
            config.ids.contains(&crate::config_payload::CONFIG_BLOB_KEY),
            "the app-config blob key must be a declared config store id"
        );
    }
}
