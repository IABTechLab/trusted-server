//! Trusted Server typed app-config for the `ts` CLI.
//!
//! This module adapts the existing [`Settings`] shape to `EdgeZero`'s typed
//! blob app-config pipeline. The on-disk TOML remains the normal
//! `trusted-server.toml` structure; the CLI serializes the validated settings
//! as a single [`edgezero_core::blob_envelope::BlobEnvelope`] value through
//! `EdgeZero`'s typed config push path.

use std::borrow::Cow;

use edgezero_core::app_config::SecretField;
#[cfg(test)]
use edgezero_core::app_config::SecretPathSegment;
use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use validator::{Validate, ValidationError, ValidationErrors};

use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::integrations::{
    adserver_mock::AdServerMockConfig,
    aps::ApsConfig,
    datadome::DataDomeConfig,
    didomi::DidomiIntegrationConfig,
    google_tag_manager::GoogleTagManagerConfig,
    gpt::GptConfig,
    gpt_diagnostics::GptDiagnosticsConfig,
    js_asset_proxy::{JS_ASSET_PROXY_INTEGRATION_ID, JsAssetProxyConfig},
    lockr::LockrConfig,
    nextjs::NextJsIntegrationConfig,
    osano::OsanoConfig,
    permutive::PermutiveConfig,
    prebid,
    sourcepoint::SourcepointConfig,
    testlight::TestlightConfig,
};
#[cfg(test)]
use crate::settings::AssetOriginAuth;
use crate::settings::{IntegrationConfig, Settings};

const DEPLOY_VALIDATION_FIELD: &str = "trusted_server";
#[cfg(test)]
const DEPLOY_VALIDATED_INTEGRATION_IDS: &[&str] = &[
    #[cfg(feature = "test-utils")]
    crate::test_support::async_html_finalization::ID,
    #[cfg(feature = "test-utils")]
    crate::test_support::secret_binding_runtime::ID,
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
    JS_ASSET_PROXY_INTEGRATION_ID,
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
    /// Creates a push-valid app-config wrapper from [`Settings`].
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when push-safe validation
    /// fails.
    pub fn new(settings: Settings) -> Result<Self, Report<TrustedServerError>> {
        let app_config = Self { settings };
        edgezero_core::app_config::validate_excluding_secrets(&app_config).map_err(|errors| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Configuration validation failed: {errors}"),
            })
        })?;
        Ok(app_config)
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
        unresolved_settings_value(&self.settings)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrustedServerAppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value
            .pointer("/auction/providers")
            .is_some_and(serde_json::Value::is_array)
        {
            return Err(serde::de::Error::custom(
                "auction.providers: removed list schema; see CHANGELOG.md for migration guidance",
            ));
        }
        crate::secret_bindings::reject_removed_references(&value)
            .and_then(|()| crate::secret_bindings::validate_original_endpoints(&value))
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        let mut settings: Settings = serde_json::from_value(value).map_err(|_| {
            serde::de::Error::custom("invalid Trusted Server configuration structure")
        })?;
        settings
            .normalize_deserialized()
            .map_err(|_| serde::de::Error::custom("invalid Trusted Server configuration"))?;
        Ok(Self { settings })
    }
}

impl Validate for TrustedServerAppConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = self.settings.validate().err().unwrap_or_default();
        if let Err(report) = validate_settings_for_deploy(&self.settings) {
            errors.add(
                DEPLOY_VALIDATION_FIELD,
                report_to_validation_error(&report, "trusted_server_deploy_validation"),
            );
        }
        if errors.errors().is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl edgezero_core::app_config::AppConfigMeta for TrustedServerAppConfig {
    fn secret_fields() -> Vec<SecretField> {
        crate::secret_bindings::secret_fields()
    }

    fn implicit_secret_lookups(
        &self,
    ) -> Result<Option<Vec<edgezero_core::app_config::ImplicitSecretLookup>>, String> {
        let plan = crate::secret_bindings::compile_bindings(&self.settings)
            .map_err(|error| error.to_string())?;
        Ok(Some(
            plan.entries()
                .iter()
                .map(|entry| edgezero_core::app_config::ImplicitSecretLookup {
                    descriptor_id: entry.descriptor_id().to_owned(),
                    field_path: entry.field_path().to_owned(),
                    store_id: crate::secret_bindings::SECRET_STORE_ID.to_owned(),
                    key: entry.key().to_owned(),
                })
                .collect(),
        ))
    }

    fn render_implicit_secret_name(
        &self,
        adapter: &str,
        lookup: &edgezero_core::app_config::ImplicitSecretLookup,
        environment: &edgezero_core::env_config::EnvConfig,
    ) -> Result<String, String> {
        let mapped_store = environment.store_name("secrets", &lookup.store_id);
        crate::secret_bindings::render_native_with_mapping(
            adapter,
            &lookup.store_id,
            &lookup.key,
            &mapped_store,
        )
        .map(|binding| binding.native_name)
        .map_err(|error| error.to_string())
    }
}

/// Runs Trusted Server push-time validation for app config.
///
/// Feature configuration implies credential roles, but contains no references or
/// values. This function compiles those roles and validates ordinary settings.
/// `EdgeZero` excludes resolved-value validators using the shared descriptor
/// metadata, then validates generated native names through the implicit hook.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when ordinary configuration or an implicit
/// credential context is invalid.
pub fn validate_settings_for_deploy(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    crate::secret_bindings::compile_bindings(settings)?;
    validate_non_secret_deploy_placeholders(settings)?;
    validate_js_asset_proxy_config(settings)?;

    let mut structural_settings = settings.clone();
    structural_settings.prepare_runtime()?;
    structural_settings.validate_admin_coverage()?;

    let plan = crate::auction::compile_auction_plan(settings)?;
    validate_enabled_integrations(settings, &plan, false)?;
    PartnerRegistry::validate_config_for_deploy(&settings.ec.partners)?;
    Ok(())
}

/// Runs Trusted Server runtime validation after implicit bindings are resolved.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when resolved secrets or runtime-only
/// configuration checks are invalid.
pub fn validate_settings_for_runtime(
    settings: &Settings,
) -> Result<(), Report<TrustedServerError>> {
    settings.reject_placeholder_secrets()?;
    validate_js_asset_proxy_config(settings)?;
    settings.validate_admin_handler_passwords()?;
    let plan = crate::auction::compile_auction_plan(settings)?;
    validate_enabled_integrations(settings, &plan, true)?;
    PartnerRegistry::from_settings(settings).map(|_| ())?;
    Ok(())
}

fn validate_js_asset_proxy_config(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let Some(raw_config) = settings.integrations.get(JS_ASSET_PROXY_INTEGRATION_ID) else {
        return Ok(());
    };

    let config: JsAssetProxyConfig = serde_json::from_value(raw_config.clone()).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "integration startup failed for `{JS_ASSET_PROXY_INTEGRATION_ID}`: configuration could not be parsed: {error}"
            ),
        })
    })?;
    config.validate().map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "integration startup failed for `{JS_ASSET_PROXY_INTEGRATION_ID}`: {error}"
            ),
        })
    })
}

fn validate_enabled_integrations(
    settings: &Settings,
    plan: &crate::auction::AuctionPlan,
    resolved_secrets: bool,
) -> Result<(), Report<TrustedServerError>> {
    validate_prebid(settings, plan)?;
    validate_integration::<ApsConfig>(settings, "aps")?;
    validate_integration::<AdServerMockConfig>(settings, "adserver_mock")?;
    #[cfg(feature = "test-utils")]
    validate_integration::<crate::test_support::async_html_finalization::FixtureConfig>(
        settings,
        crate::test_support::async_html_finalization::ID,
    )?;
    #[cfg(feature = "test-utils")]
    validate_integration::<crate::test_support::secret_binding_runtime::FixtureConfig>(
        settings,
        crate::test_support::secret_binding_runtime::ID,
    )?;
    validate_integration::<TestlightConfig>(settings, "testlight")?;
    validate_integration::<NextJsIntegrationConfig>(settings, "nextjs")?;
    validate_integration::<PermutiveConfig>(settings, "permutive")?;
    validate_integration::<LockrConfig>(settings, "lockr")?;
    validate_integration::<DidomiIntegrationConfig>(settings, "didomi")?;
    validate_integration::<SourcepointConfig>(settings, "sourcepoint")?;
    validate_integration::<OsanoConfig>(settings, "osano")?;
    validate_integration::<GoogleTagManagerConfig>(settings, "google_tag_manager")?;
    if let Some(config) = settings.integration_config::<DataDomeConfig>("datadome")? {
        if resolved_secrets {
            crate::integrations::datadome::DataDomeIntegration::validate_config_for_startup(
                config,
            )?;
        } else {
            crate::integrations::datadome::DataDomeIntegration::validate_config_for_deploy(config)?;
        }
    }
    validate_integration::<GptConfig>(settings, "gpt")?;
    validate_integration::<GptDiagnosticsConfig>(settings, "gpt_diagnostics")?;

    Ok(())
}

fn validate_prebid(
    settings: &Settings,
    plan: &crate::auction::AuctionPlan,
) -> Result<(), Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<prebid::PrebidIntegrationConfig>("prebid")?
    else {
        return Ok(());
    };
    prebid::validate_browser_config_for_startup(&config, &settings.proxy.allowed_domains)?;
    prebid::validate_browser_bidder_ownership(&config, plan)
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

fn validate_non_secret_deploy_placeholders(
    settings: &Settings,
) -> Result<(), Report<TrustedServerError>> {
    let mut insecure_fields = Vec::new();

    if crate::settings::Publisher::is_placeholder_domain(&settings.publisher.domain) {
        insecure_fields.push("publisher.domain");
    }
    if crate::settings::Publisher::is_placeholder_cookie_domain(&settings.publisher.cookie_domain) {
        insecure_fields.push("publisher.cookie_domain");
    }
    if crate::settings::Publisher::is_placeholder_origin_url(&settings.publisher.origin_url) {
        insecure_fields.push("publisher.origin_url");
    }
    if let Some(request_signing) = &settings.request_signing {
        if crate::settings::RequestSigning::is_unusable_store_id(&request_signing.config_store_id) {
            insecure_fields.push("request_signing.config_store_id");
        }
        if crate::settings::RequestSigning::is_unusable_store_id(&request_signing.secret_store_id) {
            insecure_fields.push("request_signing.secret_store_id");
        }
    }

    if insecure_fields.is_empty() {
        return Ok(());
    }

    Err(Report::new(TrustedServerError::InsecureDefault {
        field: insecure_fields.join(", "),
    }))
}

/// Serialize only unresolved feature configuration, never resolved credentials.
///
/// # Errors
/// Returns a safe configuration error if settings cannot be represented as JSON.
pub fn unresolved_settings_value(
    settings: &Settings,
) -> Result<serde_json::Value, Report<TrustedServerError>> {
    let mut value = serde_json::to_value(settings).map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "failed to serialize unresolved configuration".to_owned(),
        })
    })?;
    if let Some(datadome) = value
        .pointer_mut("/integrations/datadome")
        .and_then(serde_json::Value::as_object_mut)
    {
        datadome.remove("server_side_key_secret_name");
        if let Some(bypass) = datadome
            .get_mut("protection_test_bypass")
            .and_then(serde_json::Value::as_object_mut)
        {
            bypass.remove("credential_secret_name");
        }
    }
    Ok(value)
}

fn report_to_validation_error(
    report: &Report<TrustedServerError>,
    code: &'static str,
) -> ValidationError {
    let mut error = ValidationError::new(code);
    error.message = Some(Cow::Owned(report.to_string()));
    error
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::auction_config_types::{NotificationConfig, ProviderConfig, RoutingMode};
    use crate::redacted::Redacted;
    use crate::settings::{ProxyAssetRoute, S3SigV4AuthConfig, TrustedClientIpConfig};
    use crate::test_support::tests::crate_test_settings_str as resolved_test_settings_str;

    fn crate_test_settings_str() -> String {
        let mut value: toml::Table =
            toml::from_str(&resolved_test_settings_str()).expect("should parse resolved fixture");
        value
            .get_mut("publisher")
            .and_then(toml::Value::as_table_mut)
            .expect("should find publisher")
            .remove("proxy_secret");
        value
            .get_mut("ec")
            .and_then(toml::Value::as_table_mut)
            .expect("should find EC")
            .remove("passphrase");
        for handler in value
            .get_mut("handlers")
            .and_then(toml::Value::as_array_mut)
            .expect("should find handlers")
        {
            handler
                .as_table_mut()
                .expect("should find handler")
                .remove("password");
        }
        toml::to_string(&value).expect("should encode feature-only fixture")
    }
    use edgezero_core::app_config::AppConfigMeta;
    use edgezero_core::blob_envelope::BlobEnvelope;

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

    fn app_config_with_creative_opportunities(
        gam_unit_path: Option<&str>,
    ) -> TrustedServerAppConfig {
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

        let mut app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize app config wrapper");
        app_config.settings.proxy.allowed_domains =
            vec!["*.example".to_owned(), "*.example.com".to_owned()];
        app_config
    }

    fn serialized_creative_opportunities(gam_unit_path: Option<&str>) -> serde_json::Value {
        serde_json::to_value(app_config_with_creative_opportunities(gam_unit_path))
            .expect("should serialize app config wrapper")
            .get("creative_opportunities")
            .cloned()
            .expect("should contain creative opportunities")
    }

    fn valid_settings() -> Settings {
        let mut settings =
            Settings::from_toml(&resolved_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    #[test]
    fn settings_collections_produce_stable_envelope_hashes_across_parses() {
        let mut source = serde_json::to_value(valid_settings()).expect("should serialize settings");
        source["response_headers"] = serde_json::json!({"x-example-b": "b", "x-example-a": "a"});
        source["image_optimizer"] = serde_json::json!({"profile_sets": {
            "secondary": {"profiles": {"default": "width=200", "small": "width=100"}},
            "primary": {"profiles": {"default": "width=400", "small": "width=200"}}
        }});
        source["auction"]["allowed_context_keys"] = serde_json::json!([
            "zeta", "alpha", "gamma", "beta", "epsilon", "delta", "alpha"
        ]);
        source["auction"]["providers"] = serde_json::json!({
            "secondary": {
                "protocol": "openrtb-2.6", "endpoint": "https://secondary.example.com/auction",
                "notifications": {"suppress_seats": ["seat-b", "seat-a"]},
                "profile_config": {"second": 2, "first": 1}
            },
            "primary": {
                "protocol": "openrtb-2.6", "endpoint": "https://primary.example.com/auction",
                "notifications": {"suppress_seats": ["seat-b", "seat-a"]},
                "profile_config": {"second": 2, "first": 1}
            }
        });
        source["auction"]["bidders"] = serde_json::json!({
            "bidder-b": {"provider": "secondary"}, "bidder-a": {"provider": "primary"}
        });
        source["creative_opportunities"] = serde_json::json!({
            "gam_network_id": "99999",
            "slot": [{
                "id": "example-slot", "page_patterns": ["/*"],
                "formats": [{"width": 300, "height": 250}],
                "targeting": {"section": "example", "category": "news"},
                "providers": {"prebid": {"bidders": {
                    "bidder-b": {"placement": "b", "account": "example"},
                    "bidder-a": {"placement": "a", "account": "example"}
                }}}
            }]
        });
        // The base fixture also has two integrations with multiple config entries.
        // Exercise fresh hash seeds on every parse, including nested maps.
        let parse = || {
            serde_json::from_value::<TrustedServerAppConfig>(source.clone())
                .expect("should parse collection-rich settings")
        };
        let serialize = |config: &TrustedServerAppConfig| {
            serde_json::to_value(config).expect("should serialize typed config")
        };
        let first = serialize(&parse());
        let expected_sha = BlobEnvelope::new(first, "2026-01-01T00:00:00Z".to_owned()).sha256;
        for _ in 0..32 {
            let value = serialize(&parse());
            assert_eq!(
                value["auction"]["allowed_context_keys"],
                serde_json::json!(["alpha", "beta", "delta", "epsilon", "gamma", "zeta"]),
                "should sort and deduplicate allowlist keys"
            );
            assert_eq!(
                BlobEnvelope::new(value, "2026-01-01T00:00:00Z".to_owned()).sha256,
                expected_sha,
                "should produce one envelope hash for equal settings"
            );
        }
    }

    fn insert_aps_provider(settings: &mut Settings, account_id: &str) {
        settings.auction.providers.insert(
            "aps-main".parse().expect("should parse APS provider ID"),
            ProviderConfig {
                protocol: "openrtb-2.6".to_string(),
                profile: "aps".to_string(),
                endpoint: "https://aps.example.com/e/pb/bid".to_string(),
                timeout_ms: None,
                routing: RoutingMode::AllEligible,
                notifications: NotificationConfig::default(),
                profile_config: serde_json::json!({ "account_id": account_id }),
            },
        );
    }

    /// Source-controlled operator-facing config template.
    const EXAMPLE_TEMPLATE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    /// Supply resolved local-comparison shadows only for direct Settings tests.
    /// The public template itself contains no credential properties.
    fn template_with_resolved_required_secrets() -> String {
        EXAMPLE_TEMPLATE
            .replace(
                "\n[[handlers]]\n",
                "\n[[handlers]]\npassword = \"fictional-handler-password-0123456789\"\n",
            )
            .replace(
                "\n[publisher]\n",
                "\n[publisher]\nproxy_secret = \"fictional-publisher-secret-0123456789\"\n",
            )
            .replace(
                "\n[ec]\n",
                "\n[ec]\npassphrase = \"fictional-ec-passphrase-0123456789-abcdef\"\n",
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
                // Explanatory comments beside the example remain comments.
                let assignment = bare.split_once('=').is_some_and(|(key, _)| {
                    !key.trim().is_empty()
                        && key.trim().bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                        })
                });
                if bare.trim_start().starts_with('[') || assignment {
                    out.push(bare.to_owned());
                } else {
                    out.push(line.to_owned());
                }
            } else {
                out.push(line.to_owned());
            }
        }

        out.join("\n")
    }

    /// Every documented block should be push-ready: uncommenting it and setting
    /// the shown values must parse and pass field validation. Blocks that ship
    /// a deliberately-invalid non-secret placeholder (GTM `container_id` and
    /// `request_signing` store ids) are excluded.
    #[test]
    fn documented_integration_blocks_validate_when_uncommented() {
        let base = template_with_resolved_required_secrets();

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
        let toml = uncomment_block(&template_with_resolved_required_secrets(), "[tinybird]");
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
    fn app_config_image_optimizer_rejects_trimmed_key_collisions() {
        for (section, first, second) in [
            (
                r#"
                [image_optimizer.profile_sets.default_images]
                default_profile = "medium"
                [image_optimizer.profile_sets.default_images.profiles]
                medium = "width=100"
                " medium" = "width=200"
                "#,
                "medium",
                " medium",
            ),
            (
                r#"
                [image_optimizer.profile_sets.default_images.profiles]
                default = "width=100"
                [image_optimizer.profile_sets." default_images".profiles]
                default = "width=200"
                "#,
                "default_images",
                " default_images",
            ),
        ] {
            let toml = crate_test_settings_str() + section;
            let raw: Settings =
                toml::from_str(&toml).expect("should deserialize unnormalized settings");
            let value = serde_json::to_value(raw).expect("should serialize unnormalized settings");
            let toml_error = toml::from_str::<TrustedServerAppConfig>(&toml)
                .expect_err("should reject colliding app-config keys");
            let json_error = serde_json::from_value::<TrustedServerAppConfig>(value)
                .expect_err("should reject colliding app-config keys");

            for message in [toml_error.to_string(), json_error.to_string()] {
                assert!(message.contains("invalid Trusted Server configuration"));
                assert!(!message.contains(&format!("{first:?}")));
                assert!(!message.contains(&format!("{second:?}")));
            }
        }
    }

    #[test]
    fn app_config_image_optimizer_normalizes_non_colliding_keys() {
        let toml = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets." default_images "]
            default_profile = " medium "
            [image_optimizer.profile_sets." default_images ".profiles]
            " medium " = " width=100 "
            "#;
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize non-colliding app config");
        let profile_set = &app_config.settings().image_optimizer.profile_sets["default_images"];
        assert_eq!(
            profile_set.default_profile, "medium",
            "should trim the default profile reference"
        );
        assert_eq!(
            profile_set.profiles["medium"], "width=100",
            "should trim profile keys and values"
        );
    }

    #[test]
    fn push_validation_ignores_resolved_shadows_and_never_serializes_them() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::new("publisher_proxy".to_owned());
        settings.ec.passphrase = Redacted::new("ec_key".to_owned());
        settings.handlers[0].password = Redacted::new("handler_password".to_owned());
        settings.handlers[1].password = Redacted::new("admin_password".to_owned());
        let app_config = TrustedServerAppConfig::new(settings)
            .expect("should validate key names without values");

        let serialized =
            serde_json::to_string(&app_config).expect("should serialize key-name-only app config");
        assert!(!serialized.contains("publisher_proxy"));
        assert!(!serialized.contains("unit-test-proxy-secret"));
        assert!(!serialized.contains("proxy_secret"));
    }

    #[test]
    fn secret_metadata_lists_all_secret_paths_and_optionality() {
        let fields = TrustedServerAppConfig::secret_fields();
        let paths = fields
            .iter()
            .map(|field| (field.dotted_path(), field.optional))
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                ("publisher.proxy_secret".to_owned(), false),
                ("ec.passphrase".to_owned(), false),
                ("ec.partners[*].api_token".to_owned(), true),
                ("ec.partners[*].ts_pull_token".to_owned(), true),
                ("handlers[*].password".to_owned(), false),
                ("trusted_client_ip.shared_secret".to_owned(), false),
                ("tinybird.auction_token_secret".to_owned(), true),
                (
                    "integrations.datadome.server_side_key_secret_name".to_owned(),
                    true,
                ),
                (
                    "integrations.datadome.protection_test_bypass.credential_secret_name"
                        .to_owned(),
                    true,
                ),
                ("proxy.asset_routes[*].auth.access_key_id".to_owned(), true),
                (
                    "proxy.asset_routes[*].auth.secret_access_key".to_owned(),
                    true,
                ),
                ("proxy.asset_routes[*].auth.session_token".to_owned(), true),
            ],
            "should expose the native EdgeZero secret metadata contract"
        );
        assert!(
            fields.iter().all(|field| matches!(
                field.kind,
                edgezero_core::app_config::SecretKind::KeyInDefault
            )),
            "all Trusted Server app secrets should use the default secret store"
        );
    }

    #[test]
    fn partner_secret_metadata_makes_the_defaulted_array_optional() {
        let fields = TrustedServerAppConfig::secret_fields();

        for field in fields.iter().filter(|field| {
            matches!(
                field.dotted_path().as_str(),
                "ec.partners[*].api_token" | "ec.partners[*].ts_pull_token"
            )
        }) {
            assert!(matches!(
                &field.path[1],
                SecretPathSegment::OptionalField(name) if name == "partners"
            ));
        }
    }

    #[test]
    fn s3_credentials_are_implicit_and_optional_session_defaults_off() {
        let auth: S3SigV4AuthConfig =
            toml::from_str("region = \"us-east-1\"").expect("should parse feature-only S3 auth");
        assert!(auth.access_key_id.expose().is_empty());
        assert!(auth.secret_access_key.expose().is_empty());
        assert!(!auth.session_token_enabled);
        let serialized = serde_json::to_value(auth).expect("should serialize S3 auth");
        assert!(serialized.get("access_key_id").is_none());
        assert!(serialized.get("secret_access_key").is_none());
        assert!(serialized.get("session_token").is_none());
    }

    #[test]
    fn legacy_static_secret_store_selectors_are_accepted_but_not_serialized() {
        let mut settings = valid_settings();
        settings.tinybird.secret_store = Some("legacy-tinybird-store".to_string());
        settings
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "server_side_key_secret_store": "legacy-datadome-store",
                    "protection_test_bypass": {
                        "enabled": false,
                        "credential_secret_store": "legacy-bypass-store",
                    },
                }),
            )
            .expect("should insert legacy DataDome selectors");
        let mut route = ProxyAssetRoute::new(
            "/assets/",
            "https://examplebucket.s3.us-east-1.amazonaws.com",
        );
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: Some("legacy-s3-store".to_string()),
            access_key_id: Redacted::new("s3-access-key".to_string()),
            secret_access_key: Redacted::new("s3-secret-key".to_string()),
            session_token: None,
            session_token_enabled: false,
            origin_query: None,
        }));
        settings.proxy.asset_routes.push(route);

        settings
            .normalize_deserialized()
            .expect("should normalize settings without key collisions");
        let serialized = serde_json::to_string(&settings).expect("should serialize settings");

        for legacy_store in [
            "legacy-tinybird-store",
            "legacy-datadome-store",
            "legacy-bypass-store",
            "legacy-s3-store",
        ] {
            assert!(
                !serialized.contains(legacy_store),
                "serialized config should omit deprecated selector {legacy_store}"
            );
        }
    }

    #[test]
    fn settings_debug_redacts_resolved_static_credentials() {
        let mut settings = valid_settings();
        settings.tinybird.auction_token_secret =
            Some(Redacted::new("resolved-tinybird-secret".to_string()));
        settings
            .integrations
            .insert_config(
                "datadome",
                &serde_json::json!({
                    "enabled": true,
                    "server_side_key_secret_name": "resolved-datadome-secret",
                }),
            )
            .expect("should insert resolved DataDome config");

        let debug = format!("{settings:?}");

        assert!(!debug.contains("resolved-tinybird-secret"));
        assert!(!debug.contains("resolved-datadome-secret"));
        assert!(debug.contains("datadome"));
    }

    #[test]
    fn app_config_deserialization_does_not_finalize_runtime_templates() {
        let creative_opportunities =
            serialized_creative_opportunities(Some("/{network_id}/example"));
        let slot = creative_opportunities["slot"][0]
            .as_object()
            .expect("should serialize creative opportunity slot");

        assert!(
            slot.contains_key("gam_unit_path"),
            "push deserialization should preserve the operator config field"
        );
        assert!(
            !slot.contains_key("section_segment"),
            "push deserialization should not add runtime-only compiled fields"
        );
    }

    #[test]
    fn wrapper_rejects_legacy_auction_provider_list_with_migration_guidance() {
        let toml = format!(
            "{}\n[auction]\nproviders = [\"prebid\"]\n",
            crate_test_settings_str()
        );

        let error = toml::from_str::<TrustedServerAppConfig>(&toml)
            .expect_err("should reject the removed auction provider list schema");
        let rendered = error.to_string();
        assert!(
            rendered.contains("auction.providers"),
            "should identify the removed field: {rendered}"
        );
        assert!(
            rendered.contains("CHANGELOG.md"),
            "should direct operators to migration guidance: {rendered}"
        );
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

        assert!(
            creative_opportunities.get("enabled").is_none(),
            "default template switch should be omitted for legacy binaries"
        );
        serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(creative_opportunities)
            .expect("should accept absent GAM unit template");
    }

    #[test]
    fn disabled_creative_opportunities_flag_is_rejected_by_legacy_schema() {
        let mut toml = crate_test_settings_str();
        toml.push_str(
            r#"

[creative_opportunities]
enabled = false
gam_network_id = "99999"
"#,
        );
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize app config wrapper");
        let creative_opportunities = serde_json::to_value(app_config)
            .expect("should serialize app config wrapper")
            .get("creative_opportunities")
            .cloned()
            .expect("should contain creative opportunities");

        serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(creative_opportunities)
            .expect_err("legacy binaries should reject an explicit disabled switch");
    }

    #[test]
    fn app_config_requires_no_secret_reference_or_value() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::default();
        settings.ec.passphrase = Redacted::default();
        for handler in &mut settings.handlers {
            handler.password = Redacted::default();
        }
        let config = TrustedServerAppConfig::new(settings)
            .expect("should validate unresolved implicit credentials");
        let lookups = config
            .implicit_secret_lookups()
            .expect("should compile implicit lookups")
            .expect("should opt in");
        assert_eq!(lookups.len(), 4);
        assert!(lookups.iter().all(|lookup| lookup.key.starts_with("ts1_")));
    }

    #[test]
    fn app_config_compiles_trusted_client_ip_credential_without_serializing_shadow() {
        let mut settings = valid_settings();
        settings.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "x-ts-client-ip".to_owned(),
            auth_header: "x-ts-client-ip-auth".to_owned(),
            shared_secret: Redacted::new("trusted_client_ip_shared_secret".to_owned()),
        });

        TrustedServerAppConfig::new(settings)
            .expect("should validate the shared-secret key name without treating it as the value");
    }

    #[test]
    fn raw_config_rejects_reference_properties_even_when_empty() {
        let source: serde_json::Value =
            serde_json::to_value(valid_settings()).expect("should serialize feature config");
        for value in ["", " \t ", "fictional-native-secret-key"] {
            let mut source = source.clone();
            source["publisher"]["proxy_secret"] = serde_json::json!(value);
            let error = serde_json::from_value::<TrustedServerAppConfig>(source)
                .expect_err("should reject removed reference property");
            assert!(error.to_string().contains("publisher.proxy_secret"));
            assert!(!error.to_string().contains("fictional-native-secret-key"));
        }
    }

    #[test]
    fn app_config_new_rejects_invalid_non_secret_settings() {
        let mut settings = valid_settings();
        settings.publisher.domain = "invalid/domain".to_owned();

        let err = TrustedServerAppConfig::new(settings)
            .expect_err("should reject invalid publisher domain before creating an app config");

        assert!(
            err.to_string().contains("invalid_publisher_domain"),
            "error should identify the structural validation failure: {err:?}"
        );
    }

    #[test]
    fn runtime_validation_rejects_placeholders() {
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
        .expect("should parse placeholder settings before runtime validation");

        let err = validate_settings_for_runtime(&settings)
            .expect_err("should reject placeholder secrets at runtime");

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
        for (label, account_id) in [("empty", ""), ("whitespace-only", "   ")] {
            let mut settings = valid_settings();
            insert_aps_provider(&mut settings, account_id);

            let err = validate_settings_for_deploy(&settings)
                .expect_err("should reject blank APS account_id");

            assert!(
                format!("{err:?}").contains("account_id"),
                "should mention the APS profile account_id for {label}: {err:?}"
            );
        }
    }

    #[test]
    fn deploy_validation_normalizes_padded_aps_account_id() {
        let mut settings = valid_settings();
        insert_aps_provider(&mut settings, "  example-account  ");

        validate_settings_for_deploy(&settings)
            .expect("should accept a padded APS profile account_id after trimming it");
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

    /// Integrations that default to disabled do not validate inactive fields.
    #[test]
    fn deploy_validation_skips_field_validation_for_integrations_with_omitted_enabled() {
        let mut settings = valid_settings();
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
    fn deploy_validation_rejects_retired_aps_fields_when_explicitly_disabled() {
        let mut settings = valid_settings();
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({
                    "enabled": false,
                    "endpoint": "https://aps.example.com/e/pb/bid"
                }),
            )
            .expect("should insert disabled APS config with a retired field");

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should reject retired APS server fields when explicitly disabled");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("Integration 'aps' configuration could not be parsed"),
            "should identify the APS configuration: {rendered}"
        );
        assert!(
            rendered.contains("endpoint"),
            "should identify the retired APS field: {rendered}"
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
    fn validate_rejects_invalid_enabled_js_asset_proxy_config() {
        let mut settings = valid_settings();
        settings.integrations.insert(
            "js_asset_proxy".to_string(),
            serde_json::json!({ "enabled": true }),
        );

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject invalid JS asset proxy config");
        let message = err.to_string();
        assert!(
            message.contains("js_asset_proxy"),
            "error should mention JS asset proxy validation"
        );
        assert!(
            message.contains("empty_assets") || message.contains("assets"),
            "error should mention the missing assets"
        );
    }

    #[test]
    fn deploy_validation_requires_external_bundle_url_for_enabled_prebid() {
        let mut settings = valid_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "bundle": {
                        "modules": { "bidder": ["exampleBidderBidAdapter"] }
                    }
                }),
            )
            .expect("should insert enabled Prebid config");

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should require enabled Prebid external bundle URL");
        assert!(error.to_string().contains("external_bundle_url"));
    }

    #[test]
    fn deploy_validation_rejects_conflicting_prebid_browser_bidder_ownership() {
        let mut settings = valid_settings();
        settings.auction.enabled = true;
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
            .integration_config::<prebid::PrebidIntegrationConfig>("prebid")
            .expect("should parse Prebid config")
            .expect("should have enabled Prebid config");
        prebid.client_side_bidders = vec!["exampleBidder".to_string()];
        settings
            .integrations
            .insert_config("prebid", &prebid)
            .expect("should replace Prebid config");

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should reject conflicting browser bidder ownership");
        assert!(error.to_string().contains("exampleBidder"));
        assert!(
            error
                .to_string()
                .contains("both client-side and server-side")
        );
    }

    #[test]
    fn deploy_validation_accepts_dormant_prebid_browser_bidder_overlap() {
        let mut settings = valid_settings();
        settings.auction.enabled = false;
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
            .integration_config::<prebid::PrebidIntegrationConfig>("prebid")
            .expect("should parse Prebid config")
            .expect("should have enabled Prebid config");
        prebid.client_side_bidders = vec!["exampleBidder".to_string()];
        settings
            .integrations
            .insert_config("prebid", &prebid)
            .expect("should replace Prebid config");

        validate_settings_for_deploy(&settings)
            .expect("disabled plan overlap should remain deployable");
    }

    #[test]
    fn deploy_validation_accepts_typed_prebid_bundle_build_table() {
        let settings = valid_settings();

        validate_settings_for_deploy(&settings)
            .expect("test config with typed Prebid bundle build table should validate");
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
    fn deploy_validation_checks_bypass_activation_without_credential_references() {
        let mut settings = valid_settings();
        settings.integrations.insert(
            "datadome".to_owned(),
            serde_json::json!({
                "enabled": true, "enable_protection": false,
                "protection_test_bypass": {"enabled": true}
            }),
        );
        let error = validate_settings_for_deploy(&settings)
            .expect_err("should reject bypass without Protection");
        assert!(format!("{error:?}").contains("requires enable_protection"));
        settings
            .integrations
            .get_mut("datadome")
            .expect("should find integration")["enable_protection"] = serde_json::json!(true);
        validate_settings_for_deploy(&settings)
            .expect("should validate implicit DataDome roles without reading credentials");
        let plan = crate::secret_bindings::compile_bindings(&settings)
            .expect("should compile implicit roles");
        assert!(
            plan.entries()
                .iter()
                .any(|entry| entry.descriptor_id() == "datadome.server")
        );
        assert!(
            plan.entries()
                .iter()
                .any(|entry| entry.descriptor_id() == "datadome.bypass")
        );
    }

    #[test]
    fn validate_rejects_invalid_disabled_js_asset_proxy_assets() {
        let mut settings = valid_settings();
        settings.integrations.insert(
            JS_ASSET_PROXY_INTEGRATION_ID.to_string(),
            serde_json::json!({
                "enabled": false,
                "assets": [{
                    "path": "bad path",
                    "origin_url": "not-a-url",
                    "proxy": "disabled"
                }]
            }),
        );

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject invalid disabled asset inventory");
        let message = err.to_string();
        assert!(
            message.contains(JS_ASSET_PROXY_INTEGRATION_ID),
            "error should mention JS asset proxy validation"
        );
        assert!(
            message.contains("path") || message.contains("origin_url"),
            "error should mention the invalid asset fields"
        );
    }

    #[test]
    fn validate_trait_reports_deploy_errors() {
        let mut settings = valid_settings();
        settings.auction.enabled = true;
        settings.auction.providers =
            crate::auction::AuctionConfig::legacy_provider_map(&["missing-provider"]);
        settings
            .auction
            .providers
            .values_mut()
            .next()
            .expect("should have provider")
            .protocol = "unsupported".to_string();
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
