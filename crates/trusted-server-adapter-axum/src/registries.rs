//! Per-request store registries for the Axum dev server.
//!
//! These builders construct the `EdgeZero` [`ConfigRegistry`], [`KvRegistry`],
//! and [`SecretRegistry`] from the shared
//! [`StoresMetadata`](edgezero_core::app::StoresMetadata) so that
//! [`build_runtime_services`](crate::platform::build_runtime_services) can
//! resolve non-default logical store ids (e.g. `jwks_store`, `consent_store`)
//! through the registry-backed composite stores.
//!
//! The registries are attached to the dev server via
//! [`AxumDevServer::with_config_registry`](edgezero_adapter_axum::dev_server::AxumDevServer::with_config_registry)
//! and its `with_kv_registry`/`with_secret_registry` siblings, which insert them
//! into each request's extensions.
//!
//! **KV redb path.** Each KV id opens a deterministic
//! `.edgezero/kv-<id>.redb` database via the public
//! [`PersistentKvStore::new`] constructor. Because Trusted Server supplies the
//! whole registry to its own dev server (the dev server does not build one of
//! its own), this registry is authoritative for local KV — so the file name need
//! not match `EdgeZero`'s private `.edgezero/kv-<slug>-<hash>.redb` scheme, and
//! no path-parity test is required.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edgezero_adapter_axum::config_store::AxumConfigStore;
use edgezero_adapter_axum::key_value_store::PersistentKvStore;
use edgezero_adapter_axum::secret_store::EnvSecretStore;
use edgezero_core::app::StoresMetadata;
use edgezero_core::config_store::{ConfigStoreError, ConfigStoreHandle};
use edgezero_core::key_value_store::KvHandle;
use edgezero_core::secret_store::SecretHandle;
use edgezero_core::store_registry::{
    BoundSecretStore, ConfigRegistry, ConfigStoreBinding, KvRegistry, SecretRegistry, StoreRegistry,
};

/// Environment variable naming an explicit JSON config-store file for the
/// default app-config store, mirroring the boot-time override in
/// [`crate::app`]. It is a file-location pointer only; it never carries a config
/// value and applies only to the default config id.
const AXUM_CONFIG_PATH_ENV: &str = "TRUSTED_SERVER_AXUM_CONFIG_PATH";

/// Directory holding the dev server's local redb KV databases.
const KV_DIR: &str = ".edgezero";

/// Build the config registry from `stores`, one file-backed store per declared
/// config id.
///
/// Each id reads its `.edgezero/local-config-<id>.json` file; the default
/// app-config id additionally honors [`AXUM_CONFIG_PATH_ENV`]. A store that
/// fails to open is logged and dropped from the registry rather than aborting
/// startup. Returns `None` when no config stores are declared or the default id
/// could not be opened.
#[must_use]
pub fn build_config_registry_axum(stores: &StoresMetadata) -> Option<ConfigRegistry> {
    let meta = stores.config?;
    let mut by_id: BTreeMap<String, ConfigStoreBinding> = BTreeMap::new();
    for id in meta.ids {
        let store = match open_config_store(id, meta.default) {
            Ok(store) => store,
            Err(error) => {
                log::warn!(
                    "Axum config store `{id}` could not be opened: {error}; \
                     dropping it from the registry"
                );
                continue;
            }
        };
        by_id.insert(
            (*id).to_owned(),
            ConfigStoreBinding {
                handle: ConfigStoreHandle::new(Arc::new(store)),
                default_key: (*id).to_owned(),
            },
        );
    }
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}

/// Open the file-backed config store for `id`.
///
/// Only the default app-config store honors [`AXUM_CONFIG_PATH_ENV`], matching
/// the boot-time read in [`crate::app`]; every other id reads its own
/// `.edgezero/local-config-<id>.json` file.
fn open_config_store(id: &str, default_id: &str) -> Result<AxumConfigStore, ConfigStoreError> {
    if id == default_id
        && let Ok(path) = std::env::var(AXUM_CONFIG_PATH_ENV)
    {
        return AxumConfigStore::from_path(Path::new(&path));
    }
    AxumConfigStore::from_local_file(id)
}

/// Build the KV registry from `stores`, one redb-backed store per declared KV
/// id.
///
/// Each id opens a deterministic `.edgezero/kv-<id>.redb` database. A store that
/// fails to open is logged and dropped rather than aborting startup. Returns
/// `None` when no KV stores are declared or the default id could not be opened.
#[must_use]
pub fn build_kv_registry_axum(stores: &StoresMetadata) -> Option<KvRegistry> {
    let meta = stores.kv?;
    if let Err(error) = std::fs::create_dir_all(KV_DIR) {
        log::warn!("could not create `{KV_DIR}` directory for Axum KV stores: {error}");
    }
    let mut by_id: BTreeMap<String, KvHandle> = BTreeMap::new();
    for id in meta.ids {
        let path = kv_path(id);
        match PersistentKvStore::new(&path) {
            Ok(store) => {
                by_id.insert((*id).to_owned(), KvHandle::new(Arc::new(store)));
            }
            Err(error) => {
                log::warn!(
                    "Axum KV store `{id}` could not be opened at {}: {error}; \
                     dropping it from the registry",
                    path.display()
                );
            }
        }
    }
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}

/// Deterministic redb path for KV store `id`.
fn kv_path(id: &str) -> PathBuf {
    Path::new(KV_DIR).join(format!("kv-{id}.redb"))
}

/// Build the secret registry from `stores`.
///
/// Axum reads secrets from environment variables via the public
/// [`EnvSecretStore`], so every declared id binds to the same env-backed store;
/// the store ignores the platform name on lookup. Returns `None` when no secret
/// stores are declared.
#[must_use]
pub fn build_secret_registry_axum(stores: &StoresMetadata) -> Option<SecretRegistry> {
    let meta = stores.secrets?;
    let handle = SecretHandle::new(Arc::new(EnvSecretStore::new()));
    let mut by_id: BTreeMap<String, BoundSecretStore> = BTreeMap::new();
    for id in meta.ids {
        by_id.insert(
            (*id).to_owned(),
            BoundSecretStore::new(handle.clone(), (*id).to_owned()),
        );
    }
    StoreRegistry::from_parts(by_id, meta.default.to_owned())
}
