use std::fs;
use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use serde::{Deserialize, Serialize};
use toml::Table as TomlTable;
use trusted_server_core::request_signing::{JWKS_CONFIG_STORE_NAME, SIGNING_SECRET_STORE_NAME};
use trusted_server_core::runtime_config::{APPLICATION_CONFIG_STORE_NAME, LoadedRuntimeConfig};

use crate::error::CliError;

pub const DEFAULT_CONFIG_PATH: &str = "trusted-server.toml";
pub const STARTER_CONFIG_TEMPLATE: &str = include_str!("../../../trusted-server.example.toml");
pub const FASTLY_MANIFEST_PATH: &str = "fastly.toml";
pub const FASTLY_API_SECRET_STORE_NAME: &str = "api-keys";
pub const FASTLY_API_SECRET_KEY: &str = "api_key";

/// Validated CLI source configuration split into runtime and provider sections.
#[derive(Debug)]
pub struct ValidatedConfig {
    /// Resolved path to the source configuration file.
    pub path: PathBuf,
    /// Validated canonical runtime application configuration.
    pub loaded: LoadedRuntimeConfig,
    /// Provider/deployment settings excluded from canonical runtime config.
    pub providers: ProviderConfig,
}

/// Provider-specific deployment settings.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    /// Fastly provider settings.
    pub fastly: FastlyProviderConfig,
}

/// Fastly deployment settings excluded from canonical runtime config.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct FastlyProviderConfig {
    /// Fastly Compute service ID used by provisioning when no CLI override is provided.
    pub service_id: Option<String>,
    /// Underlying application Config Store resource settings.
    pub application_config: FastlyApplicationConfig,
    /// Underlying request-signing store resource settings.
    pub request_signing: FastlyRequestSigningConfig,
}

impl FastlyProviderConfig {
    /// Returns the configured service ID after trimming empty values.
    #[must_use]
    pub fn service_id(&self) -> Option<&str> {
        self.service_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

/// Underlying Fastly Config Store for the canonical application config.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct FastlyApplicationConfig {
    /// Fastly Config Store resource name to link as `ts_config_store`.
    pub store_name: String,
}

impl Default for FastlyApplicationConfig {
    fn default() -> Self {
        Self {
            store_name: APPLICATION_CONFIG_STORE_NAME.to_string(),
        }
    }
}

/// Underlying Fastly resources used by request signing provisioning.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct FastlyRequestSigningConfig {
    /// Fastly Config Store resource name to link as `jwks_store`.
    pub jwks_store_name: String,
    /// Fastly Secret Store resource name to link as `signing_keys`.
    pub signing_secret_store_name: String,
    /// Fastly Secret Store resource name to link as `api-keys`.
    pub runtime_api_secret_store_name: String,
}

impl Default for FastlyRequestSigningConfig {
    fn default() -> Self {
        Self {
            jwks_store_name: JWKS_CONFIG_STORE_NAME.to_string(),
            signing_secret_store_name: SIGNING_SECRET_STORE_NAME.to_string(),
            runtime_api_secret_store_name: FASTLY_API_SECRET_STORE_NAME.to_string(),
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ValidateConfigJson {
    pub valid: bool,
    pub path: String,
    pub config_hash: Option<String>,
    pub errors: Vec<String>,
}

pub fn resolve_config_path(path: Option<&Path>) -> Result<PathBuf, Report<CliError>> {
    let candidate = match path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => std::env::current_dir()
            .change_context(CliError::Io)?
            .join(path),
        None => std::env::current_dir()
            .change_context(CliError::Io)?
            .join(DEFAULT_CONFIG_PATH),
    };

    Ok(candidate)
}

pub fn ensure_writable_path(path: &Path, force: bool) -> Result<(), Report<CliError>> {
    if path.exists() && !force {
        return Err(Report::new(CliError::Io).attach(format!(
            "refusing to overwrite existing file `{}`; re-run with --force",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).change_context(CliError::Io)?;
    }

    Ok(())
}

pub fn write_starter_config(path: &Path, force: bool) -> Result<(), Report<CliError>> {
    ensure_writable_path(path, force)?;
    fs::write(path, STARTER_CONFIG_TEMPLATE).change_context(CliError::Io)
}

pub fn load_validated_config(path: Option<&Path>) -> Result<ValidatedConfig, Report<CliError>> {
    let resolved_path = resolve_config_path(path)?;

    let original_toml = fs::read_to_string(&resolved_path).map_err(|error| {
        let hint = format!(
            "failed to read config `{}`: {error}. Hint: run `ts config init` or pass `--config <path>`.",
            resolved_path.display()
        );
        Report::new(CliError::Configuration).attach(hint)
    })?;

    let (providers, app_toml) = split_source_config(&original_toml)
        .change_context(CliError::Configuration)
        .attach(format!("while parsing `{}`", resolved_path.display()))?;

    let loaded = trusted_server_core::runtime_config::load_runtime_config(&app_toml)
        .change_context(CliError::Configuration)
        .attach(format!("while validating `{}`", resolved_path.display()))?;

    Ok(ValidatedConfig {
        path: resolved_path,
        loaded,
        providers,
    })
}

fn split_source_config(toml_str: &str) -> Result<(ProviderConfig, String), Report<CliError>> {
    let mut document: TomlTable = toml::from_str(toml_str)
        .change_context(CliError::Configuration)
        .attach("failed to parse TOML configuration")?;

    let providers = match document.remove("providers") {
        Some(value) => value
            .try_into::<ProviderConfig>()
            .change_context(CliError::Configuration)
            .attach("failed to parse provider configuration")?,
        None => ProviderConfig::default(),
    };
    validate_provider_config(&providers)?;

    let app_toml = toml::to_string(&document)
        .change_context(CliError::Configuration)
        .attach("failed to serialize application configuration")?;

    Ok((providers, app_toml))
}

fn validate_provider_config(providers: &ProviderConfig) -> Result<(), Report<CliError>> {
    let fastly = &providers.fastly;

    if let Some(service_id) = fastly.service_id.as_deref() {
        validate_non_empty_exact("providers.fastly.service_id", service_id)?;
    }
    validate_non_empty_exact(
        "providers.fastly.application_config.store_name",
        &fastly.application_config.store_name,
    )?;
    validate_non_empty_exact(
        "providers.fastly.request_signing.jwks_store_name",
        &fastly.request_signing.jwks_store_name,
    )?;
    validate_non_empty_exact(
        "providers.fastly.request_signing.signing_secret_store_name",
        &fastly.request_signing.signing_secret_store_name,
    )?;
    validate_non_empty_exact(
        "providers.fastly.request_signing.runtime_api_secret_store_name",
        &fastly.request_signing.runtime_api_secret_store_name,
    )?;

    validate_distinct_names(
        "providers.fastly.application_config.store_name",
        &fastly.application_config.store_name,
        "providers.fastly.request_signing.jwks_store_name",
        &fastly.request_signing.jwks_store_name,
        "Fastly Config Store",
    )?;
    validate_distinct_names(
        "providers.fastly.request_signing.signing_secret_store_name",
        &fastly.request_signing.signing_secret_store_name,
        "providers.fastly.request_signing.runtime_api_secret_store_name",
        &fastly.request_signing.runtime_api_secret_store_name,
        "Fastly Secret Store",
    )
}

fn validate_non_empty_exact(path: &str, value: &str) -> Result<(), Report<CliError>> {
    if value.trim().is_empty() {
        return Err(
            Report::new(CliError::Configuration).attach(format!("`{path}` must not be empty"))
        );
    }
    if value.trim() != value {
        return Err(Report::new(CliError::Configuration).attach(format!(
            "`{path}` must not contain leading or trailing whitespace"
        )));
    }
    Ok(())
}

fn validate_distinct_names(
    left_path: &str,
    left_value: &str,
    right_path: &str,
    right_value: &str,
    resource_kind: &str,
) -> Result<(), Report<CliError>> {
    if left_value == right_value {
        return Err(Report::new(CliError::Configuration).attach(format!(
            "`{left_path}` and `{right_path}` must use distinct {resource_kind} names; both are `{left_value}`"
        )));
    }
    Ok(())
}

/// Resolves the Fastly service ID for provisioning.
///
/// Precedence is explicit CLI value, provider config, then `fastly.toml` in
/// the supplied manifest directories.
///
/// # Errors
///
/// Returns an error when no service ID can be found, or when a fallback
/// manifest cannot be read or parsed.
pub fn resolve_fastly_service_id(
    explicit_service_id: Option<&str>,
    fastly: &FastlyProviderConfig,
    manifest_dirs: &[PathBuf],
) -> Result<String, Report<CliError>> {
    if let Some(service_id) = explicit_service_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(service_id.to_string());
    }

    if let Some(service_id) = fastly.service_id() {
        return Ok(service_id.to_string());
    }

    for manifest_dir in unique_manifest_dirs(manifest_dirs) {
        let manifest_path = manifest_dir.join(FASTLY_MANIFEST_PATH);
        if let Some(service_id) = read_fastly_manifest_service_id(&manifest_path)? {
            return Ok(service_id);
        }
    }

    Err(Report::new(CliError::Arguments).attach(
        "missing Fastly service ID; pass `--service-id`, set `providers.fastly.service_id` in trusted-server.toml, or run from a directory containing fastly.toml with service_id",
    ))
}

fn unique_manifest_dirs(manifest_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for dir in manifest_dirs {
        if !unique.contains(dir) {
            unique.push(dir.clone());
        }
    }
    unique
}

fn read_fastly_manifest_service_id(path: &Path) -> Result<Option<String>, Report<CliError>> {
    if !path.exists() {
        return Ok(None);
    }

    let manifest = fs::read_to_string(path)
        .change_context(CliError::Io)
        .attach(format!("failed to read `{}`", path.display()))?;
    let document: TomlTable = toml::from_str(&manifest)
        .change_context(CliError::Configuration)
        .attach(format!("failed to parse `{}`", path.display()))?;

    Ok(document
        .get("service_id")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

pub fn validate_config_json(path: Option<&Path>) -> ValidateConfigJson {
    match load_validated_config(path) {
        Ok(validated) => ValidateConfigJson {
            valid: true,
            path: validated.path.display().to_string(),
            config_hash: Some(validated.loaded.config_hash),
            errors: Vec::new(),
        },
        Err(error) => {
            let resolved_path = resolve_config_path(path)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
            ValidateConfigJson {
                valid: false,
                path: resolved_path,
                config_hash: None,
                errors: vec![format!("{error:?}")],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_config_json_reports_success_for_example_config() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        fs::write(&path, STARTER_CONFIG_TEMPLATE).expect("should write starter config");

        let response = validate_config_json(Some(&path));

        assert!(response.valid, "should report valid example config");
        assert!(
            response.config_hash.is_some(),
            "should include config hash for valid config"
        );
    }

    #[test]
    fn validate_config_json_reports_missing_file() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join("missing.toml");

        let response = validate_config_json(Some(&path));

        assert!(!response.valid, "should report invalid for missing file");
        assert_eq!(response.config_hash, None, "should not have hash");
    }

    #[test]
    fn provider_config_is_excluded_from_canonical_config() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let with_provider_path = tempdir.path().join("with-provider.toml");
        let app_only_path = tempdir.path().join("app-only.toml");
        let with_provider = format!(
            "{}\n[providers.fastly]\nservice_id = \"svc_provider\"\n\n[providers.fastly.application_config]\nstore_name = \"customer_ts_config\"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&with_provider_path, with_provider).expect("should write provider config");
        fs::write(&app_only_path, STARTER_CONFIG_TEMPLATE).expect("should write app config");

        let with_provider =
            load_validated_config(Some(&with_provider_path)).expect("should load provider config");
        let app_only = load_validated_config(Some(&app_only_path)).expect("should load app config");

        assert_eq!(
            with_provider.loaded.config_hash, app_only.loaded.config_hash,
            "provider config should not affect canonical app hash"
        );
        assert!(
            !with_provider.loaded.canonical_toml.contains("[providers"),
            "canonical app config should exclude provider config section"
        );
        assert!(
            !with_provider
                .loaded
                .canonical_toml
                .contains("customer_ts_config"),
            "canonical app config should exclude provider values"
        );
        assert_eq!(
            with_provider.providers.fastly.service_id(),
            Some("svc_provider"),
            "should keep provider service ID separately"
        );
        assert_eq!(
            with_provider.providers.fastly.application_config.store_name, "customer_ts_config",
            "should parse provider app config store name"
        );
    }

    #[test]
    fn unknown_provider_field_fails_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.fastly.application_config]\nstor_name = \"typo\"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path)).expect_err("should reject unknown field");

        assert!(
            format!("{error:?}").contains("stor_name"),
            "should identify unknown provider field"
        );
    }

    #[test]
    fn unknown_provider_namespace_fails_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.aws]\nenabled = true\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path)).expect_err("should reject unknown provider");

        assert!(
            format!("{error:?}").contains("aws"),
            "should identify unknown provider namespace"
        );
    }

    #[test]
    fn runtime_api_secret_key_provider_field_fails_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.fastly.request_signing]\nruntime_api_secret_key = \"custom\"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path)).expect_err("should reject unknown field");

        assert!(
            format!("{error:?}").contains("runtime_api_secret_key"),
            "should identify unsupported runtime API secret key field"
        );
    }

    #[test]
    fn empty_provider_resource_name_fails_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.fastly.application_config]\nstore_name = \"   \"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path)).expect_err("should reject empty store name");

        assert!(
            format!("{error:?}").contains("providers.fastly.application_config.store_name"),
            "should identify invalid provider field"
        );
    }

    #[test]
    fn duplicate_provider_config_store_names_fail_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.fastly.application_config]\nstore_name = \"shared_config\"\n\n[providers.fastly.request_signing]\njwks_store_name = \"shared_config\"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path))
            .expect_err("should reject duplicate config store names");

        assert!(
            format!("{error:?}").contains("distinct Fastly Config Store names"),
            "should explain duplicate config store names"
        );
    }

    #[test]
    fn duplicate_provider_secret_store_names_fail_validation() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        let config = format!(
            "{}\n[providers.fastly.request_signing]\nsigning_secret_store_name = \"shared_secret\"\nruntime_api_secret_store_name = \"shared_secret\"\n",
            STARTER_CONFIG_TEMPLATE
        );
        fs::write(&path, config).expect("should write config");

        let error = load_validated_config(Some(&path))
            .expect_err("should reject duplicate secret store names");

        assert!(
            format!("{error:?}").contains("distinct Fastly Secret Store names"),
            "should explain duplicate secret store names"
        );
    }

    #[test]
    fn provider_defaults_are_applied_when_omitted() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        fs::write(&path, STARTER_CONFIG_TEMPLATE).expect("should write config");

        let validated = load_validated_config(Some(&path)).expect("should validate config");

        assert_eq!(
            validated.providers.fastly.application_config.store_name, APPLICATION_CONFIG_STORE_NAME,
            "should default app config store resource name"
        );
        assert_eq!(
            validated.providers.fastly.request_signing.jwks_store_name, JWKS_CONFIG_STORE_NAME,
            "should default JWKS store resource name"
        );
        assert_eq!(
            validated
                .providers
                .fastly
                .request_signing
                .signing_secret_store_name,
            SIGNING_SECRET_STORE_NAME,
            "should default signing secret store resource name"
        );
    }

    #[test]
    fn service_id_resolution_uses_expected_precedence() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        fs::write(
            tempdir.path().join(FASTLY_MANIFEST_PATH),
            "service_id = \"svc_manifest\"\n",
        )
        .expect("should write manifest");
        let fastly = FastlyProviderConfig {
            service_id: Some("svc_provider".to_string()),
            ..FastlyProviderConfig::default()
        };
        let dirs = vec![tempdir.path().to_path_buf()];

        assert_eq!(
            resolve_fastly_service_id(Some("svc_cli"), &fastly, &dirs)
                .expect("should resolve CLI service ID"),
            "svc_cli",
            "CLI should win"
        );
        assert_eq!(
            resolve_fastly_service_id(None, &fastly, &dirs)
                .expect("should resolve provider service ID"),
            "svc_provider",
            "provider config should win over manifest"
        );
        assert_eq!(
            resolve_fastly_service_id(None, &FastlyProviderConfig::default(), &dirs)
                .expect("should resolve manifest service ID"),
            "svc_manifest",
            "manifest should be fallback"
        );
    }

    #[test]
    fn service_id_resolution_errors_when_all_sources_are_missing() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let dirs = vec![tempdir.path().to_path_buf()];

        let error = resolve_fastly_service_id(None, &FastlyProviderConfig::default(), &dirs)
            .expect_err("should require service ID");

        assert!(
            format!("{error:?}").contains("missing Fastly service ID"),
            "should explain service ID sources"
        );
    }
}
