//! Browser-backed `ts audit` command namespace.
//!
//! `ts audit page <url>` is the generic page audit; `ts audit ad-templates verify
//! <url>...` is the ad-template verifier; `ts audit generate <url>` bootstraps a
//! draft config from a live page (issue #800). `ts audit <url>` is a hidden
//! compatibility alias for `ts audit generate <url>`.

pub mod ad_templates;
pub mod browser;
pub mod collector;
pub mod generate;
pub mod page;

use clap::{Args, Subcommand};

use crate::app_config::AppConfigArgs;
use crate::commands::audit::collector::BrowserOpts;
use crate::commands::audit::page::PageAuditArgs;
use crate::run::RunOutcome;

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
#[command(arg_required_else_help = true)]
pub(crate) struct AuditArgs {
    #[command(subcommand)]
    pub(crate) command: Option<AuditSubcommand>,
    /// Hidden compatibility alias: `ts audit <url>` behaves like `ts audit generate <url>`.
    #[arg(value_parser = parse_http_url, hide = true)]
    pub(crate) legacy_url: Option<url::Url>,
    #[command(flatten)]
    pub(crate) legacy_generate: LegacyGenerateArgs,
}

/// Hidden generation flags retained for the legacy `ts audit <url>` form.
#[derive(Debug, Default, Args)]
pub(crate) struct LegacyGenerateArgs {
    /// JavaScript asset audit output path.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) js_assets: Option<std::path::PathBuf>,
    /// Draft Trusted Server config output path.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) config: Option<std::path::PathBuf>,
    /// Do not write the JavaScript asset audit file.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) no_js_assets: bool,
    /// Do not write the draft Trusted Server config file.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) no_config: bool,
    /// Overwrite existing output files.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) force: bool,
    /// Cookie to send with the page request, as `name=value`. Repeatable.
    #[arg(
        long = "cookie",
        value_name = "NAME=VALUE",
        value_parser = parse_cookie,
        hide = true,
        requires = "legacy_url"
    )]
    pub(crate) cookies: Vec<(String, String)>,
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
    /// Maximum site sections to sample. Each contributes a landing page and an
    /// article, so this bounds how much of the publisher's taxonomy is covered.
    #[arg(long, default_value_t = 8)]
    pub max_sections: usize,
    /// Maximum pages to load in total, including the requested page.
    ///
    /// Set to 1 to restore single-page behavior: no crawl, no section
    /// discovery, and the audited path as the only page pattern.
    #[arg(long, default_value_t = 17)]
    pub max_pages: usize,
    /// Device profiles to audit, comma-separated: `desktop`, `mobile`.
    ///
    /// Defaults to `desktop`. Publishers often serve different GAM ad units per
    /// device, which a single-profile crawl cannot see — it would infer a
    /// template correct for the profile it used and silently wrong elsewhere.
    /// Passing both crawls each page twice and refuses to write an ad-unit path
    /// for any slot where the profiles disagree.
    #[arg(long, value_delimiter = ',', default_value = "desktop")]
    pub profiles: Vec<String>,
    /// Pause in milliseconds between page loads during the crawl.
    ///
    /// A crawl issues a dozen navigations in a row. Firing them back to back is
    /// discourteous to the origin, and request pacing is one of the signals bot
    /// protection scores, so an unpaced crawl can trigger the challenge that
    /// empties the rest of the run.
    #[arg(long, default_value_t = 750)]
    pub page_delay_ms: u64,
    #[command(flatten)]
    pub browser: BrowserOpts,
}

impl AuditAdTemplatesGenerateArgs {
    /// The crawl bounds these arguments describe.
    pub(crate) fn budget(&self) -> generate::CrawlBudget {
        generate::CrawlBudget {
            max_sections: self.max_sections,
            max_pages: self.max_pages,
        }
    }

    /// The device profiles to audit, deduplicated in the order given.
    ///
    /// # Errors
    ///
    /// Returns an error when a name is not a known profile, or when none were
    /// given.
    pub(crate) fn profiles(&self) -> Result<Vec<generate::DeviceProfile>, String> {
        let mut profiles: Vec<generate::DeviceProfile> = Vec::new();
        for raw in &self.profiles {
            let profile = generate::DeviceProfile::parse(raw)?;
            if !profiles.contains(&profile) {
                profiles.push(profile);
            }
        }
        if profiles.is_empty() {
            return Err("--profiles needs at least one of: desktop, mobile".to_string());
        }
        Ok(profiles)
    }
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
    /// Accept evidence from a page that redirected to a different origin.
    ///
    /// Off by default: slots are matched on the post-redirect path, so an
    /// off-origin page could otherwise satisfy `--strict`. Enable only for a
    /// known redirect between your own properties (e.g. apex to `www`).
    #[arg(long)]
    pub allow_cross_origin_redirect: bool,
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
/// `legacy_url` (if present) routes to artifact generation, while the `page`
/// subcommand routes to the generic read-only page audit.
///
/// # Errors
///
/// Returns a user-facing string when no URL or subcommand is provided, or when
/// the underlying command fails.
pub(crate) fn run_audit(args: &AuditArgs) -> Result<RunOutcome, String> {
    match &args.command {
        Some(AuditSubcommand::Page(page_args)) => {
            page::run_page(page_args).map(|()| RunOutcome::Success)
        }
        Some(AuditSubcommand::AdTemplates(AuditAdTemplatesCommand::Generate(gen_args))) => {
            gen_args.browser.validate()?;
            let loaded = crate::app_config::load_file_settings(&gen_args.config)?;
            let profiles = gen_args.profiles()?;
            let collectors: Vec<generate::browser_collector::BrowserAuditCollector> = profiles
                .iter()
                .map(|profile| {
                    generate::browser_collector::BrowserAuditCollector::with_profile(*profile)
                        .with_page_delay(std::time::Duration::from_millis(gen_args.page_delay_ms))
                        .with_browser_options(&gen_args.browser)
                })
                .collect();
            let selected: Vec<(&str, &dyn generate::collector::AuditCollector)> = profiles
                .iter()
                .zip(collectors.iter())
                .map(|(profile, collector)| {
                    (
                        profile.label(),
                        collector as &dyn generate::collector::AuditCollector,
                    )
                })
                .collect();
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            generate::run_update_slots(
                &generate::UpdateSlotsRequest {
                    url: gen_args.url.as_str(),
                    config_path: &loaded.app_config_path,
                    existing_creative: loaded.settings.creative_opportunities.as_ref(),
                    page_patterns: &gen_args.page_patterns,
                    replace: gen_args.replace,
                    cookies: &gen_args.cookies,
                    dry_run: gen_args.dry_run,
                    budget: gen_args.budget(),
                },
                &selected,
                &mut out,
            )
            .map(|()| RunOutcome::Success)
        }
        Some(AuditSubcommand::AdTemplates(AuditAdTemplatesCommand::Verify(verify_args))) => {
            ad_templates::run_verify(verify_args)
        }
        Some(AuditSubcommand::Generate(generate_args)) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let collector = generate::browser_collector::BrowserAuditCollector::default();
            generate::run_generate(generate_args, &collector, &mut out)
                .map(|()| RunOutcome::Success)
        }
        None => match &args.legacy_url {
            Some(_) => {
                let generate_args = legacy_generate_args(args)
                    .expect("should build generation args when legacy URL is present");
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                let collector = generate::browser_collector::BrowserAuditCollector::default();
                generate::run_generate(&generate_args, &collector, &mut out)
                    .map(|()| RunOutcome::Success)
            }
            None => Err("provide a URL or a subcommand (`page`, `ad-templates`)".to_string()),
        },
    }
}

fn legacy_generate_args(args: &AuditArgs) -> Option<generate::GenerateArgs> {
    let url = args.legacy_url.as_ref()?;
    Some(generate::GenerateArgs {
        url: url.to_string(),
        js_assets: args.legacy_generate.js_assets.clone(),
        config: args.legacy_generate.config.clone(),
        no_js_assets: args.legacy_generate.no_js_assets,
        no_config: args.legacy_generate.no_config,
        force: args.legacy_generate.force,
        cookies: args.legacy_generate.cookies.clone(),
    })
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

    #[test]
    fn legacy_url_builds_artifact_generation_args() {
        let args = AuditArgs {
            command: None,
            legacy_url: Some(
                url::Url::parse("https://www.example.com/").expect("should parse URL"),
            ),
            legacy_generate: LegacyGenerateArgs {
                js_assets: Some("audit/assets.toml".into()),
                config: Some("audit/config.toml".into()),
                no_js_assets: false,
                no_config: false,
                force: true,
                cookies: vec![("session".to_string(), "example".to_string())],
            },
        };

        let generate = legacy_generate_args(&args).expect("should build generation args");

        assert_eq!(generate.url, "https://www.example.com/");
        assert_eq!(
            generate.js_assets.as_deref(),
            Some(std::path::Path::new("audit/assets.toml"))
        );
        assert_eq!(
            generate.config.as_deref(),
            Some(std::path::Path::new("audit/config.toml"))
        );
        assert!(generate.force);
        assert_eq!(
            generate.cookies,
            [("session".to_string(), "example".to_string())]
        );
    }
}
