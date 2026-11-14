//! EC record-store references and serving-runtime capability validation.
//!
//! Provider endpoints, credentials and table/namespace selection belong to the
//! prepared `EdgeZero` binding, not to identity-graph configuration.

use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::TrustedServerError;
use crate::settings::Ec;

/// The serving adapter, independent of the configuration tool's host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcRuntime {
    /// Fastly Compute.
    Fastly,
    /// Native Axum.
    Native,
    /// Cloudflare Workers.
    Cloudflare,
    /// Fermyon Spin.
    Spin,
}

/// A logical KV ID whose prepared binding must support the EC record layout.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EcRecordStorage {
    /// Exact logical store ID from the application's declared stores.
    pub store: String,
}

/// Portable record selection. Provider mechanics remain in `EdgeZero` bindings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum EcStorageProvider {
    /// Prepared record binding, usable on supported serving adapters.
    Records(EcRecordStorage),
    /// Compatibility spelling for a Fastly record-store reference.
    Fastly(EcRecordStorage),
}

/// Published storage input. Omitted providers retain Fastly shorthand semantics.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EcStorage {
    /// Explicit record selection.
    Explicit(EcStorageProvider),
    /// Legacy Fastly shorthand, resolved against the actual serving runtime.
    Implicit(EcRecordStorage),
}

impl<'de> Deserialize<'de> for EcStorage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("provider").is_some() {
            serde_json::from_value(value).map(Self::Explicit).map_err(|_| {
                serde::de::Error::custom("ec.storage requires provider='records' and a logical store ID; provider connection settings belong in EdgeZero bindings")
            })
        } else {
            serde_json::from_value(value).map(Self::Implicit).map_err(|_| {
                serde::de::Error::custom("ec.storage.store must name a logical record store; connector fields are not accepted")
            })
        }
    }
}

/// Validated logical reference, not permission to construct another provider.
#[derive(Debug, Clone, Copy)]
pub enum EcStorageSelection<'a> {
    /// A prepared record view.
    Records(&'a str),
    /// Legacy Fastly spelling of a record reference.
    Fastly(&'a str),
}

fn invalid(message: &'static str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.to_owned(),
    })
}

impl Ec {
    /// Validate configuration shape without consulting SDKs, credentials or I/O.
    ///
    /// # Errors
    /// Rejects conflicting legacy/new selections and empty logical store IDs.
    pub fn validate_storage(&self) -> Result<(), Report<TrustedServerError>> {
        if self.ec_store.is_some() && self.storage.is_some() {
            return Err(invalid(
                "ec.ec_store and ec.storage cannot both be configured",
            ));
        }
        let store = match &self.storage {
            Some(
                EcStorage::Implicit(config)
                | EcStorage::Explicit(
                    EcStorageProvider::Fastly(config) | EcStorageProvider::Records(config),
                ),
            ) => Some(config.store.as_str()),
            None => self.ec_store.as_deref(),
        };
        if store == Some("") {
            return Err(invalid("ec.storage.store must not be empty"));
        }
        Ok(())
    }

    /// Resolve the reference against the actual adapter's record capability.
    ///
    /// Acquisition must still validate the prepared handle and layout. This
    /// method never prepares a provider or changes a KV handle's routing.
    ///
    /// # Errors
    /// Rejects unsupported runtimes and Fastly shorthand outside Fastly.
    pub fn resolve_storage(
        &self,
        runtime: EcRuntime,
    ) -> Result<Option<EcStorageSelection<'_>>, Report<TrustedServerError>> {
        self.validate_storage()?;
        let Some(storage) = &self.storage else {
            return match (self.ec_store.as_deref(), runtime) {
                (None, _) => Ok(None),
                (Some(store), EcRuntime::Fastly) => Ok(Some(EcStorageSelection::Fastly(store))),
                (Some(_), _) => Err(invalid(
                    "legacy ec.ec_store is Fastly-only; select an explicit prepared record binding",
                )),
            };
        };
        match (storage, runtime) {
            (
                EcStorage::Explicit(EcStorageProvider::Records(config)),
                EcRuntime::Native | EcRuntime::Fastly,
            ) => Ok(Some(EcStorageSelection::Records(&config.store))),
            (
                EcStorage::Implicit(config)
                | EcStorage::Explicit(EcStorageProvider::Fastly(config)),
                EcRuntime::Fastly,
            ) => Ok(Some(EcStorageSelection::Fastly(&config.store))),
            (EcStorage::Implicit(_), _) => {
                Err(invalid("ec.storage.provider is required outside Fastly"))
            }
            _ => Err(invalid(
                "ec.storage.provider is unsupported by the selected serving runtime",
            )),
        }
    }
}
