//! Runtime helpers for Trusted Server blob app-config payloads.
//!
//! The `ts` CLI delegates blob construction and config-store writes to
//! `EdgeZero`'s typed config push path. Runtime loading only needs to verify the
//! stored [`edgezero_core::blob_envelope::BlobEnvelope`] and reconstruct
//! [`Settings`] from its data value.

use std::sync::Arc;

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::{Report, ResultExt as _};

use crate::config::TrustedServerAppConfig;
use crate::error::TrustedServerError;
use crate::integrations::IntegrationBuilder;
use crate::permissions::{PermissionPolicy, PolicyReference, held_policy, load_policy};
use crate::platform::{PlatformSecretStore, PolicyOrigin, StoreName};
use crate::secret_resolution::resolve_secret_references_with;
use crate::settings::{PermissionsConfig, Settings};

/// Canonical logical secret store used by Trusted Server app-config secrets.
pub const DEFAULT_SECRET_STORE_ID: &str = "trusted_server_secrets";

/// Default logical config-store id, from `[stores.config].default` in `edgezero.toml`.
///
/// Derived at build time so every adapter uses the repository manifest's default.
pub const DEFAULT_CONFIG_STORE_ID: &str = env!("TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID");

/// Default config-store key containing the Trusted Server app-config blob.
///
/// Intentionally matches the logical store ID: an ordinary `ts config push`
/// writes there unless `--key` selects another key. This constant does not apply
/// runtime overrides; use [`crate::settings_data::config_key`] with the adapter's
/// runtime configuration, or [`crate::settings_data::default_config_key`] for
/// process-environment overrides.
pub const CONFIG_BLOB_KEY: &str = DEFAULT_CONFIG_STORE_ID;

/// Reconstruct runtime [`Settings`] from a serialized config blob envelope,
/// validating against the built-in integrations only.
///
/// Secret references are resolved after envelope verification and before
/// deserialization. The envelope data itself is never mutated or rewritten.
/// The permission policy the settings name is then read from
/// `policy_origin`, checked and parsed, and kept with the settings, which
/// keep only its name and digest in `[permissions]`. A policy source is not
/// read for a digest whose rules this instance already holds.
///
/// A deployment that composes builders of its own calls
/// [`settings_from_config_blob_with`] instead, so a `[demand]` or
/// `[ad-server]` name one of them supplies is not refused here.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, secret resolution fails, resolved
/// settings are invalid, or the settings name no permission policy or one
/// that cannot be read, is not the file that was pushed, or is not valid.
pub fn settings_from_config_blob(
    envelope_json: &str,
    secret_store: &dyn PlatformSecretStore,
    default_secret_store_name: &StoreName,
    policy_origin: PolicyOrigin<'_>,
) -> Result<Settings, Report<TrustedServerError>> {
    settings_from_config_blob_with(
        envelope_json,
        secret_store,
        default_secret_store_name,
        policy_origin,
        &[],
    )
}

/// Reconstruct runtime [`Settings`] from a serialized config blob envelope,
/// validating against the built-in integrations followed by
/// `extra_integrations`, with the permission policy read from
/// `policy_origin` as [`settings_from_config_blob`] reads it.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, secret resolution fails, resolved
/// settings are invalid, or the settings name no permission policy or one
/// that cannot be read, is not the file that was pushed, or is not valid.
pub fn settings_from_config_blob_with(
    envelope_json: &str,
    secret_store: &dyn PlatformSecretStore,
    default_secret_store_name: &StoreName,
    policy_origin: PolicyOrigin<'_>,
    extra_integrations: &[IntegrationBuilder],
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope: BlobEnvelope = serde_json::from_str(envelope_json).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: "failed to parse Trusted Server app-config blob envelope".to_string(),
        })
        .attach(error.to_string())
    })?;
    envelope.verify().map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: "Trusted Server app-config blob failed integrity verification".to_string(),
        })
        .attach(error.to_string())
    })?;

    // The modules' own secret settings are found through the builders the
    // settings are validated against, so a module a deployment added has its
    // secrets looked up here as a stock one does.
    let builders = crate::integrations::all_builders(extra_integrations).collect::<Vec<_>>();
    let mut data = envelope.into_data();
    remove_inactive_secret_references(&mut data);
    crate::module_secrets::clear_unused(&mut data, &builders);
    let resolved = resolve_secret_references_with::<TrustedServerAppConfig>(
        &mut data,
        secret_store,
        default_secret_store_name,
        crate::module_secrets::secret_fields(&builders),
    )?;
    let mut settings = Settings::from_json_value(data)?;
    // The configuration view masks every leaf a secret was written into.
    settings.set_resolved_secrets(resolved);
    crate::config::validate_settings_for_runtime_with(&settings, extra_integrations)?;
    crate::inspect::config::validate_patterns(&settings)?;
    let policy = load_domain_policy(&mut settings.permissions, policy_origin)?;
    log_permission_baseline(&policy);
    settings.attach_permission_policy(policy);
    Ok(settings)
}

/// Loads the permission policy `permissions` names from `origin`.
///
/// The policy's text is parsed into the policy, so `permissions` keeps only
/// its name and digest. A digest names one file, so a source is not read
/// for a digest whose rules this instance already holds. Text the blob
/// carries is always checked, because that text is already in hand.
fn load_domain_policy(
    permissions: &mut PermissionsConfig,
    origin: PolicyOrigin<'_>,
) -> Result<Arc<PermissionPolicy>, Report<TrustedServerError>> {
    let refuse = |message: String| Report::new(TrustedServerError::Configuration { message });
    let Some(policy) = permissions.policy.clone() else {
        return Err(refuse(
            "the configuration names no permission policy, so name the publisher's policy \
             file in `[permissions] policy` and push it with `ts config push --permissions \
             <file>`"
                .to_owned(),
        ));
    };
    let Some(sha256) = permissions.sha256.clone() else {
        return Err(refuse(format!(
            "the permission policy `{policy}` has no SHA-256, so the configuration was not \
             pushed with its file, which `ts config push --permissions <file>` supplies"
        )));
    };
    let blob_text = permissions.text.take();
    let reference = PolicyReference { policy, sha256 };
    let text = match origin {
        PolicyOrigin::Blob => blob_text.ok_or_else(|| {
            refuse(format!(
                "the configuration carries no text for the permission policy `{}`, which \
                 `ts config push --permissions <file>` writes for this adapter",
                reference.policy
            ))
        })?,
        PolicyOrigin::Source(source) => {
            if let Some(policy) = held_policy(&reference) {
                return Ok(policy);
            }
            source
                .policy_text(&reference)
                .change_context(TrustedServerError::Configuration {
                    message: format!(
                        "the permission policy `{}` cannot be read",
                        reference.policy
                    ),
                })?
        }
    };
    load_policy(reference, &text)
}

/// Logs the policy's declared default once per settings load, so an operator
/// can see which permissions an unmatched request is granted without a
/// signal, and which jurisdiction its consent gates apply.
fn log_permission_baseline(policy: &PermissionPolicy) {
    let maps = policy.maps();
    let granted: Vec<String> = maps
        .baseline(None, None)
        .permissions()
        .iter()
        .map(|permission| permission.to_string())
        .collect();
    log::debug!(
        "Permission baseline: policy `{}` ({}) top node, jurisdiction {}; granted without a signal: [{}]",
        policy.name(),
        policy.sha256(),
        maps.default_jurisdiction(),
        granted.join(", ")
    );
}

fn remove_inactive_secret_references(data: &mut serde_json::Value) {
    // References of a table `[analytics]` does not select are cleared before resolution.
    if !crate::settings::document_selects_analytics_module(data)
        && let Some(table) = data
            .pointer_mut("/analytics/tinybird")
            .and_then(serde_json::Value::as_object_mut)
    {
        table.remove("auction_token_secret");
        table.remove("access_token_secret");
    }

    if let Some(partners) = data
        .pointer_mut("/ec/partners")
        .and_then(serde_json::Value::as_array_mut)
    {
        for partner in partners {
            let Some(partner) = partner.as_object_mut() else {
                continue;
            };
            if !json_bool_or_string_is_true(partner.get("pull_sync_enabled")) {
                partner.remove("ts_pull_token");
            }
        }
    }
}

fn json_bool_or_string_is_true(value: Option<&serde_json::Value>) -> bool {
    matches!(value, Some(serde_json::Value::Bool(true)))
        || matches!(value, Some(serde_json::Value::String(value)) if value == "true")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::module::{HMAC_MODULE_KEY, HOST_SIGNALS_MODULE_KEY};
    use crate::platform::{PlatformError, StoreId};
    use crate::redacted::Redacted;
    use crate::settings::{
        AnalyticsConfig, AssetOriginAuth, EcPartner, ProxyAssetRoute, S3SigV4AuthConfig,
        TinybirdSettings, TrustedClientIpConfig,
    };
    use crate::test_support::tests::{
        crate_test_settings_str, edit_origin, hmac_passphrase, sample_permissions,
        select_hmac_module, select_host_signals_module, set_route_auth,
    };

    /// Test settings as `ts config push` writes them, naming the repository's
    /// sample policy.
    fn test_settings() -> Settings {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        settings.permissions = sample_permissions();
        settings
    }

    struct EchoSecretStore;

    impl PlatformSecretStore for EchoSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            let value = match key {
                "placeholder_proxy" => "change-me-proxy-secret",
                "unit-test-proxy-secret" => "unit-test-proxy-secret-32-bytes-ok",
                _ => key,
            };
            Ok(value.as_bytes().to_vec())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    struct UnifiedSecretStore;

    impl PlatformSecretStore for UnifiedSecretStore {
        fn get_bytes(
            &self,
            store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            if store_name.as_ref() != "ts_secrets" || key.starts_with("unused-") {
                return Err(Report::new(PlatformError::SecretStore));
            }
            let value = match key {
                "unit-test-proxy-secret" => "unit-test-proxy-secret-32-bytes-ok",
                "tinybird-token-key" => "resolved-tinybird-token",
                "access_key_id" | "s3-access-key" => "AKIAIOSFODNN7EXAMPLE",
                "secret_access_key" | "s3-secret-key" => "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "s3-session-key" => "resolved-session-token",
                "partner-api-token-key" => "resolved-partner-api-token-32-bytes-ok",
                "partner-pull-token-key" => "resolved-partner-pull-token-32-bytes-ok",
                "trusted-client-ip-key" => "resolved-trusted-client-ip-secret-32-bytes",
                "host-signals-passphrase-key" => "resolved-host-signals-passphrase-32-bytes-ok",
                "labeled-passphrase-key" => "resolved-labeled-passphrase-32-bytes",
                _ => key,
            };
            Ok(value.as_bytes().to_vec())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    fn partner_with_pull_sync(enabled: bool, token_key: &str) -> EcPartner {
        let mut value = serde_json::json!({
            "name": "Example Partner",
            "source_domain": "partner.example.com",
            "api_token": "partner-api-token-key",
            "pull_sync_enabled": enabled,
            "ts_pull_token": token_key,
        });
        if enabled {
            value["pull_sync_url"] =
                serde_json::Value::String("https://partner.example.com/sync".to_string());
            value["pull_sync_allowed_domains"] = serde_json::json!(["partner.example.com"]);
        }
        serde_json::from_value(value).expect("should build pull-sync partner")
    }

    /// The blob envelope `ts config push` writes for `settings`, naming the
    /// repository's sample policy.
    fn envelope_json(settings: &Settings) -> String {
        let data = serde_json::to_value(settings).expect("should serialize settings to JSON");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    fn load_settings(envelope_json: &str) -> Result<Settings, Report<TrustedServerError>> {
        settings_from_config_blob(
            envelope_json,
            &EchoSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
        )
    }

    /// The ad server implementation a crate outside core supplies.
    static EXTERNAL_ADSERVER: crate::auction::demand::AdServerImplementation =
        crate::auction::demand::AdServerImplementation {
            id: "ad-server.example",
            build: build_external_adserver,
        };

    fn build_external_adserver(
        name: &str,
        _settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<
        std::sync::Arc<dyn crate::auction::provider::AuctionProvider>,
        Report<TrustedServerError>,
    > {
        Ok(std::sync::Arc::new(
            crate::auction::test_support::adserver_fixture::FixtureAdServer::new(name, 500),
        ))
    }

    /// Holds the one key a deployment's module names, and refuses the secret
    /// itself as a key, so a leaf looked up twice fails.
    struct ModuleSecretStore;

    impl PlatformSecretStore for ModuleSecretStore {
        fn get_bytes(
            &self,
            store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            match key {
                "module-key" => Ok(b"resolved-module-secret".to_vec()),
                "resolved-module-secret" | "unused-module-key" => {
                    Err(Report::new(PlatformError::SecretStore))
                }
                _ => EchoSecretStore.get_bytes(store_name, key),
            }
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    fn module_lock_is_on(table: &serde_json::Map<String, serde_json::Value>) -> bool {
        table.get("lock").and_then(serde_json::Value::as_bool) == Some(true)
    }

    const MODULE_SECRETS: &[crate::integrations::ModuleSecretSetting] =
        &[crate::integrations::ModuleSecretSetting {
            path: &["key_name"],
            in_use: module_lock_is_on,
        }];

    /// A builder a deployment added, whose module names a secret in its own
    /// table.
    fn module_with_a_secret() -> IntegrationBuilder {
        IntegrationBuilder::new(
            "probe",
            "example-crate",
            crate::integrations::registry_test_support::probe_registration,
            crate::integrations::registry_test_support::validate_nothing,
        )
        .with_module_name("testing.probe")
        .with_secret_settings(MODULE_SECRETS)
    }

    fn load_with_module(settings: &Settings) -> Settings {
        settings_from_config_blob_with(
            &envelope_json(settings),
            &ModuleSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
            &[module_with_a_secret()],
        )
        .expect("should load settings with the module's builder")
    }

    #[test]
    fn the_load_looks_up_a_secret_a_deployment_s_module_declares() {
        let mut settings = test_settings();
        settings
            .insert_module_config(
                "testing",
                "testing.probe",
                &serde_json::json!({ "lock": true, "key_name": "module-key" }),
            )
            .expect("should insert the module's table");

        let loaded = load_with_module(&settings);

        assert_eq!(
            loaded.section_table("testing", "probe").get("key_name"),
            Some(&serde_json::json!("resolved-module-secret")),
            "should hold the secret where the table held the name of its key"
        );
    }

    #[test]
    fn the_load_clears_a_declared_secret_the_module_s_table_does_not_use() {
        let mut settings = test_settings();
        settings
            .insert_module_config(
                "testing",
                "testing.probe",
                &serde_json::json!({ "lock": false, "key_name": "unused-module-key" }),
            )
            .expect("should insert the module's table");

        let loaded = load_with_module(&settings);

        assert_eq!(
            loaded.section_table("testing", "probe").get("key_name"),
            None,
            "should clear a key name nothing uses, where looking it up would fail"
        );
    }

    #[test]
    fn a_table_s_key_name_is_left_as_written_without_its_module_s_builder() {
        let mut settings = test_settings();
        settings
            .insert_module_config(
                "testing",
                "testing.probe",
                &serde_json::json!({ "lock": true, "key_name": "module-key" }),
            )
            .expect("should insert the module's table");

        let loaded = settings_from_config_blob(
            &envelope_json(&settings),
            &ModuleSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should load settings without the module's builder");

        assert_eq!(
            loaded.section_table("testing", "probe").get("key_name"),
            Some(&serde_json::json!("module-key")),
            "should look nothing up for a table whose module declared nothing to this load"
        );
    }

    /// The settings are validated as they load, so a deployment that composes
    /// a builder hands it to the load. Without it the load refuses the ad
    /// server that builder supplies, and with it the settings load.
    #[test]
    fn the_load_accepts_an_external_ad_server_only_when_given_its_builder() {
        let mut settings = test_settings();
        settings.adserver = crate::provider_table::ProviderChoice::new(
            Some("example".to_string()),
            std::collections::BTreeMap::from([("example".to_string(), serde_json::Map::new())]),
        );
        let envelope = envelope_json(&settings);
        let extra = [
            IntegrationBuilder::implementations("example-adserver", "example-crate")
                .with_adserver(&EXTERNAL_ADSERVER),
        ];

        let error =
            load_settings(&envelope).expect_err("built-ins alone should not know this ad server");
        assert!(
            error.to_string().contains("example"),
            "should name the ad server: {error:?}"
        );

        let loaded = settings_from_config_blob_with(
            &envelope,
            &EchoSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
            &extra,
        )
        .expect("should load settings naming the external builder's ad server");
        assert_eq!(
            loaded.adserver.selected().first().copied(),
            Some("example"),
            "the loaded settings should still select the external ad server"
        );
    }

    #[test]
    fn payload_round_trips_through_blob_envelope() {
        let original = test_settings();
        let reconstructed =
            load_settings(&envelope_json(&original)).expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.domain, original.publisher.domain,
            "should preserve publisher domain"
        );
        assert_eq!(
            reconstructed.ec.pull_sync_concurrency, original.ec.pull_sync_concurrency,
            "should preserve numeric fields"
        );
        assert_eq!(
            reconstructed.proxy.allowed_domains, original.proxy.allowed_domains,
            "should preserve arrays"
        );
    }

    /// The settings of an example module, with a flag that defaults to off.
    #[derive(Debug, serde::Deserialize, serde::Serialize, validator::Validate)]
    #[serde(deny_unknown_fields)]
    struct ExampleModuleSettings {
        #[serde(default)]
        opted_in: bool,
        endpoint: String,
    }

    impl crate::settings::IntegrationConfig for ExampleModuleSettings {}

    #[test]
    fn a_module_s_table_survives_the_blob_round_trip() {
        let mut original = test_settings();
        original
            .insert_module_config(
                "example",
                "example.notice",
                &ExampleModuleSettings {
                    opted_in: true,
                    endpoint: "https://api.example.com".to_string(),
                },
            )
            .expect("should insert the example module's table");

        let reconstructed =
            load_settings(&envelope_json(&original)).expect("should reconstruct settings");
        let config = reconstructed
            .module_config::<ExampleModuleSettings>("example.notice")
            .expect("should read the example module's table")
            .expect("should still select the example module");

        assert!(
            config.opted_in,
            "should preserve a flag the table set away from its default"
        );
        assert_eq!(
            config.endpoint, "https://api.example.com",
            "should preserve the table's other settings"
        );
    }

    #[test]
    fn resolves_all_static_credentials_from_the_mapped_default_store() {
        let mut original = test_settings();
        original.analytics = Some(AnalyticsConfig {
            module: Some("analytics.tinybird".to_string()),
            tinybird: Some(TinybirdSettings {
                api_host: "api.example.com".to_string(),
                auction_token_secret: Some(Redacted::new("tinybird-token-key".to_string())),
                ..TinybirdSettings::default()
            }),
        });
        let mut route = ProxyAssetRoute::new(
            "/assets/",
            "https://examplebucket.s3.us-east-1.amazonaws.com",
        );
        set_route_auth(
            &mut route,
            Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
                region: "us-east-1".to_string(),
                secret_store: Some("legacy-s3-store".to_string()),
                access_key_id: Redacted::new("s3-access-key".to_string()),
                secret_access_key: Redacted::new("s3-secret-key".to_string()),
                session_token: Some(Redacted::new("s3-session-key".to_string())),
                origin_query: None,
            })),
        );
        original.proxy.asset_routes.push(route);
        original
            .ec
            .partners
            .push(partner_with_pull_sync(true, "partner-pull-token-key"));

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve every static credential from the mapped store");

        assert_eq!(
            reconstructed
                .analytics_module()
                .and_then(|module| module.auction_token_secret.as_ref())
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-tinybird-token")
        );
        let auth = reconstructed.proxy.asset_routes[0]
            .origin_source()
            .and_then(|origin| origin.auth)
            .expect("should preserve S3 auth");
        let AssetOriginAuth::S3SigV4(auth) = auth;
        assert_eq!(auth.access_key_id.expose(), "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            auth.secret_access_key.expose(),
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"
        );
        assert_eq!(
            auth.session_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-session-token")
        );
        assert!(auth.secret_store.is_none());
        assert_eq!(
            reconstructed.ec.partners[0]
                .api_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-partner-api-token-32-bytes-ok")
        );
        assert_eq!(
            reconstructed.ec.partners[0]
                .ts_pull_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-partner-pull-token-32-bytes-ok")
        );
    }

    #[test]
    fn resolves_trusted_client_ip_shared_secret_from_default_store() {
        let mut original = test_settings();
        original.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "x-ts-client-ip".to_owned(),
            auth_header: "x-ts-client-ip-auth".to_owned(),
            shared_secret: Redacted::new("trusted-client-ip-key".to_owned()),
        });

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve the trusted client IP shared secret");

        assert_eq!(
            reconstructed
                .trusted_client_ip
                .as_ref()
                .expect("should retain trusted client IP configuration")
                .shared_secret
                .expose(),
            "resolved-trusted-client-ip-secret-32-bytes",
            "should replace the key reference with the resolved shared secret"
        );
    }

    #[test]
    fn missing_trusted_client_ip_shared_secret_fails_resolution() {
        let mut original = test_settings();
        original.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "x-ts-client-ip".to_owned(),
            auth_header: "x-ts-client-ip-auth".to_owned(),
            shared_secret: Redacted::new("unused-trusted-client-ip-key".to_owned()),
        });

        let error = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect_err("should reject a missing trusted client IP shared secret");
        let message = error.to_string();

        assert!(
            message.contains("trusted_client_ip.shared_secret"),
            "should identify the unresolved secret field: {error:?}"
        );
        assert!(
            !message.contains("unused-trusted-client-ip-key"),
            "should not expose the secret key reference: {error:?}"
        );
    }

    #[test]
    fn a_labeled_hmac_block_resolves_its_passphrase_from_the_secret_store() {
        // `secret_fields` can only list fixed paths, so a block under a label
        // of the operator's choosing is found by reading the configuration.
        // Without that, the key name would reach settings as the passphrase.
        let mut data =
            serde_json::to_value(test_settings()).expect("should serialize settings to JSON");
        data["ec"] = serde_json::json!({
            "module": "primary",
            "primary": {
                "implementation": "hmac",
                "passphrase": "labeled-passphrase-key",
            },
        });
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
        let envelope_json = serde_json::to_string(&envelope).expect("should serialize envelope");

        let reconstructed = settings_from_config_blob(
            &envelope_json,
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve the labeled block's passphrase");

        let written =
            serde_json::to_value(&reconstructed).expect("should serialize the loaded settings");
        assert_eq!(
            written["ec"]["primary"]["passphrase"], "resolved-labeled-passphrase-32-bytes",
            "should replace the key name with the value from the secret store"
        );
    }

    /// Answers `ec_key`, refuses to be asked for that key's value as though it
    /// were a key name, and otherwise answers as [`UnifiedSecretStore`] does.
    struct RefusesAValueAsAKey;

    const CROSS_NAMED_PASSPHRASE: &str = "resolved-cross-named-passphrase-32-bytes";

    impl PlatformSecretStore for RefusesAValueAsAKey {
        fn get_bytes(
            &self,
            store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            match key {
                "ec_key" => Ok(CROSS_NAMED_PASSPHRASE.as_bytes().to_vec()),
                CROSS_NAMED_PASSPHRASE => Err(Report::new(PlatformError::SecretStore)),
                _ => UnifiedSecretStore.get_bytes(store_name, key),
            }
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    /// A block named after one built-in implementation that configures the
    /// other is on the fixed secret list already, so its passphrase is
    /// resolved once and not read back as a key name.
    #[test]
    fn a_built_in_block_naming_the_other_built_in_resolves_its_passphrase_once() {
        use crate::secret_resolution::ConfiguredSecretFields as _;

        for (name, implementation) in [("hmac", "host_signals"), ("host_signals", "hmac")] {
            let mut data =
                serde_json::to_value(test_settings()).expect("should serialize settings to JSON");
            data["ec"] = serde_json::json!({
                "module": name,
                (name): { "implementation": implementation, "passphrase": "ec_key" },
            });

            let listed = TrustedServerAppConfig::configured_secret_fields(&data);
            let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
            let envelope_json =
                serde_json::to_string(&envelope).expect("should serialize envelope");
            let reconstructed = settings_from_config_blob(
                &envelope_json,
                &RefusesAValueAsAKey,
                &StoreName::from("ts_secrets"),
                PolicyOrigin::Blob,
            )
            .unwrap_or_else(|error| panic!("[ec.{name}] should load: {error:?}"));

            assert!(
                listed.is_empty(),
                "[ec.{name}] is on the fixed list, so it is not listed again"
            );
            let written =
                serde_json::to_value(&reconstructed).expect("should serialize the loaded settings");
            assert_eq!(
                written["ec"][name]["passphrase"], CROSS_NAMED_PASSPHRASE,
                "[ec.{name}] should carry the stored passphrase"
            );
        }
    }

    #[test]
    fn omitted_s3_secret_references_resolve_default_store_keys() {
        let mut original = test_settings();
        let mut route = ProxyAssetRoute::new(
            "/default-s3/",
            "https://examplebucket.s3.us-east-1.amazonaws.com",
        );
        set_route_auth(
            &mut route,
            Some(AssetOriginAuth::S3SigV4(
                toml::from_str("region = \"us-east-1\"").expect("should apply S3 secret defaults"),
            )),
        );
        original.proxy.asset_routes.push(route);

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve default S3 secret keys");

        let AssetOriginAuth::S3SigV4(auth) = reconstructed.proxy.asset_routes[0]
            .origin_source()
            .and_then(|origin| origin.auth)
            .expect("should preserve S3 auth");
        assert_eq!(auth.access_key_id.expose(), "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            auth.secret_access_key.expose(),
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"
        );
    }

    #[test]
    fn source_origin_auth_resolves_its_credentials_from_the_store() {
        let mut original = test_settings();
        edit_origin(&mut original, |origin| {
            origin.url = "https://storage.example.com/bucket/site".to_string()
        });
        edit_origin(&mut original, |origin| {
            origin.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
                region: "test-region-1".to_string(),
                secret_store: None,
                access_key_id: Redacted::new("s3-access-key".to_string()),
                secret_access_key: Redacted::new("s3-secret-key".to_string()),
                session_token: Some(Redacted::new("s3-session-key".to_string())),
                origin_query: None,
            }))
        });

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve the publisher origin credentials from the mapped store");

        let Some(AssetOriginAuth::S3SigV4(auth)) =
            reconstructed.origin_source().and_then(|origin| origin.auth)
        else {
            panic!("should keep the origin auth");
        };
        assert_eq!(auth.access_key_id.expose(), "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            auth.secret_access_key.expose(),
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"
        );
        assert_eq!(
            auth.session_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-session-token")
        );
    }

    #[test]
    fn partner_without_api_token_loads_without_secret_resolution() {
        let mut original = test_settings();
        let partner = serde_json::from_value(serde_json::json!({
            "name": "Example Partner",
            "source_domain": "partner.example.com",
            "bidstream_enabled": true,
        }))
        .expect("should build partner without API token");
        original.ec.partners.push(partner);

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should load partner without resolving an API token");

        assert!(
            reconstructed.ec.partners[0].api_token.is_none(),
            "should preserve omitted API token"
        );
    }

    #[test]
    fn string_true_pull_sync_flag_retains_and_resolves_its_token() {
        let mut original = test_settings();
        original
            .ec
            .partners
            .push(partner_with_pull_sync(true, "partner-pull-token-key"));
        let mut data = serde_json::to_value(original).expect("should serialize settings");
        data["ec"]["partners"][0]["pull_sync_enabled"] =
            serde_json::Value::String("true".to_owned());
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
        let envelope_json = serde_json::to_string(&envelope).expect("should serialize envelope");

        let reconstructed = settings_from_config_blob(
            &envelope_json,
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve a pull token enabled by a string boolean");

        assert_eq!(
            reconstructed.ec.partners[0]
                .ts_pull_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-partner-pull-token-32-bytes-ok"),
            "should retain and resolve the active pull token"
        );
    }

    #[test]
    fn string_false_pull_sync_flag_removes_a_stale_token() {
        let mut original = test_settings();
        original
            .ec
            .partners
            .push(partner_with_pull_sync(false, "unused-partner-pull-token"));
        let mut data = serde_json::to_value(original).expect("should serialize settings");
        data["ec"]["partners"][0]["pull_sync_enabled"] =
            serde_json::Value::String("false".to_owned());
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
        let envelope_json = serde_json::to_string(&envelope).expect("should serialize envelope");

        let reconstructed = settings_from_config_blob(
            &envelope_json,
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should skip a pull token disabled by a string boolean");

        assert!(
            reconstructed.ec.partners[0].ts_pull_token.is_none(),
            "should remove the inactive pull token"
        );
    }

    #[test]
    fn active_partner_pull_sync_fails_when_its_token_is_missing() {
        let mut original = test_settings();
        original
            .ec
            .partners
            .push(partner_with_pull_sync(true, "unused-partner-pull-token"));

        let error = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect_err("should reject a missing active pull-sync token");

        assert!(error.to_string().contains("ec.partners[0].ts_pull_token"));
    }

    #[test]
    fn inactive_optional_features_do_not_resolve_stale_secret_references() {
        let mut original = test_settings();
        original
            .ec
            .partners
            .push(partner_with_pull_sync(false, "unused-partner-pull-token"));

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should skip inactive optional feature references");

        assert!(reconstructed.ec.partners[0].ts_pull_token.is_none());
    }

    #[test]
    fn an_unselected_analytics_module_table_is_refused_without_resolving_its_secret() {
        let mut original = test_settings();
        original.analytics = Some(AnalyticsConfig {
            module: None,
            tinybird: Some(TinybirdSettings {
                api_host: "api.example.com".to_string(),
                auction_token_secret: Some(Redacted::new("unused-telemetry-key".to_string())),
                ..TinybirdSettings::default()
            }),
        });

        let error = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect_err("should refuse a table nothing selects");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[analytics] selects no module"),
            "should name the section and why: {rendered}"
        );
        assert!(
            !rendered.contains("unused-telemetry-key") && !rendered.contains("secret reference"),
            "should not have tried to resolve the stale secret reference: {rendered}"
        );
    }

    /// A table for a module its section does not select is refused, and its
    /// secret references are dropped before resolution, so the operator reads
    /// the table's own fault rather than a secret-store failure that follows
    /// from it.
    #[test]
    fn an_unselected_module_s_table_is_refused_without_resolving_its_secrets() {
        let original = test_settings();
        let mut data = serde_json::to_value(&original).expect("should serialize settings to JSON");
        data.as_object_mut()
            .expect("settings should serialize as an object")
            .insert(
                "testing".to_owned(),
                serde_json::json!({
                    "probe": { "lock": true, "key_name": "unused-module-key" },
                }),
            );
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        let envelope = serde_json::to_string(&envelope).expect("should serialize envelope");

        let error = settings_from_config_blob_with(
            &envelope,
            &ModuleSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
            &[module_with_a_secret()],
        )
        .expect_err("should refuse a table its section does not select");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[testing] selects no module"),
            "should name the section and what it is missing: {rendered}"
        );
        assert!(
            !rendered.contains("unused-module-key"),
            "should not have tried to resolve the stale secret reference: {rendered}"
        );
    }

    #[test]
    fn legacy_blob_without_rewrite_creatives_preserves_rewriting() {
        let data =
            serde_json::to_value(test_settings()).expect("should serialize settings to JSON");
        let auction = data
            .get("auction")
            .and_then(serde_json::Value::as_object)
            .expect("should serialize auction settings as an object");
        assert!(
            !auction.contains_key("rewrite_creatives"),
            "should omit the default rewrite setting from the payload"
        );
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        let envelope_json = serde_json::to_string(&envelope).expect("should serialize envelope");

        let reconstructed =
            load_settings(&envelope_json).expect("should reconstruct legacy settings");

        assert!(
            reconstructed.auction.rewrite_creatives,
            "should enable creative rewriting for legacy blobs"
        );
    }

    #[test]
    fn disabled_rewrite_creatives_survives_blob_round_trip() {
        let mut original = test_settings();
        original.auction.rewrite_creatives = false;

        let reconstructed = load_settings(&envelope_json(&original))
            .expect("should reconstruct disabled rewriting");

        assert!(
            !reconstructed.auction.rewrite_creatives,
            "should preserve the explicit rewrite opt-out"
        );
    }

    #[test]
    fn strings_that_look_like_json_scalars_round_trip_as_strings() {
        let mut original = test_settings();
        original.publisher.proxy_secret =
            Redacted::new("12345678901234567890123456789012".to_string());
        select_hmac_module(
            &mut original.ec,
            HMAC_MODULE_KEY,
            "12345678901234567890123456789012",
        );
        original
            .response_headers
            .insert("x-example".to_string(), "true".to_string());

        let reconstructed =
            load_settings(&envelope_json(&original)).expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "numeric-looking proxy secret should remain a string"
        );
        assert_eq!(
            hmac_passphrase(&reconstructed.ec, HMAC_MODULE_KEY),
            hmac_passphrase(&original.ec, HMAC_MODULE_KEY),
            "numeric-looking passphrase should remain a string"
        );
        assert_eq!(
            reconstructed.response_headers.get("x-example"),
            Some(&"true".to_string()),
            "boolean-looking header value should remain a string"
        );
    }

    #[test]
    fn runtime_validation_accepts_short_resolved_proxy_secret() {
        let mut settings = test_settings();
        settings.publisher.proxy_secret = Redacted::new("short_proxy".to_owned());

        let reconstructed = load_settings(&envelope_json(&settings))
            .expect("should accept an existing short proxy secret");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            "short_proxy",
            "should preserve the resolved proxy secret"
        );
    }

    #[test]
    fn runtime_validation_rejects_short_resolved_passphrase() {
        let mut settings = test_settings();
        select_hmac_module(&mut settings.ec, HMAC_MODULE_KEY, "short_key");

        let err = load_settings(&envelope_json(&settings))
            .expect_err("should reject a short resolved passphrase");

        assert!(
            err.to_string().contains("short_passphrase") || err.to_string().contains("validation"),
            "error should indicate runtime validation: {err:?}"
        );
        assert!(
            !err.to_string().contains("short_key"),
            "error should not expose the secret value"
        );
    }

    #[test]
    fn resolves_the_host_signals_passphrase_from_the_mapped_store() {
        let mut original = test_settings();
        select_host_signals_module(&mut original.ec, "host-signals-passphrase-key");

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
            PolicyOrigin::Blob,
        )
        .expect("should resolve the host_signals passphrase from the mapped store");

        assert_eq!(
            reconstructed
                .ec
                .module_blocks
                .get(HOST_SIGNALS_MODULE_KEY)
                .and_then(crate::settings::EcModuleBlock::host_signals_settings)
                .map(|config| config.passphrase.expose().as_str()),
            Some("resolved-host-signals-passphrase-32-bytes-ok")
        );
    }

    #[test]
    fn runtime_validation_rejects_a_short_resolved_host_signals_passphrase() {
        let mut settings = test_settings();
        select_host_signals_module(&mut settings.ec, "short_key");

        let err = load_settings(&envelope_json(&settings))
            .expect_err("should reject a short resolved host_signals passphrase");

        assert!(
            err.to_string().contains("short_passphrase"),
            "error should name the passphrase check: {err:?}"
        );
        assert!(
            !err.to_string().contains("short_key"),
            "error should not expose the secret value"
        );
    }

    #[test]
    fn placeholder_rejection_happens_after_secret_resolution() {
        let mut settings = test_settings();
        settings.publisher.proxy_secret = Redacted::new("placeholder_proxy".to_owned());

        let err = load_settings(&envelope_json(&settings))
            .expect_err("should reject a placeholder resolved from the secret store");

        assert!(
            err.to_string().contains("Insecure default"),
            "error should identify the insecure default: {err:?}"
        );
        assert!(
            !err.to_string().contains("change-me-proxy-secret"),
            "error should not expose the resolved secret value"
        );
    }

    #[test]
    fn tampered_blob_hash_is_rejected() {
        let mut envelope: BlobEnvelope =
            serde_json::from_str(&envelope_json(&test_settings())).expect("should parse envelope");
        envelope.sha256 = "ff".repeat(32);
        let tampered =
            serde_json::to_string(&envelope).expect("should serialize tampered envelope");

        let err = load_settings(&tampered).expect_err("should reject hash mismatch");

        assert!(
            err.to_string().contains("integrity verification"),
            "error should mention integrity verification"
        );
    }

    mod permission_policy {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;
        use crate::permissions::{SAMPLE_POLICY_NAME, SAMPLE_POLICY_TEXT, policy_sha256};
        use crate::platform::PlatformPolicySource;
        use crate::test_support::tests::policy_only_this_test_loads;

        /// Policy files by name, as a store of them would hold them, counting
        /// every read.
        #[derive(Default)]
        struct MemoryPolicies {
            files: BTreeMap<String, String>,
            reads: AtomicUsize,
        }

        impl MemoryPolicies {
            fn holding<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
                Self {
                    files: files
                        .into_iter()
                        .map(|(name, text)| (name.to_owned(), text.to_owned()))
                        .collect(),
                    reads: AtomicUsize::new(0),
                }
            }

            fn reads(&self) -> usize {
                self.reads.load(Ordering::SeqCst)
            }
        }

        impl PlatformPolicySource for MemoryPolicies {
            fn policy_text(
                &self,
                reference: &PolicyReference,
            ) -> Result<String, Report<PlatformError>> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.files.get(&reference.policy).cloned().ok_or_else(|| {
                    Report::new(PlatformError::PolicySource)
                        .attach(format!("no file named `{}`", reference.policy))
                })
            }
        }

        /// The `[permissions]` a push writes for an adapter with a policy
        /// source: the name and the digest of `text`, and no file.
        fn pinned(policy: &str, text: &str) -> PermissionsConfig {
            PermissionsConfig {
                policy: Some(policy.to_owned()),
                sha256: Some(policy_sha256(text)),
                text: None,
            }
        }

        fn maps(settings: &Settings) -> *const crate::permissions::PermissionMaps {
            std::ptr::from_ref(
                settings
                    .permission_policy()
                    .expect("should keep the policy")
                    .maps(),
            )
        }

        fn load_with(
            permissions: PermissionsConfig,
            origin: PolicyOrigin<'_>,
        ) -> Result<Settings, Report<TrustedServerError>> {
            let mut settings = test_settings();
            settings.permissions = permissions;
            let data = serde_json::to_value(&settings).expect("should serialize settings");
            let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
            settings_from_config_blob(
                &serde_json::to_string(&envelope).expect("should serialize envelope"),
                &EchoSecretStore,
                &StoreName::from("trusted_server_secrets"),
                origin,
            )
        }

        fn refusal(result: Result<Settings, Report<TrustedServerError>>) -> String {
            let error = result.expect_err("the domain should not load");
            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::Configuration { .. }
                ),
                "should be a configuration error, which answers 500: {error:?}"
            );
            format!("{error:?}")
        }

        #[test]
        fn a_domain_naming_no_policy_does_not_load() {
            let message = refusal(load_with(PermissionsConfig::default(), PolicyOrigin::Blob));

            assert!(
                message.contains("names no permission policy")
                    && message.contains("`[permissions] policy`"),
                "should say what is missing: {message}"
            );
        }

        #[test]
        fn a_policy_with_no_digest_does_not_load() {
            let permissions = PermissionsConfig {
                sha256: None,
                ..sample_permissions()
            };

            let message = refusal(load_with(permissions, PolicyOrigin::Blob));

            assert!(message.contains("`sample` has no SHA-256"), "{message}");
        }

        #[test]
        fn a_blob_without_its_file_does_not_load_where_the_file_travels_in_the_blob() {
            let permissions = PermissionsConfig {
                text: None,
                ..sample_permissions()
            };

            let message = refusal(load_with(permissions, PolicyOrigin::Blob));

            assert!(
                message.contains("carries no text for the permission policy `sample`"),
                "{message}"
            );
        }

        #[test]
        fn a_file_that_is_not_the_pushed_file_does_not_load() {
            let permissions = PermissionsConfig {
                text: Some(format!("{SAMPLE_POLICY_TEXT}\n# edited after the push\n")),
                ..sample_permissions()
            };

            let message = refusal(load_with(permissions, PolicyOrigin::Blob));

            assert!(
                message.contains("`sample` is not the file that was pushed"),
                "{message}"
            );
        }

        #[test]
        fn a_pushed_file_that_is_not_a_valid_policy_does_not_load() {
            let text = "groups: [\n";
            let permissions = PermissionsConfig {
                policy: Some("broken".to_owned()),
                sha256: Some(policy_sha256(text)),
                text: Some(text.to_owned()),
            };

            let message = refusal(load_with(permissions, PolicyOrigin::Blob));

            assert!(message.contains("`broken` is not valid"), "{message}");
        }

        #[test]
        fn the_named_policy_loads_and_only_its_name_and_digest_stay_in_the_settings() {
            let settings = load_with(sample_permissions(), PolicyOrigin::Blob)
                .expect("a blob carrying its policy should load");

            let policy = settings
                .permission_policy()
                .expect("should keep the policy with the settings");
            assert_eq!(policy.name(), SAMPLE_POLICY_NAME);
            assert_eq!(policy.sha256(), policy_sha256(SAMPLE_POLICY_TEXT));
            assert_eq!(
                settings.permissions,
                PermissionsConfig {
                    text: None,
                    ..sample_permissions()
                },
                "the parsed file should leave only its name and digest"
            );
        }

        #[test]
        fn two_loads_that_pin_one_digest_parse_the_file_once() {
            let load = || {
                load_with(sample_permissions(), PolicyOrigin::Blob)
                    .expect("a blob carrying its policy should load")
            };
            let (first, second) = (load(), load());

            assert!(
                std::ptr::eq(maps(&first), maps(&second)),
                "the second load should keep the rules the first one parsed"
            );
        }

        #[test]
        fn a_policy_source_supplies_the_file_and_any_text_in_the_blob_is_unused() {
            for (group, blob_text) in [
                ("source-without-blob-text", None),
                ("source-over-blob-text", Some("not the pushed file")),
            ] {
                let text = policy_only_this_test_loads(group);
                let source = MemoryPolicies::holding([("standard", text.as_str())]);
                let permissions = PermissionsConfig {
                    text: blob_text.map(str::to_owned),
                    ..pinned("standard", &text)
                };

                let settings = load_with(permissions, PolicyOrigin::Source(&source))
                    .expect("the source's file should load");

                assert_eq!(source.reads(), 1, "the file should come from the source");
                assert_eq!(
                    settings
                        .permission_policy()
                        .expect("should keep the policy")
                        .name(),
                    "standard"
                );
            }
        }

        #[test]
        fn a_policy_the_source_cannot_read_does_not_load() {
            let text = policy_only_this_test_loads("source-cannot-read");

            let message = refusal(load_with(
                pinned("standard", &text),
                PolicyOrigin::Source(&MemoryPolicies::default()),
            ));

            assert!(
                message.contains("`standard` cannot be read") && message.contains("no file named"),
                "should name the policy and the source's reason: {message}"
            );
        }

        #[test]
        fn a_second_host_pinning_a_held_digest_reads_nothing() {
            let text = policy_only_this_test_loads("second-host");
            let source = MemoryPolicies::holding([("standard", text.as_str())]);

            let first = load_with(pinned("standard", &text), PolicyOrigin::Source(&source))
                .expect("the first host should load");
            assert_eq!(source.reads(), 1, "the first host should read the file");
            let second = load_with(pinned("standard", &text), PolicyOrigin::Source(&source))
                .expect("the second host should load");

            assert_eq!(
                source.reads(),
                1,
                "the second host should read nothing while the rules are held"
            );
            let policy = second.permission_policy().expect("should keep the policy");
            assert_eq!(policy.name(), "standard");
            assert_eq!(policy.sha256(), policy_sha256(&text));
            assert!(
                std::ptr::eq(maps(&first), maps(&second)),
                "the second host should be given the rules the first one loaded"
            );
        }

        #[test]
        fn the_first_load_of_a_digest_reads_the_file_and_checks_it() {
            let text = policy_only_this_test_loads("first-load");
            let edited = format!("{text}# edited after the push\n");
            let source = MemoryPolicies::holding([("standard", edited.as_str())]);

            for reads in 1..=2 {
                let message = refusal(load_with(
                    pinned("standard", &text),
                    PolicyOrigin::Source(&source),
                ));

                assert!(
                    message.contains("`standard` is not the file that was pushed"),
                    "{message}"
                );
                assert_eq!(
                    source.reads(),
                    reads,
                    "a refused file should leave nothing held, so each load reads it"
                );
            }
        }

        #[test]
        fn a_digest_the_instance_does_not_hold_is_read() {
            let pushed_first = policy_only_this_test_loads("pushed-first");
            let pushed_next = policy_only_this_test_loads("pushed-next");
            let first_source = MemoryPolicies::holding([("standard", pushed_first.as_str())]);
            let next_source = MemoryPolicies::holding([("standard", pushed_next.as_str())]);

            let first = load_with(
                pinned("standard", &pushed_first),
                PolicyOrigin::Source(&first_source),
            )
            .expect("the first digest should load");
            let next = load_with(
                pinned("standard", &pushed_next),
                PolicyOrigin::Source(&next_source),
            )
            .expect("the next digest should load");

            assert_eq!(
                next_source.reads(),
                1,
                "a digest the instance does not hold should be read, whatever its name"
            );
            assert_eq!(
                next.permission_policy()
                    .expect("should keep the policy")
                    .sha256(),
                policy_sha256(&pushed_next)
            );
            assert!(
                !std::ptr::eq(maps(&first), maps(&next)),
                "each digest should have its own rules"
            );
        }
    }
}
