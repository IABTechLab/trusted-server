use std::process;

use clap::{Parser, Subcommand};
use edgezero_cli::args::{
    AuthArgs, BuildArgs, ConfigDiffArgs, ConfigPushArgs, ConfigValidateArgs, DeployArgs,
    ProvisionArgs, ServeArgs,
};
use trusted_server_core::config::TrustedServerAppConfig;

use crate::commands::audit::{AuditArgs, run_audit};
use crate::commands::config::ad_templates::{AdTemplatesCommand, run_ad_templates};
use crate::commands::config::init::{ConfigInitArgs, run_config_init};
use crate::prebid_bundle::{NpmPrebidBundleGenerator, PrebidBundleArgs, run_bundle};

#[derive(Debug, Parser)]
#[command(name = "ts", about = "Trusted Server CLI")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in / out / status against an `EdgeZero` adapter.
    Auth(AuthArgs),
    /// Browser-backed page and ad-template audits.
    Audit(AuditArgs),
    /// Build the project for a target adapter.
    Build(BuildArgs),
    /// Trusted Server app-config commands.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Deploy the project through a target adapter.
    Deploy(DeployArgs),
    /// Trusted Server Prebid commands.
    Prebid(PrebidArgs),
    /// Provision platform resources through a target adapter.
    Provision(ProvisionArgs),
    /// Serve the project locally through a target adapter.
    Serve(ServeArgs),
    /// Local developer tools (e.g. the macOS-only production-hostname proxy).
    #[command(subcommand)]
    Dev(crate::commands::dev::DevCommand),
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Diagnose server-side ad-template configuration and path matching.
    #[command(name = "ad-templates", subcommand)]
    AdTemplates(AdTemplatesCommand),
    /// Initialize a Trusted Server config file from the example template.
    Init(ConfigInitArgs),
    /// Diff `trusted-server.toml` against the live `EdgeZero` config.
    Diff(ConfigDiffArgs),
    /// Push `trusted-server.toml` as a blob envelope through `EdgeZero`.
    Push(ConfigPushArgs),
    /// Validate `edgezero.toml` and the typed Trusted Server config.
    Validate(ConfigValidateArgs),
}

#[derive(Debug, clap::Args)]
struct PrebidArgs {
    #[command(subcommand)]
    command: PrebidCommand,
}

#[derive(Debug, Subcommand)]
enum PrebidCommand {
    /// Generate a local external Prebid bundle and update config metadata.
    Bundle(PrebidBundleArgs),
}

/// Run the CLI using process arguments.
///
/// # Errors
///
/// Returns an error when command parsing, config validation, `EdgeZero`
/// delegation, audit collection, config initialization, or Prebid bundle generation fails.
pub fn run_from_env() -> Result<(), String> {
    dispatch(Args::parse())
}

fn dispatch(args: Args) -> Result<(), String> {
    match args.command {
        Command::Auth(args) => edgezero_cli::run_auth(&args),
        Command::Audit(args) => run_audit(&args),
        Command::Build(args) => edgezero_cli::run_build(&args),
        Command::Config(ConfigCommand::AdTemplates(args)) => run_ad_templates(&args),
        Command::Config(ConfigCommand::Init(args)) => run_config_init(&args),
        Command::Config(ConfigCommand::Diff(args)) => {
            match edgezero_cli::run_config_diff_typed::<TrustedServerAppConfig>(&args) {
                Ok(edgezero_cli::DiffExit { code: 0 }) => Ok(()),
                Ok(edgezero_cli::DiffExit { code }) => process::exit(code),
                Err(err) => Err(err),
            }
        }
        Command::Config(ConfigCommand::Push(args)) => {
            edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>(&args)
        }
        Command::Config(ConfigCommand::Validate(args)) => {
            edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&args)
        }
        Command::Deploy(args) => edgezero_cli::run_deploy(&args),
        Command::Prebid(prebid) => {
            let mut generator = NpmPrebidBundleGenerator;
            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();
            match prebid.command {
                PrebidCommand::Bundle(args) => {
                    run_bundle(&args, &mut generator, &mut stdout, &mut stderr)
                }
            }
        }
        Command::Provision(args) => edgezero_cli::run_provision(&args),
        Command::Serve(args) => edgezero_cli::run_serve(&args),
        Command::Dev(command) => crate::commands::dev::run(command),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;
    use edgezero_cli::args::{AuthSub, ConfigDiffArgs, ConfigPushArgs, ConfigValidateArgs};

    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(args).expect("should parse args")
    }

    #[test]
    fn parses_build_with_adapter_args() {
        let args = parse(&[
            "ts",
            "build",
            "--adapter",
            "fastly",
            "--",
            "--release",
            "--flag=value",
        ]);
        let Command::Build(build) = args.command else {
            panic!("expected build command");
        };
        assert_eq!(build.adapter, "fastly");
        assert_eq!(build.adapter_args, ["--release", "--flag=value"]);
    }

    #[test]
    fn parses_auth_status() {
        let args = parse(&["ts", "auth", "status", "--adapter", "fastly"]);
        let Command::Auth(auth) = args.command else {
            panic!("expected auth command");
        };
        let AuthSub::Status { adapter } = auth.sub else {
            panic!("expected status command");
        };
        assert_eq!(adapter, "fastly");
    }

    #[test]
    fn config_init_accepts_legacy_config_alias() {
        let args = parse(&[
            "ts",
            "config",
            "init",
            "--config",
            "custom/trusted-server.toml",
        ]);
        let Command::Config(ConfigCommand::Init(init)) = args.command else {
            panic!("expected config init command");
        };
        assert_eq!(
            init.app_config,
            PathBuf::from("custom/trusted-server.toml"),
            "legacy --config alias should still work"
        );
    }

    #[test]
    fn config_push_uses_edgezero_defaults() {
        let args = parse(&["ts", "config", "push", "--adapter", "fastly"]);
        let Command::Config(ConfigCommand::Push(push)) = args.command else {
            panic!("expected config push command");
        };
        let default_push = ConfigPushArgs::default();
        assert_eq!(push.adapter, "fastly");
        assert_eq!(push.app_config, default_push.app_config);
        assert_eq!(push.manifest, default_push.manifest);
        assert_eq!(push.store, default_push.store);
        assert!(!push.local);
        assert!(!push.dry_run);
        assert!(!push.no_env);
    }

    #[test]
    fn config_diff_uses_edgezero_defaults() {
        let args = parse(&["ts", "config", "diff", "--adapter", "fastly"]);
        let Command::Config(ConfigCommand::Diff(diff)) = args.command else {
            panic!("expected config diff command");
        };
        let default_diff = ConfigDiffArgs::default();
        assert_eq!(diff.adapter, "fastly");
        assert_eq!(diff.app_config, default_diff.app_config);
        assert_eq!(diff.manifest, default_diff.manifest);
        assert_eq!(diff.store, default_diff.store);
        assert!(!diff.local);
        assert!(!diff.exit_code);
        assert!(!diff.no_env);
    }

    #[test]
    fn config_validate_uses_edgezero_app_config_flag() {
        let args = parse(&[
            "ts",
            "config",
            "validate",
            "--app-config",
            "publisher-a.toml",
            "--no-env",
            "--strict",
        ]);
        let Command::Config(ConfigCommand::Validate(validate)) = args.command else {
            panic!("expected config validate command");
        };
        assert_eq!(validate.app_config, Some(PathBuf::from("publisher-a.toml")));
        assert!(validate.no_env);
        assert!(validate.strict);

        let default_validate = ConfigValidateArgs::default();
        assert_eq!(validate.manifest, default_validate.manifest);
    }

    #[test]
    fn config_ad_templates_match_parses_app_config_flags() {
        let args = parse(&[
            "ts",
            "config",
            "ad-templates",
            "match",
            "--app-config",
            "publisher-a.toml",
            "--no-env",
            "--details",
            "/news/story",
        ]);
        let Command::Config(ConfigCommand::AdTemplates(AdTemplatesCommand::Match(match_args))) =
            args.command
        else {
            panic!("expected ad-templates match command");
        };
        assert_eq!(
            match_args.config.app_config,
            Some(PathBuf::from("publisher-a.toml"))
        );
        assert!(match_args.config.no_env);
        assert!(match_args.details);
        assert_eq!(match_args.path_or_url, "/news/story");
    }

    #[test]
    fn config_ad_templates_check_parses_expected_slots() {
        let args = parse(&[
            "ts",
            "config",
            "ad-templates",
            "check",
            "/sports/game",
            "--expected-slot",
            "atf",
            "--expected-slot",
            "sports-sidebar",
            "--allow-extra-slots",
        ]);
        let Command::Config(ConfigCommand::AdTemplates(AdTemplatesCommand::Check(check_args))) =
            args.command
        else {
            panic!("expected ad-templates check command");
        };
        assert_eq!(check_args.path_or_url, "/sports/game");
        assert_eq!(check_args.expected_slots, ["atf", "sports-sidebar"]);
        assert!(check_args.allow_extra_slots);
        assert!(!check_args.expect_no_slots);
    }

    #[test]
    fn audit_legacy_url_parses_as_page_alias() {
        let args = parse(&["ts", "audit", "https://www.example.com/"]);
        assert!(matches!(args.command, Command::Audit(_)));
    }

    #[test]
    fn audit_page_subcommand_parses() {
        let args = parse(&["ts", "audit", "page", "https://www.example.com/"]);
        assert!(matches!(args.command, Command::Audit(_)));
    }

    #[test]
    fn audit_ad_templates_verify_parses() {
        let args = parse(&[
            "ts",
            "audit",
            "ad-templates",
            "verify",
            "https://www.example.com/",
        ]);
        assert!(matches!(args.command, Command::Audit(_)));
    }

    #[test]
    fn audit_ad_templates_without_verify_is_error() {
        assert!(Args::try_parse_from(["ts", "audit", "ad-templates"]).is_err());
    }

    #[test]
    fn audit_rejects_non_http_url() {
        assert!(Args::try_parse_from(["ts", "audit", "ftp://www.example.com/"]).is_err());
    }

    #[test]
    fn prebid_bundle_defaults_match_spec() {
        let args = parse(&["ts", "prebid", "bundle"]);
        let Command::Prebid(prebid) = args.command else {
            panic!("expected prebid command");
        };
        let PrebidCommand::Bundle(bundle) = prebid.command;
        assert_eq!(bundle.config, PathBuf::from("trusted-server.toml"));
        assert_eq!(bundle.out, PathBuf::from("dist/prebid"));
    }

    #[test]
    fn prebid_bundle_accepts_custom_paths() {
        let args = parse(&[
            "ts",
            "prebid",
            "bundle",
            "--config",
            "publisher.toml",
            "--out",
            "build/prebid",
        ]);
        let Command::Prebid(prebid) = args.command else {
            panic!("expected prebid command");
        };
        let PrebidCommand::Bundle(bundle) = prebid.command;
        assert_eq!(bundle.config, PathBuf::from("publisher.toml"));
        assert_eq!(bundle.out, PathBuf::from("build/prebid"));
    }

    #[test]
    fn prebid_bundle_does_not_accept_adapter_option() {
        let error = Args::try_parse_from(["ts", "prebid", "bundle", "--adapter", "fastly"])
            .expect_err("should reject prebid adapter option");
        assert!(
            error.to_string().contains("unexpected argument")
                || error.to_string().contains("Found argument"),
            "error should explain unsupported option"
        );
    }
}
