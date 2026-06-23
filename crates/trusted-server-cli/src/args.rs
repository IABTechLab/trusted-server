use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "ts", about = "Trusted Server CLI")]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in / out / status against an `EdgeZero` adapter.
    Auth(AuthArgs),
    /// Build the project for a target adapter.
    Build(DelegateArgs),
    /// Trusted Server app-config commands.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Deploy the project through a target adapter.
    Deploy(DelegateArgs),
    /// Provision platform resources through a target adapter.
    Provision(DelegateArgs),
    /// Serve the project locally through a target adapter.
    Serve(DelegateArgs),
}

#[derive(Debug, clap::Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in through the adapter's native auth flow.
    Login(AuthSubcommandArgs),
    /// Sign out through the adapter's native auth flow.
    Logout(AuthSubcommandArgs),
    /// Show the current adapter auth status.
    Status(AuthSubcommandArgs),
}

#[derive(Debug, clap::Args)]
pub struct AuthSubcommandArgs {
    /// Target adapter name.
    #[arg(long, required = true)]
    pub adapter: String,
    /// Arguments passed through to `EdgeZero`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub edgezero_args: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct DelegateArgs {
    /// Target adapter name.
    #[arg(long, required = true)]
    pub adapter: String,
    /// Arguments passed through to `EdgeZero`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub edgezero_args: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Initialize a Trusted Server config file from the example template.
    Init(ConfigInitArgs),
    /// Validate and hash a local Trusted Server config file.
    Validate(ConfigValidateArgs),
    /// Push the Trusted Server config blob through `EdgeZero`.
    Push(ConfigPushArgs),
}

#[derive(Debug, clap::Args)]
pub struct ConfigInitArgs {
    /// Target config path.
    #[arg(long, default_value = "trusted-server.toml")]
    pub config: PathBuf,
    /// Overwrite an existing target file.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, clap::Args)]
pub struct ConfigValidateArgs {
    /// Trusted Server config path.
    #[arg(long, default_value = "trusted-server.toml")]
    pub config: PathBuf,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ConfigPushArgs {
    /// Target adapter name.
    #[arg(long, required = true)]
    pub adapter: String,
    /// Trusted Server config path.
    #[arg(long, default_value = "trusted-server.toml")]
    pub config: PathBuf,
    /// `EdgeZero` manifest path.
    #[arg(long, default_value = "edgezero.toml")]
    pub manifest: PathBuf,
    /// Logical config-store id.
    #[arg(long, default_value = "app_config")]
    pub store: String,
    /// Push to local adapter state.
    #[arg(long)]
    pub local: bool,
    /// Resolve and report without mutating platform or local state.
    #[arg(long)]
    pub dry_run: bool,
    /// Adapter runtime config path.
    #[arg(long)]
    pub runtime_config: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_build_with_passthrough_args() {
        let args = Args::try_parse_from([
            "ts",
            "build",
            "--adapter",
            "fastly",
            "--",
            "--release",
            "--flag=value",
        ])
        .expect("should parse build command");
        let Command::Build(build) = args.command else {
            panic!("expected build command");
        };
        assert_eq!(build.adapter, "fastly");
        assert_eq!(build.edgezero_args, ["--release", "--flag=value"]);
    }

    #[test]
    fn parses_auth_with_passthrough_args() {
        let args = Args::try_parse_from([
            "ts",
            "auth",
            "login",
            "--adapter",
            "fastly",
            "--",
            "--profile",
            "dev",
        ])
        .expect("should parse auth command");
        let Command::Auth(auth) = args.command else {
            panic!("expected auth command");
        };
        let AuthCommand::Login(login) = auth.command else {
            panic!("expected login command");
        };
        assert_eq!(login.adapter, "fastly");
        assert_eq!(login.edgezero_args, ["--profile", "dev"]);
    }

    #[test]
    fn config_push_defaults_match_spec() {
        let args = Args::try_parse_from(["ts", "config", "push", "--adapter", "fastly"])
            .expect("should parse config push");
        let Command::Config(ConfigCommand::Push(push)) = args.command else {
            panic!("expected config push command");
        };
        assert_eq!(push.config, PathBuf::from("trusted-server.toml"));
        assert_eq!(push.manifest, PathBuf::from("edgezero.toml"));
        assert_eq!(push.store, "app_config");
        assert!(!push.local);
        assert!(!push.dry_run);
    }
}
