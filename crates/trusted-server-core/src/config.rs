//! Trusted Server typed app-config for the `ts` CLI.
//!
//! This module adapts the existing [`Settings`] shape to `EdgeZero`'s typed
//! blob app-config pipeline. The on-disk TOML remains the normal
//! `trusted-server.toml` structure; the CLI serializes the validated settings
//! as a single [`edgezero_core::blob_envelope::BlobEnvelope`] value through
//! `EdgeZero`'s typed config push path.

use std::borrow::Cow;
use std::collections::HashSet;

use edgezero_core::app_config::{SecretField, SecretKind, SecretPathSegment};
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
use crate::settings::{AssetOriginAuth, IntegrationConfig, Settings};

const DEPLOY_VALIDATION_FIELD: &str = "trusted_server";
const MIN_PROXY_SECRET_LENGTH: usize = 32;
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
        self.settings.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrustedServerAppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut settings = Settings::deserialize(deserializer)?;
        settings.normalize_deserialized();
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
        let field = |path: Vec<SecretPathSegment>, optional| SecretField {
            kind: SecretKind::KeyInDefault,
            optional,
            path,
        };
        let object = |name: &'static str| SecretPathSegment::Field(Cow::Borrowed(name));
        let optional_object =
            |name: &'static str| SecretPathSegment::OptionalField(Cow::Borrowed(name));

        vec![
            field(vec![object("publisher"), object("proxy_secret")], false),
            field(vec![object("ec"), object("passphrase")], false),
            field(
                vec![
                    object("ec"),
                    optional_object("partners"),
                    SecretPathSegment::ArrayEach,
                    object("api_token"),
                ],
                false,
            ),
            field(
                vec![
                    object("ec"),
                    optional_object("partners"),
                    SecretPathSegment::ArrayEach,
                    object("ts_pull_token"),
                ],
                true,
            ),
            field(
                vec![
                    object("handlers"),
                    SecretPathSegment::ArrayEach,
                    object("password"),
                ],
                false,
            ),
            field(
                vec![optional_object("tinybird"), object("auction_token_secret")],
                true,
            ),
            field(
                vec![
                    optional_object("integrations"),
                    optional_object("datadome"),
                    object("server_side_key_secret_name"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("integrations"),
                    optional_object("datadome"),
                    optional_object("protection_test_bypass"),
                    object("credential_secret_name"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("proxy"),
                    optional_object("asset_routes"),
                    SecretPathSegment::ArrayEach,
                    optional_object("auth"),
                    object("access_key_id"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("proxy"),
                    optional_object("asset_routes"),
                    SecretPathSegment::ArrayEach,
                    optional_object("auth"),
                    object("secret_access_key"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("proxy"),
                    optional_object("asset_routes"),
                    SecretPathSegment::ArrayEach,
                    optional_object("auth"),
                    object("session_token"),
                ],
                true,
            ),
        ]
    }
}

/// Runs Trusted Server push-time validation for app config.
///
/// Secret fields contain secret-store key names at this stage, so this function
/// deliberately excludes checks that require resolved values. The `EdgeZero` CLI
/// additionally calls [`edgezero_core::app_config::validate_excluding_secrets`]
/// to remove validators attached to those leaves.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when non-secret configuration or a secret key
/// reference is invalid.
pub fn validate_settings_for_deploy(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    validate_secret_key_references(settings)?;
    validate_non_secret_deploy_placeholders(settings)?;

    let mut structural_settings = settings.clone();
    structural_settings.prepare_runtime()?;
    structural_settings.validate_admin_coverage()?;

    let enabled_auction_providers = validate_enabled_integrations(settings, false)?;
    validate_auction_provider_names(settings, &enabled_auction_providers)?;
    PartnerRegistry::validate_config_for_deploy(&settings.ec.partners)?;
    Ok(())
}

/// Runs Trusted Server runtime validation after secret references are resolved.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when resolved secrets or runtime-only
/// configuration checks are invalid.
pub fn validate_settings_for_runtime(
    settings: &Settings,
) -> Result<(), Report<TrustedServerError>> {
    settings.reject_placeholder_secrets()?;
    validate_proxy_secret_strength(settings)?;
    settings.validate_admin_handler_passwords()?;
    let enabled_auction_providers = validate_enabled_integrations(settings, true)?;
    validate_auction_provider_names(settings, &enabled_auction_providers)?;
    PartnerRegistry::from_config(&settings.ec.partners).map(|_| ())?;
    Ok(())
}

fn validate_enabled_integrations(
    settings: &Settings,
    resolved_secrets: bool,
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

fn validate_secret_key_references(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    validate_secret_key_reference(
        "publisher.proxy_secret",
        settings.publisher.proxy_secret.expose(),
    )?;
    validate_secret_key_reference("ec.passphrase", settings.ec.passphrase.expose())?;

    for (index, partner) in settings.ec.partners.iter().enumerate() {
        validate_secret_key_reference(
            &format!("ec.partners[{index}].api_token"),
            partner.api_token.expose(),
        )?;
        if let Some(token) = &partner.ts_pull_token {
            validate_secret_key_reference(
                &format!("ec.partners[{index}].ts_pull_token"),
                token.expose(),
            )?;
        }
    }

    for (index, handler) in settings.handlers.iter().enumerate() {
        validate_secret_key_reference(
            &format!("handlers[{index}].password"),
            handler.password.expose(),
        )?;
    }

    if settings.tinybird.enabled {
        let token = settings
            .tinybird
            .auction_token_secret
            .as_ref()
            .ok_or_else(|| missing_secret_key_reference("tinybird.auction_token_secret"))?;
        validate_secret_key_reference("tinybird.auction_token_secret", token.expose())?;
    }

    if let Some(datadome) = settings.integration_config::<DataDomeConfig>("datadome")? {
        if datadome.enable_protection {
            let key = datadome
                .server_side_key_secret_name
                .as_ref()
                .ok_or_else(|| {
                    missing_secret_key_reference(
                        "integrations.datadome.server_side_key_secret_name",
                    )
                })?;
            validate_secret_key_reference(
                "integrations.datadome.server_side_key_secret_name",
                key.expose(),
            )?;
        }
        if let Some(bypass) = datadome
            .protection_test_bypass
            .as_ref()
            .filter(|bypass| bypass.enabled)
        {
            let credential = bypass.credential_secret_name.as_ref().ok_or_else(|| {
                missing_secret_key_reference(
                    "integrations.datadome.protection_test_bypass.credential_secret_name",
                )
            })?;
            validate_secret_key_reference(
                "integrations.datadome.protection_test_bypass.credential_secret_name",
                credential.expose(),
            )?;
        }
    }

    for (index, route) in settings.proxy.asset_routes.iter().enumerate() {
        let Some(AssetOriginAuth::S3SigV4(auth)) = route.auth.as_ref() else {
            continue;
        };
        validate_secret_key_reference(
            &format!("proxy.asset_routes[{index}].auth.access_key_id"),
            auth.access_key_id.expose(),
        )?;
        validate_secret_key_reference(
            &format!("proxy.asset_routes[{index}].auth.secret_access_key"),
            auth.secret_access_key.expose(),
        )?;
        if let Some(token) = &auth.session_token {
            validate_secret_key_reference(
                &format!("proxy.asset_routes[{index}].auth.session_token"),
                token.expose(),
            )?;
        }
    }

    Ok(())
}

fn validate_secret_key_reference(
    path: &str,
    key_name: &str,
) -> Result<(), Report<TrustedServerError>> {
    if key_name.is_empty() {
        return Err(missing_secret_key_reference(path));
    }
    Ok(())
}

fn missing_secret_key_reference(path: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("secret key reference at `{path}` must not be empty"),
    })
}

fn validate_proxy_secret_strength(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    if settings.publisher.proxy_secret.expose().len() < MIN_PROXY_SECRET_LENGTH {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "publisher.proxy_secret must be at least {MIN_PROXY_SECRET_LENGTH} bytes after secret resolution"
            ),
        }));
    }
    Ok(())
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
    use super::*;
    use crate::redacted::Redacted;
    use crate::settings::{ProxyAssetRoute, S3SigV4AuthConfig};
    use crate::test_support::tests::crate_test_settings_str;
    use edgezero_core::app_config::AppConfigMeta;

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
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    /// Source-controlled operator-facing config template.
    const EXAMPLE_TEMPLATE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    /// Returns the template with required secret-store key references replaced
    /// by resolved test values, so direct [`Settings`] parsing can exercise the
    /// optional blocks this module uncomments.
    fn template_with_resolved_required_secrets() -> String {
        EXAMPLE_TEMPLATE
            .replace(
                "password = \"handler_password\"",
                "password = \"unit-test-resolved-handler-password-0001\"",
            )
            .replace(
                "proxy_secret = \"publisher_proxy_secret\"",
                "proxy_secret = \"unit-test-resolved-publisher-proxy-secret-0001\"",
            )
            .replace(
                "passphrase = \"ec_passphrase\"",
                "passphrase = \"unit-test-resolved-ec-passphrase-secret-0001\"",
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
    fn push_validation_accepts_secret_key_names() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::new("publisher_proxy".to_owned());
        settings.ec.passphrase = Redacted::new("ec_key".to_owned());
        settings.handlers[0].password = Redacted::new("handler_password".to_owned());
        settings.handlers[1].password = Redacted::new("admin_password".to_owned());
        let app_config = TrustedServerAppConfig::new(settings)
            .expect("should validate key names without values");

        let serialized =
            serde_json::to_string(&app_config).expect("should serialize key-name-only app config");
        assert!(serialized.contains("publisher_proxy"));
        assert!(!serialized.contains("unit-test-proxy-secret"));
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
                ("ec.partners[*].api_token".to_owned(), false),
                ("ec.partners[*].ts_pull_token".to_owned(), true),
                ("handlers[*].password".to_owned(), false),
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
    fn omitted_s3_secret_references_materialize_as_defaults() {
        let auth: S3SigV4AuthConfig =
            toml::from_str("region = \"us-east-1\"").expect("should apply S3 secret defaults");

        assert_eq!(auth.access_key_id.expose(), "access_key_id");
        assert_eq!(auth.secret_access_key.expose(), "secret_access_key");

        let serialized = serde_json::to_value(auth).expect("should serialize S3 auth");
        assert_eq!(serialized["access_key_id"], "access_key_id");
        assert_eq!(serialized["secret_access_key"], "secret_access_key");
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
            origin_query: None,
        }));
        settings.proxy.asset_routes.push(route);

        settings.normalize_deserialized();
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
    fn app_config_new_rejects_empty_secret_key_reference() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::new(String::new());

        let err = TrustedServerAppConfig::new(settings)
            .expect_err("should reject an empty secret key reference");

        assert!(
            err.to_string().contains("publisher.proxy_secret"),
            "error should identify the empty secret reference: {err:?}"
        );
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
    fn runtime_validation_rejects_short_proxy_secret() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::new("short".to_owned());

        let err = validate_settings_for_runtime(&settings)
            .expect_err("should reject a short resolved proxy secret");

        assert!(
            err.to_string().contains("at least 32 bytes"),
            "error should identify the required proxy-secret strength: {err:?}"
        );
        assert!(
            !err.to_string().contains("short"),
            "error should not expose the resolved secret"
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
        for (enable_protection, name, expected_message) in [
            (false, "datadome_test_bypass", "requires enable_protection"),
            (true, "", "credential_secret_name"),
        ] {
            let mut settings = valid_settings();
            settings
                .integrations
                .insert_config(
                    "datadome",
                    &serde_json::json!({
                        "enabled": true,
                        "enable_protection": enable_protection,
                        "server_side_key_secret_name": "datadome_server_side_key",
                        "protection_test_bypass": {
                            "enabled": true,
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
