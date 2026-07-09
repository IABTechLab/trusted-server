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
    lockr::LockrConfig, nextjs::NextJsIntegrationConfig, osano::OsanoConfig,
    permutive::PermutiveConfig, prebid, sourcepoint::SourcepointConfig, testlight::TestlightConfig,
};
use crate::settings::{IntegrationConfig, SecretFieldMode, Settings};

const DEPLOY_VALIDATION_FIELD: &str = "trusted_server";
#[cfg(test)]
const DEPLOY_VALIDATED_INTEGRATION_IDS: &[&str] = &[
    "prebid",
    "aps",
    "adserver_mock",
    "testlight",
    "nextjs",
    "permutive",
    "lockr",
    "didomi",
    "sourcepoint",
    "osano",
    "google_tag_manager",
    "datadome",
    "gpt",
];

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
        let mode = settings.secret_field_mode();
        let settings = Settings::finalize_deserialized(settings, "Configuration", mode)
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
    // Empty on purpose: EdgeZero's `#[secret]` reflection only handles
    // top-level fields, while Trusted Server secrets are nested/array
    // fields. Secret-store references are instead handled in-repo by
    // `crate::secret_refs` (gated by `[secrets]`), which mirrors EdgeZero's
    // key-names-at-rest semantics so the nested `#[secret]` derive can
    // replace it once available upstream.
    const SECRET_FIELDS: &'static [edgezero_core::app_config::SecretField] = &[];
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
    let mode = settings.secret_field_mode();
    match mode {
        // Store mode: secret fields hold key names; value checks
        // (placeholders, token length) run at runtime against the resolved
        // values instead.
        SecretFieldMode::KeyNames => {
            settings.validate_secret_key_names()?;
            reject_conflicting_secret_env_overrides(std::env::vars())?;
        }
        SecretFieldMode::ResolvedValues => settings.reject_placeholder_secrets()?,
    }
    let enabled_auction_providers = validate_enabled_integrations(settings)?;
    validate_auction_provider_names(settings, &enabled_auction_providers)?;
    PartnerRegistry::from_config_with_secret_mode(&settings.ec.partners, mode).map(|_| ())?;
    Ok(())
}

/// Returns `true` if `name` is a `TRUSTED_SERVER__…` env override that targets
/// a store-mode secret field.
///
/// The `ts config push` env overlay maps `TRUSTED_SERVER__<SECTION>__…__<KEY>`
/// onto config fields. For the fields covered by store mode, such an override
/// would replace the secret-ref key NAME with the override's value.
fn is_covered_secret_override(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let Some(rest) = upper.strip_prefix("TRUSTED_SERVER__") else {
        return false;
    };
    rest == "PUBLISHER__PROXY_SECRET"
        || rest == "EC__PASSPHRASE"
        || (rest.starts_with("EC__PARTNERS__")
            && (rest.ends_with("__API_TOKEN") || rest.ends_with("__TS_PULL_TOKEN")))
        || (rest.starts_with("HANDLERS__") && rest.ends_with("__PASSWORD"))
}

/// Rejects `TRUSTED_SERVER__…` env overrides for secret fields while store
/// mode is enabled.
///
/// `ts config push` applies the env overlay by default, which would overwrite
/// a secret-ref key NAME with the override's value and persist it as
/// **plaintext** in the pushed config blob (then fail at runtime because the
/// value is not a valid secret key). In store mode those overrides must not be
/// set — seed the secret store instead. This rejects the dangerous combination
/// at push / diff / validate time, regardless of `--no-env`, so a later push
/// that forgets the flag cannot leak.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming each offending
/// variable when one or more covered overrides are set to a non-empty value.
fn reject_conflicting_secret_env_overrides<I>(vars: I) -> Result<(), Report<TrustedServerError>>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut offenders: Vec<String> = vars
        .into_iter()
        .filter(|(name, value)| !value.is_empty() && is_covered_secret_override(name))
        .map(|(name, _)| name)
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    offenders.sort_unstable();
    offenders.dedup();
    Err(Report::new(TrustedServerError::Configuration {
        message: format!(
            "[secrets] store mode is enabled, but these environment overrides would overwrite \
             secret key names with plaintext values at push time: {}. Unset them (seed the \
             secret store with the real values instead) or disable store mode.",
            offenders.join(", ")
        ),
    }))
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
    validate_integration::<OsanoConfig>(settings, "osano")?;
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
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::crate_test_settings_str;

    fn valid_settings() -> Settings {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
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

    fn store_mode_settings_with_key_names() -> Settings {
        let mut settings = valid_settings();
        settings.secrets.enabled = true;
        settings.publisher.proxy_secret = Redacted::new("proxy_secret".to_owned());
        settings.ec.passphrase = Redacted::new("ec_passphrase".to_owned());
        for handler in &mut settings.handlers {
            handler.password = Redacted::new("admin_password".to_owned());
        }
        settings
    }

    #[test]
    fn deploy_validation_accepts_key_name_secrets_in_store_mode() {
        let settings = store_mode_settings_with_key_names();

        validate_settings_for_deploy(&settings)
            .expect("should accept key-name secrets in store mode");
    }

    #[test]
    fn deploy_validation_accepts_key_name_partner_tokens_in_store_mode() {
        let mut settings = store_mode_settings_with_key_names();
        let partner_toml = r#"
            name = "Example Partner"
            source_domain = "partner.example"
            api_token = "partner_api_token"
        "#;
        settings.ec.partners =
            vec![toml::from_str::<EcPartner>(partner_toml).expect("should parse partner fixture")];

        validate_settings_for_deploy(&settings)
            .expect("should accept short key-name partner tokens in store mode");
    }

    #[test]
    fn deploy_validation_rejects_whitespace_key_names_in_store_mode() {
        let mut settings = store_mode_settings_with_key_names();
        settings.ec.passphrase = Redacted::new("has space".to_owned());

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject whitespace in secret key names");

        assert!(
            err.to_string().contains("ec.passphrase"),
            "error should mention the offending field: {err}"
        );
    }

    #[test]
    fn deploy_validation_rejects_empty_key_names_in_store_mode() {
        let mut settings = store_mode_settings_with_key_names();
        settings.publisher.proxy_secret = Redacted::new(String::new());

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject empty secret key names");

        assert!(
            err.to_string().contains("publisher.proxy_secret"),
            "error should mention the offending field: {err}"
        );
    }

    #[test]
    fn rejects_covered_secret_env_overrides_in_store_mode() {
        let vars = vec![
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            (
                "TRUSTED_SERVER__EC__PASSPHRASE".to_owned(),
                "real-secret".to_owned(),
            ),
            (
                "TRUSTED_SERVER__EC__PARTNERS__0__TS_PULL_TOKEN".to_owned(),
                "real-token".to_owned(),
            ),
            (
                "TRUSTED_SERVER__HANDLERS__1__PASSWORD".to_owned(),
                "real-pass".to_owned(),
            ),
        ];

        let err = reject_conflicting_secret_env_overrides(vars)
            .expect_err("covered secret overrides must be rejected in store mode");
        let message = err.to_string();

        assert!(
            message.contains("TRUSTED_SERVER__EC__PASSPHRASE")
                && message.contains("TS_PULL_TOKEN")
                && message.contains("HANDLERS__1__PASSWORD"),
            "error should name every offending override: {message}"
        );
    }

    #[test]
    fn allows_non_secret_and_empty_env_overrides() {
        let vars = vec![
            // Non-secret override — legitimate, must not be rejected.
            (
                "TRUSTED_SERVER__PUBLISHER__DOMAIN".to_owned(),
                "example.com".to_owned(),
            ),
            // Covered field but empty value — does not overlay, so allowed.
            (
                "TRUSTED_SERVER__PUBLISHER__PROXY_SECRET".to_owned(),
                String::new(),
            ),
            ("HOME".to_owned(), "/home/op".to_owned()),
        ];

        reject_conflicting_secret_env_overrides(vars)
            .expect("non-secret and empty overrides must be allowed");
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
    fn deploy_validation_rejects_external_prebid_bundle_without_proxy_allowed_domains() {
        let mut settings = valid_settings();
        settings.proxy.allowed_domains.clear();

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject external Prebid bundle without proxy allowlist");

        assert!(
            err.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_covers_registered_integration_builders() {
        let validated_ids: HashSet<&'static str> =
            DEPLOY_VALIDATED_INTEGRATION_IDS.iter().copied().collect();
        let missing_ids = crate::integrations::registered_builder_ids()
            .filter(|id| !validated_ids.contains(id))
            .collect::<Vec<_>>();

        assert!(
            missing_ids.is_empty(),
            "deploy validation should cover all registered integration builders: {missing_ids:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_invalid_osano_config() {
        let mut settings = valid_settings();
        settings
            .integrations
            .insert_config(
                "osano",
                &serde_json::json!({ "enabled": true, "typo": true }),
            )
            .expect("should insert Osano config");

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject invalid Osano config during deploy validation");
        let error_text = format!("{err:?}");

        assert!(
            error_text.contains("osano") || error_text.contains("typo"),
            "error should mention Osano or the invalid field: {err:?}"
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
