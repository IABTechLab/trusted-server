//! Strict native initialization from prepared stores.
//!
//! This finite snapshot bridge exists until core's platform store traits become
//! asynchronous. No provider is polled by synchronous request consumers.

use std::collections::BTreeMap;
use std::sync::Arc;

use edgezero_adapter_axum::dev_server::PreparedStores;
use edgezero_core::app::App;
use edgezero_core::error::EdgeError;
use error_stack::Report;
use trusted_server_core::config_payload::{
    DEFAULT_CONFIG_STORE_ID, DEFAULT_SECRET_STORE_ID, VerifiedConfigBlob,
};
use trusted_server_core::integrations::datadome::{DataDomeConfig, ProtectionMatcherConfig};
use trusted_server_core::platform::{
    PlatformConfigStore, PlatformError, PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableHttpClient, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;

use crate::app::build_state_with_settings;
use crate::platform::{AxumPlatformBackend, AxumPlatformGeo};

/// Immutable inputs shared by per-request native runtime services.
///
/// Only declared stores and explicitly loaded keys exist in these snapshots.
/// Missing keys and all management writes fail rather than consulting legacy
/// environment variables or opening another provider.
pub struct NativeStoreSnapshot {
    pub(crate) config_store: Arc<dyn PlatformConfigStore>,
    pub(crate) secret_store: Arc<dyn PlatformSecretStore>,
}

struct ConfigSnapshot {
    key: String,
    envelope: String,
}

impl PlatformConfigStore for ConfigSnapshot {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        if store_name.as_ref() == DEFAULT_CONFIG_STORE_ID && !self.key.is_empty() && key == self.key
        {
            Ok(self.envelope.clone())
        } else {
            Err(Report::new(PlatformError::ConfigStore)
                .attach("config key is not present in the immutable native startup snapshot"))
        }
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(read_only_config())
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(read_only_config())
    }
}

struct SecretSnapshot(BTreeMap<String, Vec<u8>>);

impl PlatformSecretStore for SecretSnapshot {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        if store_name.as_ref() == DEFAULT_SECRET_STORE_ID
            && let Some(value) = self.0.get(key)
        {
            return Ok(value.clone());
        }
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret key is not present in the immutable native startup snapshot"))
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(read_only_secret())
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(read_only_secret())
    }
}

/// Explicit-settings router seam. Network/store-dependent tests must supply
/// services through `routes_with_settings_and_services` instead. No env fallback.
pub(crate) fn explicit_settings_services() -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(ConfigSnapshot {
            key: String::new(),
            envelope: String::new(),
        }))
        .secret_store(Arc::new(SecretSnapshot(BTreeMap::new())))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(AxumPlatformBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(AxumPlatformGeo))
        .client_info(Default::default())
        .build()
}

fn read_only_config() -> Report<PlatformError> {
    Report::new(PlatformError::ConfigStore)
        .attach("native startup snapshots are read-only; publish inputs and restart")
}

fn read_only_secret() -> Report<PlatformError> {
    Report::new(PlatformError::SecretStore)
        .attach("native startup snapshots are read-only; publish inputs and restart")
}

fn configuration_error(message: &str) -> EdgeError {
    EdgeError::validation(message)
}

fn settings_error(error: Report<trusted_server_core::error::TrustedServerError>) -> EdgeError {
    // Core configuration errors deliberately omit secret values. Do not attach
    // provider errors or a serialized config to native startup diagnostics.
    let message = error.current_context().to_string();
    drop(error);
    configuration_error(&message)
}

fn validate_snapshot_consumers(settings: &Settings) -> Result<(), EdgeError> {
    let origin = settings
        .publisher
        .origin_url
        .parse::<edgezero_core::http::Uri>()
        .map_err(|_| {
            configuration_error("publisher.origin_url must be an absolute HTTP or HTTPS URI")
        })?;
    if !matches!(origin.scheme_str(), Some("http" | "https")) || origin.authority().is_none() {
        return Err(configuration_error(
            "publisher.origin_url must be an absolute HTTP or HTTPS URI",
        ));
    }
    if settings
        .request_signing
        .as_ref()
        .is_some_and(|signing| signing.enabled)
    {
        return Err(configuration_error(
            "native snapshots do not support request_signing.enabled; disable signing or add declared jwks_store/signing_keys snapshots with KID and key-pair validation before readiness",
        ));
    }
    if let Some(datadome) = settings
        .integrations
        .get_typed::<DataDomeConfig>("datadome")
        .map_err(settings_error)?
        && datadome.enable_protection
        && (!datadome.protection_excluded_ip_cidr_sources.is_empty()
            || datadome.protection_exclusion_rules.iter().any(|rule| {
                rule.enabled && matches!(rule.matcher, ProtectionMatcherConfig::IpCidrSource { .. })
            }))
    {
        return Err(configuration_error(
            "native snapshots do not support DataDome dynamic Config Store CIDR sources; use inline protection_excluded_ip_cidrs/ip_cidr rules or disable those sources before starting",
        ));
    }
    Ok(())
}

/// Load and validate the finite native inputs before the framework listens.
///
/// The framework retains ownership of the App, clock, policies, listener,
/// readiness, and signal/drain lifecycle. Selected-provider failures are fatal.
///
/// # Errors
/// Returns an error for missing bindings/values, bounded-read failures, invalid
/// envelopes or runtime settings, unsupported store consumers, or invalid plans.
pub async fn initialize_native(app: &mut App, stores: &PreparedStores) -> Result<(), EdgeError> {
    let limits = app.config_extraction_limits().validate()?;
    let clock = app.monotonic_clock();
    let deadline = clock.deadline_after(limits.timeout);
    let binding = stores
        .config()
        .and_then(|registry| registry.named_ref(DEFAULT_CONFIG_STORE_ID))
        .ok_or_else(|| {
            configuration_error(
                "required declared config store trusted_server_config is not prepared",
            )
        })?;
    let secrets = stores
        .secrets()
        .and_then(|registry| registry.named_ref(DEFAULT_SECRET_STORE_ID))
        .ok_or_else(|| {
            configuration_error(
                "required declared secret store trusted_server_secrets is not prepared",
            )
        })?;

    let config_read = binding.handle.get_bounded(
        &binding.default_key,
        &clock,
        deadline,
        limits.max_backend_bytes.min(limits.max_total_bytes),
        limits.max_blob_bytes.min(limits.max_total_bytes),
    ).await.map_err(|_| configuration_error(
        "failed bounded read of prepared Trusted Server config; check the selected provider, binding key, extraction limits and deadline; legacy environment fallback is disabled",
    ))?;
    let mut remaining = limits
        .max_total_bytes
        .checked_sub(config_read.backend_bytes)
        .ok_or_else(|| {
            configuration_error("native config extraction exceeds the total byte budget")
        })?;
    let envelope = config_read.value.ok_or_else(|| configuration_error(
        "Trusted Server config envelope is missing at the prepared binding key; publish a TS BlobEnvelope before starting",
    ))?;
    let verified = VerifiedConfigBlob::parse(envelope.as_ref()).map_err(settings_error)?;
    verified
        .check_structure(limits.max_json_depth, limits.max_json_nodes)
        .map_err(settings_error)?;
    let keys = verified.required_secret_keys().map_err(settings_error)?;
    let mut values = BTreeMap::new();
    for key in keys {
        if remaining == 0 {
            return Err(configuration_error(
                "native secret extraction exhausts the total byte budget",
            ));
        }
        let read = secrets.get_bytes_bounded(
            &key,
            &clock,
            deadline,
            limits.max_backend_bytes.min(remaining),
            limits.max_secret_bytes.min(remaining),
        ).await.map_err(|_| configuration_error(
            "failed bounded read of a required app-config secret; check the selected trusted_server_secrets provider, required references, extraction limits and deadline; legacy environment fallback is disabled",
        ))?;
        remaining = remaining.checked_sub(read.backend_bytes).ok_or_else(|| {
            configuration_error("native secret extraction exceeds the total byte budget")
        })?;
        let value = read.value.ok_or_else(|| configuration_error(
            "a required app-config secret is missing in trusted_server_secrets; publish every active reference before starting",
        ))?;
        values.insert(key, value.to_vec());
    }
    let secret_store = Arc::new(SecretSnapshot(values));
    let settings = verified
        .into_settings(
            secret_store.as_ref(),
            &StoreName::from(DEFAULT_SECRET_STORE_ID),
        )
        .map_err(settings_error)?;
    validate_snapshot_consumers(&settings)?;
    let state = build_state_with_settings(settings).map_err(settings_error)?;
    if deadline.is_expired_at(clock.now()) {
        return Err(configuration_error(
            "native application initialization exceeds its extraction deadline",
        ));
    }
    let snapshot = Arc::new(NativeStoreSnapshot {
        config_store: Arc::new(ConfigSnapshot {
            key: binding.default_key.clone(),
            envelope: envelope.to_string(),
        }),
        secret_store,
    });
    app.insert_state(snapshot);
    app.insert_state(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::app::Hooks as _;

    #[test]
    fn signing_is_rejected_with_an_actionable_startup_error() {
        let settings = Settings {
            publisher: trusted_server_core::settings::Publisher {
                origin_url: "https://origin.publisher.example.com".to_owned(),
                ..Default::default()
            },
            request_signing: Some(trusted_server_core::settings::RequestSigning {
                enabled: true,
                config_store_id: "fixture-config".to_owned(),
                secret_store_id: "fixture-secrets".to_owned(),
            }),
            ..Settings::default()
        };
        let error = validate_snapshot_consumers(&settings)
            .expect_err("should reject signing before readiness");
        assert!(error.to_string().contains("disable signing"));
        assert!(error.to_string().contains("KID"));
    }

    #[test]
    fn native_origin_requires_an_absolute_http_transport_uri() {
        for origin in [
            "not-a-valid-url",
            "ftp://origin.publisher.example.com",
            "/relative",
        ] {
            let mut settings = datadome_settings("");
            settings.publisher.origin_url = origin.to_owned();
            let error = validate_snapshot_consumers(&settings)
                .expect_err("should reject an unusable native origin before readiness");
            assert!(error.to_string().contains("publisher.origin_url"));
        }
    }

    fn datadome_settings(extra: &str) -> Settings {
        Settings::from_toml(&format!(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "fixture-admin-password"
            [publisher]
            domain = "publisher.example.com"
            cookie_domain = ".publisher.example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "fixture-proxy-secret-32-bytes-minimum"
            [ec]
            passphrase = "fixture-passphrase-32-bytes-minimum"
            [integrations.datadome]
            enabled = true
            enable_protection = true
            server_side_key_secret_name = "fixture-datadome-key"
            {extra}
        "#
        ))
        .expect("should parse consumer validation fixture")
    }

    #[test]
    fn dynamic_datadome_sources_are_rejected_but_inline_sources_are_supported() {
        for extra in [
            "protection_excluded_ip_cidr_sources = [{ config_store = 'extra-store', key = 'cidrs' }]",
            "protection_exclusion_rules = [{ id = 'fixture-rule', type = 'ip_cidr_source', config_store = 'extra-store', key = 'cidrs' }]",
        ] {
            let error = validate_snapshot_consumers(&datadome_settings(extra))
                .expect_err("should reject dynamic config reads before readiness");
            assert!(error.to_string().contains("use inline"));
        }
        validate_snapshot_consumers(&datadome_settings(
            "protection_excluded_ip_cidrs = ['192.0.2.0/24']",
        ))
        .expect("should allow self-contained inline CIDRs");
    }

    #[test]
    fn snapshots_reject_missing_keys_wrong_stores_and_management_writes() {
        let config = ConfigSnapshot {
            key: "config".to_owned(),
            envelope: "envelope".to_owned(),
        };
        let config_name = StoreName::from(DEFAULT_CONFIG_STORE_ID);
        assert_eq!(
            config
                .get(&config_name, "config")
                .expect("should read loaded envelope"),
            "envelope"
        );
        assert!(config.get(&config_name, "missing").is_err());
        assert!(config.get(&StoreName::from("other"), "config").is_err());
        assert!(
            config
                .put(&StoreId::from(DEFAULT_CONFIG_STORE_ID), "config", "new")
                .is_err()
        );
        assert!(
            config
                .delete(&StoreId::from(DEFAULT_CONFIG_STORE_ID), "config")
                .is_err()
        );
        let secrets = SecretSnapshot(BTreeMap::from([(
            "token".to_owned(),
            b"fixture-value".to_vec(),
        )]));
        let secret_name = StoreName::from(DEFAULT_SECRET_STORE_ID);
        let mut returned = secrets
            .get_bytes(&secret_name, "token")
            .expect("should read loaded secret");
        returned.clear();
        assert_eq!(
            secrets
                .get_bytes(&secret_name, "token")
                .expect("should retain immutable value"),
            b"fixture-value"
        );
        assert!(secrets.get_bytes(&secret_name, "missing").is_err());
        assert!(
            secrets
                .get_bytes(&StoreName::from("other"), "token")
                .is_err()
        );
        assert!(
            secrets
                .create(&StoreId::from(DEFAULT_SECRET_STORE_ID), "token", "new")
                .is_err()
        );
        assert!(
            secrets
                .delete(&StoreId::from(DEFAULT_SECRET_STORE_ID), "token")
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_prepared_stores_fail_initialization() {
        let mut app = App::new(crate::app::TrustedServerApp::routes());
        let error = initialize_native(&mut app, &PreparedStores::default())
            .await
            .expect_err("should reject absent prepared stores");
        assert!(error.to_string().contains("not prepared"));
    }
}
