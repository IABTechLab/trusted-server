use clap::{Parser, Subcommand};
use edgezero_cli::args::{
    AuthArgs, BuildArgs, ConfigPushArgs, ConfigValidateArgs, DeployArgs, ProvisionArgs, ServeArgs,
};
use trusted_server_core::config::TrustedServerAppConfig;

use crate::config_init::{run_config_init, ConfigInitArgs};

#[derive(Debug, Parser)]
#[command(name = "ts", about = "Trusted Server CLI")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in / out / status against an EdgeZero adapter.
    Auth(AuthArgs),
    /// Build the project for a target adapter.
    Build(BuildArgs),
    /// Trusted Server app-config commands.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Deploy the project through a target adapter.
    Deploy(DeployArgs),
    /// Provision platform resources through a target adapter.
    Provision(ProvisionArgs),
    /// Serve the project locally through a target adapter.
    Serve(ServeArgs),
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Initialize a Trusted Server config file from the example template.
    Init(ConfigInitArgs),
    /// Push `trusted-server.toml` as a blob envelope through EdgeZero.
    Push(ConfigPushArgs),
    /// Validate `edgezero.toml` and the typed Trusted Server config.
    Validate(ConfigValidateArgs),
}

/// Run the CLI using process arguments.
///
/// # Errors
///
/// Returns an error when command parsing, config validation, EdgeZero
/// delegation, or config initialization fails.
pub fn run_from_env() -> Result<(), String> {
    dispatch(Args::parse())
}

fn dispatch(args: Args) -> Result<(), String> {
    match args.command {
        Command::Auth(args) => edgezero_cli::run_auth(&args),
        Command::Build(args) => edgezero_cli::run_build(&args),
        Command::Config(ConfigCommand::Init(args)) => run_config_init(&args),
        Command::Config(ConfigCommand::Push(args)) => {
            edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>(&args)
        }
        Command::Config(ConfigCommand::Validate(args)) => {
            edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&args)
        }
        Command::Deploy(args) => edgezero_cli::run_deploy(&args),
        Command::Provision(args) => edgezero_cli::run_provision(&args),
        Command::Serve(args) => edgezero_cli::run_serve(&args),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;
    use edgezero_cli::args::{AuthSub, ConfigPushArgs, ConfigValidateArgs};

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
}
