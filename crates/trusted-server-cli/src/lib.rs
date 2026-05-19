mod audit;
mod config;
mod dev;
mod error;
mod fastly;
mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use dialoguer::Confirm;
use error_stack::{Report, ResultExt};

use crate::error::CliError;
use crate::fastly::api::ReqwestFastlyApi;
use crate::fastly::auth::{
    SystemCredentialStore, fastly_auth_status, login_fastly, logout_fastly, resolve_fastly_api_key,
};
use crate::fastly::provision::{
    ProvisionApplyOutcome, apply_fastly_provisioning_with_outcome, plan_fastly_provisioning,
};
use crate::output::{format_report, write_json, write_stderr_line, write_stdout_line};

#[derive(Debug, Parser)]
#[command(name = "ts")]
#[command(about = "Trusted Server CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Audit(AuditArgs),
    Dev {
        #[command(subcommand)]
        command: dev::DevCommand,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    Provision {
        #[command(subcommand)]
        command: ProvisionCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Init(ConfigInitArgs),
    Validate(ConfigValidateArgs),
}

#[derive(Debug, Args)]
struct ConfigInitArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ConfigValidateArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct AuditArgs {
    url: String,
    #[arg(long)]
    js_assets: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    no_js_assets: bool,
    #[arg(long)]
    no_config: bool,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    Fastly {
        #[command(subcommand)]
        command: FastlyAuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FastlyAuthCommand {
    Login,
    Status(FastlyAuthStatusArgs),
    Logout,
}

#[derive(Debug, Args)]
struct FastlyAuthStatusArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ProvisionCommand {
    Fastly {
        #[command(subcommand)]
        command: FastlyProvisionCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FastlyProvisionCommand {
    Plan(FastlyProvisionArgs),
    Apply(FastlyProvisionApplyArgs),
}

#[derive(Debug, Args)]
struct FastlyProvisionArgs {
    #[arg(long)]
    service_id: Option<String>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct FastlyProvisionApplyArgs {
    #[arg(long)]
    service_id: Option<String>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    runtime_api_key: Option<String>,
    #[arg(
        long,
        help = "Use the management Fastly API token as the runtime token. Warning: this stores a full management token in the runtime secret store; use only for smoke tests or trusted local environments."
    )]
    reuse_management_api_key: bool,
}

#[must_use]
pub fn run() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = write_stderr_line(format_report(&error));
            if matches!(error.current_context(), CliError::Cancelled) {
                ExitCode::from(130)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

fn execute() -> Result<(), Report<CliError>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Config { command } => run_config(command),
        Command::Audit(args) => run_audit(&args),
        Command::Dev { command } => run_dev(command),
        Command::Auth { command } => run_auth(command),
        Command::Provision { command } => run_provision(command),
    }
}

fn run_config(command: ConfigCommand) -> Result<(), Report<CliError>> {
    match command {
        ConfigCommand::Init(args) => {
            let path = config::resolve_config_path(args.config.as_deref())?;
            config::write_starter_config(&path, args.force)?;
            write_stdout_line(format!("Initialized config at {}", path.display()))
        }
        ConfigCommand::Validate(args) => {
            if args.json {
                let response = config::validate_config_json(args.config.as_deref());
                let valid = response.valid;
                write_json(&response)?;
                if valid {
                    Ok(())
                } else {
                    Err(Report::new(CliError::Configuration)
                        .attach("configuration validation failed"))
                }
            } else {
                let validated = config::load_validated_config(args.config.as_deref())?;
                write_stdout_line(format!(
                    "Config valid: {}\nConfig hash: {}",
                    validated.path.display(),
                    validated.loaded.config_hash
                ))
            }
        }
    }
}

fn run_audit(args: &AuditArgs) -> Result<(), Report<CliError>> {
    if args.no_js_assets && args.no_config {
        return Err(Report::new(CliError::Arguments)
            .attach("nothing to do: both --no-js-assets and --no-config were set"));
    }

    let url = parse_audit_url(&args.url)?;
    let outputs = audit::perform_audit(&url)?;

    let js_assets_path = if args.no_js_assets {
        None
    } else {
        Some(config::resolve_config_path(
            args.js_assets
                .as_deref()
                .or_else(|| Some(std::path::Path::new("js-assets.toml"))),
        )?)
    };
    let config_path = if args.no_config {
        None
    } else {
        Some(config::resolve_config_path(args.config.as_deref())?)
    };

    let written = audit::write_audit_outputs(
        &outputs,
        js_assets_path.as_deref(),
        config_path.as_deref(),
        args.force,
    )?;

    let integrations = outputs
        .artifact
        .detected_integrations
        .iter()
        .map(|integration| integration.id.clone())
        .collect::<Vec<_>>();

    write_stdout_line(format!(
        "Audited {}\nTitle: {}\nJS assets: {}\nThird-party assets: {}\nDetected integrations: {}\nWrote: {}",
        outputs.artifact.audited_url,
        outputs
            .artifact
            .page_title
            .clone()
            .unwrap_or_else(|| "<unknown>".to_string()),
        outputs.artifact.js_asset_count,
        outputs.artifact.third_party_asset_count,
        if integrations.is_empty() {
            "none".to_string()
        } else {
            integrations.join(", ")
        },
        if written.is_empty() {
            "none".to_string()
        } else {
            written.join(", ")
        }
    ))
}

fn run_dev(command: dev::DevCommand) -> Result<(), Report<CliError>> {
    match command {
        dev::DevCommand::Serve(args) => run_dev_serve(&args),
    }
}

fn run_dev_serve(args: &dev::ServeArgs) -> Result<(), Report<CliError>> {
    let validated = config::load_validated_config(args.config.as_deref())?;
    let status = dev::run_dev_command(args.adapter, &validated, &args.env, &args.passthrough)?;
    if status.success() {
        Ok(())
    } else {
        Err(Report::new(CliError::Development).attach(format!(
            "`fastly compute serve` exited with status {status}"
        )))
    }
}

fn run_auth(command: AuthCommand) -> Result<(), Report<CliError>> {
    let store = SystemCredentialStore;
    match command {
        AuthCommand::Fastly {
            command: FastlyAuthCommand::Login,
        } => {
            login_fastly(&store)?;
            write_stdout_line("Stored Fastly API key in secure storage")
        }
        AuthCommand::Fastly {
            command: FastlyAuthCommand::Status(args),
        } => {
            let status = fastly_auth_status(&store)?;
            if args.json {
                write_json(&status)
            } else {
                write_stdout_line(format!(
                    "Environment credential: {}\nStored credential: {}\nEffective source: {}",
                    if status.has_env_credential {
                        "present"
                    } else {
                        "missing"
                    },
                    if status.has_stored_credential {
                        "present"
                    } else {
                        "missing"
                    },
                    match status.effective_source {
                        Some(crate::fastly::auth::CredentialSource::Environment) => "environment",
                        Some(crate::fastly::auth::CredentialSource::SecureStorage) =>
                            "secure-storage",
                        None => "none",
                    }
                ))
            }
        }
        AuthCommand::Fastly {
            command: FastlyAuthCommand::Logout,
        } => {
            logout_fastly(&store)?;
            write_stdout_line("Removed stored Fastly credential")
        }
    }
}

const FASTLY_RUNTIME_API_KEY_ENV: &str = "FASTLY_RUNTIME_API_KEY";

struct RuntimeApiKeySelection {
    value: Option<String>,
    reused_management_key: bool,
}

fn parse_audit_url(value: &str) -> Result<url::Url, Report<CliError>> {
    let url = url::Url::parse(value).map_err(|error| {
        Report::new(CliError::Arguments).attach(format!("invalid audit URL `{value}`: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Report::new(CliError::Arguments).attach(format!(
            "`ts audit` only supports http/https URLs, got `{}`",
            url.scheme()
        )));
    }
    Ok(url)
}

fn resolve_runtime_api_key_for_apply(
    management_api_key: &str,
    explicit_runtime_api_key: Option<&str>,
    reuse_management_api_key: bool,
    request_signing_enabled: bool,
) -> Result<RuntimeApiKeySelection, Report<CliError>> {
    if !request_signing_enabled {
        return Ok(RuntimeApiKeySelection {
            value: None,
            reused_management_key: false,
        });
    }

    let explicit_runtime_api_key = explicit_runtime_api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let env_runtime_api_key = std::env::var(FASTLY_RUNTIME_API_KEY_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let selected_sources = usize::from(explicit_runtime_api_key.is_some())
        + usize::from(env_runtime_api_key.is_some())
        + usize::from(reuse_management_api_key);
    if selected_sources > 1 {
        return Err(Report::new(CliError::Arguments).attach(format!(
            "choose only one runtime Fastly API key source: `--runtime-api-key`, {FASTLY_RUNTIME_API_KEY_ENV}, or `--reuse-management-api-key`"
        )));
    }

    if let Some(value) = explicit_runtime_api_key {
        return Ok(RuntimeApiKeySelection {
            value: Some(value),
            reused_management_key: false,
        });
    }
    if let Some(value) = env_runtime_api_key {
        return Ok(RuntimeApiKeySelection {
            value: Some(value),
            reused_management_key: false,
        });
    }
    if reuse_management_api_key {
        return Ok(RuntimeApiKeySelection {
            value: Some(management_api_key.to_string()),
            reused_management_key: true,
        });
    }

    Ok(RuntimeApiKeySelection {
        value: None,
        reused_management_key: false,
    })
}

fn confirm_reuse_management_api_key() -> Result<(), Report<CliError>> {
    let confirmed = Confirm::new()
        .with_prompt(
            "Reuse the management Fastly API key as a runtime secret? This stores a full management token where edge runtime code can read it.",
        )
        .default(false)
        .interact()
        .change_context(CliError::Cancelled)?;
    if confirmed {
        Ok(())
    } else {
        Err(Report::new(CliError::Cancelled).attach("user declined management API key reuse"))
    }
}

fn fastly_manifest_dirs(config_path: &Path) -> Result<Vec<PathBuf>, Report<CliError>> {
    let mut dirs = Vec::new();
    if let Some(parent) = config_path.parent() {
        dirs.push(parent.to_path_buf());
    }
    dirs.push(std::env::current_dir().change_context(CliError::Io)?);
    Ok(dirs)
}

fn run_provision(command: ProvisionCommand) -> Result<(), Report<CliError>> {
    let store = SystemCredentialStore;
    let resolved = resolve_fastly_api_key(&store)?;
    write_stderr_line(format!(
        "Using Fastly credential source: {}",
        match resolved.source {
            crate::fastly::auth::CredentialSource::Environment => "environment",
            crate::fastly::auth::CredentialSource::SecureStorage => "secure-storage",
        }
    ))?;
    let api = ReqwestFastlyApi::new(resolved.value.clone())?;

    match command {
        ProvisionCommand::Fastly {
            command: FastlyProvisionCommand::Plan(args),
        } => {
            let validated = config::load_validated_config(args.config.as_deref())?;
            let service_id = config::resolve_fastly_service_id(
                args.service_id.as_deref(),
                &validated.providers.fastly,
                &fastly_manifest_dirs(&validated.path)?,
            )?;
            let plan = plan_fastly_provisioning(&api, &validated, &service_id)?;
            if args.json {
                write_json(&plan.json)
            } else {
                write_stdout_line(format!(
                    "Service: {}\nLatest version: {}\nTarget version: {}\nActions: {}\nWarnings: {}",
                    plan.json.service_id,
                    plan.json.service_version.latest_version,
                    plan.json.service_version.target_version,
                    if plan.json.actions.is_empty() {
                        "none".to_string()
                    } else {
                        plan.json
                            .actions
                            .iter()
                            .map(|action| {
                                format!(
                                    "{} {}",
                                    action.detail,
                                    action.remote_id.as_deref().unwrap_or("")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    },
                    if plan.json.warnings.is_empty() {
                        "none".to_string()
                    } else {
                        plan.json.warnings.join("; ")
                    }
                ))
            }
        }
        ProvisionCommand::Fastly {
            command: FastlyProvisionCommand::Apply(args),
        } => {
            let validated = config::load_validated_config(args.config.as_deref())?;
            let request_signing_enabled = validated
                .loaded
                .settings
                .request_signing
                .as_ref()
                .is_some_and(|request_signing| request_signing.enabled);
            let runtime_api_key = resolve_runtime_api_key_for_apply(
                &resolved.value,
                args.runtime_api_key.as_deref(),
                args.reuse_management_api_key,
                request_signing_enabled,
            )?;
            if runtime_api_key.reused_management_key {
                confirm_reuse_management_api_key()?;
            }
            let service_id = config::resolve_fastly_service_id(
                args.service_id.as_deref(),
                &validated.providers.fastly,
                &fastly_manifest_dirs(&validated.path)?,
            )?;
            let outcome = apply_fastly_provisioning_with_outcome(
                &api,
                &validated,
                &service_id,
                runtime_api_key.value.as_deref(),
                args.yes,
            )?;
            match outcome {
                ProvisionApplyOutcome::Success(applied) => write_apply_result(&applied, args.json),
                ProvisionApplyOutcome::Failure(failure) => {
                    if args.json {
                        write_json(&failure.json)?;
                    }
                    Err(failure.error)
                }
            }
        }
    }
}

fn write_apply_result(
    applied: &crate::fastly::provision::ProvisionApplyJson,
    json: bool,
) -> Result<(), Report<CliError>> {
    if json {
        write_json(applied)
    } else {
        write_stdout_line(format!(
            "Applied {} change(s) to service {} using version {}\nActivated version: {}",
            applied.completed_actions.len(),
            applied.service_id,
            applied.service_version.target_version,
            if applied.activated_version {
                "yes"
            } else {
                "no"
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_command_debug_asserts() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parse_audit_url_accepts_http_and_https() {
        assert!(parse_audit_url("http://publisher.example").is_ok());
        assert!(parse_audit_url("https://publisher.example").is_ok());
    }

    #[test]
    fn parse_audit_url_rejects_non_http_schemes() {
        for url in [
            "file:///etc/passwd",
            "data:text/html,hello",
            "chrome://version",
        ] {
            let error = parse_audit_url(url).expect_err("should reject non-http URL");
            assert!(
                format!("{error:?}").contains("only supports http/https"),
                "should explain scheme restriction"
            );
        }
    }
}
