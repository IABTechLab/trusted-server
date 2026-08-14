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
    use std::sync::Arc;

    use super::*;
    use crate::integrations::IntegrationRegistry;
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

    fn settings_with_browser_bidder_overlap(auction_enabled: bool) -> Settings {
        let mut settings = test_settings();
        settings.proxy.allowed_domains = vec!["*.example".to_string()];
        settings.auction.enabled = auction_enabled;
        settings.auction.providers = crate::auction::AuctionConfig::legacy_provider_map(&["pbs"]);
        settings.auction.bidders.insert(
            "exampleBidder"
                .parse()
                .expect("should parse server-side bidder"),
            crate::auction::BidderRouteConfig {
                provider: "pbs".parse().expect("should parse provider"),
            },
        );
        let mut prebid = settings
            .integration_config::<crate::integrations::prebid::PrebidIntegrationConfig>("prebid")
            .expect("should parse Prebid config")
            .expect("should have enabled Prebid config");
        prebid.client_side_bidders = vec!["exampleBidder".to_string()];
        settings
            .integrations
            .insert_config("prebid", &prebid)
            .expect("should replace Prebid config");
        settings
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
            settings_from_config_blob(&envelope_json).expect("should reconstruct legacy settings");

        assert!(
            reconstructed.auction.rewrite_creatives,
            "should enable creative rewriting for legacy blobs"
        );
    }

    #[test]
    fn disabled_rewrite_creatives_survives_blob_round_trip() {
        let mut original = test_settings();
        original.auction.rewrite_creatives = false;

        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should reconstruct disabled rewriting");

        assert!(
            !reconstructed.auction.rewrite_creatives,
            "should preserve the explicit rewrite opt-out"
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

    #[test]
    fn runtime_blob_rejects_enabled_browser_bidder_ownership_conflict() {
        let original = settings_with_browser_bidder_overlap(true);
        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should decode conflicting runtime blob before registry construction");
        let plan = Arc::new(
            crate::auction::compile_auction_plan(&reconstructed)
                .expect("should compile decoded enabled auction plan"),
        );

        let error = match IntegrationRegistry::with_plan(&reconstructed, plan) {
            Ok(_) => panic!("runtime registry should reject enabled ownership conflict"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("exampleBidder"));
        assert!(
            error
                .to_string()
                .contains("both client-side and server-side")
        );
    }

    #[test]
    fn runtime_blob_accepts_disabled_browser_bidder_ownership_overlap() {
        let original = settings_with_browser_bidder_overlap(false);
        let reconstructed = settings_from_config_blob(&envelope_json(&original))
            .expect("should decode dormant conflicting runtime blob");
        let plan = Arc::new(
            crate::auction::compile_auction_plan(&reconstructed)
                .expect("should compile decoded disabled auction plan"),
        );

        IntegrationRegistry::with_plan(&reconstructed, plan)
            .expect("runtime registry should accept disabled ownership overlap");
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
