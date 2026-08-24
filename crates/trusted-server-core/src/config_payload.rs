//! Runtime helpers for Trusted Server blob app-config payloads.
//!
//! The `ts` CLI delegates blob construction and config-store writes to
//! `EdgeZero`'s typed config push path. Runtime loading only needs to verify the
//! stored [`edgezero_core::blob_envelope::BlobEnvelope`] and reconstruct
//! [`Settings`] from its data value.

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;

use crate::config::TrustedServerAppConfig;
use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};
use crate::secret_resolution::resolve_secret_references;
use crate::settings::Settings;

/// Canonical logical secret store used by Trusted Server app-config secrets.
pub const DEFAULT_SECRET_STORE_ID: &str = "trusted_server_secrets";

/// Default config-store key containing the Trusted Server app-config blob.
pub const CONFIG_BLOB_KEY: &str = "trusted_server_config";

/// Reconstruct runtime [`Settings`] from a serialized config blob envelope.
///
/// Secret references are resolved after envelope verification and before
/// deserialization. The envelope data itself is never mutated or rewritten.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, secret resolution fails, or resolved
/// settings are invalid.
pub fn settings_from_config_blob(
    envelope_json: &str,
    secret_store: &dyn PlatformSecretStore,
    default_secret_store_name: &StoreName,
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

    let mut data = envelope.into_data();
    remove_inactive_secret_references(&mut data);
    resolve_secret_references::<TrustedServerAppConfig>(
        &mut data,
        secret_store,
        default_secret_store_name,
    )?;
    let settings = Settings::from_json_value(data)?;
    crate::config::validate_settings_for_runtime(&settings)?;
    Ok(settings)
}

fn remove_inactive_secret_references(data: &mut serde_json::Value) {
    if data
        .pointer("/tinybird/enabled")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        && let Some(tinybird) = data
            .get_mut("tinybird")
            .and_then(serde_json::Value::as_object_mut)
    {
        tinybird.remove("auction_token_secret");
        tinybird.remove("access_token_secret");
    }

    let Some(datadome) = data
        .pointer_mut("/integrations/datadome")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let integration_enabled =
        datadome.get("enabled").and_then(serde_json::Value::as_bool) != Some(false);
    let protection_enabled = integration_enabled
        && datadome
            .get("enable_protection")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    if !protection_enabled {
        datadome.remove("server_side_key_secret_name");
    }

    let bypass_enabled = protection_enabled
        && datadome
            .get("protection_test_bypass")
            .and_then(serde_json::Value::as_object)
            .and_then(|bypass| bypass.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    if !bypass_enabled
        && let Some(bypass) = datadome
            .get_mut("protection_test_bypass")
            .and_then(serde_json::Value::as_object_mut)
    {
        bypass.remove("credential_secret_name");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{PlatformError, StoreId};
    use crate::redacted::Redacted;
    use crate::settings::{AssetOriginAuth, ProxyAssetRoute, S3SigV4AuthConfig};
    use crate::test_support::tests::crate_test_settings_str;
    use serde::Deserialize;

    // Intentionally mirrors `AuctionConfig` before `rewrite_creatives` existed.
    // Do not add fields introduced after that snapshot: this test proves a
    // default payload remains readable by the previous binary schema.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyAuctionConfig {
        #[serde(rename = "enabled")]
        _enabled: bool,
        #[serde(rename = "providers")]
        _providers: Vec<String>,
        #[serde(rename = "mediator")]
        _mediator: Option<String>,
        #[serde(rename = "timeout_ms")]
        _timeout_ms: u32,
        #[serde(rename = "creative_store")]
        _creative_store: String,
        #[serde(rename = "allowed_context_keys")]
        _allowed_context_keys: std::collections::HashSet<String>,
    }

    fn test_settings() -> Settings {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
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
                "datadome-server-key" => "resolved-datadome-server-key",
                "datadome-bypass-key" => "resolved-datadome-bypass-credential-32-bytes",
                "s3-access-key" => "AKIAIOSFODNN7EXAMPLE",
                "s3-secret-key" => "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "s3-session-key" => "resolved-session-token",
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
        )
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
            reconstructed.handlers.len(),
            original.handlers.len(),
            "should preserve arrays"
        );
    }

    #[test]
    fn resolves_all_static_credentials_from_the_mapped_default_store() {
        let mut original = test_settings();
        original.tinybird.enabled = true;
        original.tinybird.api_host = "api.example.com".to_string();
        original.tinybird.auction_token_secret =
            Some(Redacted::new("tinybird-token-key".to_string()));
        original
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "enable_protection": true,
                    "server_side_key_secret_name": "datadome-server-key",
                    "protection_test_bypass": {
                        "enabled": true,
                        "credential_secret_name": "datadome-bypass-key",
                    },
                }),
            )
            .expect("should configure DataDome references");
        let mut route = ProxyAssetRoute::new(
            "/assets/",
            "https://examplebucket.s3.us-east-1.amazonaws.com",
        );
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: Some("legacy-s3-store".to_string()),
            access_key_id: Redacted::new("s3-access-key".to_string()),
            secret_access_key: Redacted::new("s3-secret-key".to_string()),
            session_token: Some(Redacted::new("s3-session-key".to_string())),
            origin_query: None,
        }));
        original.proxy.asset_routes.push(route);

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
        )
        .expect("should resolve every static credential from the mapped store");

        assert_eq!(
            reconstructed
                .tinybird
                .auction_token_secret
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-tinybird-token")
        );
        let datadome = reconstructed
            .integration_config::<crate::integrations::datadome::DataDomeConfig>("datadome")
            .expect("should parse DataDome config")
            .expect("should enable DataDome");
        assert_eq!(
            datadome
                .server_side_key_secret_name
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-datadome-server-key")
        );
        let bypass = datadome
            .protection_test_bypass
            .as_ref()
            .expect("should configure bypass");
        assert_eq!(
            bypass
                .credential_secret_name
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("resolved-datadome-bypass-credential-32-bytes")
        );
        let auth = reconstructed.proxy.asset_routes[0]
            .auth
            .as_ref()
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
    }

    #[test]
    fn inactive_optional_features_do_not_resolve_stale_secret_references() {
        let mut original = test_settings();
        original.tinybird.auction_token_secret =
            Some(Redacted::new("unused-tinybird-key".to_string()));
        original
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "enable_protection": false,
                    "server_side_key_secret_name": "unused-datadome-key",
                    "protection_test_bypass": {
                        "enabled": false,
                        "credential_secret_name": "unused-bypass-key",
                    },
                }),
            )
            .expect("should configure inactive references");

        let reconstructed = settings_from_config_blob(
            &envelope_json(&original),
            &UnifiedSecretStore,
            &StoreName::from("ts_secrets"),
        )
        .expect("should skip inactive optional feature references");

        assert!(reconstructed.tinybird.auction_token_secret.is_none());
        let datadome = reconstructed
            .integration_config::<crate::integrations::datadome::DataDomeConfig>("datadome")
            .expect("should parse inactive DataDome config")
            .expect("client-side DataDome remains enabled");
        assert!(datadome.server_side_key_secret_name.is_none());
        assert!(
            datadome
                .protection_test_bypass
                .as_ref()
                .is_some_and(|bypass| bypass.credential_secret_name.is_none())
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
    fn default_auction_payload_is_accepted_by_legacy_schema() {
        let data =
            serde_json::to_value(test_settings()).expect("should serialize settings to JSON");
        let auction = data
            .get("auction")
            .cloned()
            .expect("should serialize auction settings");

        serde_json::from_value::<LegacyAuctionConfig>(auction)
            .expect("should deserialize the default payload with the legacy schema");
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
        original.ec.passphrase = Redacted::new("12345678901234567890123456789012".to_string());
        original.handlers[0].password = Redacted::new("true".to_string());

        let reconstructed =
            load_settings(&envelope_json(&original)).expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "numeric-looking proxy secret should remain a string"
        );
        assert_eq!(
            reconstructed.ec.passphrase.expose(),
            original.ec.passphrase.expose(),
            "numeric-looking passphrase should remain a string"
        );
        assert_eq!(
            reconstructed.handlers[0].password.expose(),
            original.handlers[0].password.expose(),
            "boolean-looking handler password should remain a string"
        );
    }

    #[test]
    fn runtime_validation_rejects_short_resolved_proxy_secret() {
        let mut settings = test_settings();
        settings.publisher.proxy_secret = Redacted::new("short_proxy".to_owned());

        let err = load_settings(&envelope_json(&settings))
            .expect_err("should reject a short resolved proxy secret");

        assert!(
            err.to_string().contains("at least 32 bytes"),
            "error should indicate runtime validation: {err:?}"
        );
        assert!(
            !err.to_string().contains("short_proxy"),
            "error should not expose the secret value"
        );
    }

    #[test]
    fn runtime_validation_rejects_short_resolved_passphrase() {
        let mut settings = test_settings();
        settings.ec.passphrase = Redacted::new("short_key".to_owned());

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
}
