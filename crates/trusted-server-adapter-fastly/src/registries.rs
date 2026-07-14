//! Per-request store registries for the Fastly adapter.
//!
//! `EdgeZero`'s own Fastly registry builders (and `dispatch_with_registries`) are
//! private to `edgezero-adapter-fastly`, and this adapter drives its own
//! `oneshot` dispatch from `main.rs`. So the three registries are built here and
//! inserted into the request extensions, where
//! [`build_per_request_services`](crate::app) reads them back to construct the
//! registry-backed composite config/secret stores and the named-KV surface.
//!
//! Every store opens **by logical id** (decision D7 — the logical id equals the
//! physical Fastly store name; an operator who needs a different physical name
//! remaps it at provisioning time, not at runtime).
//!
//! # Failure policy
//!
//! A per-store open failure is **non-fatal**: the id is logged and dropped from
//! the registry. Eagerly opening every declared id and failing the request when
//! any one of them is unavailable would let a single unprovisioned or deprecated
//! store (e.g. `creative_store`) break all traffic, converting Fastly's lazy,
//! per-route fail-closed model into eager fail-everything. When the **default**
//! id cannot be assembled, [`StoreRegistry::from_parts`] yields `None` and the
//! registry is not inserted at all; the strict composite then errors on first
//! read rather than silently falling back to a default store.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_adapter_fastly::config_store::FastlyConfigStore;
use edgezero_adapter_fastly::key_value_store::FastlyKvStore;
use edgezero_adapter_fastly::secret_store::FastlySecretStore;
use edgezero_core::app::StoresMetadata;
use edgezero_core::config_store::{
    ConfigStore as EdgeConfigStore, ConfigStoreError, ConfigStoreHandle,
};
use edgezero_core::key_value_store::KvHandle;
use edgezero_core::secret_store::SecretHandle;
use edgezero_core::store_registry::{
    BoundSecretStore, ConfigRegistry, ConfigStoreBinding, KvRegistry, SecretRegistry, StoreRegistry,
};
use fastly::config_store::OpenError;
use fastly::ConfigStore;

/// Plain key/value reader over a Fastly Config Store.
///
/// Deliberately **not** `EdgeZero`'s [`FastlyConfigStore`]: that store treats
/// every value as an app-config `BlobEnvelope` (or a chunk pointer to one) and
/// rejects anything else as corrupt platform state. That is right for the
/// app-config blob store and wrong for every other config store Trusted Server
/// declares, whose values are plain scalars — `jwks_store` holds
/// `active-kids`/`current-kid` strings and raw JWK JSON, `datadome_ip_bypass`
/// holds CIDR entries. Reading those through the envelope-aware store fails with
/// "value at key `…` is neither a valid `BlobEnvelope` nor a valid chunk
/// pointer", which would break discovery, signature verification, key rotation,
/// and the `DataDome` IP bypass.
///
/// This reads exactly as
/// [`FastlyPlatformConfigStore::get`](crate::platform::FastlyPlatformConfigStore)
/// does today, so the registry cutover is behavior-preserving.
///
/// **Do not collapse the two store kinds into one.** See
/// [`build_config_registry`] for which id gets which.
struct PlainFastlyConfigStore {
    inner: ConfigStore,
}

impl PlainFastlyConfigStore {
    /// Open the Fastly Config Store named `name` for plain reads.
    fn try_open(name: &str) -> Result<Self, OpenError> {
        ConfigStore::try_open(name).map(|inner| Self { inner })
    }
}

#[async_trait(?Send)]
impl EdgeConfigStore for PlainFastlyConfigStore {
    async fn get(&self, key: &str) -> Result<Option<String>, ConfigStoreError> {
        self.inner.try_get(key).map_err(|error| {
            ConfigStoreError::internal(io::Error::other(format!(
                "config store lookup failed for `{key}`: {error}"
            )))
        })
    }
}

/// Build the config registry, opening one Fastly Config Store per declared id.
///
/// The two kinds of config store bind to **different** readers, because their
/// contents differ:
///
/// - the **default** id (`trusted_server_config`) holds the app-config
///   `BlobEnvelope` — possibly split across chunk entries — so it binds to
///   `EdgeZero`'s chunk-aware [`FastlyConfigStore`], which reassembles and
///   verifies it transparently;
/// - every **non-default** id (`jwks_store`, `datadome_ip_bypass`) holds plain
///   scalar values, so it binds to [`PlainFastlyConfigStore`]. The envelope-aware
///   store would reject those values as corrupt (see its doc comment) — this
///   split is load-bearing, not an accident.
///
/// Under D7 each binding's `default_key` is the logical id itself. A store that
/// fails to open is logged and dropped; returns `None` when no config stores are
/// declared or the default id could not be opened.
#[must_use]
pub(crate) fn build_config_registry(stores: &StoresMetadata) -> Option<ConfigRegistry> {
    let meta = stores.config?;
    let mut by_id: BTreeMap<String, ConfigStoreBinding> = BTreeMap::new();
    for id in meta.ids {
        let handle = if *id == meta.default {
            match FastlyConfigStore::try_open(id) {
                Ok(store) => ConfigStoreHandle::new(Arc::new(store)),
                Err(error) => {
                    log::warn!(
                        "Fastly app-config store `{id}` could not be opened: {error}; \
                         dropping it from the registry"
                    );
                    continue;
                }
            }
        } else {
            match PlainFastlyConfigStore::try_open(id) {
                Ok(store) => ConfigStoreHandle::new(Arc::new(store)),
                Err(error) => {
                    log::warn!(
                        "Fastly config store `{id}` could not be opened: {error}; \
                         dropping it from the registry"
                    );
                    continue;
                }
            }
        };
        by_id.insert(
            (*id).to_owned(),
            ConfigStoreBinding {
                handle,
                default_key: (*id).to_owned(),
            },
        );
    }
    warn_if_default_missing("config", &by_id, meta.default);
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}

/// Build the KV registry, opening one Fastly KV Store per declared id.
///
/// A per-store open failure is non-fatal (see the module-level failure policy).
/// Returns `None` when no KV stores are declared or the default id could not be
/// opened.
#[must_use]
pub(crate) fn build_kv_registry(stores: &StoresMetadata) -> Option<KvRegistry> {
    let meta = stores.kv?;
    let mut by_id: BTreeMap<String, KvHandle> = BTreeMap::new();
    for id in meta.ids {
        match FastlyKvStore::open(id) {
            Ok(store) => {
                by_id.insert((*id).to_owned(), KvHandle::new(Arc::new(store)));
            }
            Err(error) => log::warn!(
                "Fastly KV store `{id}` could not be opened: {error}; \
                 dropping it from the registry"
            ),
        }
    }
    warn_if_default_missing("KV", &by_id, meta.default);
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}

/// Build the secret registry.
///
/// [`FastlySecretStore`] is stateless — it opens the named Fastly Secret Store
/// on each `get_bytes(store_name, key)` call — so one provider handle is shared
/// across every binding and each declared id is bound to its own store name
/// (D7: the logical id). Returns `None` when no secret stores are declared.
#[must_use]
pub(crate) fn build_secret_registry(stores: &StoresMetadata) -> Option<SecretRegistry> {
    let meta = stores.secrets?;
    let handle = SecretHandle::new(Arc::new(FastlySecretStore));
    let mut by_id: BTreeMap<String, BoundSecretStore> = BTreeMap::new();
    for id in meta.ids {
        by_id.insert(
            (*id).to_owned(),
            BoundSecretStore::new(handle.clone(), (*id).to_owned()),
        );
    }
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}

/// Logs when the default id of a kind could not be assembled, which drops the
/// whole registry.
fn warn_if_default_missing<H>(kind: &str, by_id: &BTreeMap<String, H>, default_id: &str) {
    if !by_id.contains_key(default_id) {
        log::warn!(
            "Fastly {kind} registry default id `{default_id}` could not be opened; \
             dropping the {kind} registry"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{build_config_registry, build_kv_registry, build_secret_registry};
    use trusted_server_core::stores::STORES_METADATA;

    #[test]
    fn build_config_registry_resolves_declared_ids() {
        // Arrange + Act: build from the shared store metadata against the
        // Viceroy-provisioned config stores.
        let registry =
            build_config_registry(&STORES_METADATA).expect("should build the config registry");

        // Assert: the default id and a declared non-default id both resolve.
        assert!(
            registry.default().is_some(),
            "the default config store id should resolve"
        );
        assert_eq!(
            registry.default_id(),
            "trusted_server_config",
            "the registry default id should come from the shared store metadata"
        );
        assert!(
            registry.named("jwks_store").is_some(),
            "the declared non-default `jwks_store` id should resolve"
        );

        // An id that is not declared is strictly absent — never a fallback.
        assert!(
            registry.named("nope").is_none(),
            "an undeclared config id should not resolve"
        );
    }

    #[test]
    fn config_registry_reads_plain_values_from_non_default_stores() {
        // Regression guard for the envelope-only `FastlyConfigStore`: non-default
        // config stores hold plain scalars (here `jwks_store`'s `active-kids`),
        // not app-config BlobEnvelopes. Binding them to EdgeZero's envelope-aware
        // store makes every such read fail as "corrupt", which would break
        // discovery, signature verification, key rotation, and the DataDome IP
        // bypass. They must read back verbatim.
        let registry =
            build_config_registry(&STORES_METADATA).expect("should build the config registry");
        let binding = registry
            .named("jwks_store")
            .expect("the declared `jwks_store` id should resolve");

        let value = futures::executor::block_on(binding.handle.get("active-kids"))
            .expect("a plain config value should read without envelope validation")
            .expect("`active-kids` should be present in the jwks_store");

        assert!(
            value.contains("ts-2025-10-A"),
            "should return the plain config value verbatim, not an envelope-decode error"
        );
    }

    #[test]
    fn build_kv_registry_resolves_declared_ids() {
        let registry = build_kv_registry(&STORES_METADATA).expect("should build the KV registry");

        assert!(
            registry.named("consent_store").is_some(),
            "the declared `consent_store` id should resolve so consent can select it by name"
        );
        assert!(
            registry.named("nope").is_none(),
            "an undeclared KV id should not resolve"
        );
    }

    #[test]
    fn build_secret_registry_binds_every_declared_id() {
        let registry =
            build_secret_registry(&STORES_METADATA).expect("should build the secret registry");

        assert!(
            registry.named("signing_keys").is_some(),
            "the declared `signing_keys` id should bind to its own Fastly secret store"
        );
        assert!(
            registry.named("nope").is_none(),
            "an undeclared secret id should not resolve"
        );
    }
}
