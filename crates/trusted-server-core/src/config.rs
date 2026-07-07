//! Trusted Server typed app-config for the `ts` CLI.
//!
//! This module adapts the existing [`Settings`] shape to `EdgeZero`'s typed
//! blob app-config pipeline. The on-disk TOML remains the normal
//! `trusted-server.toml` structure; the CLI serializes the validated settings
//! as a single [`edgezero_core::blob_envelope::BlobEnvelope`] value through
//! `EdgeZero`'s typed config push path.

use std::borrow::Cow;
use std::collections::HashSet;

use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use validator::{Validate, ValidationError, ValidationErrors};

use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::integrations::{
    adserver_mock::AdServerMockConfig, aps::ApsConfig, datadome::DataDomeConfig,
    didomi::DidomiIntegrationConfig, google_tag_manager::GoogleTagManagerConfig, gpt::GptConfig,
    lockr::LockrConfig, nextjs::NextJsIntegrationConfig, permutive::PermutiveConfig, prebid,
    sourcepoint::SourcepointConfig, testlight::TestlightConfig,
};
use crate::settings::{IntegrationConfig, Settings};

const DEPLOY_VALIDATION_FIELD: &str = "trusted_server";

/// Typed app-config root used by the `ts` CLI.
///
/// This wrapper preserves the existing [`Settings`] TOML/JSON shape while
/// giving the CLI a single type that implements `EdgeZero`'s app-config metadata
/// traits and Trusted Server deploy-time validation.
#[derive(Debug, Clone)]
pub struct TrustedServerAppConfig {
    settings: Settings,
}

impl TrustedServerAppConfig {
    /// Creates a validated app-config wrapper from [`Settings`].
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when deploy validation
    /// fails.
    pub fn new(settings: Settings) -> Result<Self, Report<TrustedServerError>> {
        validate_settings_for_deploy(&settings)?;
        Ok(Self { settings })
    }

    /// Consumes the wrapper and returns the inner [`Settings`].
    #[must_use]
    pub fn into_settings(self) -> Settings {
        self.settings
    }

    /// Returns the inner [`Settings`].
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

impl Serialize for TrustedServerAppConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.settings.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrustedServerAppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let settings = Settings::deserialize(deserializer)?;
        let settings = Settings::finalize_deserialized(settings, "Configuration")
            .map_err(serde::de::Error::custom)?;
        Ok(Self { settings })
    }
}

impl Validate for TrustedServerAppConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        validate_settings_for_deploy(&self.settings)
            .map_err(|report| report_to_validation_errors(&report))
    }
}

impl edgezero_core::app_config::AppConfigMeta for TrustedServerAppConfig {
    // Phase 1 intentionally preserves the existing inline-settings model:
    // `ts config push` publishes the validated Trusted Server config as one
    // app-config blob. Migrating app-level secrets to `EdgeZero` secret-store
    // references (via `#[secret]` on `Settings`) is Phase 3; until then no
    // fields are externalized, so the secret-field set is empty.
    fn secret_fields() -> Vec<edgezero_core::app_config::SecretField> {
        Vec::new()
    }
}

/// Runs Trusted Server deploy-time validation for pushed app config.
///
/// This supplements [`Settings`] structural validation with checks that should
/// fail before an operator publishes a config blob: placeholder secrets,
/// enabled integration startup checks, auction provider references, and EC
/// partner registry construction.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the config should not be deployed.
pub fn validate_settings_for_deploy(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    settings.reject_placeholder_secrets()?;
    let enabled_auction_providers = validate_enabled_integrations(settings)?;
    validate_auction_provider_names(settings, &enabled_auction_providers)?;
    PartnerRegistry::from_config(&settings.ec.partners).map(|_| ())?;
    Ok(())
}

fn validate_enabled_integrations(
    settings: &Settings,
) -> Result<HashSet<&'static str>, Report<TrustedServerError>> {
    let mut enabled_auction_providers = HashSet::new();

    if validate_prebid(settings)? {
        enabled_auction_providers.insert("prebid");
    }
    if validate_integration::<ApsConfig>(settings, "aps")? {
        enabled_auction_providers.insert("aps");
    }
    if validate_integration::<AdServerMockConfig>(settings, "adserver_mock")? {
        enabled_auction_providers.insert("adserver_mock");
    }
    validate_integration::<TestlightConfig>(settings, "testlight")?;
    validate_integration::<NextJsIntegrationConfig>(settings, "nextjs")?;
    validate_integration::<PermutiveConfig>(settings, "permutive")?;
    validate_integration::<LockrConfig>(settings, "lockr")?;
    validate_integration::<DidomiIntegrationConfig>(settings, "didomi")?;
    validate_integration::<SourcepointConfig>(settings, "sourcepoint")?;
    validate_integration::<GoogleTagManagerConfig>(settings, "google_tag_manager")?;
    validate_integration::<DataDomeConfig>(settings, "datadome")?;
    validate_integration::<GptConfig>(settings, "gpt")?;

    Ok(enabled_auction_providers)
}

fn validate_prebid(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    prebid::validate_config_for_startup(settings).map(|config| config.is_some())
}

fn validate_integration<T>(
    settings: &Settings,
    integration_id: &str,
) -> Result<bool, Report<TrustedServerError>>
where
    T: IntegrationConfig,
{
    settings
        .integration_config::<T>(integration_id)
        .map(|config| config.is_some())
}

fn validate_auction_provider_names(
    settings: &Settings,
    enabled_auction_providers: &HashSet<&'static str>,
) -> Result<(), Report<TrustedServerError>> {
    if !settings.auction.enabled {
        return Ok(());
    }

    for provider_name in settings
        .auction
        .providers
        .iter()
        .chain(settings.auction.mediator.iter())
    {
        if !enabled_auction_providers.contains(provider_name.as_str()) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "auction provider `{provider_name}` is listed in [auction] but no enabled integration provides it"
                ),
            }));
        }
    }

    Ok(())
}

fn report_to_validation_errors(report: &Report<TrustedServerError>) -> ValidationErrors {
    let mut error = ValidationError::new("trusted_server_deploy_validation");
    error.message = Some(Cow::Owned(report.to_string()));

    let mut errors = ValidationErrors::new();
    errors.add(DEPLOY_VALIDATION_FIELD, error);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::crate_test_settings_str;

    fn valid_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
    }

    #[test]
    fn wrapper_serializes_as_settings_shape() {
        let settings = valid_settings();
        let app_config =
            TrustedServerAppConfig::new(settings.clone()).expect("should build app config wrapper");

        let settings_value = serde_json::to_value(&settings).expect("should serialize settings");
        let wrapper_value =
            serde_json::to_value(&app_config).expect("should serialize app config wrapper");

        assert_eq!(
            wrapper_value, settings_value,
            "should preserve settings JSON shape"
        );
    }

    #[test]
    fn wrapper_deserializes_from_settings_shape() {
        let toml = crate_test_settings_str();
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize app config wrapper");

        assert_eq!(
            app_config.settings().publisher.domain,
            "test-publisher.com",
            "should load publisher settings"
        );
    }

    #[test]
    fn deploy_validation_rejects_placeholders() {
        let settings = Settings::from_toml(
            r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "change-me-proxy-secret"

[ec]
passphrase = "production-secret-key-32-bytes-min"

[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "production-admin-password-32-bytes"
"#,
        )
        .expect("should parse placeholder settings before deploy validation");

        let err =
            validate_settings_for_deploy(&settings).expect_err("should reject placeholder secrets");

        assert!(
            err.to_string().contains("Insecure default"),
            "error should mention insecure default"
        );
    }

    #[test]
    fn validate_trait_reports_deploy_errors() {
        let mut settings = valid_settings();
        settings.auction.enabled = true;
        settings.auction.providers = vec!["missing-provider".to_string()];
        let app_config = TrustedServerAppConfig { settings };

        let err = app_config
            .validate()
            .expect_err("should reject invalid auction provider");

        assert!(
            err.to_string().contains("missing-provider"),
            "validation error should mention invalid provider"
        );
    }
}
