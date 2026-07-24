//! Runtime helpers for Trusted Server blob app-config payloads.
//!
//! The `ts` CLI delegates blob construction and config-store writes to
//! `EdgeZero`'s typed config push path. Runtime loading only needs to verify the
//! stored [`edgezero_core::blob_envelope::BlobEnvelope`] and reconstruct
//! [`Settings`] from its data value.

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Default config-store key containing the Trusted Server app-config blob.
pub const CONFIG_BLOB_KEY: &str = "trusted_server_config";

/// Reconstruct validated [`Settings`] from a serialized config blob envelope.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, or contains invalid settings data.
pub fn settings_from_config_blob(
    envelope_json: &str,
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

    let settings = Settings::from_json_value(envelope.into_data())?;
    settings.reject_placeholder_secrets()?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        original.ec.providers.hmac = Some(crate::settings::HmacProviderConfig {
            passphrase: Redacted::new("12345678901234567890123456789012".to_string()),
        });
        original.handlers[0].password = Redacted::new("true".to_string());

        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "numeric-looking proxy secret should remain a string"
        );
        assert_eq!(
            reconstructed
                .ec
                .providers
                .hmac
                .as_ref()
                .expect("should reconstruct the hmac provider")
                .passphrase
                .expose(),
            original
                .ec
                .providers
                .hmac
                .as_ref()
                .expect("should keep the hmac provider")
                .passphrase
                .expose(),
            "numeric-looking passphrase should remain a string"
        );
        assert_eq!(
            reconstructed.handlers[0].password.expose(),
            original.handlers[0].password.expose(),
            "boolean-looking handler password should remain a string"
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
