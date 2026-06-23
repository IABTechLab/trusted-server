use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trusted_server_core::config_payload::{
    build_config_payload, settings_from_config_blob, ConfigPayload,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::integrations::{
    adserver_mock::AdServerMockConfig, aps::ApsConfig, datadome::DataDomeConfig,
    didomi::DidomiIntegrationConfig, google_tag_manager::GoogleTagManagerConfig, gpt::GptConfig,
    lockr::LockrConfig, nextjs::NextJsIntegrationConfig, permutive::PermutiveConfig, prebid,
    sourcepoint::SourcepointConfig, testlight::TestlightConfig,
};
use trusted_server_core::settings::{IntegrationConfig, Settings};
use validator::Validate as _;

use crate::args::{ConfigInitArgs, ConfigValidateArgs};
use crate::error::{cli_error, report_error, CliResult};

const EXAMPLE_CONFIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../trusted-server.example.toml"
));

#[derive(Debug)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub payload: ConfigPayload,
}

#[derive(Serialize)]
struct ValidateJson<'a> {
    valid: bool,
    config_path: String,
    entry_count: Option<usize>,
    config_hash: Option<&'a str>,
    errors: Vec<String>,
}

pub fn run_init(args: &ConfigInitArgs, out: &mut dyn Write) -> CliResult<()> {
    if args.config.exists() && !args.force {
        return cli_error(format!(
            "{} already exists; pass --force to overwrite",
            args.config.display()
        ));
    }

    if let Some(parent) = args
        .config
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            report_error(format!(
                "failed to create parent directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    fs::write(&args.config, EXAMPLE_CONFIG).map_err(|error| {
        report_error(format!(
            "failed to write config {}: {error}",
            args.config.display()
        ))
    })?;
    writeln!(out, "Initialized config at {}", args.config.display())
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    Ok(())
}

pub fn run_validate(
    args: &ConfigValidateArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()> {
    match load_config(&args.config) {
        Ok(loaded) => {
            if args.json {
                let response = ValidateJson {
                    valid: true,
                    config_path: absolute_display(&loaded.path),
                    entry_count: Some(1),
                    config_hash: Some(&loaded.payload.hash),
                    errors: Vec::new(),
                };
                serde_json::to_writer(&mut *out, &response).map_err(|error| {
                    report_error(format!(
                        "failed to serialize validation JSON output: {error}"
                    ))
                })?;
                writeln!(out).map_err(|error| {
                    report_error(format!("failed to write command output: {error}"))
                })?;
            } else {
                writeln!(out, "Config valid: {}", absolute_display(&loaded.path)).map_err(
                    |error| report_error(format!("failed to write command output: {error}")),
                )?;
                writeln!(out, "Config entries: 1").map_err(|error| {
                    report_error(format!("failed to write command output: {error}"))
                })?;
                writeln!(out, "Config hash: {}", loaded.payload.hash).map_err(|error| {
                    report_error(format!("failed to write command output: {error}"))
                })?;
            }
            Ok(())
        }
        Err(error) => {
            let message = format_config_error(&args.config, &error);
            if args.json {
                let response = ValidateJson {
                    valid: false,
                    config_path: absolute_display(&args.config),
                    entry_count: None,
                    config_hash: None,
                    errors: vec![message],
                };
                serde_json::to_writer(&mut *out, &response).map_err(|error| {
                    report_error(format!(
                        "failed to serialize validation JSON output: {error}"
                    ))
                })?;
                writeln!(out).map_err(|error| {
                    report_error(format!("failed to write command output: {error}"))
                })?;
            } else {
                writeln!(err, "{message}").map_err(|error| {
                    report_error(format!("failed to write error output: {error}"))
                })?;
            }
            Err(error)
        }
    }
}

pub fn load_config(path: &Path) -> CliResult<LoadedConfig> {
    let contents = fs::read_to_string(path).map_err(|error| {
        report_error(format!(
            "missing {}: run `ts config init` or pass --config <path>: {error}",
            path.display()
        ))
    })?;
    let settings = Settings::from_toml(&contents)
        .map_err(|error| report_error(format!("invalid app config: {error:?}")))?;
    settings.validate().map_err(|error| {
        report_error(format!(
            "invalid app config: Configuration validation failed: {error}"
        ))
    })?;
    settings
        .reject_placeholder_secrets()
        .map_err(|error| report_error(format!("invalid app config: {error:?}")))?;
    let payload = build_config_payload(&settings)
        .map_err(|error| report_error(format!("failed to build config payload: {error:?}")))?;
    let runtime_settings = settings_from_config_blob(&payload.envelope_json).map_err(|error| {
        report_error(format!(
            "invalid app config: blob payload failed runtime reconstruction: {error:?}"
        ))
    })?;
    validate_runtime_startup(&runtime_settings)?;
    Ok(LoadedConfig {
        path: path.to_path_buf(),
        payload,
    })
}

fn validate_runtime_startup(settings: &Settings) -> CliResult<()> {
    let enabled_auction_providers = validate_enabled_integrations(settings)?;
    validate_auction_provider_names(settings, &enabled_auction_providers)?;
    PartnerRegistry::from_config(&settings.ec.partners)
        .map(|_| ())
        .map_err(|error| {
            report_error(format!(
                "invalid app config: EC partner registry startup failed: {error:?}"
            ))
        })?;
    Ok(())
}

fn validate_enabled_integrations(
    settings: &Settings,
) -> CliResult<std::collections::HashSet<&'static str>> {
    let mut enabled_auction_providers = std::collections::HashSet::new();

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

fn validate_prebid(settings: &Settings) -> CliResult<bool> {
    prebid::validate_config_for_startup(settings)
        .map(|config| config.is_some())
        .map_err(|error| {
            report_error(format!(
                "invalid app config: integration startup failed for `prebid`: {error:?}"
            ))
        })
}

fn validate_integration<T>(settings: &Settings, integration_id: &str) -> CliResult<bool>
where
    T: IntegrationConfig,
{
    settings
        .integration_config::<T>(integration_id)
        .map(|config| config.is_some())
        .map_err(|error| {
            report_error(format!(
                "invalid app config: integration startup failed for `{integration_id}`: {error:?}"
            ))
        })
}

fn validate_auction_provider_names(
    settings: &Settings,
    enabled_auction_providers: &std::collections::HashSet<&'static str>,
) -> CliResult<()> {
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
            return cli_error(format!(
                "invalid app config: auction startup failed: provider `{provider_name}` is listed in [auction] but no enabled integration provides it"
            ));
        }
    }

    Ok(())
}

fn absolute_display(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn format_config_error(path: &Path, error: &error_stack::Report<crate::error::CliError>) -> String {
    let mut message = format!("Config invalid: {}: {error:?}", path.display());
    if !path.exists() {
        message.push_str("\nHint: run `ts config init` or pass --config <path>");
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn valid_config() -> String {
        r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "production-proxy-secret"

[ec]
passphrase = "production-secret-key-32-bytes-min"

[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "production-admin-password-32-bytes"
"#
        .to_string()
    }

    #[test]
    fn init_writes_default_config_and_refuses_overwrite() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        let mut out = Vec::new();

        run_init(
            &ConfigInitArgs {
                config: path.clone(),
                force: false,
            },
            &mut out,
        )
        .expect("should initialize config");
        assert!(path.exists(), "should write config file");

        let err = run_init(
            &ConfigInitArgs {
                config: path,
                force: false,
            },
            &mut Vec::new(),
        )
        .expect_err("should refuse overwrite");
        assert!(
            err.to_string().contains("already exists"),
            "error should mention existing file"
        );
    }

    #[test]
    fn validate_json_success_reports_hash() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        fs::write(&path, valid_config()).expect("should write config");
        let mut out = Vec::new();

        run_validate(
            &ConfigValidateArgs {
                config: path,
                json: true,
            },
            &mut out,
            &mut Vec::new(),
        )
        .expect("should validate config");

        let value: serde_json::Value = serde_json::from_slice(&out).expect("should parse JSON");
        assert_eq!(value["valid"], true);
        assert!(
            value["entry_count"].as_u64().is_some(),
            "entry count should be numeric"
        );
        assert!(
            value["config_hash"]
                .as_str()
                .expect("should have hash")
                .starts_with("sha256:"),
            "hash should use sha256 prefix"
        );
    }

    #[test]
    fn validate_rejects_unknown_fields() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        fs::write(
            &path,
            format!("{}\nunknown_top_level = true\n", valid_config()),
        )
        .expect("should write config");

        let err = load_config(&path).expect_err("should reject unknown field");
        assert!(
            format!("{err:?}").contains("unknown_top_level"),
            "error should mention unknown field"
        );
    }

    #[test]
    fn validate_rejects_enabled_integration_startup_errors() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        fs::write(
            &path,
            format!(
                r#"{}

[integrations.prebid]
enabled = true
server_url = "not-a-url"
"#,
                valid_config()
            ),
        )
        .expect("should write config");

        let err = load_config(&path).expect_err("should reject invalid enabled integration");
        let message = format!("{err:?}");
        assert!(
            message.contains("integration startup failed")
                || message.contains("auction startup failed"),
            "error should mention runtime startup validation"
        );
        assert!(
            message.contains("server_url") || message.contains("url"),
            "error should mention invalid URL"
        );
    }

    #[test]
    fn validate_rejects_prebid_startup_rule_errors() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        fs::write(
            &path,
            format!(
                r#"{}

[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[[integrations.prebid.bid_param_override_rules]]
when = {{ bidder = "kargo" }}
set = {{}}
"#,
                valid_config()
            ),
        )
        .expect("should write config");

        let err = load_config(&path).expect_err("should reject invalid Prebid runtime rule");
        let message = format!("{err:?}");
        assert!(
            message.contains("prebid"),
            "error should mention Prebid validation"
        );
        assert!(
            message.contains("set"),
            "error should mention the invalid override set"
        );
    }

    #[test]
    fn validate_rejects_placeholders_from_init_template() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        let mut out = Vec::new();
        run_init(
            &ConfigInitArgs {
                config: path.clone(),
                force: false,
            },
            &mut out,
        )
        .expect("should initialize config");

        let err = load_config(&path).expect_err("template should require edits before validation");
        let error = format!("{err:?}");
        assert!(
            error.contains("Insecure default") || error.contains("placeholder password"),
            "error should mention an unreplaced placeholder secret"
        );
    }
}
