//! Trusted Server typed app-config for the `ts` CLI.
//!
//! This module adapts the existing [`Settings`] shape to `EdgeZero`'s typed
//! blob app-config pipeline. The on-disk TOML remains the normal
//! `trusted-server.toml` structure; the CLI serializes the validated settings
//! as a single [`edgezero_core::blob_envelope::BlobEnvelope`] value through
//! `EdgeZero`'s typed config push path.

use std::borrow::Cow;

use edgezero_core::app_config::{SecretField, SecretKind, SecretPathSegment};
use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

use crate::ec::provider::{HMAC_PROVIDER_KEY, HOST_SIGNALS_PROVIDER_KEY};
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;

use crate::integrations::datadome::DataDomeConfig;
use crate::integrations::{IntegrationBuilder, prebid};
use crate::settings::{AssetOriginAuth, Ec, PROVIDER_IMPLEMENTATION_KEY, Settings};

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
        remove_labeled_provider_secret_errors(&mut errors, &self.settings.ec);
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

/// Removes the passphrase checks on Edge Cookie provider blocks written under
/// a label.
///
/// Push-time validation reads a configuration whose secret fields hold
/// secret-store key names rather than the secrets themselves, so a value check
/// such as the 32-byte passphrase minimum would be judging a key name.
/// `EdgeZero`'s `validate_excluding_secrets` removes those checks for the
/// leaves [`secret_fields`](edgezero_core::app_config::AppConfigMeta::secret_fields)
/// lists, which covers the `[ec.hmac]` block. A block under a label of the
/// operator's choosing has no fixed path that list can hold, so its check is
/// removed here instead. The check itself is unchanged, and runs wherever
/// settings are loaded with their secrets resolved.
fn remove_labeled_provider_secret_errors(errors: &mut ValidationErrors, ec: &Ec) {
    let Some(ValidationErrorsKind::Struct(ec_errors)) = errors.errors_mut().get_mut("ec") else {
        return;
    };
    let labeled = ec
        .provider_blocks
        .hmac_blocks()
        .map(|(name, _)| name)
        .filter(|name| *name != HMAC_PROVIDER_KEY)
        .chain(
            ec.provider_blocks
                .host_signals_blocks()
                .map(|(name, _)| name)
                .filter(|name| *name != HOST_SIGNALS_PROVIDER_KEY),
        );
    for name in labeled {
        let Some(ValidationErrorsKind::Struct(block_errors)) = ec_errors.errors_mut().get_mut(name)
        else {
            continue;
        };
        block_errors.errors_mut().remove("passphrase");
        if block_errors.errors().is_empty() {
            ec_errors.errors_mut().remove(name);
        }
    }
    // An `ec` entry holding nothing would keep the whole result an error, the
    // same reason `EdgeZero` prunes emptied containers after its own removals.
    let ec_is_empty = ec_errors.errors().is_empty();
    if ec_is_empty {
        errors.errors_mut().remove("ec");
    }
}

impl crate::secret_resolution::ConfiguredSecretFields for TrustedServerAppConfig {
    /// The passphrase of every Edge Cookie provider block that configures a
    /// provider built into core under a label.
    ///
    /// [`secret_fields`](edgezero_core::app_config::AppConfigMeta::secret_fields)
    /// lists the passphrases of the `[ec.hmac]` and `[ec.host_signals]`
    /// blocks, the one path each of those providers' blocks has when its name
    /// is its implementation. The same provider under a label of the
    /// operator's choosing holds that secret at `ec.<label>.passphrase`, which
    /// is only knowable from the configuration itself.
    fn configured_secret_fields(data: &serde_json::Value) -> Vec<SecretField> {
        labeled_provider_block_names(data)
            .map(|name| SecretField {
                kind: SecretKind::KeyInDefault,
                optional: true,
                path: vec![
                    SecretPathSegment::Field(Cow::Borrowed("ec")),
                    SecretPathSegment::Field(Cow::Owned(name)),
                    SecretPathSegment::Field(Cow::Borrowed("passphrase")),
                ],
            })
            .collect()
    }
}

/// The names of the `[ec.<name>]` blocks in a serialized configuration that
/// configure a provider built into core under a label.
///
/// A block named after the implementation it configures is the fixed path
/// `secret_fields` already lists, and a block naming any other implementation
/// holds that implementation's settings, which core does not read.
fn labeled_provider_block_names(data: &serde_json::Value) -> impl Iterator<Item = String> + '_ {
    data.get("ec")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, block)| {
            let Some(implementation) = block
                .get(PROVIDER_IMPLEMENTATION_KEY)
                .and_then(serde_json::Value::as_str)
            else {
                return false;
            };
            (implementation == HMAC_PROVIDER_KEY || implementation == HOST_SIGNALS_PROVIDER_KEY)
                && name.as_str() != implementation
        })
        .map(|(name, _)| name.clone())
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
            field(vec![object("ec"), object("passphrase")], true),
            field(
                vec![object("ec"), optional_object("hmac"), object("passphrase")],
                true,
            ),
            field(
                vec![
                    object("ec"),
                    optional_object("host_signals"),
                    object("passphrase"),
                ],
                true,
            ),
            field(
                vec![
                    object("ec"),
                    optional_object("partners"),
                    SecretPathSegment::ArrayEach,
                    object("api_token"),
                ],
                true,
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
                vec![
                    optional_object("trusted_client_ip"),
                    object("shared_secret"),
                ],
                false,
            ),
            field(
                vec![optional_object("tinybird"), object("auction_token_secret")],
                true,
            ),
            field(
                vec![
                    optional_object("integration"),
                    optional_object("datadome"),
                    object("server_side_key_secret_name"),
                ],
                true,
            ),
            field(
                vec![
                    optional_object("integration"),
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

/// Runs Trusted Server push-time validation for app config with the built-in
/// integrations only.
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
    validate_settings_for_deploy_with(settings, &[])
}

/// Runs push-time validation with the built-in integrations followed by the
/// externally supplied builders an adapter registers. Every builder validates,
/// enabled or not, so a typo in a disabled block is still caught.
///
/// As in [`validate_settings_for_deploy`], secret fields still hold key names
/// here, so no check reads a resolved secret value.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when non-secret configuration or a secret key
/// reference is invalid, or when a builder rejects its own configuration.
pub fn validate_settings_for_deploy_with(
    settings: &Settings,
    extra_integrations: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    // The selection is checked first, so a block nothing runs is reported as
    // that rather than as whatever its unread settings fail next.
    settings.integration.validate_selection()?;
    validate_secret_key_references(settings)?;
    validate_non_secret_deploy_placeholders(settings)?;

    let mut structural_settings = settings.clone();
    structural_settings.prepare_runtime()?;
    structural_settings.validate_admin_coverage()?;

    let plan = crate::auction::compile_auction_plan(settings)?;
    validate_integration_blocks(settings, &plan, extra_integrations)?;
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
    settings.validate_admin_handler_passwords()?;
    let plan = crate::auction::compile_auction_plan(settings)?;
    validate_integration_blocks(settings, &plan, &[])?;
    PartnerRegistry::from_config(&settings.ec.partners).map(|_| ())?;
    Ok(())
}

/// Validates every integration block against the compiled auction plan.
///
/// Prebid, APS and the ad server mock are auction plan providers rather than
/// builders, so they are checked here by name, and a Prebid browser bidder is
/// checked against the providers the plan carries. Every builder then
/// validates its own block, the built-in ones first and then
/// `extra_integrations`.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when any integration block fails its
/// validation.
fn validate_integration_blocks(
    settings: &Settings,
    plan: &crate::auction::AuctionPlan,
    extra_integrations: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    validate_prebid(settings, plan)?;
    for builder in crate::integrations::all_builders(extra_integrations) {
        builder.validate(settings)?;
    }
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
    if let Some(passphrase) = &settings.ec.passphrase {
        validate_secret_key_reference("ec.passphrase", passphrase.expose())?;
    }
    for (name, hmac) in settings.ec.provider_blocks.hmac_blocks() {
        validate_secret_key_reference(&format!("ec.{name}.passphrase"), hmac.passphrase.expose())?;
    }
    for (name, host_signals) in settings.ec.provider_blocks.host_signals_blocks() {
        validate_secret_key_reference(
            &format!("ec.{name}.passphrase"),
            host_signals.passphrase.expose(),
        )?;
    }

    for (index, partner) in settings.ec.partners.iter().enumerate() {
        if let Some(token) = &partner.api_token {
            validate_secret_key_reference(
                &format!("ec.partners[{index}].api_token"),
                token.expose(),
            )?;
        }
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

    if let Some(trusted_client_ip) = &settings.trusted_client_ip {
        validate_secret_key_reference(
            "trusted_client_ip.shared_secret",
            trusted_client_ip.shared_secret.expose(),
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
                    missing_secret_key_reference("integration.datadome.server_side_key_secret_name")
                })?;
            validate_secret_key_reference(
                "integration.datadome.server_side_key_secret_name",
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
                    "integration.datadome.protection_test_bypass.credential_secret_name",
                )
            })?;
            validate_secret_key_reference(
                "integration.datadome.protection_test_bypass.credential_secret_name",
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
    if key_name.trim().is_empty() {
        return Err(missing_secret_key_reference(path));
    }
    Ok(())
}

fn missing_secret_key_reference(path: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("secret key reference at `{path}` must not be empty"),
    })
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::integrations::js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID;
    use crate::integrations::{
        IntegrationRegistration, lockr::LockrConfig, permutive::PermutiveConfig,
        sourcepoint::SourcepointConfig,
    };
    use crate::redacted::Redacted;
    use crate::settings::{ProxyAssetRoute, S3SigV4AuthConfig, TrustedClientIpConfig};
    use crate::test_support::tests::{crate_test_settings_str, select_hmac_provider};
    use edgezero_core::app_config::AppConfigMeta;

    /// Message an external builder rejects with, so the test can prove the
    /// rejection reached the caller intact.
    const EXTERNAL_REJECTION_MESSAGE: &str = "seam probe refuses to deploy";

    /// Stands in for a vendor integration builder that never registers.
    fn build_nothing(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(None)
    }

    fn reject_deploy(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Configuration {
            message: EXTERNAL_REJECTION_MESSAGE.to_string(),
        }))
    }

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

    fn insert_aps_provider(settings: &mut Settings, account_id: &str) {
        let table = serde_json::Map::from_iter([
            ("implementation".to_string(), serde_json::json!("aps")),
            (
                "endpoint".to_string(),
                serde_json::json!("https://aps.example.com/e/pb/bid"),
            ),
            ("routing".to_string(), serde_json::json!("all_eligible")),
            ("account_id".to_string(), serde_json::json!(account_id)),
        ]);
        settings.demand = crate::provider_table::ProviderList::new(
            vec!["aps_main".to_string()],
            std::collections::BTreeMap::from([("aps_main".to_string(), table)]),
        );
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

    /// Every documented block should be push-ready, so uncommenting it,
    /// naming the integration and setting the shown values must parse and
    /// pass field validation. Blocks that ship a deliberately-invalid
    /// non-secret placeholder (GTM `container_id` and `request_signing`
    /// store ids) are excluded.
    #[test]
    fn documented_integration_blocks_validate_when_uncommented_and_named() {
        let base = template_with_resolved_required_secrets();

        for (header, id) in [
            ("[integration.permutive]", "permutive"),
            ("[integration.lockr]", "lockr"),
            ("[integration.sourcepoint]", "sourcepoint"),
        ] {
            let toml = uncomment_block(&base, header)
                .replace("provider = []", &format!("provider = [\"{id}\"]"));
            let settings = Settings::from_toml(&toml)
                .unwrap_or_else(|err| panic!("uncommented {header} should parse: {err:?}"));

            match id {
                "permutive" => assert!(
                    settings
                        .integration_config::<PermutiveConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to a valid config"
                ),
                "lockr" => assert!(
                    settings
                        .integration_config::<LockrConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to a valid config"
                ),
                "sourcepoint" => assert!(
                    settings
                        .integration_config::<SourcepointConfig>(id)
                        .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                        .is_some(),
                    "{header} should resolve to a valid config"
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
        select_hmac_provider(&mut settings.ec, HMAC_PROVIDER_KEY, "ec_key");
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
    fn push_validation_accepts_a_host_signals_passphrase_key_name() {
        // The block is named `host_signals` in the configuration and in the
        // registered secret path, so push validation has to skip the passphrase
        // check under that name.
        let mut settings = valid_settings();
        settings.ec.provider = Some(crate::ec::provider::EcProviderSelection::from(
            HOST_SIGNALS_PROVIDER_KEY,
        ));
        settings.ec.provider_blocks.clear();
        settings.ec.provider_blocks.insert(
            HOST_SIGNALS_PROVIDER_KEY.to_owned(),
            crate::settings::EcProviderBlock::from(crate::settings::HostSignalsProviderConfig {
                passphrase: Redacted::new("host_signals_key".to_owned()),
            }),
        );

        let app_config = TrustedServerAppConfig::new(settings)
            .expect("should validate the host_signals passphrase as a key name");

        let serialized =
            serde_json::to_string(&app_config).expect("should serialize key-name-only app config");
        assert!(serialized.contains("host_signals_key"));
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
                ("ec.passphrase".to_owned(), true),
                ("ec.hmac.passphrase".to_owned(), true),
                ("ec.host_signals.passphrase".to_owned(), true),
                ("ec.partners[*].api_token".to_owned(), true),
                ("ec.partners[*].ts_pull_token".to_owned(), true),
                ("handlers[*].password".to_owned(), false),
                ("trusted_client_ip.shared_secret".to_owned(), false),
                ("tinybird.auction_token_secret".to_owned(), true),
                (
                    "integration.datadome.server_side_key_secret_name".to_owned(),
                    true,
                ),
                (
                    "integration.datadome.protection_test_bypass.credential_secret_name".to_owned(),
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
            .integration
            .insert_config(
                "datadome",
                &serde_json::json!({
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
            .integration
            .insert_config(
                "datadome",
                &serde_json::json!({
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
            rendered.contains("[demand] provider"),
            "should name where the setting moved to: {rendered}"
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
    fn app_config_new_accepts_trusted_client_ip_secret_key_reference() {
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
    fn app_config_new_rejects_whitespace_secret_key_reference() {
        let mut settings = valid_settings();
        settings.publisher.proxy_secret = Redacted::new(" \t ".to_owned());

        let err = TrustedServerAppConfig::new(settings)
            .expect_err("should reject a whitespace-only secret key reference");

        assert!(
            err.to_string().contains("publisher.proxy_secret"),
            "error should identify the whitespace-only secret reference: {err:?}"
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
    fn runtime_validation_rejects_placeholders() {
        let settings = Settings::from_toml(
            r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "change-me-proxy-secret"

[geo]
assume_single_jurisdiction = true

[ec]
provider = "hmac"

[ec.hmac]
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

    /// A block written for an integration the provider list does not name is
    /// refused by deploy validation, so `ts config validate` reports it before
    /// the configuration reaches a deployment.
    #[test]
    fn deploy_validation_rejects_a_block_for_an_integration_that_is_not_named() {
        let mut settings = valid_settings();
        settings.integration.insert(
            "adserver_mock".to_owned(),
            serde_json::json!({ "endpoint": "https://mediator.example.com/mediate" }),
        );

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should reject a block nothing on the list names");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[integration.adserver_mock]") && rendered.contains("provider"),
            "should name the block and where to name the integration: {rendered}"
        );
    }

    /// An id no builder in this deployment supplies is refused where the
    /// registry is built, not here, because a vendor crate the CLI never links
    /// may supply it.
    #[test]
    fn deploy_validation_accepts_an_id_it_does_not_know() {
        let mut settings = valid_settings();
        settings.integration.select("a_vendors_own_integration");

        validate_settings_for_deploy(&settings)
            .expect("deploy validation should leave unknown ids to the registry");
    }

    #[test]
    fn deploy_validation_rejects_an_aps_demand_setting_it_does_not_know() {
        let mut settings = valid_settings();
        let table = serde_json::Map::from_iter([
            ("implementation".to_string(), serde_json::json!("aps")),
            (
                "endpoint".to_string(),
                serde_json::json!("https://aps.example.com/e/pb/bid"),
            ),
            (
                "account_id".to_string(),
                serde_json::json!("example-account"),
            ),
            ("enabled".to_string(), serde_json::json!(false)),
        ]);
        settings.demand = crate::provider_table::ProviderList::new(
            vec!["aps_main".to_string()],
            std::collections::BTreeMap::from([("aps_main".to_string(), table)]),
        );

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should reject a setting the APS implementation does not know");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("aps_main"),
            "should identify the demand source: {rendered}"
        );
        assert!(
            rendered.contains("enabled"),
            "should identify the setting it does not know: {rendered}"
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

    /// Counts calls to [`record_validate_call`]. A builder holds plain fn
    /// pointers and cannot capture, so the recording has to go through a
    /// static.
    static RECORDED_VALIDATE_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn record_validate_call(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        RECORDED_VALIDATE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }

    /// Every builder handed to deploy validation has its `validate` run, and
    /// reporting disabled does not excuse a builder from validating.
    #[test]
    fn deploy_validation_runs_every_builder_it_is_given() {
        RECORDED_VALIDATE_CALLS.store(0, Ordering::SeqCst);
        let extra_integrations = [IntegrationBuilder::new(
            "seam-probe-integration",
            "seam-probe-crate",
            build_nothing,
            record_validate_call,
        )];
        validate_settings_for_deploy_with(&valid_settings(), &extra_integrations)
            .expect("should accept settings whose external builder reports disabled");

        assert_eq!(
            RECORDED_VALIDATE_CALLS.load(Ordering::SeqCst),
            1,
            "the external builder should validate even though it reports disabled"
        );
    }

    /// Deploy validation reaches each built-in builder's own config type, one
    /// id at a time, by planting a block no config type can deserialize. A
    /// string where the block belongs fails for all of them, whatever settings
    /// each one takes.
    ///
    /// This catches deploy validation ceasing to validate the built-ins. It
    /// cannot catch a builder deleted from `BUILT_IN_BUILDERS`, because the
    /// loop below reads the same constant the validation walks, and no independent
    /// list of the built-ins exists in the crate.
    #[test]
    fn deploy_validation_reaches_every_built_in_builder() {
        for id in crate::integrations::builders()
            .iter()
            .filter(|builder| builder.supplies_integration())
            .map(IntegrationBuilder::id)
        {
            let mut settings = valid_settings();
            settings.integration.select(id);
            settings
                .integration
                .insert(id.to_owned(), serde_json::json!("not-a-block"));

            assert!(
                validate_settings_for_deploy(&settings).is_err(),
                "deploy validation should reach the `{id}` builder and reject its planted config"
            );
        }
    }

    /// A selected Prebid block that names bundle modules has to name where the
    /// bundle is served from as well, and deploy validation says so. Selection
    /// is what makes Prebid run here, so it stands in for the `enabled` flag
    /// this branch took off the config.
    #[test]
    fn deploy_validation_requires_external_bundle_url_for_selected_prebid() {
        let mut settings = valid_settings();
        settings.integration.select("prebid");
        settings
            .integration
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "bundle": {
                        "modules": { "bidder": ["exampleBidderBidAdapter"] }
                    }
                }),
            )
            .expect("should insert the Prebid config");

        let error = validate_settings_for_deploy(&settings)
            .expect_err("should require enabled Prebid external bundle URL");
        assert!(error.to_string().contains("external_bundle_url"));
    }

    /// Every built-in page integration refuses a setting it does not know, so
    /// a misspelt key in its block fails deploy validation naming the
    /// integration and the key, rather than being ignored.
    #[test]
    fn every_integration_rejects_a_setting_it_does_not_know() {
        for id in crate::integrations::builders()
            .iter()
            .filter(|builder| builder.supplies_integration())
            .map(IntegrationBuilder::id)
        {
            let mut settings = valid_settings();
            settings
                .integration
                .insert_config(id, &serde_json::json!({ "no_such_setting": true }))
                .expect("should insert the planted block");

            let error = match validate_settings_for_deploy(&settings) {
                Ok(()) => panic!("`{id}` should refuse a setting it does not know"),
                Err(error) => format!("{error:?}"),
            };
            assert!(
                error.contains(id) && error.contains("no_such_setting"),
                "`{id}` should name itself and the unknown setting: {error}"
            );
        }
    }

    /// Validation reaches the block of the one integration the auction plan
    /// still carries, being Prebid, which has no builder and so is not covered
    /// by `deploy_validation_reaches_every_built_in_builder`. It is planted
    /// with a block its config type cannot deserialize, and the rejection must
    /// name the integration, so a failure elsewhere in validation cannot pass
    /// for it.
    #[test]
    fn validation_reaches_the_plan_backed_prebid_block() {
        {
            let id = "prebid";
            let mut settings = valid_settings();
            settings.integration.select(id);
            settings
                .integration
                .insert(id.to_owned(), serde_json::json!("not-a-block"));
            let expected = format!("Integration '{id}'");

            let Err(deploy_error) = validate_settings_for_deploy(&settings) else {
                panic!("deploy validation should reject the planted `{id}` block");
            };
            assert!(
                format!("{deploy_error:?}").contains(&expected),
                "deploy validation should reject the `{id}` block by name: {deploy_error:?}"
            );

            let Err(runtime_error) = validate_settings_for_runtime(&settings) else {
                panic!("runtime validation should reject the planted `{id}` block");
            };
            assert!(
                format!("{runtime_error:?}").contains(&expected),
                "runtime validation should reject the `{id}` block by name: {runtime_error:?}"
            );
        }
    }

    #[test]
    fn deploy_validation_surfaces_an_external_integration_builders_rejection() {
        let extra = [IntegrationBuilder::new(
            "seam-probe",
            "seam-probe-crate",
            build_nothing,
            reject_deploy,
        )];

        let err = validate_settings_for_deploy_with(&valid_settings(), &extra)
            .expect_err("should surface the external integration builder's rejection");

        assert!(
            err.to_string().contains(EXTERNAL_REJECTION_MESSAGE),
            "should keep the external builder's message intact: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_invalid_osano_config() {
        let mut settings = valid_settings();
        settings
            .integration
            .insert_config("osano", &serde_json::json!({"typo": true }))
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
                .integration
                .insert_config(
                    "datadome",
                    &serde_json::json!({
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
    fn validate_rejects_invalid_js_asset_proxy_assets() {
        let mut settings = valid_settings();
        settings
            .integration
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &serde_json::json!({
                    "assets": [{
                        "path": "bad path",
                        "origin_url": "not-a-url",
                        "proxy": "disabled"
                    }]
                }),
            )
            .expect("should insert the asset inventory");

        let err = validate_settings_for_deploy(&settings)
            .expect_err("should reject an invalid asset inventory");
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
        settings.demand = crate::provider_table::ProviderList::new(
            vec!["missing_provider".to_string()],
            std::collections::BTreeMap::from([(
                "missing_provider".to_string(),
                serde_json::Map::from_iter([(
                    "implementation".to_string(),
                    serde_json::json!("no_such_implementation"),
                )]),
            )]),
        );
        let app_config = TrustedServerAppConfig { settings };

        let err = app_config
            .validate()
            .expect_err("should reject invalid auction provider");

        assert!(
            err.to_string().contains("no_such_implementation"),
            "validation error should name the implementation this build does not have"
        );
    }
}
