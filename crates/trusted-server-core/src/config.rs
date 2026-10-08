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

use crate::ec::module::{HMAC_MODULE_KEY, HOST_SIGNALS_MODULE_KEY};
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;

use crate::integrations::IntegrationBuilder;
use crate::settings::{AssetOriginAuth, Ec, MODULE_IMPLEMENTATION_KEY, Settings};

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

/// The builders [`TrustedServerAppConfig`] validates against besides core's
/// own, set once by the tool that validates.
static DEPLOY_INTEGRATIONS: std::sync::OnceLock<Vec<IntegrationBuilder>> =
    std::sync::OnceLock::new();

/// Registers the integration builders [`TrustedServerAppConfig`] validates
/// against besides core's own.
///
/// `EdgeZero` validates an app config through the [`Validate`] trait, which
/// takes no arguments, so a tool that validates a deployment's settings
/// registers that deployment's builders here before it validates anything.
/// The first call wins and a later one changes nothing.
pub fn register_deploy_integrations(builders: Vec<IntegrationBuilder>) {
    let _ = DEPLOY_INTEGRATIONS.set(builders);
}

/// The builders a tool registered for deploy validation, or none.
fn deploy_integrations() -> &'static [IntegrationBuilder] {
    DEPLOY_INTEGRATIONS.get().map_or(&[], Vec::as_slice)
}

impl Validate for TrustedServerAppConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = self.settings.validate().err().unwrap_or_default();
        remove_labeled_module_secret_errors(&mut errors, &self.settings.ec);
        if let Err(report) =
            validate_settings_for_deploy_with(&self.settings, deploy_integrations())
        {
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

/// Removes the passphrase checks on Edge Cookie module blocks written under
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
fn remove_labeled_module_secret_errors(errors: &mut ValidationErrors, ec: &Ec) {
    let Some(ValidationErrorsKind::Struct(ec_errors)) = errors.errors_mut().get_mut("ec") else {
        return;
    };
    let labeled = ec
        .module_blocks
        .hmac_blocks()
        .map(|(name, _)| name)
        .filter(|name| *name != HMAC_MODULE_KEY)
        .chain(
            ec.module_blocks
                .host_signals_blocks()
                .map(|(name, _)| name)
                .filter(|name| *name != HOST_SIGNALS_MODULE_KEY),
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
    /// The passphrase of every Edge Cookie module block that configures a
    /// module built into core under a label.
    ///
    /// [`secret_fields`](edgezero_core::app_config::AppConfigMeta::secret_fields)
    /// lists the passphrases of the `[ec.hmac]` and `[ec.host_signals]`
    /// blocks, the one path each of those modules' blocks has when its name
    /// is its implementation. The same module under a label of the
    /// operator's choosing holds that secret at `ec.<label>.passphrase`, which
    /// is only knowable from the configuration itself.
    fn configured_secret_fields(data: &serde_json::Value) -> Vec<SecretField> {
        labeled_module_block_names(data)
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
/// configure a module built into core under a label.
///
/// A block named after either built-in implementation is a fixed path
/// `secret_fields` already lists, whichever of the two it configures, so it is
/// left out rather than listed twice. A block naming any other implementation
/// holds that implementation's settings, which core does not read.
fn labeled_module_block_names(data: &serde_json::Value) -> impl Iterator<Item = String> + '_ {
    data.get("ec")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, block)| {
            let Some(implementation) = block
                .get(MODULE_IMPLEMENTATION_KEY)
                .and_then(serde_json::Value::as_str)
            else {
                return false;
            };
            let built_in = |key: &str| key == HMAC_MODULE_KEY || key == HOST_SIGNALS_MODULE_KEY;
            built_in(implementation) && !built_in(name.as_str())
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

        let mut fields = vec![
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
        ];
        // The settings a module declares in its own table, for core's
        // modules and the ones a tool registered for deploy validation.
        let builders = crate::integrations::all_builders(deploy_integrations()).collect::<Vec<_>>();
        fields.extend(crate::module_secrets::secret_fields(&builders));
        fields
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
    settings.validate_module_sections()?;
    validate_secret_key_references(settings, extra_integrations)?;
    validate_non_secret_deploy_placeholders(settings)?;

    let mut structural_settings = settings.clone();
    structural_settings.prepare_runtime()?;
    structural_settings.validate_admin_coverage()?;

    // The plan is compiled with the builders the blocks are validated
    // against, so a `[demand]` or `[ad-server]` name one of them supplies
    // compiles here.
    let plan = crate::auction::compile_auction_plan_with(settings, extra_integrations)?;
    validate_integration_blocks(settings, &plan, extra_integrations)?;
    PartnerRegistry::validate_config_for_deploy(&settings.ec.partners)?;
    settings.ec.validate_resolve_allowed_origins()?;
    Ok(())
}

/// Runs Trusted Server runtime validation after secret references are
/// resolved, against the built-in integrations only.
///
/// A deployment whose `[demand]` or `[ad-server]` names an implementation a
/// builder of its own supplies calls [`validate_settings_for_runtime_with`]
/// with that builder, because this function cannot see it and refuses the
/// name as one no builder registers.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when resolved secrets or runtime-only
/// configuration checks are invalid.
pub fn validate_settings_for_runtime(
    settings: &Settings,
) -> Result<(), Report<TrustedServerError>> {
    validate_settings_for_runtime_with(settings, &[])
}

/// Runs runtime validation with the built-in integrations followed by the
/// builders a deployment supplies.
///
/// This runs while the settings load, before any application state is built,
/// so a deployment that composes builders supplies them here as well as to
/// the state build. Each builder also validates its own table.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when resolved secrets or runtime-only
/// configuration checks are invalid, or when a builder rejects its own
/// configuration.
pub fn validate_settings_for_runtime_with(
    settings: &Settings,
    extra_integrations: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    settings.reject_placeholder_secrets()?;
    settings.validate_admin_handler_passwords()?;
    let plan = crate::auction::compile_auction_plan_with(settings, extra_integrations)?;
    validate_integration_blocks(settings, &plan, extra_integrations)?;
    PartnerRegistry::from_config(&settings.ec.partners).map(|_| ())?;
    Ok(())
}

/// Validates every integration block, the built-in ones first and then
/// `extra_integrations`.
///
/// Each builder validates its own block, and one with a rule that depends on
/// what the auction plan selects then checks its block against the plan.
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
    for builder in crate::integrations::all_builders(extra_integrations) {
        builder.validate(settings)?;
        if let Some(validate) = builder.plan_validator() {
            validate(settings, plan)?;
        }
    }
    Ok(())
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

fn validate_secret_key_references(
    settings: &Settings,
    extra_integrations: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    validate_secret_key_reference(
        "publisher.proxy_secret",
        settings.publisher.proxy_secret.expose(),
    )?;
    if let Some(passphrase) = &settings.ec.passphrase {
        validate_secret_key_reference("ec.passphrase", passphrase.expose())?;
    }
    for (name, hmac) in settings.ec.module_blocks.hmac_blocks() {
        validate_secret_key_reference(&format!("ec.{name}.passphrase"), hmac.passphrase.expose())?;
    }
    for (name, host_signals) in settings.ec.module_blocks.host_signals_blocks() {
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

    // Each setting a selected module declares as naming a secret, where the
    // module's table puts it to use.
    for builder in crate::integrations::all_builders(extra_integrations) {
        let secrets = builder.secret_settings();
        let Some((section, written)) = builder
            .module_name()
            .filter(|_| !secrets.is_empty())
            .and_then(|name| settings.module_selection(name))
        else {
            continue;
        };
        let table = settings.section_table(section, written);
        for secret in secrets.iter().filter(|secret| (secret.in_use)(&table)) {
            let path = format!("{section}.{written}.{}", secret.path.join("."));
            let reference = crate::module_secrets::value_at(&table, secret.path)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| missing_secret_key_reference(&path))?;
            validate_secret_key_reference(&path, reference)?;
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
    use crate::integrations::IntegrationRegistration;
    use crate::integrations::js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID;
    use crate::redacted::Redacted;
    use crate::settings::{ProxyAssetRoute, S3SigV4AuthConfig, TrustedClientIpConfig};
    use crate::test_support::template::{template_with_resolved_required_secrets, uncomment_block};
    use crate::test_support::tests::{
        crate_test_settings_str, crate_test_settings_str_with_ec_section, select_hmac_module,
    };
    use edgezero_core::app_config::AppConfigMeta;
    use edgezero_core::blob_envelope::BlobEnvelope;

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
        source["demand"] = serde_json::json!({
            "modules": ["secondary", "primary"],
            "secondary": {
                "implementation": "auction-protocol.openrtb", "endpoint": "https://secondary.example.com/auction",
                "routing": "all_eligible",
                "notifications": {"suppress_seats": ["seat-b", "seat-a"]}
            },
            "primary": {
                "implementation": "auction-protocol.openrtb", "endpoint": "https://primary.example.com/auction",
                "routing": "all_eligible",
                "notifications": {"suppress_seats": ["seat-b", "seat-a"]}
            }
        });
        source["auction"]["bidders"] = serde_json::json!({
            "bidder-b": {"module": "secondary"}, "bidder-a": {"module": "primary"}
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
        select_hmac_module(&mut settings.ec, HMAC_MODULE_KEY, "ec_key");
        settings.handlers[0].password = Redacted::new("handler_password".to_owned());
        settings.handlers[1].password = Redacted::new("admin_password".to_owned());
        let app_config = TrustedServerAppConfig::new(settings)
            .expect("should validate key names without values");

        let serialized =
            serde_json::to_string(&app_config).expect("should serialize key-name-only app config");
        assert!(serialized.contains("publisher_proxy"));
        assert!(!serialized.contains("unit-test-proxy-secret"));
    }

    /// The crate test configuration selecting the built-in HMAC module
    /// under the label `primary`, with `passphrase` as the block's passphrase.
    fn labeled_hmac_settings_str(passphrase: &str) -> String {
        crate_test_settings_str_with_ec_section(&format!(
            "[ec]\nmodule = \"primary\"\n\n[ec.primary]\nimplementation = \"hmac\"\npassphrase = \"{passphrase}\"\n"
        ))
    }

    #[test]
    fn push_validation_accepts_a_key_name_in_a_labeled_hmac_block() {
        // At push time a secret field holds the name of a secret-store key, so
        // a value check such as the 32-byte passphrase minimum would be judging
        // the key name. `ec_key` is far shorter than any passphrase.
        let toml = labeled_hmac_settings_str("ec_key");
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize a labeled module block");
        let mut settings = app_config.into_settings();
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];

        TrustedServerAppConfig::new(settings)
            .expect("should validate the key name without judging it as a passphrase");

        // The value check still exists. It runs where settings are loaded with
        // their secrets resolved, which here reads `ec_key` as the passphrase.
        let err = Settings::from_toml(&toml)
            .expect_err("a short passphrase in a labeled block should be rejected on load");
        assert!(
            format!("{err:?}").contains("ec.primary.passphrase: short_passphrase"),
            "should report the short passphrase at the labeled block's path: {err:?}"
        );
    }

    #[test]
    fn configured_secret_fields_names_only_labeled_hmac_blocks() {
        use crate::secret_resolution::ConfiguredSecretFields as _;

        let data = serde_json::json!({
            "ec": {
                "module": "primary",
                "ec_store": "ec_identity_store",
                "hmac": { "passphrase": "ec_key" },
                "primary": { "implementation": "hmac", "passphrase": "labeled_ec_key" },
                "acme": { "implementation": "acme", "endpoint": "https://ec.acme.example.com" },
            }
        });

        let paths = TrustedServerAppConfig::configured_secret_fields(&data)
            .iter()
            .map(SecretField::dotted_path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec!["ec.primary.passphrase".to_owned()],
            "only a block naming the hmac implementation under a label needs a path the \
             fixed list cannot hold"
        );
    }

    #[test]
    fn push_validation_keeps_the_sections_other_errors_for_a_labeled_block() {
        // A block under a label has no fixed secret path, so push validation
        // strips the error that judged its key name as a passphrase, and only
        // that one: another error in the Edge Cookie section is still reported.
        let toml = labeled_hmac_settings_str("ec_key");
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize a labeled module block");
        let mut settings = app_config.into_settings();
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        settings.ec.partners = vec![
            serde_json::from_value(serde_json::json!({
                "name": "Example partner",
                "source_domain": "https://partner.example",
            }))
            .expect("should deserialize a partner"),
        ];

        // Read from the section's own errors, because deploy validation
        // reports a bad partner as well.
        let errors = TrustedServerAppConfig { settings }
            .validate()
            .expect_err("an error beside the passphrase should still be reported");
        let Some(ValidationErrorsKind::Struct(ec)) = errors.errors().get("ec") else {
            panic!("the Edge Cookie section should keep its error: {errors}");
        };
        assert!(
            ec.errors().contains_key("partners"),
            "the partner error should be reported: {errors}"
        );
        assert!(
            !ec.errors().contains_key("primary"),
            "the key name should not be judged as a passphrase: {errors}"
        );
    }

    #[test]
    fn push_validation_rejects_an_empty_key_name_in_a_labeled_hmac_block() {
        let toml = labeled_hmac_settings_str("");
        let app_config: TrustedServerAppConfig =
            toml::from_str(&toml).expect("should deserialize a labeled module block");
        let mut settings = app_config.into_settings();
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];

        let err = TrustedServerAppConfig::new(settings)
            .expect_err("should reject an empty secret key reference in a labeled block");
        assert!(
            err.to_string().contains("ec.primary.passphrase"),
            "error should identify the labeled block's empty reference: {err:?}"
        );
    }

    #[test]
    fn push_validation_accepts_a_host_signals_passphrase_key_name() {
        // The block is named `host_signals` in the configuration and in the
        // registered secret path, so push validation has to skip the passphrase
        // check under that name.
        let mut settings = valid_settings();
        settings.ec.module = Some(crate::ec::module::EcModuleSelection::from(
            HOST_SIGNALS_MODULE_KEY,
        ));
        settings.ec.module_blocks.clear();
        settings.ec.module_blocks.insert(
            HOST_SIGNALS_MODULE_KEY.to_owned(),
            crate::settings::EcModuleBlock::from(crate::settings::HostSignalsModuleConfig {
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

        for legacy_store in ["legacy-tinybird-store", "legacy-s3-store"] {
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
            .insert_module_config(
                "testing",
                "testing.example",
                &serde_json::json!({
                    "key_name": "resolved-module-secret",
                }),
            )
            .expect("should insert a module's table holding a resolved secret");

        let debug = format!("{settings:?}");

        assert!(!debug.contains("resolved-tinybird-secret"));
        assert!(!debug.contains("resolved-module-secret"));
        assert!(debug.contains("example"));
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
            "{}\n",
            crate_test_settings_str()
                .replace("[auction]\n", "[auction]\nproviders = [\"example\"]\n")
        );

        let error = toml::from_str::<TrustedServerAppConfig>(&toml)
            .expect_err("should reject the removed auction provider list schema");
        let rendered = error.to_string();
        assert!(
            rendered.contains("auction.providers"),
            "should identify the removed field: {rendered}"
        );
        assert!(
            rendered.contains("[demand] modules"),
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
    fn app_config_new_rejects_a_resolve_allowed_origin_that_is_not_bare() {
        let mut settings = valid_settings();
        settings.ec.resolve_allowed_origins = vec!["https://www.example.com/".to_owned()];

        let err = TrustedServerAppConfig::new(settings)
            .expect_err("should refuse at push an entry the settings refuse at load");

        assert!(
            err.to_string().contains("`https://www.example.com/`"),
            "error should name the entry: {err:?}"
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
module = "hmac"

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

    /// A table written for a module its section does not select is refused
    /// when the settings load, so `ts config validate` reports it before the
    /// configuration reaches a deployment.
    #[test]
    fn a_table_for_a_module_its_section_does_not_select_is_refused() {
        let mut value = serde_json::to_value(valid_settings()).expect("should serialize settings");
        value
            .as_object_mut()
            .expect("settings should serialize as an object")
            .insert(
                "cmp".to_owned(),
                serde_json::json!({ "modules": ["osano"], "didomi": { "api_key": "k" } }),
            );

        let error =
            Settings::from_json_value(value).expect_err("should reject a table nothing selects");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[cmp.didomi] is configured") && rendered.contains("does not select"),
            "should name the table and the selection it is missing from: {rendered}"
        );
    }

    /// An id no builder in this deployment supplies is refused where the
    /// registry is built, not here, because a vendor crate the CLI never links
    /// may supply it.
    #[test]
    fn deploy_validation_accepts_an_id_it_does_not_know() {
        let mut settings = valid_settings();
        settings.select_module("testing", "testing.a_vendors_own_integration");

        validate_settings_for_deploy(&settings)
            .expect("deploy validation should leave unknown ids to the registry");
    }

    /// Counts calls to [`record_validate_call`]. A builder holds plain fn
    /// pointers and cannot capture, so the recording has to go through a
    /// static.
    static RECORDED_VALIDATE_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn record_validate_call(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        RECORDED_VALIDATE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }

    /// The ad server implementation a crate outside core supplies.
    static EXTERNAL_ADSERVER: crate::auction::demand::AdServerImplementation =
        crate::auction::demand::AdServerImplementation {
            id: "ad-server.example",
            build: build_external_adserver,
        };

    fn build_external_adserver(
        name: &str,
        _settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<
        std::sync::Arc<dyn crate::auction::provider::AuctionProvider>,
        Report<TrustedServerError>,
    > {
        Ok(std::sync::Arc::new(
            crate::auction::test_support::adserver_fixture::FixtureAdServer::new(name, 500),
        ))
    }

    /// The builder of the crate that supplies [`EXTERNAL_ADSERVER`].
    fn external_adserver_builders() -> [IntegrationBuilder; 1] {
        [
            IntegrationBuilder::implementations("example-adserver", "example-crate")
                .with_adserver(&EXTERNAL_ADSERVER),
        ]
    }

    /// Settings whose `[ad-server] module` names the external ad server.
    fn settings_naming_the_external_adserver() -> Settings {
        let mut settings = valid_settings();
        settings.adserver = crate::provider_table::ProviderChoice::new(
            Some("example".to_string()),
            std::collections::BTreeMap::from([("example".to_string(), serde_json::Map::new())]),
        );
        settings
    }

    /// Deploy validation compiles the plan with the builders it validates the
    /// blocks against, so it and the running service agree on which ad server
    /// names are valid. Both halves are asserted, so the test fails if the
    /// built-in path stops refusing the name or the external path stops
    /// accepting it.
    #[test]
    fn deploy_validation_accepts_an_external_ad_server_only_when_given_its_builder() {
        let settings = settings_naming_the_external_adserver();

        let error = validate_settings_for_deploy(&settings)
            .expect_err("built-ins alone should not know this ad server");
        assert!(
            error.to_string().contains("example"),
            "should name the ad server: {error:?}"
        );

        validate_settings_for_deploy_with(&settings, &external_adserver_builders())
            .expect("the external builder's ad server should pass deploy validation");
    }

    /// Runtime validation runs while the settings load, before any application
    /// state exists, so a deployment that composes a builder supplies it here
    /// too. Both halves are asserted, so the test fails if the built-in path
    /// stops refusing the name or the external path stops accepting it.
    #[test]
    fn runtime_validation_accepts_an_external_ad_server_only_when_given_its_builder() {
        let settings = settings_naming_the_external_adserver();

        let error = validate_settings_for_runtime(&settings)
            .expect_err("built-ins alone should not know this ad server");
        assert!(
            error.to_string().contains("example"),
            "should name the ad server: {error:?}"
        );

        validate_settings_for_runtime_with(&settings, &external_adserver_builders())
            .expect("the external builder's ad server should pass runtime validation");
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
        )
        .with_module_name("testing.seam-probe-integration")];
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
        for builder in crate::integrations::builders()
            .iter()
            .filter(|builder| builder.supplies_integration())
        {
            let name = builder
                .module_name()
                .expect("every built-in page integration names its module");
            let section = builder
                .section()
                .expect("every built-in page integration has a section");
            let mut settings = valid_settings();
            settings
                .insert_module_config(
                    section,
                    name,
                    &serde_json::json!({ "no_such_setting": true }),
                )
                .expect("should insert the planted table");

            assert!(
                validate_settings_for_deploy(&settings).is_err(),
                "deploy validation should reach the `{name}` builder and reject its planted config"
            );
        }
    }

    /// Every built-in page integration refuses a setting it does not know, so
    /// a misspelt key in its block fails deploy validation naming the
    /// integration and the key, rather than being ignored.
    #[test]
    fn every_integration_rejects_a_setting_it_does_not_know() {
        for builder in crate::integrations::builders()
            .iter()
            .filter(|builder| builder.supplies_integration())
        {
            let name = builder
                .module_name()
                .expect("every built-in page integration names its module");
            let section = builder
                .section()
                .expect("every built-in page integration has a section");
            let mut settings = valid_settings();
            settings
                .insert_module_config(
                    section,
                    name,
                    &serde_json::json!({ "no_such_setting": true }),
                )
                .expect("should insert the planted table");

            let error = match validate_settings_for_deploy(&settings) {
                Ok(()) => panic!("`{name}` should refuse a setting it does not know"),
                Err(error) => format!("{error:?}"),
            };
            let written = crate::module_name::short_form(section, name);
            assert!(
                error.contains(&format!("[{section}.{written}]"))
                    && error.contains("no_such_setting"),
                "`{name}` should name its table and the unknown setting: {error}"
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
        )
        .with_module_name("testing.seam-probe")];

        let err = validate_settings_for_deploy_with(&valid_settings(), &extra)
            .expect_err("should surface the external integration builder's rejection");

        assert!(
            err.to_string().contains(EXTERNAL_REJECTION_MESSAGE),
            "should keep the external builder's message intact: {err:?}"
        );
    }

    /// A builder's own rules run when the settings load with that builder, so
    /// a deployment that composes it is held to them at startup.
    #[test]
    fn runtime_validation_surfaces_an_external_integration_builders_rejection() {
        let extra = [IntegrationBuilder::new(
            "seam-probe",
            "seam-probe-crate",
            build_nothing,
            reject_deploy,
        )
        .with_module_name("testing.seam-probe")];

        validate_settings_for_runtime(&valid_settings())
            .expect("the settings should pass without the external builder");

        let err = validate_settings_for_runtime_with(&valid_settings(), &extra)
            .expect_err("should surface the external integration builder's rejection");

        assert!(
            err.to_string().contains(EXTERNAL_REJECTION_MESSAGE),
            "should keep the external builder's message intact: {err:?}"
        );
    }

    fn module_key_is_in_use(table: &serde_json::Map<String, serde_json::Value>) -> bool {
        table.get("lock").and_then(serde_json::Value::as_bool) == Some(true)
    }

    const MODULE_SECRETS: &[crate::integrations::ModuleSecretSetting] =
        &[crate::integrations::ModuleSecretSetting {
            path: &["spare", "key_name"],
            in_use: module_key_is_in_use,
        }];

    /// A builder a deployment added, whose module names a secret in its own
    /// table.
    fn module_with_a_secret() -> IntegrationBuilder {
        IntegrationBuilder::new(
            "probe",
            "example-crate",
            crate::integrations::registry_test_support::probe_registration,
            crate::integrations::registry_test_support::validate_nothing,
        )
        .with_module_name("testing.probe")
        .with_secret_settings(MODULE_SECRETS)
    }

    #[test]
    fn deploy_validation_refuses_a_module_s_secret_setting_in_use_that_names_no_key() {
        for (table, expected) in [
            (
                serde_json::json!({ "lock": true }),
                Some("testing.probe.spare.key_name"),
            ),
            (
                serde_json::json!({ "lock": true, "spare": { "key_name": "  " } }),
                Some("testing.probe.spare.key_name"),
            ),
            (
                serde_json::json!({ "lock": true, "spare": { "key_name": "module_key" } }),
                None,
            ),
            // A setting the table does not put to use is not checked.
            (serde_json::json!({ "lock": false }), None),
        ] {
            let mut settings = valid_settings();
            settings
                .insert_module_config("testing", "testing.probe", &table)
                .expect("should insert the module's table");

            let result = validate_settings_for_deploy_with(&settings, &[module_with_a_secret()]);

            match expected {
                Some(path) => {
                    let error = result.expect_err("should refuse a setting that names no key");
                    assert!(
                        format!("{error:?}").contains(path),
                        "should name the setting for {table}: {error:?}"
                    );
                }
                None => result.unwrap_or_else(|error| {
                    panic!("should accept {table}: {error:?}");
                }),
            }
        }
    }

    const PLAN_RULE_MESSAGE: &str = "probe module refuses an enabled auction";

    fn refuse_an_enabled_auction(
        _settings: &Settings,
        plan: &crate::auction::AuctionPlan,
    ) -> Result<(), Report<TrustedServerError>> {
        if plan.enabled() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: PLAN_RULE_MESSAGE.to_owned(),
            }));
        }
        Ok(())
    }

    #[test]
    fn deploy_validation_runs_a_builder_s_check_against_the_auction_plan() {
        let extra = [IntegrationBuilder::new(
            "probe",
            "example-crate",
            crate::integrations::registry_test_support::probe_registration,
            crate::integrations::registry_test_support::validate_nothing,
        )
        .with_module_name("testing.probe")
        .with_plan_validator(refuse_an_enabled_auction)];
        let mut settings = valid_settings();

        settings.auction.enabled = false;
        validate_settings_for_deploy_with(&settings, &extra)
            .expect("should accept a plan the module's rule allows");

        settings.auction.enabled = true;
        let error = validate_settings_for_deploy_with(&settings, &extra)
            .expect_err("should refuse a plan the module's rule does not allow");
        assert!(
            error.to_string().contains(PLAN_RULE_MESSAGE),
            "should keep the module's message intact: {error:?}"
        );
        validate_settings_for_deploy(&settings)
            .expect("should apply no such rule without the module's builder");
    }

    #[test]
    fn deploy_validation_checks_no_secret_setting_without_the_module_s_builder() {
        let mut settings = valid_settings();
        settings
            .insert_module_config(
                "testing",
                "testing.probe",
                &serde_json::json!({ "lock": true }),
            )
            .expect("should insert the module's table");

        validate_settings_for_deploy(&settings)
            .expect("should check nothing for a module that declared nothing to this validation");
    }

    #[test]
    fn validate_rejects_invalid_js_asset_proxy_assets() {
        let mut settings = valid_settings();
        settings
            .insert_module_config(
                "proxy",
                "js_asset_proxy",
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
