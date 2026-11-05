//! Pure EC persistence configuration and serving-runtime resolution.

use edgezero_store_celld::settings::CelldKvSettings;
use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::TrustedServerError;
use crate::settings::Ec;

/// The serving adapter, independent of the host running configuration tooling.
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

/// A named Fastly binding without an inferred provider in published config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FastlyEcStorage {
    /// Exact Fastly binding name.
    pub store: String,
}

/// Single-region `DynamoDB` configuration. Credentials remain outside app config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DynamoDbEcStorage {
    /// Explicit AWS region.
    pub region: String,
    /// Existing retained table.
    pub table: String,
    /// Exact stable namespace, without normalization.
    pub namespace: String,
    /// Explicit loopback emulator endpoint, never a production override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<String>,
}

/// Explicit provider configuration, validated without SDKs or network access.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum EcStorageProvider {
    /// Fastly's conditional KV binding.
    Fastly(FastlyEcStorage),
    /// Native single-region `DynamoDB`.
    DynamoDb(DynamoDbEcStorage),
    /// Native scoped conditional records, not ordinary KV.
    Celld(CelldKvSettings),
}

/// Published storage input. An omitted provider stays omitted on serialization.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EcStorage {
    /// Explicit provider selection.
    Explicit(EcStorageProvider),
    /// Fastly shorthand, resolved only when the serving runtime is known.
    Implicit(FastlyEcStorage),
}

impl<'de> Deserialize<'de> for EcStorage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("provider").is_some() {
            // Report field names, never raw serde input that could contain an
            // endpoint or an accidentally supplied credential value.
            let fields = match value.get("provider").and_then(serde_json::Value::as_str) {
                Some("fastly") => {
                    "ec.storage.store is required for fastly; other provider fields are forbidden"
                }
                Some("dynamodb") => {
                    "ec.storage.region, ec.storage.table and ec.storage.namespace are required for dynamodb; only ec.storage.local_endpoint is optional"
                }
                Some("celld") => {
                    "ec.storage.endpoint, ec.storage.store and ec.storage.access are required for celld; access requires a valid mode and credential reference, not a token value"
                }
                _ => "ec.storage.provider must be fastly, dynamodb or celld",
            };
            serde_json::from_value(value)
                .map(Self::Explicit)
                .map_err(|_| serde::de::Error::custom(fields))
        } else {
            serde_json::from_value(value).map(Self::Implicit).map_err(|_| {
                serde::de::Error::custom("ec.storage.store is required; provider-specific fields require an explicit provider")
            })
        }
    }
}

/// Validated selection for adapter-owned provider construction.
#[derive(Debug, Clone, Copy)]
pub enum EcStorageSelection<'a> {
    /// Exact Fastly binding.
    Fastly(&'a str),
    /// Native `DynamoDB` settings.
    DynamoDb(&'a DynamoDbEcStorage),
    /// Native celld settings, without a captured token.
    Celld(&'a CelldKvSettings),
}

fn invalid(message: &'static str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.to_owned(),
    })
}

impl DynamoDbEcStorage {
    /// Validate exact bindings and the explicit local-only endpoint shape.
    ///
    /// # Errors
    /// Returns field-oriented configuration errors without echoing input values.
    pub fn validate(&self) -> Result<(), Report<TrustedServerError>> {
        if self.region.is_empty() || self.region.chars().any(char::is_whitespace) {
            return Err(invalid(
                "ec.storage.region is required and must not contain whitespace",
            ));
        }
        if !(3..=255).contains(&self.table.len())
            || !self
                .table
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(invalid(
                "ec.storage.table must be a valid DynamoDB table name",
            ));
        }
        if self.namespace.is_empty()
            || self.namespace.len() > 512
            || self.namespace.chars().any(char::is_control)
        {
            return Err(invalid(
                "ec.storage.namespace must contain 1 through 512 bytes without control characters",
            ));
        }
        if let Some(endpoint) = &self.local_endpoint {
            let url=url::Url::parse(endpoint).map_err(|_|invalid("ec.storage.local_endpoint must be a loopback HTTP root URL with an explicit port"))?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.port().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid(
                    "ec.storage.local_endpoint must be a loopback HTTP root URL with an explicit port",
                ));
            }
        }
        Ok(())
    }
}

impl Ec {
    /// Validate publication shape without selecting according to the CLI host.
    ///
    /// # Errors
    /// Rejects simultaneous legacy/new input or malformed provider bindings.
    pub fn validate_storage(&self) -> Result<(), Report<TrustedServerError>> {
        if self.ec_store.is_some() && self.storage.is_some() {
            return Err(invalid(
                "ec.ec_store and ec.storage cannot both be configured",
            ));
        }
        match &self.storage {
            Some(
                EcStorage::Implicit(config)
                | EcStorage::Explicit(EcStorageProvider::Fastly(config)),
            ) if config.store.is_empty() => Err(invalid("ec.storage.store must not be empty")),
            Some(EcStorage::Explicit(EcStorageProvider::DynamoDb(config))) => config.validate(),
            Some(EcStorage::Explicit(EcStorageProvider::Celld(config))) => config
                .validate()
                .map_err(|_| invalid("ec.storage celld endpoint, store or access is invalid")),
            _ => Ok(()),
        }
    }

    /// Resolve validated provider input against the actual serving adapter.
    ///
    /// # Errors
    /// Rejects unsupported explicit providers and omitted providers off Fastly.
    pub fn resolve_storage(
        &self,
        runtime: EcRuntime,
    ) -> Result<Option<EcStorageSelection<'_>>, Report<TrustedServerError>> {
        self.validate_storage()?;
        let Some(storage) = &self.storage else {
            return Ok(if runtime == EcRuntime::Fastly {
                self.ec_store.as_deref().map(EcStorageSelection::Fastly)
            } else {
                None
            });
        };
        match (storage, runtime) {
            (
                EcStorage::Implicit(config)
                | EcStorage::Explicit(EcStorageProvider::Fastly(config)),
                EcRuntime::Fastly,
            ) => Ok(Some(EcStorageSelection::Fastly(&config.store))),
            (EcStorage::Explicit(EcStorageProvider::DynamoDb(config)), EcRuntime::Native) => {
                Ok(Some(EcStorageSelection::DynamoDb(config)))
            }
            (EcStorage::Explicit(EcStorageProvider::Celld(config)), EcRuntime::Native) => {
                Ok(Some(EcStorageSelection::Celld(config)))
            }
            (EcStorage::Implicit(_), _) => {
                Err(invalid("ec.storage.provider is required outside Fastly"))
            }
            _ => Err(invalid(
                "ec.storage.provider is unsupported by the selected serving runtime",
            )),
        }
    }
}
