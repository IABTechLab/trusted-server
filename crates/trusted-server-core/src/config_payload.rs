//! Runtime helpers for Trusted Server blob app-config payloads.
//!
//! The `ts` CLI delegates blob construction and config-store writes to
//! `EdgeZero`'s typed config push path. Runtime loading only needs to verify the
//! stored [`edgezero_core::blob_envelope::BlobEnvelope`] and reconstruct
//! [`Settings`] from its data value.

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;

use crate::error::TrustedServerError;
use crate::platform::PlatformSecretStore;
use crate::secret_refs::resolve_secret_refs;
use crate::settings::Settings;

/// Default config-store key containing the Trusted Server app-config blob.
pub const CONFIG_BLOB_KEY: &str = "trusted_server_config";

/// Reconstruct validated [`Settings`] from a serialized config blob envelope.
///
/// Inline mode only: blobs with `[secrets]` mode enabled fail closed. Use
/// [`settings_from_config_blob_with_secrets`] on adapters with a wired
/// secret store.
///
/// # Errors
///
/// See [`settings_from_config_blob_with_secrets`].
pub fn settings_from_config_blob(
    envelope_json: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    settings_from_config_blob_with_secrets(envelope_json, None)
}

/// Reconstruct validated [`Settings`] from a config blob envelope, resolving
/// secret-store references when the blob enables `[secrets]` mode.
///
/// Secret refs are resolved on the verified blob JSON before [`Settings`]
/// parsing, so validation (including placeholder rejection) always runs
/// against resolved values.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, contains invalid settings data,
/// enables `[secrets]` mode while `secret_store` is `None`, or references a
/// secret that cannot be resolved.
pub fn settings_from_config_blob_with_secrets(
    envelope_json: &str,
    secret_store: Option<&dyn PlatformSecretStore>,
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
    let store_mode = data
        .pointer("/secrets/enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if store_mode {
        let secret_store = secret_store.ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "[secrets] mode is enabled but this adapter has no secret store wired; \
                          disable [secrets] or use an adapter with secret-store support"
                    .to_string(),
            })
        })?;
        resolve_secret_refs(&mut data, secret_store)?;
    }

    let settings = Settings::from_json_value(data)?;
    settings.reject_placeholder_secrets()?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::platform::test_support::HashMapSecretStore;
    use crate::redacted::Redacted;
    use crate::test_support::tests::crate_test_settings_str;

    fn test_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
    }

    fn envelope_json(settings: &Settings) -> String {
        let data = serde_json::to_value(settings).expect("should serialize settings to JSON");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_string());
        serde_json::to_string(&envelope).expect("should serialize envelope")
    }

    #[test]
    fn payload_round_trips_through_blob_envelope() {
        let original = test_settings();
        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should reconstruct settings");

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
    fn strings_that_look_like_json_scalars_round_trip_as_strings() {
        let mut original = test_settings();
        original.publisher.proxy_secret = Redacted::new("1234567890".to_string());
        original.ec.passphrase = Redacted::new("12345678901234567890123456789012".to_string());
        original.handlers[0].password = Redacted::new("true".to_string());

        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should reconstruct settings");

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

    fn store_mode_envelope_json() -> String {
        let mut settings = test_settings();
        settings.secrets.enabled = true;
        settings.publisher.proxy_secret = Redacted::new("proxy_secret".to_owned());
        settings.ec.passphrase = Redacted::new("ec_passphrase".to_owned());
        settings.handlers[0].password = Redacted::new("secure_password".to_owned());
        settings.handlers[1].password = Redacted::new("admin_password".to_owned());
        envelope_json(&settings)
    }

    fn store_mode_secret_store() -> HashMapSecretStore {
        HashMapSecretStore::new(HashMap::from([
            ("proxy_secret".to_owned(), b"resolved-proxy-secret".to_vec()),
            (
                "ec_passphrase".to_owned(),
                b"resolved-passphrase-32-bytes-min!".to_vec(),
            ),
            (
                "secure_password".to_owned(),
                b"resolved-secure-password".to_vec(),
            ),
            (
                "admin_password".to_owned(),
                b"resolved-admin-password".to_vec(),
            ),
        ]))
    }

    #[test]
    fn store_mode_blob_resolves_secrets_before_parse() {
        let store = store_mode_secret_store();

        let settings =
            settings_from_config_blob_with_secrets(&store_mode_envelope_json(), Some(&store))
                .expect("should resolve store-mode blob");

        assert_eq!(
            settings.publisher.proxy_secret.expose(),
            "resolved-proxy-secret",
            "should expose resolved proxy secret"
        );
        assert_eq!(
            settings.ec.passphrase.expose(),
            "resolved-passphrase-32-bytes-min!",
            "should expose resolved passphrase"
        );
        assert_eq!(
            settings.handlers[1].password.expose(),
            "resolved-admin-password",
            "should expose resolved handler password"
        );
    }

    #[test]
    fn store_mode_blob_without_secret_store_fails_closed() {
        let err = settings_from_config_blob_with_secrets(&store_mode_envelope_json(), None)
            .expect_err("should fail closed without a secret store");

        assert!(
            err.to_string().contains("[secrets]"),
            "error should mention [secrets] mode: {err}"
        );
    }

    #[test]
    fn store_mode_blob_with_short_resolved_passphrase_fails_validation() {
        let store = HashMapSecretStore::new(HashMap::from([
            ("proxy_secret".to_owned(), b"resolved-proxy-secret".to_vec()),
            ("ec_passphrase".to_owned(), b"too-short".to_vec()),
            (
                "secure_password".to_owned(),
                b"resolved-secure-password".to_vec(),
            ),
            (
                "admin_password".to_owned(),
                b"resolved-admin-password".to_vec(),
            ),
        ]));

        let err = settings_from_config_blob_with_secrets(&store_mode_envelope_json(), Some(&store))
            .expect_err("should reject short resolved passphrase");

        assert!(
            err.to_string().contains("validation failed"),
            "error should mention validation: {err}"
        );
    }

    #[test]
    fn inline_blob_ignores_secret_store() {
        let original = test_settings();
        let store = store_mode_secret_store();

        let reconstructed =
            settings_from_config_blob_with_secrets(&envelope_json(&original), Some(&store))
                .expect("should load inline blob unchanged");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "inline mode should not resolve secrets"
        );
    }

    #[test]
    fn tampered_blob_hash_is_rejected() {
        let mut envelope: BlobEnvelope =
            serde_json::from_str(&envelope_json(&test_settings())).expect("should parse envelope");
        envelope.sha256 = "ff".repeat(32);
        let tampered =
            serde_json::to_string(&envelope).expect("should serialize tampered envelope");

        let err = settings_from_config_blob(&tampered).expect_err("should reject hash mismatch");

        assert!(
            err.to_string().contains("integrity verification"),
            "error should mention integrity verification"
        );
    }
}
