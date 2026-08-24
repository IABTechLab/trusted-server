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
use crate::commands::audit::collector::{BrowserOpts, GenerateBrowserOpts};
use crate::commands::audit::page::PageAuditArgs;
use crate::error::{CliResult, cli_error};
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
    ///
    /// The hidden flags below all `requires` this positional, so putting one
    /// before a subcommand (`ts audit --chrome X generate <url>`) is rejected
    /// rather than silently dropped. `value_name` keeps that rejection from
    /// naming the field: an operator told to supply `<LEGACY_URL>` cannot find
    /// it in `--help`, because the alias is deliberately undocumented.
    #[arg(value_parser = parse_http_url, hide = true, value_name = "URL")]
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
    #[command(flatten)]
    pub(crate) browser: LegacyBrowserOpts,
}

/// Hidden browser flags retained for the legacy `ts audit <url>` form.
#[derive(Debug, Args)]
pub(crate) struct LegacyBrowserOpts {
    /// Path to the Chrome/Chromium executable.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) chrome: Option<std::path::PathBuf>,
    /// Run a visible browser instead of Chrome's new headless mode.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) headful: bool,
    /// Do not answer the standard IAB consent APIs for the fresh audit profile.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) no_assume_consent: bool,
    /// Route the browser through this proxy.
    #[arg(long, value_name = "HOST:PORT", hide = true, requires = "legacy_url")]
    pub(crate) browser_proxy: Option<String>,
    /// Quiet window in milliseconds that marks the page settled.
    #[arg(
        long,
        default_value_t = crate::commands::audit::collector::GENERATE_SETTLE_QUIET_MS,
        hide = true,
        requires = "legacy_url"
    )]
    pub(crate) settle_quiet_ms: u64,
    /// Hard cap in milliseconds on waiting for the page to settle.
    #[arg(
        long,
        default_value_t = crate::commands::audit::collector::GENERATE_SETTLE_MAX_MS,
        hide = true,
        requires = "legacy_url"
    )]
    pub(crate) settle_max_ms: u64,
    /// Navigate to origins whose TLS certificate does not validate.
    #[arg(long, hide = true, requires = "legacy_url")]
    pub(crate) danger_accept_invalid_certs: bool,
}

impl Default for LegacyBrowserOpts {
    fn default() -> Self {
        Self {
            chrome: None,
            headful: false,
            no_assume_consent: false,
            browser_proxy: None,
            settle_quiet_ms: crate::commands::audit::collector::GENERATE_SETTLE_QUIET_MS,
            settle_max_ms: crate::commands::audit::collector::GENERATE_SETTLE_MAX_MS,
            danger_accept_invalid_certs: false,
        }
    }
}

impl From<&LegacyBrowserOpts> for GenerateBrowserOpts {
    fn from(options: &LegacyBrowserOpts) -> Self {
        Self {
            chrome: options.chrome.clone(),
            headful: options.headful,
            no_assume_consent: options.no_assume_consent,
            browser_proxy: options.browser_proxy.clone(),
            settle_quiet_ms: options.settle_quiet_ms,
            settle_max_ms: options.settle_max_ms,
            danger_accept_invalid_certs: options.danger_accept_invalid_certs,
        }
    }
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
    /// Perform a deterministic scroll pass after each page initially settles.
    #[arg(long)]
    pub scroll: bool,
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
    /// Browser and consent options shared with `ts audit generate`.
    #[command(flatten)]
    pub browser: GenerateBrowserOpts,
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
            let app_config_path = crate::app_config::resolve_app_config_file(&gen_args.config)?;
            let raw_config = std::fs::read_to_string(&app_config_path).map_err(|error| {
                format!("failed to read {}: {error}", app_config_path.display())
            })?;
            let existing_creative = creative_config(&raw_config, &app_config_path)?;
            let profiles = gen_args.profiles()?;
            let collectors: Vec<generate::browser_collector::BrowserAuditCollector> = profiles
                .iter()
                .map(|profile| {
                    generate::browser_collector::BrowserAuditCollector::with_profile(*profile)
                        .with_page_delay(std::time::Duration::from_millis(gen_args.page_delay_ms))
                        .with_browser_options(&gen_args.browser)
                        .with_scroll(gen_args.scroll)
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
            let stderr = std::io::stderr();
            let mut err = stderr.lock();
            generate::run_update_slots(
                &generate::UpdateSlotsRequest {
                    url: gen_args.url.as_str(),
                    config_path: &app_config_path,
                    existing_creative: existing_creative.as_ref(),
                    page_patterns: &gen_args.page_patterns,
                    replace: gen_args.replace,
                    cookies: &gen_args.cookies,
                    dry_run: gen_args.dry_run,
                    budget: gen_args.budget(),
                },
                &selected,
                &mut out,
                &mut err,
            )
            .map(|()| RunOutcome::Success)
        }
        Some(AuditSubcommand::AdTemplates(AuditAdTemplatesCommand::Verify(verify_args))) => {
            ad_templates::run_verify(verify_args)
        }
        Some(AuditSubcommand::Generate(generate_args)) => {
            generate_args.browser.validate()?;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let collector = generate::browser_collector::BrowserAuditCollector::default()
                .with_browser_options(&generate_args.browser);
            generate::run_generate(generate_args, &collector, &mut out)
                .map(|()| RunOutcome::Success)
        }
        None => match args.legacy_url.as_ref() {
            Some(url) => {
                let generate_args = legacy_generate_args(args, url);
                generate_args.browser.validate()?;
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                let collector = generate::browser_collector::BrowserAuditCollector::default()
                    .with_browser_options(&generate_args.browser);
                generate::run_generate(&generate_args, &collector, &mut out)
                    .map(|()| RunOutcome::Success)
            }
            None => Err(
                "provide a URL or a subcommand (`generate`, `page`, `ad-templates`)".to_string(),
            ),
        },
    }
}

/// Reads the config's `[creative_opportunities]` section, when it has one.
///
/// An unrelated invalid setting elsewhere in the document must not hide the
/// section — the runtime rejects such a file, but the operator still has to be
/// able to update slots in it — so the document is read as plain TOML rather
/// than through [`Settings`](trusted_server_core::settings::Settings).
///
/// A section that is present but unreadable is *not* treated as absent.
/// `CreativeOpportunitiesConfig` uses `deny_unknown_fields`, so one mistyped key
/// would otherwise leave the merge with nothing to merge into and replace the
/// operator's entire slot array.
///
/// # Errors
///
/// Returns a user-facing error when the document is malformed or the section is
/// present but cannot be deserialized.
fn creative_config(
    document: &str,
    path: &std::path::Path,
) -> CliResult<Option<trusted_server_core::creative_opportunities::CreativeOpportunitiesConfig>> {
    // Plain `format!`, not `report_error`: the top-level `[ts]` printer already
    // logs whatever is returned here, and this message embeds a multi-line
    // `toml::de::Error`, so logging it here too would print the whole block
    // twice. The guidance leads so the parse error can trail unbroken.
    let value = toml::from_str::<toml::Value>(document).map_err(|error| {
        format!(
            "failed to parse {} before generating slots; fix the TOML syntax and re-run:\n{error}",
            path.display()
        )
    })?;
    let Some(section) = value.get("creative_opportunities").cloned() else {
        return Ok(None);
    };
    match section.try_into() {
        Ok(config) => Ok(Some(config)),
        Err(error) => cli_error(format!(
            "failed to read the existing `[creative_opportunities]` section, so generating \
             slots would discard the configured ones: {error}. Fix the section (or delete it) \
             and re-run"
        )),
    }
}

fn legacy_generate_args(args: &AuditArgs, url: &url::Url) -> generate::GenerateArgs {
    generate::GenerateArgs {
        url: url.to_string(),
        js_assets: args.legacy_generate.js_assets.clone(),
        config: args.legacy_generate.config.clone(),
        no_js_assets: args.legacy_generate.no_js_assets,
        no_config: args.legacy_generate.no_config,
        force: args.legacy_generate.force,
        cookies: args.legacy_generate.cookies.clone(),
        browser: GenerateBrowserOpts::from(&args.legacy_generate.browser),
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
    fn invalid_setting_outside_the_section_still_yields_creative_config() {
        let document = "unknown_runtime_key = true\n\
            [creative_opportunities]\ngam_network_id = \"123\"\n";

        let creative = creative_config(document, std::path::Path::new("trusted-server.toml"))
            .expect("an unrelated invalid setting must not hide creative config")
            .expect("the section is present");

        assert_eq!(creative.gam_network_id, "123");
    }

    #[test]
    fn absent_section_reads_as_absent() {
        let creative = creative_config(
            "[auction]\nenabled = true\n",
            std::path::Path::new("trusted-server.toml"),
        )
        .expect("should read the document");

        assert!(
            creative.is_none(),
            "a document with no `[creative_opportunities]` has no configured slots"
        );
    }

    #[test]
    fn malformed_document_is_rejected_before_creative_config_extraction() {
        let error = creative_config(
            "[creative_opportunities\ngam_network_id = \"123\"\n",
            std::path::Path::new("/tmp/example/trusted-server.toml"),
        )
        .expect_err("should reject malformed TOML");

        assert!(
            error.contains("failed to parse /tmp/example/trusted-server.toml"),
            "error should name the config file it could not parse, got {error}"
        );
        assert!(
            error.contains("fix the TOML syntax and re-run:\n"),
            "the guidance should lead so the multi-line parse error trails it, got {error}"
        );
    }

    #[test]
    fn unreadable_section_is_refused_rather_than_read_as_absent() {
        // `deny_unknown_fields` makes one mistyped key inside the section fail
        // to deserialize. Reading that as "no slots configured" would let a
        // merge replace the operator's entire slot array.
        let document = "[creative_opportunities]\n\
            gam_network_id = \"123\"\n\
            gam_netwrok_id = \"123\"\n\
            [[creative_opportunities.slot]]\n\
            id = \"header\"\n\
            div_id = \"ad-header\"\n\
            page_patterns = [\"/\"]\n\
            formats = [{ width = 728, height = 90 }]\n";

        let error = creative_config(document, std::path::Path::new("trusted-server.toml"))
            .expect_err("should refuse an unreadable section");

        assert!(
            error.contains("would discard the configured ones"),
            "error should say what merging would cost, got {error}"
        );
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
                browser: LegacyBrowserOpts {
                    headful: true,
                    ..LegacyBrowserOpts::default()
                },
            },
        };

        let generate = legacy_generate_args(
            &args,
            args.legacy_url.as_ref().expect("should have legacy URL"),
        );

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
        assert!(
            generate.browser.headful,
            "browser flags passed to the legacy form should reach generation"
        );
    }
}
