use std::io::Write;

use clap::Parser as _;

use crate::args::{Args, AuthCommand, Command, ConfigCommand};
use crate::audit::browser_collector::BrowserAuditCollector;
use crate::audit::collector::AuditCollector;
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
    let audit = BrowserAuditCollector;
    let mut services = CliServices {
        edgezero: &mut delegate,
        audit: &audit,
    };
    dispatch(args, &mut services, &mut stdout, &mut stderr)
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
    let audit = BrowserAuditCollector;
    let mut services = CliServices {
        edgezero: &mut delegate,
        audit: &audit,
    };
    dispatch(parsed, &mut services, out, err)
}

struct CliServices<'a> {
    edgezero: &'a mut dyn EdgeZeroDelegate,
    audit: &'a dyn AuditCollector,
}

fn dispatch(
    args: Args,
    services: &mut CliServices<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()> {
    match args.command {
        Command::Audit(audit) => crate::audit::run_audit(&audit, services.audit, out),
        Command::Auth(auth) => match auth.command {
            AuthCommand::Login(login) => services.edgezero.run_lifecycle(
                LifecycleCommand::AuthLogin,
                &login.adapter,
                &login.edgezero_args,
            ),
            AuthCommand::Logout(logout) => services.edgezero.run_lifecycle(
                LifecycleCommand::AuthLogout,
                &logout.adapter,
                &logout.edgezero_args,
            ),
            AuthCommand::Status(status) => services.edgezero.run_lifecycle(
                LifecycleCommand::AuthStatus,
                &status.adapter,
                &status.edgezero_args,
            ),
        },
        Command::Build(build) => services.edgezero.run_lifecycle(
            LifecycleCommand::Build,
            &build.adapter,
            &build.edgezero_args,
        ),
        Command::Config(ConfigCommand::Init(init)) => run_init(&init, out),
        Command::Config(ConfigCommand::Validate(validate)) => run_validate(&validate, out, err),
        Command::Config(ConfigCommand::Push(push)) => {
            let loaded = load_config(&push.config)?;
            let request = ConfigPushRequest {
                adapter: push.adapter,
                manifest: push.manifest,
                store: push.store,
                local: push.local,
                dry_run: push.dry_run,
                runtime_config: push.runtime_config,
                entries: loaded.payload.entries.into_iter().collect(),
                settings_entry_count: loaded.payload.settings_entries.len(),
                config_hash: loaded.payload.hash,
            };
            services.edgezero.push_config(&request, out)
        }
        Command::Deploy(deploy) => services.edgezero.run_lifecycle(
            LifecycleCommand::Deploy,
            &deploy.adapter,
            &deploy.edgezero_args,
        ),
        Command::Provision(provision) => services.edgezero.run_lifecycle(
            LifecycleCommand::Provision,
            &provision.adapter,
            &provision.edgezero_args,
        ),
        Command::Serve(serve) => services.edgezero.run_lifecycle(
            LifecycleCommand::Serve,
            &serve.adapter,
            &serve.edgezero_args,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;

    use tempfile::TempDir;
    use url::Url;

    use super::*;
    use crate::audit::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};
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

    struct FakeAuditCollector {
        calls: Cell<usize>,
    }

    impl AuditCollector for FakeAuditCollector {
        fn collect_page(&self, _target_url: &Url) -> CliResult<CollectedPage> {
            self.calls.set(self.calls.get() + 1);
            Ok(CollectedPage {
                requested_url: "https://publisher.example/page".to_string(),
                final_url: "https://publisher.example/page".to_string(),
                page_title: Some("Example Publisher".to_string()),
                html: r#"<html><head><script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script></head></html>"#.to_string(),
                script_tags: vec![CollectedScriptTag {
                    src: Some("https://www.googletagmanager.com/gtm.js?id=GTM-ABC123".to_string()),
                    inline_text: None,
                }],
                network_requests: vec![CollectedRequest {
                    url: "https://cdn.publisher.example/app.js".to_string(),
                    method: "GET".to_string(),
                    resource_type: Some("script".to_string()),
                    status: None,
                }],
                warnings: Vec::new(),
            })
        }
    }

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(args).expect("should parse args")
    }

    fn dispatch_for_test(
        args: Args,
        delegate: &mut FakeEdgeZeroDelegate,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> CliResult<()> {
        let audit = FakeAuditCollector {
            calls: Cell::new(0),
        };
        let mut services = CliServices {
            edgezero: delegate,
            audit: &audit,
        };
        dispatch(args, &mut services, out, err)
    }

    #[test]
    fn build_delegates_to_edgezero_with_passthrough() {
        let args = parse(&["ts", "build", "--adapter", "fastly", "--", "--release"]);
        let mut delegate = FakeEdgeZeroDelegate::default();
        dispatch_for_test(args, &mut delegate, &mut Vec::new(), &mut Vec::new())
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
        dispatch_for_test(args, &mut delegate, &mut Vec::new(), &mut Vec::new())
            .expect("should dispatch auth status");

        assert_eq!(delegate.lifecycle_calls.len(), 1);
        assert_eq!(delegate.lifecycle_calls[0].0, LifecycleCommand::AuthStatus);
        assert_eq!(delegate.lifecycle_calls[0].1, "fastly");
    }

    #[test]
    fn audit_uses_audit_collector_without_edgezero_delegate() {
        let temp = TempDir::new().expect("should create temp dir");
        let assets_path = temp.path().join("js-assets.toml");
        let args = Args::try_parse_from([
            "ts",
            "audit",
            "https://publisher.example/page",
            "--js-assets",
            assets_path.to_str().expect("path should be UTF-8"),
            "--no-config",
        ])
        .expect("should parse audit args");
        let mut delegate = FakeEdgeZeroDelegate::default();
        let audit = FakeAuditCollector {
            calls: Cell::new(0),
        };
        let mut services = CliServices {
            edgezero: &mut delegate,
            audit: &audit,
        };
        let mut out = Vec::new();

        dispatch(args, &mut services, &mut out, &mut Vec::new()).expect("should dispatch audit");

        assert_eq!(audit.calls.get(), 1, "should use audit collector");
        assert!(
            delegate.lifecycle_calls.is_empty(),
            "should not call EdgeZero lifecycle delegate"
        );
        assert!(
            delegate.push_calls.is_empty(),
            "should not call EdgeZero config push delegate"
        );
        assert!(assets_path.exists(), "should write audit artifact");
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

        dispatch_for_test(args, &mut delegate, &mut out, &mut Vec::new())
            .expect("should dispatch push");

        assert_eq!(delegate.push_calls.len(), 1);
        let call = &delegate.push_calls[0];
        assert_eq!(call.adapter, "fastly");
        assert!(call.dry_run, "should forward dry-run");
        assert_eq!(call.store, "app_config");
        assert!(
            call.entries
                .iter()
                .any(|(key, _value)| key == trusted_server_core::config_payload::CONFIG_HASH_KEY),
            "should include hash metadata"
        );
    }
}
