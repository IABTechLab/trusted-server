//! Portable logical store metadata for the Trusted Server application.
//!
//! [`STORES_METADATA`] is the single source of truth for the logical store ids
//! the app declares, mirroring the `[stores.*]` tables in the workspace-root
//! `edgezero.toml`. Every adapter's `Hooks::stores()` returns this same const,
//! so the EdgeZero store registry is wired identically across the Fastly, Axum,
//! Cloudflare, and Spin runtimes. The anti-drift test in this module asserts the
//! const and the manifest never diverge.

use edgezero_core::app::{StoreMetadata, StoresMetadata};

/// Logical store metadata declared by Trusted Server, shared by every adapter's
/// [`edgezero_core::app::Hooks::stores`] implementation.
///
/// The `default` of each kind is the general-purpose registry slot; the named
/// ids carry the real reads (request signing, EC identity, consent, DataDome,
/// Tinybird, S3). Keep this in lockstep with `edgezero.toml` — the
/// `stores_metadata_matches_edgezero_manifest` test enforces it.
pub const STORES_METADATA: StoresMetadata = StoresMetadata {
    config: Some(StoreMetadata {
        default: "trusted_server_config",
        ids: &["trusted_server_config", "jwks_store", "datadome-ip-bypass"],
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
    secrets: Some(StoreMetadata {
        default: "trusted_server_secrets",
        ids: &[
            "trusted_server_secrets",
            "signing_keys",
            "ts_secrets",
            "s3-auth",
        ],
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
}
