use std::process;

use clap::{Parser, Subcommand};
use edgezero_cli::args::{
    ActiveVersionArgs, AuthArgs, BuildArgs, ConfigDiffArgs, ConfigGcArgs, ConfigPushArgs,
    ConfigValidateArgs, DeployArgs, HealthcheckArgs, ProvisionArgs, RollbackArgs, ServeArgs,
};
use trusted_server_core::config::TrustedServerAppConfig;

use crate::commands::audit::{AuditArgs, run_audit};
use crate::commands::config::ad_templates::{AdTemplatesCommand, run_ad_templates};
use crate::commands::config::init::{ConfigInitArgs, run_config_init};
use crate::prebid_bundle::{NpmPrebidBundleGenerator, PrebidBundleArgs, run_bundle};

#[derive(Debug, Parser)]
#[command(name = "ts", version, about = "Trusted Server CLI")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the currently active deployment version for a target adapter.
    ActiveVersion(ActiveVersionArgs),
    /// Browser-backed page and ad-template audits.
    Audit(Box<AuditArgs>),
    /// Sign in / out / status against an `EdgeZero` adapter.
    Auth(AuthArgs),
    /// Build the project for a target adapter.
    Build(BuildArgs),
    /// Trusted Server app-config commands.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Deploy the project through a target adapter.
    Deploy(DeployArgs),
    /// Probe a deployed version until it reports healthy.
    Healthcheck(HealthcheckArgs),
    /// Trusted Server Prebid commands.
    Prebid(PrebidArgs),
    /// Provision platform resources through a target adapter.
    Provision(ProvisionArgs),
    /// Roll a service back to a previously active deployment version.
    Rollback(RollbackArgs),
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
    /// Reclaim orphaned chunk entries leaked from prior oversized pushes.
    Gc(ConfigGcArgs),
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

/// Process-level outcome for commands that distinguish drift from tool errors.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunOutcome {
    /// Command completed without drift.
    Success,
    /// Command ran successfully and found assertion drift.
    AssertionFailed,
}

impl RunOutcome {
    /// Stable process exit code for this outcome.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::AssertionFailed => 1,
        }
    }
}

/// Run the CLI using process arguments.
///
/// # Errors
///
/// Returns an error when command parsing, config validation, `EdgeZero`
/// delegation, audit collection, config initialization, or Prebid bundle generation fails.
pub fn run_from_env() -> Result<RunOutcome, String> {
    dispatch(Args::parse())
}

fn dispatch(args: Args) -> Result<RunOutcome, String> {
    match args.command {
        Command::ActiveVersion(args) => {
            edgezero_cli::run_active_version(&args).map(|()| RunOutcome::Success)
        }
        Command::Auth(args) => edgezero_cli::run_auth(&args).map(|()| RunOutcome::Success),
        Command::Audit(args) => run_audit(&args),
        Command::Build(args) => edgezero_cli::run_build(&args).map(|()| RunOutcome::Success),
        Command::Config(ConfigCommand::AdTemplates(args)) => run_ad_templates(&args),
        Command::Config(ConfigCommand::Init(args)) => {
            run_config_init(&args).map(|()| RunOutcome::Success)
        }
        Command::Config(ConfigCommand::Diff(args)) => {
            match edgezero_cli::run_config_diff_typed::<TrustedServerAppConfig>(&args) {
                Ok(edgezero_cli::DiffExit { code: 0 }) => Ok(RunOutcome::Success),
                Ok(edgezero_cli::DiffExit { code: 1 }) => Ok(RunOutcome::AssertionFailed),
                Ok(edgezero_cli::DiffExit { code }) => process::exit(code),
                Err(err) => Err(err),
            }
        }
        Command::Config(ConfigCommand::Gc(args)) => {
            edgezero_cli::run_config_gc(&args).map(|()| RunOutcome::Success)
        }
        Command::Config(ConfigCommand::Push(args)) => {
            edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>(&args)
                .map(|()| RunOutcome::Success)
        }
        Command::Config(ConfigCommand::Validate(args)) => {
            edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&args)
                .map(|()| RunOutcome::Success)
        }
        Command::Deploy(args) => edgezero_cli::run_deploy(&args).map(|()| RunOutcome::Success),
        Command::Healthcheck(args) => {
            edgezero_cli::run_healthcheck(&args).map(|()| RunOutcome::Success)
        }
        Command::Prebid(prebid) => {
            let mut generator = NpmPrebidBundleGenerator;
            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();
            match prebid.command {
                PrebidCommand::Bundle(args) => {
                    run_bundle(&args, &mut generator, &mut stdout, &mut stderr)
                        .map(|()| RunOutcome::Success)
                }
            }
        }
        Command::Provision(args) => {
            edgezero_cli::run_provision(&args).map(|()| RunOutcome::Success)
        }
        Command::Rollback(args) => edgezero_cli::run_rollback(&args).map(|()| RunOutcome::Success),
        Command::Serve(args) => edgezero_cli::run_serve(&args).map(|()| RunOutcome::Success),
        Command::Dev(command) => crate::commands::dev::run(command).map(|()| RunOutcome::Success),
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
    fn parses_active_version() {
        let args = parse(&[
            "ts",
            "active-version",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
        ]);
        let Command::ActiveVersion(active_version) = args.command else {
            panic!("expected active-version command");
        };
        assert_eq!(active_version.adapter, "fastly");
        assert_eq!(active_version.service_id, "service-123");
    }

    #[test]
    fn parses_healthcheck_with_retry_defaults() {
        let args = parse(&[
            "ts",
            "healthcheck",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--version",
            "7",
            "--domain",
            "edge.example",
        ]);
        let Command::Healthcheck(healthcheck) = args.command else {
            panic!("expected healthcheck command");
        };
        assert_eq!(healthcheck.domain, "edge.example");
        assert_eq!(healthcheck.version, "7");
        assert_eq!(healthcheck.retry, 3, "should default to 3 retries");
        assert_eq!(
            healthcheck.retry_delay, 5,
            "should default to a 5s retry delay"
        );
        assert_eq!(healthcheck.timeout, 10, "should default to a 10s timeout");
        assert!(!healthcheck.staging, "should probe production by default");
    }

    #[test]
    fn parses_healthcheck_with_staging_overrides() {
        let args = parse(&[
            "ts",
            "healthcheck",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--version",
            "7",
            "--domain",
            "edge.example",
            "--staging",
            "--retry",
            "9",
            "--retry-delay",
            "2",
            "--timeout",
            "30",
        ]);
        let Command::Healthcheck(healthcheck) = args.command else {
            panic!("expected healthcheck command");
        };
        assert!(healthcheck.staging);
        assert_eq!(healthcheck.retry, 9);
        assert_eq!(healthcheck.retry_delay, 2);
        assert_eq!(healthcheck.timeout, 30);
    }

    #[test]
    fn healthcheck_requires_domain() {
        Args::try_parse_from([
            "ts",
            "healthcheck",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--version",
            "7",
        ])
        .expect_err("should reject healthcheck without a domain");
    }

    #[test]
    fn parses_rollback_with_explicit_target() {
        let args = parse(&[
            "ts",
            "rollback",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--version",
            "8",
            "--rollback-to",
            "7",
        ]);
        let Command::Rollback(rollback) = args.command else {
            panic!("expected rollback command");
        };
        assert_eq!(rollback.version, "8");
        assert_eq!(rollback.rollback_to, Some("7".to_owned()));
        assert!(!rollback.staging);
    }

    #[test]
    fn parses_staging_rollback_without_target() {
        let args = parse(&[
            "ts",
            "rollback",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--version",
            "8",
            "--staging",
        ]);
        let Command::Rollback(rollback) = args.command else {
            panic!("expected rollback command");
        };
        assert!(rollback.staging);
        assert_eq!(
            rollback.rollback_to, None,
            "staging rollback should not need an explicit target"
        );
    }

    #[test]
    fn rollback_requires_service_id() {
        Args::try_parse_from(["ts", "rollback", "--adapter", "fastly", "--version", "8"])
            .expect_err("should reject rollback without a service id");
    }

    #[test]
    fn parses_deploy_with_staging_flags() {
        let args = parse(&[
            "ts",
            "deploy",
            "--adapter",
            "fastly",
            "--service-id",
            "service-123",
            "--staging",
        ]);
        let Command::Deploy(deploy) = args.command else {
            panic!("expected deploy command");
        };
        assert_eq!(deploy.service_id, Some("service-123".to_owned()));
        assert!(deploy.staging);
    }

    #[test]
    fn deploy_rejects_renamed_stage_flag_before_separator() {
        // `--stage` was renamed to `--staging`, and adapter passthrough is
        // `last = true` (only captured after `--`). A stray `--stage` before the
        // separator must fail closed at parse time rather than being swallowed as
        // passthrough, which would leave `staging` false and route a
        // staging-intended deploy to production.
        Args::try_parse_from(["ts", "deploy", "--adapter", "fastly", "--stage"])
            .expect_err("should reject the renamed-away --stage flag, not route it to production");
    }

    #[test]
    fn deploy_captures_adapter_passthrough_after_separator() {
        let args = parse(&[
            "ts",
            "deploy",
            "--adapter",
            "fastly",
            "--",
            "--comment",
            "ci",
        ]);
        let Command::Deploy(deploy) = args.command else {
            panic!("expected deploy command");
        };
        assert!(!deploy.staging, "should default to a production deploy");
        assert_eq!(
            deploy.adapter_args,
            vec!["--comment", "ci"],
            "should capture args after -- as adapter passthrough"
        );
    }

    #[test]
    fn audit_does_not_accept_adapter_option() {
        let error = Args::try_parse_from([
            "ts",
            "audit",
            "https://publisher.example",
            "--adapter",
            "fastly",
        ])
        .expect_err("should reject audit adapter option");
        assert!(
            error.to_string().contains("unexpected argument")
                || error.to_string().contains("Found argument"),
            "error should explain unsupported option"
        );
    }

    #[test]
    fn run_outcomes_use_documented_exit_codes() {
        assert_eq!(RunOutcome::Success.exit_code(), 0);
        assert_eq!(RunOutcome::AssertionFailed.exit_code(), 1);
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
    fn config_gc_previews_by_default() {
        let args = parse(&["ts", "config", "gc", "--adapter", "fastly"]);
        let Command::Config(ConfigCommand::Gc(gc)) = args.command else {
            panic!("expected config gc command");
        };
        assert_eq!(gc.adapter, "fastly");
        assert_eq!(
            gc.older_than, None,
            "should not require an older-than window to preview"
        );
        assert!(!gc.dry_run);
        assert!(!gc.no_env);
    }

    #[test]
    fn config_gc_parses_destructive_sweep() {
        let args = parse(&[
            "ts",
            "config",
            "gc",
            "--adapter",
            "fastly",
            "--yes",
            "--older-than",
            "7d",
        ]);
        let Command::Config(ConfigCommand::Gc(gc)) = args.command else {
            panic!("expected config gc command");
        };
        assert!(gc.yes);
        assert_eq!(gc.older_than, Some("7d".to_owned()));
    }

    #[test]
    fn config_gc_rejects_dry_run_with_yes() {
        Args::try_parse_from([
            "ts",
            "config",
            "gc",
            "--adapter",
            "fastly",
            "--dry-run",
            "--yes",
        ])
        .expect_err("should reject conflicting --dry-run and --yes");
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
    fn config_ad_templates_check_requires_an_expectation_mode() {
        assert!(Args::try_parse_from(["ts", "config", "ad-templates", "check", "/news"]).is_err());
    }

    #[test]
    fn config_ad_templates_check_rejects_extra_slots_with_no_slots_mode() {
        assert!(
            Args::try_parse_from([
                "ts",
                "config",
                "ad-templates",
                "check",
                "/news",
                "--expect-no-slots",
                "--allow-extra-slots",
            ])
            .is_err()
        );
    }

    #[test]
    fn config_ad_templates_explain_rejects_removed_edgezero_model() {
        assert!(
            Args::try_parse_from([
                "ts",
                "config",
                "ad-templates",
                "explain",
                "/news",
                "--edgezero-enabled",
            ])
            .is_err()
        );
    }

    #[test]
    fn bare_audit_namespace_displays_help_as_an_error() {
        let error = Args::try_parse_from(["ts", "audit"]).expect_err("should require audit mode");

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn audit_legacy_url_parses_with_artifact_generation_flags() {
        let args = parse(&[
            "ts",
            "audit",
            "https://www.example.com/",
            "--js-assets",
            "audit/assets.toml",
            "--config",
            "audit/config.toml",
            "--force",
            "--cookie",
            "session=example",
        ]);
        let Command::Audit(audit) = args.command else {
            panic!("expected audit command");
        };
        assert_eq!(
            audit.legacy_generate.js_assets,
            Some(PathBuf::from("audit/assets.toml"))
        );
        assert_eq!(
            audit.legacy_generate.config,
            Some(PathBuf::from("audit/config.toml"))
        );
        assert!(audit.legacy_generate.force);
        assert_eq!(
            audit.legacy_generate.cookies,
            [("session".to_string(), "example".to_string())]
        );
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
    fn audit_browser_options_are_shared_by_generate_and_verify() {
        for mode in ["generate", "verify"] {
            let args = parse(&[
                "ts",
                "audit",
                "ad-templates",
                mode,
                "https://www.example.com/",
                "--chrome",
                "/tmp/test-chrome",
                "--headful",
                "--browser-proxy",
                "127.0.0.1:8080",
                "--no-assume-consent",
                "--settle-quiet-ms",
                "100",
                "--settle-max-ms",
                "200",
            ]);
            let Command::Audit(audit) = args.command else {
                panic!("expected audit command");
            };
            let (chrome, headful, no_assume_consent, browser_proxy, validation) =
                match audit.command.expect("should parse audit subcommand") {
                    crate::commands::audit::AuditSubcommand::AdTemplates(
                        crate::commands::audit::AuditAdTemplatesCommand::Generate(args),
                    ) => {
                        let validation = args.browser.validate();
                        (
                            args.browser.chrome,
                            args.browser.headful,
                            args.browser.no_assume_consent,
                            args.browser.browser_proxy,
                            validation,
                        )
                    }
                    crate::commands::audit::AuditSubcommand::AdTemplates(
                        crate::commands::audit::AuditAdTemplatesCommand::Verify(args),
                    ) => {
                        let validation = args.browser.validate();
                        (
                            args.browser.chrome,
                            args.browser.headful,
                            args.browser.no_assume_consent,
                            args.browser.browser_proxy,
                            validation,
                        )
                    }
                    _ => panic!("expected ad-template mode"),
                };
            assert_eq!(chrome, Some(PathBuf::from("/tmp/test-chrome")));
            assert!(headful);
            assert!(no_assume_consent);
            assert_eq!(browser_proxy.as_deref(), Some("127.0.0.1:8080"));
            validation.expect("should validate settle bounds");
        }
    }

    #[test]
    fn audit_generate_does_not_expose_the_ignored_browser_profile_flag() {
        assert!(
            Args::try_parse_from([
                "ts",
                "audit",
                "ad-templates",
                "generate",
                "https://www.example.com/",
                "--browser-profile",
                "mobile",
            ])
            .is_err(),
            "generation device selection must use --profiles"
        );
    }

    #[test]
    fn browser_settle_quiet_cannot_exceed_maximum() {
        let args = parse(&[
            "ts",
            "audit",
            "page",
            "https://www.example.com/",
            "--settle-quiet-ms",
            "201",
            "--settle-max-ms",
            "200",
        ]);
        let Command::Audit(audit) = args.command else {
            panic!("expected audit command");
        };
        let crate::commands::audit::AuditSubcommand::Page(page) =
            audit.command.expect("should parse page subcommand")
        else {
            panic!("expected page audit");
        };
        assert!(page.browser.validate().is_err());
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
