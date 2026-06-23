use std::io::Write;

use clap::Parser as _;

use crate::args::{Args, AuthCommand, Command, ConfigCommand};
use crate::config_command::{load_config, run_init, run_validate};
use crate::edgezero_delegate::{
    ConfigPushRequest, EdgeZeroDelegate, LifecycleCommand, ProductionEdgeZeroDelegate,
};
use crate::error::CliResult;

/// Run the CLI using process arguments and standard output streams.
///
/// # Errors
///
/// Returns an error when command parsing, config validation, `EdgeZero`
/// delegation, or output writing fails.
pub fn run_from_env() -> CliResult<()> {
    let args = Args::parse();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut delegate = ProductionEdgeZeroDelegate;
    dispatch(args, &mut delegate, &mut stdout, &mut stderr)
}

/// Run the CLI from explicit arguments and output streams.
///
/// # Errors
///
/// Returns an error when command parsing, config validation, `EdgeZero`
/// delegation, or output writing fails.
pub fn run_with_io<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> CliResult<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let parsed = Args::try_parse_from(args).map_err(|error| {
        crate::error::report_error(format!("failed to parse command arguments: {error}"))
    })?;
    let mut delegate = ProductionEdgeZeroDelegate;
    dispatch(parsed, &mut delegate, out, err)
}

fn dispatch(
    args: Args,
    delegate: &mut dyn EdgeZeroDelegate,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()> {
    match args.command {
        Command::Auth(auth) => match auth.command {
            AuthCommand::Login(login) => delegate.run_lifecycle(
                LifecycleCommand::AuthLogin,
                &login.adapter,
                &login.edgezero_args,
            ),
            AuthCommand::Logout(logout) => delegate.run_lifecycle(
                LifecycleCommand::AuthLogout,
                &logout.adapter,
                &logout.edgezero_args,
            ),
            AuthCommand::Status(status) => delegate.run_lifecycle(
                LifecycleCommand::AuthStatus,
                &status.adapter,
                &status.edgezero_args,
            ),
        },
        Command::Build(build) => delegate.run_lifecycle(
            LifecycleCommand::Build,
            &build.adapter,
            &build.edgezero_args,
        ),
        Command::Config(ConfigCommand::Init(init)) => run_init(&init, out),
        Command::Config(ConfigCommand::Validate(validate)) => run_validate(&validate, out, err),
        Command::Config(ConfigCommand::Push(push)) => {
            let loaded = load_config(&push.config)?;
            let config_key =
                edgezero_core::env_config::EnvConfig::from_env().store_key("config", &push.store);
            let request = ConfigPushRequest {
                adapter: push.adapter,
                manifest: push.manifest,
                store: push.store.clone(),
                local: push.local,
                dry_run: push.dry_run,
                runtime_config: push.runtime_config,
                entries: vec![(config_key, loaded.payload.envelope_json)],
                config_hash: loaded.payload.hash,
            };
            delegate.push_config(&request, out)
        }
        Command::Deploy(deploy) => delegate.run_lifecycle(
            LifecycleCommand::Deploy,
            &deploy.adapter,
            &deploy.edgezero_args,
        ),
        Command::Provision(provision) => delegate.run_lifecycle(
            LifecycleCommand::Provision,
            &provision.adapter,
            &provision.edgezero_args,
        ),
        Command::Serve(serve) => delegate.run_lifecycle(
            LifecycleCommand::Serve,
            &serve.adapter,
            &serve.edgezero_args,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::edgezero_delegate::tests::FakeEdgeZeroDelegate;

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

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(args).expect("should parse args")
    }

    #[test]
    fn build_delegates_to_edgezero_with_passthrough() {
        let args = parse(&["ts", "build", "--adapter", "fastly", "--", "--release"]);
        let mut delegate = FakeEdgeZeroDelegate::default();
        dispatch(args, &mut delegate, &mut Vec::new(), &mut Vec::new())
            .expect("should dispatch build");

        assert_eq!(delegate.lifecycle_calls.len(), 1);
        assert_eq!(delegate.lifecycle_calls[0].0, LifecycleCommand::Build);
        assert_eq!(delegate.lifecycle_calls[0].1, "fastly");
        assert_eq!(delegate.lifecycle_calls[0].2, ["--release"]);
    }

    #[test]
    fn auth_status_delegates_to_edgezero() {
        let args = parse(&["ts", "auth", "status", "--adapter", "fastly"]);
        let mut delegate = FakeEdgeZeroDelegate::default();
        dispatch(args, &mut delegate, &mut Vec::new(), &mut Vec::new())
            .expect("should dispatch auth status");

        assert_eq!(delegate.lifecycle_calls.len(), 1);
        assert_eq!(delegate.lifecycle_calls[0].0, LifecycleCommand::AuthStatus);
        assert_eq!(delegate.lifecycle_calls[0].1, "fastly");
    }

    #[test]
    fn config_push_validates_and_forwards_entries() {
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        let manifest_path = temp.path().join("edgezero.toml");
        fs::write(&config_path, valid_config()).expect("should write config");
        fs::write(&manifest_path, "[app]\nname = \"trusted-server\"\n")
            .expect("should write manifest placeholder");
        let args = Args::try_parse_from([
            "ts",
            "config",
            "push",
            "--adapter",
            "fastly",
            "--config",
            config_path.to_str().expect("path should be UTF-8"),
            "--manifest",
            manifest_path.to_str().expect("path should be UTF-8"),
            "--dry-run",
        ])
        .expect("should parse push args");
        let mut delegate = FakeEdgeZeroDelegate::default();
        let mut out = Vec::new();

        dispatch(args, &mut delegate, &mut out, &mut Vec::new()).expect("should dispatch push");

        assert_eq!(delegate.push_calls.len(), 1);
        let call = &delegate.push_calls[0];
        assert_eq!(call.adapter, "fastly");
        assert!(call.dry_run, "should forward dry-run");
        assert_eq!(call.store, "app_config");
        assert_eq!(call.entries.len(), 1, "should push one logical blob entry");
        assert_eq!(
            call.entries[0].0, "app_config",
            "should use the config store id as the blob key"
        );
        let envelope: edgezero_core::blob_envelope::BlobEnvelope =
            serde_json::from_str(&call.entries[0].1).expect("should parse blob envelope");
        envelope.verify().expect("should verify blob envelope");
    }
}
