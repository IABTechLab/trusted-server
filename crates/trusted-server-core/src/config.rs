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
    gpt_diagnostics::GptDiagnosticsConfig, lockr::LockrConfig, nextjs::NextJsIntegrationConfig,
    osano::OsanoConfig, permutive::PermutiveConfig, prebid, sourcepoint::SourcepointConfig,
    testlight::TestlightConfig,
};
use crate::settings::{IntegrationConfig, Settings};

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
    "gpt_diagnostics",
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
    // references needs nested/array extraction support and operator migration
    // work tracked separately.
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
    validate_integration::<OsanoConfig>(settings, "osano")?;
    validate_integration::<GoogleTagManagerConfig>(settings, "google_tag_manager")?;
    if let Some(config) = settings.integration_config::<DataDomeConfig>("datadome")? {
        crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup(config)?;
    }
    validate_integration::<GptConfig>(settings, "gpt")?;
    validate_integration::<GptDiagnosticsConfig>(settings, "gpt_diagnostics")?;

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

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LegacyCreativeOpportunitiesConfig {
        gam_network_id: String,
        #[serde(default)]
        auction_timeout_ms: Option<u32>,
        #[serde(default)]
        price_granularity: serde_json::Value,
        #[serde(default)]
        slot: Vec<serde_json::Value>,
    }

    fn serialized_creative_opportunities(gam_unit_path: Option<&str>) -> serde_json::Value {
        let mut toml = crate_test_settings_str();
        toml.push_str(
            r#"

[creative_opportunities]
gam_network_id = "99999"

[[creative_opportunities.slot]]
id = "example-slot"
page_patterns = ["/*"]
formats = [{ width = 300, height = 250 }]
"#,
        );
        if let Some(gam_unit_path) = gam_unit_path {
            toml.push_str(&format!("gam_unit_path = {gam_unit_path:?}\n"));
        }

        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize app config wrapper");
        serde_json::to_value(app_config)
            .expect("should serialize app config wrapper")
            .get("creative_opportunities")
            .cloned()
            .expect("should contain creative opportunities")
    }

    fn valid_settings() -> Settings {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    /// Source-controlled operator-facing config template.
    const EXAMPLE_TEMPLATE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    /// Returns the template with its deliberately-invalid placeholder admin
    /// password swapped for a valid one, so parse-time validation succeeds and
    /// the test can exercise the optional blocks it uncomments.
    fn template_with_valid_admin_password() -> String {
        EXAMPLE_TEMPLATE.replace(
            "password = \"replace-with-admin-password-32-bytes\"",
            "password = \"unit-test-admin-password-that-is-long-enough\"",
        )
    }

    /// Uncomments the contiguous `#`-prefixed block that begins at the line
    /// `# {header}`, leaving the rest of the template untouched. Stops at the
    /// first line that is not a comment (a blank line ends the block).
    fn uncomment_block(template: &str, header: &str) -> String {
        let header_line = format!("# {header}");
        let mut out = Vec::new();
        let mut uncommenting = false;

        for line in template.lines() {
            if line == header_line {
                uncommenting = true;
            } else if uncommenting && !line.trim_start().starts_with('#') {
                uncommenting = false;
            }

            if uncommenting {
                let bare = line
                    .strip_prefix("# ")
                    .or_else(|| line.strip_prefix('#'))
                    .unwrap_or(line);
                out.push(bare.to_owned());
            } else {
                out.push(line.to_owned());
            }
        }

        out.join("\n")
    }

    /// Every documented block should be push-ready: uncommenting it and setting
    /// the shown values must parse and pass field validation. Blocks that ship
    /// a deliberately-invalid placeholder (admin password, `ec.passphrase`, GTM
    /// `container_id`, `request_signing` store ids) are excluded.
    #[test]
    fn documented_integration_blocks_validate_when_uncommented() {
        let base = template_with_valid_admin_password();

        for (header, id) in [
            ("[integrations.permutive]", "permutive"),
            ("[integrations.lockr]", "lockr"),
            ("[integrations.sourcepoint]", "sourcepoint"),
        ] {
            let toml = uncomment_block(&base, header);
            let settings = Settings::from_toml(&toml)
                .unwrap_or_else(|err| panic!("uncommented {header} should parse: {err:?}"));

            match id {
                "permutive" => assert!(
                    settings
                        .integration_config::<PermutiveConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to an enabled, valid config"
                ),
                "lockr" => assert!(
                    settings
                        .integration_config::<LockrConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to an enabled, valid config"
                ),
                "sourcepoint" => assert!(
                    settings
                        .integration_config::<SourcepointConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to an enabled, valid config"
                ),
                other => panic!("unhandled integration id {other}"),
            }
        }
    }

    /// The `[tinybird]` block is top-level and validated at parse time, so
    /// uncommenting it with the documented `api_host` must parse cleanly.
    #[test]
    fn documented_tinybird_block_validates_when_uncommented() {
        let toml = uncomment_block(&template_with_valid_admin_password(), "[tinybird]");
        let settings = Settings::from_toml(&toml)
            .expect("uncommented [tinybird] with documented api_host should parse and validate");
        assert!(
            settings.tinybird.enabled && !settings.tinybird.api_host.is_empty(),
            "tinybird should be enabled with a non-empty api_host"
        );
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
    fn dynamic_gam_unit_templates_are_rejected_by_legacy_schema() {
        for gam_unit_path in ["/{network_id}/example", "/example/{slot_id}"] {
            let creative_opportunities = serialized_creative_opportunities(Some(gam_unit_path));
            let err =
                serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(creative_opportunities)
                    .expect_err("should reject dynamic GAM unit template");

            assert!(
                err.to_string().contains("section_segment"),
                "legacy error should name section_segment: {err}"
            );
        }
    }

    #[test]
    fn static_gam_unit_template_is_accepted_by_legacy_schema() {
        let creative_opportunities = serialized_creative_opportunities(Some("/99999/example/home"));

        serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(creative_opportunities)
            .expect("should accept static GAM unit template");
    }

    #[test]
    fn absent_gam_unit_template_is_accepted_by_legacy_schema() {
        let creative_opportunities = serialized_creative_opportunities(None);

        serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(creative_opportunities)
            .expect("should accept absent GAM unit template");
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
    fn deploy_validation_rejects_example_publisher_hosts() {
        let mut settings = valid_settings();
        settings.publisher.domain = "example.com".to_string();
        settings.publisher.cookie_domain = ".example.com".to_string();
        settings.publisher.origin_url = "https://origin.example.com".to_string();

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject unedited example publisher hosts");
        let text = format!("{err:?}");

        assert!(
            text.contains("publisher.domain")
                && text.contains("publisher.cookie_domain")
                && text.contains("publisher.origin_url"),
            "should flag all three example publisher placeholders: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_placeholder_request_signing_store_ids() {
        let mut settings = valid_settings();
        settings.request_signing = Some(crate::settings::RequestSigning {
            enabled: true,
            config_store_id: "<management-config-store-id>".to_string(),
            secret_store_id: "<management-secret-store-id>".to_string(),
        });

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject placeholder request-signing store ids when enabled");
        let text = format!("{err:?}");

        assert!(
            text.contains("request_signing.config_store_id")
                && text.contains("request_signing.secret_store_id"),
            "should flag both request-signing store ids: {err:?}"
        );
    }

    /// The rotate/deactivate admin routes are registered unconditionally and
    /// read the store IDs without consulting `enabled`, so a disabled block with
    /// placeholder IDs would still reach key management at runtime.
    #[test]
    fn deploy_validation_rejects_placeholder_store_ids_while_request_signing_is_disabled() {
        let mut settings = valid_settings();
        settings.request_signing = Some(crate::settings::RequestSigning {
            enabled: false,
            config_store_id: "<management-config-store-id>".to_string(),
            secret_store_id: "<management-secret-store-id>".to_string(),
        });

        let err = validate_settings_for_deploy(&settings).expect_err(
            "should reject placeholder store ids even while request signing is disabled",
        );
        let text = format!("{err:?}");

        assert!(
            text.contains("request_signing.config_store_id")
                && text.contains("request_signing.secret_store_id"),
            "should flag both request-signing store ids: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_empty_request_signing_store_ids() {
        let mut settings = valid_settings();
        settings.request_signing = Some(crate::settings::RequestSigning {
            enabled: true,
            config_store_id: String::new(),
            secret_store_id: "   ".to_string(),
        });

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject empty and whitespace-only store ids");
        let text = format!("{err:?}");

        assert!(
            text.contains("request_signing.config_store_id")
                && text.contains("request_signing.secret_store_id"),
            "should flag both request-signing store ids: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_blank_aps_account_id() {
        // `deserialize_account_id` trims then rejects an empty result, so blank
        // and whitespace-only ids fail at parse time.
        for (label, account_id) in [("empty", ""), ("whitespace-only", "   ")] {
            let mut settings = valid_settings();
            settings
                .integrations
                .insert_config(
                    "aps",
                    &serde_json::json!({
                        "enabled": true,
                        "account_id": account_id,
                        "endpoint": "https://aps.example.com/e/pb/bid"
                    }),
                )
                .expect("should insert APS config");

            let err = validate_settings_for_deploy(&settings)
                .expect_err("should reject blank APS account_id when enabled");

            assert!(
                format!("{err:?}").contains("aps"),
                "should mention the APS integration for {label} account_id: {err:?}"
            );
        }
    }

    #[test]
    fn deploy_validation_normalizes_padded_aps_account_id() {
        // Surrounding whitespace is normalized (trimmed) at deserialization, so
        // a padded-but-otherwise-valid id deploys and reaches APS trimmed.
        let mut settings = valid_settings();
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({
                    "enabled": true,
                    "account_id": "  example-account  ",
                    "endpoint": "https://aps.example.com/e/pb/bid"
                }),
            )
            .expect("should insert APS config");

        validate_settings_for_deploy(&settings)
            .expect("should accept a padded-but-valid APS account_id (trimmed at deserialization)");
    }

    #[test]
    fn deploy_validation_rejects_padded_request_signing_store_ids() {
        let mut settings = valid_settings();
        settings.request_signing = Some(crate::settings::RequestSigning {
            enabled: false,
            config_store_id: " management-config-store ".to_string(),
            secret_store_id: "management-secret-store ".to_string(),
        });

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject store ids with surrounding whitespace");
        let text = format!("{err:?}");

        assert!(
            text.contains("request_signing.config_store_id")
                && text.contains("request_signing.secret_store_id"),
            "should flag both padded store ids: {err:?}"
        );
    }

    /// `enabled` defaults to `false` for APS, so a section that omits the flag
    /// resolves to disabled and must not have its fields validated — otherwise
    /// the documented template placeholder breaks existing configs on upgrade.
    #[test]
    fn deploy_validation_skips_field_validation_for_integrations_with_omitted_enabled() {
        let mut settings = valid_settings();
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({
                    "pub_id": "your-aps-publisher-id",
                    "endpoint": "https://aps.example.com/e/dtb/bid"
                }),
            )
            .expect("should insert APS config");
        // `endpoint` parses as a plain string but would fail the `url`
        // validator, so this section only survives if validation is skipped for
        // integrations that resolve to disabled.
        settings
            .integrations
            .insert_config(
                "adserver_mock",
                &serde_json::json!({ "endpoint": "not-a-valid-url" }),
            )
            .expect("should insert adserver_mock config");

        validate_settings_for_deploy(&settings).expect(
            "should skip field validation for integrations that resolve to disabled via default",
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
    fn deploy_validation_rejects_invalid_datadome_test_bypass() {
        for (enable_protection, store, name, expected_message) in [
            (
                false,
                "ts_secrets",
                "datadome_test_bypass",
                "requires enable_protection",
            ),
            (true, "", "datadome_test_bypass", "credential_secret_store"),
            (true, "ts_secrets", "", "credential_secret_name"),
        ] {
            let mut settings = valid_settings();
            settings
                .integrations
                .insert_config(
                    "datadome",
                    &serde_json::json!({
                        "enabled": true,
                        "enable_protection": enable_protection,
                        "protection_test_bypass": {
                            "enabled": true,
                            "credential_secret_store": store,
                            "credential_secret_name": name,
                        },
                    }),
                )
                .expect("should insert DataDome config");

            let err = validate_settings_for_deploy(&settings)
                .expect_err("should reject invalid DataDome test bypass");
            assert!(
                format!("{err:?}").contains(expected_message),
                "error should mention the invalid bypass setting: {err:?}"
            );
        }
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
