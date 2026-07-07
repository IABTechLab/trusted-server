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
use crate::commands::audit::collector::BrowserOpts;
use crate::commands::audit::page::PageAuditArgs;

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

/// Parses a `name=value` cookie argument into its `(name, value)` parts.
///
/// Splits on the first `=` so cookie values may themselves contain `=`. The name
/// must be non-empty; the value may be empty.
///
/// # Errors
///
/// Returns a user-facing string when the input has no `=` or an empty name.
pub(crate) fn parse_cookie(raw: &str) -> Result<(String, String), String> {
    let (name, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("invalid cookie `{raw}` (expected NAME=VALUE)"))?;
    if name.is_empty() {
        return Err(format!("invalid cookie `{raw}` (empty name)"));
    }
    Ok((name.to_string(), value.to_string()))
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
    /// Scrape a live page's GPT slots and update the config's
    /// `[creative_opportunities]` slots in place.
    Generate(AuditAdTemplatesGenerateArgs),
    /// Verify ad-template slots for one or more live URLs.
    Verify(AuditAdTemplatesVerifyArgs),
}

/// Arguments for `ts audit ad-templates generate <url>`.
#[derive(Debug, Args)]
pub(crate) struct AuditAdTemplatesGenerateArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// Page URL to scrape for GPT slots (http or https).
    #[arg(value_parser = parse_http_url)]
    pub url: url::Url,
    /// Glob applied to every slot discovered this run (e.g. `/`, `/news/*`).
    /// Repeatable. Defaults to the scraped URL's path. Re-running with a
    /// different pattern unions it into slots already in the config.
    #[arg(long = "page-pattern", value_name = "GLOB")]
    pub page_patterns: Vec<String>,
    /// Replace all existing slots instead of merging this run into them.
    #[arg(long)]
    pub replace: bool,
    /// Preview the updated config on stdout instead of writing it.
    #[arg(long)]
    pub dry_run: bool,
    /// Cookie to send with the page request, as `name=value`. Repeatable.
    /// Use to carry an existing session (e.g. a valid bot-protection clearance
    /// cookie) so the origin serves the real page instead of a challenge.
    #[arg(long = "cookie", value_name = "NAME=VALUE", value_parser = parse_cookie)]
    pub cookies: Vec<(String, String)>,
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
    /// Cookie to send with each page request, as `name=value`. Repeatable.
    /// Use to carry an existing session (e.g. a valid bot-protection clearance
    /// cookie) so the origin serves the real page instead of a challenge.
    #[arg(long = "cookie", value_name = "NAME=VALUE", value_parser = parse_cookie)]
    pub cookies: Vec<(String, String)>,
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
        Some(AuditSubcommand::AdTemplates(AuditAdTemplatesCommand::Generate(gen_args))) => {
            let loaded = crate::app_config::load_settings(&gen_args.config)?;
            let collector = generate::browser_collector::BrowserAuditCollector;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            generate::run_update_slots(
                gen_args.url.as_str(),
                &loaded.app_config_path,
                loaded.settings.creative_opportunities.as_ref(),
                &gen_args.page_patterns,
                gen_args.replace,
                &gen_args.cookies,
                gen_args.dry_run,
                &collector,
                &mut out,
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cookie_splits_on_first_equals() {
        let (name, value) = parse_cookie("datadome=abc=def~ghi").expect("should parse cookie");
        assert_eq!(name, "datadome", "name should be the pre-`=` portion");
        assert_eq!(
            value, "abc=def~ghi",
            "value should keep later `=` characters"
        );
    }

    #[test]
    fn parse_cookie_allows_empty_value() {
        let (name, value) = parse_cookie("session=").expect("should parse empty value");
        assert_eq!(name, "session");
        assert!(value.is_empty(), "empty value should be allowed");
    }

    #[test]
    fn parse_cookie_rejects_missing_equals() {
        let err = parse_cookie("datadome").expect_err("should reject missing `=`");
        assert!(
            err.contains("NAME=VALUE"),
            "error should show expected form"
        );
    }

    #[test]
    fn parse_cookie_rejects_empty_name() {
        let err = parse_cookie("=value").expect_err("should reject empty name");
        assert!(err.contains("empty name"), "error should name the problem");
    }
}
