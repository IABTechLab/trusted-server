//! Browser-backed `ts audit` command namespace.
//!
//! `ts audit page <url>` is the generic page audit; `ts audit ad-templates verify
//! <url>...` is the ad-template verifier; `ts audit generate <url>` bootstraps a
//! draft config from a live page (issue #800). `ts audit <url>` is a hidden
//! compatibility alias for `ts audit page <url>`.

pub mod ad_templates;
pub mod browser;
pub mod collector;
pub mod generate;
pub mod page;

use clap::{Args, Subcommand};

use crate::app_config::AppConfigArgs;
use crate::audit::collector::BrowserOpts;
use crate::audit::page::PageAuditArgs;

/// Parses and validates an `http`/`https` URL, rejecting all other schemes.
///
/// # Errors
///
/// Returns a user-facing string when the input is not a valid `http`/`https` URL.
pub(crate) fn parse_http_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|error| format!("invalid URL `{raw}`: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "unsupported URL scheme `{other}` (expected http or https)"
        )),
    }
}

/// `ts audit` arguments: an optional subcommand plus a hidden legacy URL positional.
#[derive(Debug, Args)]
pub(crate) struct AuditArgs {
    #[command(subcommand)]
    pub(crate) command: Option<AuditSubcommand>,
    /// Hidden compatibility alias: `ts audit <url>` behaves like `ts audit page <url>`.
    #[arg(value_parser = parse_http_url, hide = true)]
    pub(crate) legacy_url: Option<url::Url>,
}

/// `ts audit` subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum AuditSubcommand {
    /// Audit a single page and print a read-only summary.
    Page(PageAuditArgs),
    /// Verify configured ad-template slots against live page evidence.
    #[command(name = "ad-templates", subcommand)]
    AdTemplates(AuditAdTemplatesCommand),
    /// Bootstrap a draft Trusted Server config + JS asset audit from a live page.
    Generate(generate::GenerateArgs),
}

/// `ts audit ad-templates` subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum AuditAdTemplatesCommand {
    /// Verify ad-template slots for one or more live URLs.
    Verify(AuditAdTemplatesVerifyArgs),
}

/// Arguments for `ts audit ad-templates verify <url>...`.
#[derive(Debug, Args)]
pub(crate) struct AuditAdTemplatesVerifyArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// One or more page URLs to verify (http or https).
    #[arg(required = true, value_parser = parse_http_url)]
    pub urls: Vec<url::Url>,
    /// Exit non-zero when a matched slot is missing or only partially confirmed.
    #[arg(long)]
    pub strict: bool,
    /// Emit machine-readable JSON instead of human output.
    #[arg(long)]
    pub json: bool,
    /// Perform a deterministic scroll pass after the initial settle.
    #[arg(long)]
    pub scroll: bool,
    #[command(flatten)]
    pub browser: BrowserOpts,
}

/// Dispatches a `ts audit` invocation.
///
/// `legacy_url` (if present) and the `page` subcommand both route to the generic
/// page audit; `ad-templates verify` routes to the verifier.
///
/// # Errors
///
/// Returns a user-facing string when no URL or subcommand is provided, or when
/// the underlying command fails.
pub(crate) fn run_audit(args: &AuditArgs) -> Result<(), String> {
    match &args.command {
        Some(AuditSubcommand::Page(page_args)) => page::run_page(page_args),
        Some(AuditSubcommand::AdTemplates(AuditAdTemplatesCommand::Verify(verify_args))) => {
            ad_templates::run_verify(verify_args)
        }
        Some(AuditSubcommand::Generate(generate_args)) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let collector = generate::browser_collector::BrowserAuditCollector;
            generate::run_generate(generate_args, &collector, &mut out)
        }
        None => match &args.legacy_url {
            Some(url) => page::run_page_url(url, false),
            None => Err("provide a URL or a subcommand (`page`, `ad-templates`)".to_string()),
        },
    }
}
