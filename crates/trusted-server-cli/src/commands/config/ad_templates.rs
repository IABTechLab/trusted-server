use std::collections::BTreeSet;
use std::io::{self, Write};

use crate::ad_templates::expected::normalize_path_or_url;
use crate::ad_templates::output::escape_terminal_text;
use crate::app_config::{AppConfigArgs, load_settings};
use clap::{ArgGroup, Args, Subcommand};
use http::Method;
use trusted_server_core::auction::types::MediaType;
use trusted_server_core::creative_opportunities::{
    AdStackGateInput, AdStackGateName, CreativeOpportunityFormat, CreativeOpportunitySlot,
    RuntimeAdStackExpected, evaluate_ad_stack_gate, match_slots, validate_page_pattern,
};

use crate::run::RunOutcome;

enum CheckFailure {
    Tool(String),
    Assertion(String),
}

#[derive(Debug, Subcommand)]
pub enum AdTemplatesCommand {
    /// Validate ad-template config and summarize deploy-time implications.
    Lint(AdTemplatesLintArgs),
    /// Show creative opportunity slots matching a page path or URL.
    Match(AdTemplatesMatchArgs),
    /// Assert that a page path or URL matches the expected slot set.
    Check(AdTemplatesCheckArgs),
    /// Explain why a page path or URL would or would not run the ad stack.
    Explain(AdTemplatesExplainArgs),
}

#[derive(Debug, Args)]
pub struct AdTemplatesLintArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
}

#[derive(Debug, Args)]
pub struct AdTemplatesMatchArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// Page path or full URL to evaluate.
    pub path_or_url: String,
    /// Include slot div, GAM path, formats, and providers.
    #[arg(long)]
    pub details: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("expectation")
        .required(true)
        .args(["expected_slots", "expect_no_slots"])
))]
pub struct AdTemplatesCheckArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// Page path or full URL to evaluate.
    pub path_or_url: String,
    /// Expected slot id. Repeat for multiple slots.
    #[arg(long = "expected-slot", value_name = "ID")]
    pub expected_slots: Vec<String>,
    /// Assert that no slots match the page path or URL.
    #[arg(long)]
    pub expect_no_slots: bool,
    /// Allow additional matched slots beyond --expected-slot values.
    #[arg(long, conflicts_with = "expect_no_slots")]
    pub allow_extra_slots: bool,
}

#[derive(Debug, Args)]
pub struct AdTemplatesExplainArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// Page path or full URL to evaluate.
    pub path_or_url: String,
    /// HTTP method to model.
    #[arg(long, default_value = "GET", value_parser = parse_http_method)]
    pub method: Method,
    /// Model a non-navigation request.
    #[arg(long)]
    pub non_navigation: bool,
    /// Model a prefetch request.
    #[arg(long)]
    pub prefetch: bool,
    /// Model a known crawler user agent.
    #[arg(long)]
    pub bot: bool,
    /// Model consent denying server-side auction.
    #[arg(long)]
    pub consent_denied: bool,
}

fn parse_http_method(raw: &str) -> Result<Method, String> {
    let normalized = raw.to_ascii_uppercase();
    Method::from_bytes(normalized.as_bytes())
        .map_err(|error| format!("invalid HTTP method `{raw}`: {error}"))
}

/// Run an ad-template CLI command.
///
/// # Errors
///
/// Returns a user-facing string when config loading, matching, or assertion
/// checks fail.
pub fn run_ad_templates(args: &AdTemplatesCommand) -> Result<RunOutcome, String> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let AdTemplatesCommand::Check(args) = args {
        return match run_check_classified(args, &mut out) {
            Ok(()) => Ok(RunOutcome::Success),
            Err(CheckFailure::Tool(error)) => Err(error),
            Err(CheckFailure::Assertion(message)) => {
                let stderr = io::stderr();
                let mut err = stderr.lock();
                writeln!(err, "{message}").map_err(output_error)?;
                Ok(RunOutcome::AssertionFailed)
            }
        };
    }
    run_ad_templates_with_writer(args, &mut out).map(|()| RunOutcome::Success)
}

fn run_ad_templates_with_writer(
    args: &AdTemplatesCommand,
    out: &mut dyn Write,
) -> Result<(), String> {
    match args {
        AdTemplatesCommand::Lint(args) => run_lint(args, out),
        AdTemplatesCommand::Match(args) => run_match(args, out),
        AdTemplatesCommand::Check(args) => run_check(args, out),
        AdTemplatesCommand::Explain(args) => run_explain(args, out),
    }
}

fn run_lint(args: &AdTemplatesLintArgs, out: &mut dyn Write) -> Result<(), String> {
    let loaded = load_settings(&args.config)?;
    writeln!(out, "app config: {}", loaded.app_config_path.display()).map_err(output_error)?;

    let Some(config) = &loaded.settings.creative_opportunities else {
        writeln!(out, "server-side ad templates: not configured").map_err(output_error)?;
        return Ok(());
    };

    writeln!(
        out,
        "server-side ad templates: configured ({} slot{})",
        config.slot.len(),
        plural(config.slot.len())
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "gam_network_id: {}",
        escape_terminal_text(&config.gam_network_id)
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "auction_timeout_ms: {}",
        config
            .auction_timeout_ms
            .unwrap_or(loaded.settings.auction.timeout_ms)
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "creative_opportunities.enabled: {}",
        if config.enabled { "true" } else { "false" }
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "auction.enabled: {}",
        if loaded.settings.auction.enabled {
            "true"
        } else {
            "false"
        }
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "auction.providers: {}",
        if loaded.settings.auction.providers.is_empty() {
            "(none)".to_string()
        } else {
            escape_terminal_text(&loaded.settings.auction.providers.join(", ")).into_owned()
        }
    )
    .map_err(output_error)?;

    if config.slot.is_empty() {
        writeln!(out, "status: disabled because no slots are configured").map_err(output_error)?;
    } else if !config.enabled {
        writeln!(
            out,
            "status: slots are configured, but [creative_opportunities].enabled is false"
        )
        .map_err(output_error)?;
    } else if !loaded.settings.auction.enabled {
        writeln!(
            out,
            "status: slots are configured, but [auction].enabled is false"
        )
        .map_err(output_error)?;
    } else if loaded.settings.auction.providers.is_empty() {
        writeln!(
            out,
            "status: slots are configured, but [auction].providers is empty"
        )
        .map_err(output_error)?;
    } else {
        writeln!(out, "status: eligible for legacy-path server-side auctions")
            .map_err(output_error)?;
    }

    for slot in &config.slot {
        for pattern in &slot.page_patterns {
            if let Err(error) = validate_page_pattern(pattern) {
                writeln!(
                    out,
                    "invalid page pattern for slot `{}`: {}",
                    escape_terminal_text(&slot.id),
                    escape_terminal_text(&error),
                )
                .map_err(output_error)?;
            }
        }
    }

    Ok(())
}

fn run_match(args: &AdTemplatesMatchArgs, out: &mut dyn Write) -> Result<(), String> {
    let loaded = load_settings(&args.config)?;
    let path = normalize_path_or_url(&args.path_or_url)?;
    let Some(config) = &loaded.settings.creative_opportunities else {
        writeln!(
            out,
            "{path}: no slots matched (creative_opportunities not configured)"
        )
        .map_err(output_error)?;
        return Ok(());
    };
    let matched = match_slots(&config.slot, &path);

    write_match_result(
        out,
        &path,
        &matched,
        &config.gam_network_id,
        &config.section_for_path(&path),
        args.details,
    )
}

fn run_check(args: &AdTemplatesCheckArgs, out: &mut dyn Write) -> Result<(), String> {
    run_check_classified(args, out).map_err(|failure| match failure {
        CheckFailure::Tool(error) | CheckFailure::Assertion(error) => error,
    })
}

fn run_check_classified(
    args: &AdTemplatesCheckArgs,
    out: &mut dyn Write,
) -> Result<(), CheckFailure> {
    let loaded = load_settings(&args.config).map_err(CheckFailure::Tool)?;
    let path = normalize_path_or_url(&args.path_or_url).map_err(CheckFailure::Tool)?;
    let matched = loaded
        .settings
        .creative_opportunities
        .as_ref()
        .map(|config| match_slots(&config.slot, &path))
        .unwrap_or_default();
    let actual: BTreeSet<&str> = matched.iter().map(|slot| slot.id.as_str()).collect();

    if args.expect_no_slots {
        if actual.is_empty() {
            writeln!(out, "{path}: OK, no slots matched")
                .map_err(output_error)
                .map_err(CheckFailure::Tool)?;
            return Ok(());
        }
        return Err(CheckFailure::Assertion(format!(
            "{path}: expected no slots, matched {}",
            join_set(&actual)
        )));
    }

    let expected: BTreeSet<&str> = args.expected_slots.iter().map(String::as_str).collect();
    let missing: BTreeSet<&str> = expected.difference(&actual).copied().collect();
    let extra: BTreeSet<&str> = actual.difference(&expected).copied().collect();

    if missing.is_empty() && (args.allow_extra_slots || extra.is_empty()) {
        writeln!(out, "{path}: OK, matched {}", join_set(&actual))
            .map_err(output_error)
            .map_err(CheckFailure::Tool)?;
        return Ok(());
    }

    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("missing {}", join_set(&missing)));
    }
    if !args.allow_extra_slots && !extra.is_empty() {
        problems.push(format!("unexpected {}", join_set(&extra)));
    }
    Err(CheckFailure::Assertion(format!(
        "{path}: {}",
        problems.join("; ")
    )))
}

fn run_explain(args: &AdTemplatesExplainArgs, out: &mut dyn Write) -> Result<(), String> {
    let loaded = load_settings(&args.config)?;
    let path = normalize_path_or_url(&args.path_or_url)?;
    writeln!(out, "path: {path}").map_err(output_error)?;

    let has_matches = if let Some(config) = &loaded.settings.creative_opportunities {
        let matched = match_slots(&config.slot, &path);
        write_match_result(
            out,
            &path,
            &matched,
            &config.gam_network_id,
            &config.section_for_path(&path),
            true,
        )?;
        !matched.is_empty()
    } else {
        writeln!(out, "creative_opportunities: not configured").map_err(output_error)?;
        false
    };

    let method_pass = args.method == Method::GET;
    let navigation_pass = !args.non_navigation;
    let consent_pass = !args.consent_denied;
    let auction_enabled = loaded.settings.auction.enabled;
    let ad_templates_enabled = loaded
        .settings
        .creative_opportunities
        .as_ref()
        .is_some_and(|config| config.enabled);
    let providers_configured = !loaded.settings.auction.providers.is_empty();

    let gate = evaluate_ad_stack_gate(AdStackGateInput {
        method_get: method_pass,
        navigation: navigation_pass,
        prefetch: args.prefetch,
        bot: args.bot,
        matched_slots: has_matches,
        consent_allows_auction: Some(consent_pass),
        auction_enabled,
        ad_templates_enabled,
    });
    let blocked: Vec<AdStackGateName> = gate.blocking_gates().collect();
    write_gate(
        out,
        "method GET",
        !blocked.contains(&AdStackGateName::MethodGet),
    )?;
    write_gate(
        out,
        "navigation",
        !blocked.contains(&AdStackGateName::Navigation),
    )?;
    write_gate(
        out,
        "not prefetch",
        !blocked.contains(&AdStackGateName::NotPrefetch),
    )?;
    write_gate(out, "not bot", !blocked.contains(&AdStackGateName::NotBot))?;
    write_gate(
        out,
        "consent allows auction",
        !blocked.contains(&AdStackGateName::ConsentAllowsAuction),
    )?;
    write_gate(
        out,
        "auction.enabled",
        !blocked.contains(&AdStackGateName::AuctionEnabled),
    )?;
    write_gate(
        out,
        "creative_opportunities.enabled",
        !blocked.contains(&AdStackGateName::AdTemplatesEnabled),
    )?;
    write_gate(
        out,
        "matched slots",
        !blocked.contains(&AdStackGateName::MatchedSlots),
    )?;
    writeln!(
        out,
        "advisory auction providers configured: {}",
        if providers_configured { "yes" } else { "no" }
    )
    .map_err(output_error)?;
    writeln!(
        out,
        "server-side ad stack: {}",
        match gate.expected {
            RuntimeAdStackExpected::Yes => "yes",
            RuntimeAdStackExpected::No => "no",
            // `explain` always supplies a consent decision, which is the only
            // input that yields `Unknown`; the arm is here for exhaustiveness.
            RuntimeAdStackExpected::Unknown => "unknown",
        }
    )
    .map_err(output_error)?;

    Ok(())
}

fn write_match_result(
    out: &mut dyn Write,
    path: &str,
    matched: &[&CreativeOpportunitySlot],
    gam_network_id: &str,
    section: &str,
    details: bool,
) -> Result<(), String> {
    if matched.is_empty() {
        writeln!(out, "{}: no slots matched", escape_terminal_text(path)).map_err(output_error)?;
        return Ok(());
    }

    let ids = matched
        .iter()
        .map(|slot| escape_terminal_text(&slot.id).into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "{}: matched {ids}", escape_terminal_text(path)).map_err(output_error)?;

    if details {
        for slot in matched {
            writeln!(out, "- {}", format_slot(slot, gam_network_id, section))
                .map_err(output_error)?;
        }
    }

    Ok(())
}

fn write_gate(out: &mut dyn Write, label: &str, pass: bool) -> Result<(), String> {
    writeln!(out, "gate {label}: {}", if pass { "pass" } else { "block" }).map_err(output_error)
}

/// Formats one matched slot for `--details` output.
///
/// `section` is the value the runtime derives from the evaluated path, so a
/// `{section}` template renders the same unit path the live request would use.
fn format_slot(slot: &CreativeOpportunitySlot, gam_network_id: &str, section: &str) -> String {
    let formats = slot
        .formats
        .iter()
        .map(format_format)
        .collect::<Vec<_>>()
        .join(", ");
    let providers = format_providers(slot);
    // `None` means a dynamic template renders past GAM's unit-path byte limit —
    // a config the runtime rejects, so surface it rather than printing a path.
    let gam_unit_path = slot
        .render_gam_unit_path(gam_network_id, section)
        .unwrap_or_else(|| "<unrenderable: exceeds GAM unit-path byte limit>".to_string());
    format!(
        "{} div={} gam={} patterns=[{}] formats=[{}] providers=[{}]",
        escape_terminal_text(&slot.id),
        escape_terminal_text(slot.resolved_div_id()),
        escape_terminal_text(&gam_unit_path),
        escape_terminal_text(&slot.page_patterns.join(", ")),
        formats,
        providers,
    )
}

fn format_format(format: &CreativeOpportunityFormat) -> String {
    let media_type = match format.media_type {
        MediaType::Banner => "banner",
        MediaType::Video => "video",
        MediaType::Native => "native",
    };
    format!("{}x{} {media_type}", format.width, format.height)
}

fn format_providers(slot: &CreativeOpportunitySlot) -> String {
    let mut providers = Vec::new();
    if slot.providers.aps.is_some() {
        providers.push("aps");
    }
    if slot.providers.prebid.is_some() {
        providers.push("prebid");
    }
    if providers.is_empty() {
        return "none".to_string();
    }
    providers.join(", ")
}

/// Renders a set of config-derived slot ids for the terminal.
///
/// Config can arrive from a pushed blob or the env overlay, not only from a file
/// the operator read, so the ids are escaped before they reach a terminal — the
/// assertion-failure path prints them too.
fn join_set(set: &BTreeSet<&str>) -> String {
    if set.is_empty() {
        return "(none)".to_string();
    }
    set.iter()
        .map(|id| escape_terminal_text(id).into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "used as a map_err fn that receives io::Error by value"
)]
fn output_error(err: io::Error) -> String {
    format!("failed to write command output: {err}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const EXAMPLE_CONFIG: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    fn project_with_config(config: &str) -> (TempDir, AppConfigArgs) {
        let temp = TempDir::new().expect("should create temp dir");
        let manifest_path = temp.path().join("edgezero.toml");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(&manifest_path, "[app]\nname = \"trusted-server\"\n")
            .expect("should write manifest");
        fs::write(&config_path, config).expect("should write app config");
        (
            temp,
            AppConfigArgs {
                app_config: Some(config_path),
                manifest: manifest_path,
                no_env: true,
            },
        )
    }

    fn config_with_slots() -> String {
        let base_config = EXAMPLE_CONFIG
            .replace(
                "replace-with-admin-password-32-bytes",
                "test-admin-password-32-bytes-minimum",
            )
            .replace(
                "trusted-server-placeholder-secret",
                "test-ec-passphrase-32-bytes-minimum",
            )
            .replace(
                "change-me-proxy-secret",
                "test-proxy-secret-32-bytes-minimum",
            );
        format!(
            "{base_config}\n\
             [[creative_opportunities.slot]]\n\
             id = \"atf\"\n\
             page_patterns = [\"/news/*\", \"/\"]\n\
             formats = [{{ width = 300, height = 250 }}]\n\
             targeting = {{ zone = \"atf\" }}\n\
             [creative_opportunities.slot.providers.prebid]\n\
             bidders = {{}}\n\
             \n\
             [[creative_opportunities.slot]]\n\
             id = \"sports-sidebar\"\n\
             div_id = \"sports-ad\"\n\
             page_patterns = [\"/sports/*\"]\n\
             formats = [{{ width = 300, height = 600 }}]\n"
        )
    }

    #[test]
    fn match_reports_slots_for_path() {
        let (_temp, config) = project_with_config(&config_with_slots());
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Match(AdTemplatesMatchArgs {
                config,
                path_or_url: "https://example.com/news/story?utm=1".to_string(),
                details: true,
            }),
            &mut out,
        )
        .expect("should match slots");

        let output = String::from_utf8(out).expect("should be utf8");
        assert!(
            output.contains("/news/story: matched atf"),
            "should report matched slot"
        );
        assert!(
            output.contains("formats=[300x250 banner]"),
            "should include details"
        );
    }

    #[test]
    fn check_rejects_unexpected_extra_slots_by_default() {
        let (_temp, config) = project_with_config(&config_with_slots());

        let err = run_ad_templates_with_writer(
            &AdTemplatesCommand::Check(AdTemplatesCheckArgs {
                config,
                path_or_url: "/sports/game".to_string(),
                expected_slots: vec!["atf".to_string()],
                expect_no_slots: false,
                allow_extra_slots: false,
            }),
            &mut Vec::new(),
        )
        .expect_err("should reject mismatch");

        assert!(
            err.contains("missing atf") && err.contains("unexpected sports-sidebar"),
            "should describe missing and unexpected slots"
        );
    }

    #[test]
    fn check_accepts_no_slots() {
        let (_temp, config) = project_with_config(&config_with_slots());
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Check(AdTemplatesCheckArgs {
                config,
                path_or_url: "/weather/today".to_string(),
                expected_slots: Vec::new(),
                expect_no_slots: true,
                allow_extra_slots: false,
            }),
            &mut out,
        )
        .expect("should accept no slots");

        let output = String::from_utf8(out).expect("should be utf8");
        assert!(
            output.contains("/weather/today: OK, no slots matched"),
            "should report no-slot assertion"
        );
    }

    #[test]
    fn explain_keeps_provider_state_separate_from_runtime_verdict() {
        let config_text =
            config_with_slots().replace("[auction]\nenabled = false", "[auction]\nenabled = true");
        let (_temp, config) = project_with_config(&config_text);
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Explain(AdTemplatesExplainArgs {
                config,
                path_or_url: "/news/story".to_string(),
                method: Method::GET,
                non_navigation: false,
                prefetch: false,
                bot: false,
                consent_denied: false,
            }),
            &mut out,
        )
        .expect("should explain path");

        let output = String::from_utf8(out).expect("should be utf8");
        assert!(
            output.contains("server-side ad stack: yes"),
            "runtime verdict should not include provider configuration"
        );
        assert!(
            output.contains("advisory auction providers configured: no"),
            "provider state should be a separate advisory"
        );
    }

    #[test]
    fn lint_reports_configured_slot_count_and_auction_state() {
        let (_temp, config) = project_with_config(&config_with_slots());
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Lint(AdTemplatesLintArgs { config }),
            &mut out,
        )
        .expect("should lint configured slots");

        let output = String::from_utf8(out).expect("should be utf8");
        assert!(
            output.contains("server-side ad templates: configured (2 slots)"),
            "should report the configured slot count"
        );
        assert!(
            output.contains("auction.enabled:"),
            "should report the auction kill-switch state"
        );
        assert!(!output.contains("legacy fallback"));
    }

    #[test]
    fn lint_and_explain_report_the_disabled_template_switch() {
        // `[creative_opportunities].enabled = false` is a runtime kill switch:
        // the publisher path matches no slots at all while it is off, so the
        // diagnostics must not claim the ad stack would run.
        let config_text = config_with_slots().replace("enabled = true", "enabled = false");
        let (_temp, config) = project_with_config(&config_text);
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Lint(AdTemplatesLintArgs {
                config: config.clone(),
            }),
            &mut out,
        )
        .expect("should lint a disabled template switch");
        let lint_output = String::from_utf8(out).expect("should be utf8");

        assert!(
            lint_output.contains("creative_opportunities.enabled: false"),
            "lint should report the template switch state: {lint_output}"
        );
        assert!(
            lint_output.contains(
                "status: slots are configured, but [creative_opportunities].enabled is false"
            ),
            "lint status should name the template switch: {lint_output}"
        );

        let mut out = Vec::new();
        run_ad_templates_with_writer(
            &AdTemplatesCommand::Explain(AdTemplatesExplainArgs {
                config,
                path_or_url: "/news/story".to_string(),
                method: Method::GET,
                non_navigation: false,
                prefetch: false,
                bot: false,
                consent_denied: false,
            }),
            &mut out,
        )
        .expect("should explain a disabled template switch");
        let explain_output = String::from_utf8(out).expect("should be utf8");

        assert!(
            explain_output.contains("gate creative_opportunities.enabled: block"),
            "explain should fail the template-switch gate: {explain_output}"
        );
        assert!(
            explain_output.contains("server-side ad stack: no"),
            "explain verdict should follow the switch: {explain_output}"
        );
    }

    #[test]
    fn lint_reports_page_patterns_the_runtime_drops() {
        let config_text = config_with_slots().replace(
            "page_patterns = [\"/news/*\", \"/\"]",
            "page_patterns = [\"/news/*\", \"[\"]",
        );
        let (_temp, config) = project_with_config(&config_text);
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Lint(AdTemplatesLintArgs { config }),
            &mut out,
        )
        .expect("should lint mixed valid and invalid patterns");
        let output = String::from_utf8(out).expect("should be utf8");

        assert!(
            output.contains("invalid page pattern for slot `atf`")
                && output.contains("page pattern '[' is not a valid glob"),
            "lint should surface the runtime-dropped pattern: {output}"
        );
    }

    #[test]
    fn public_check_reports_drift_as_assertion_outcome() {
        let (_temp, config) = project_with_config(&config_with_slots());

        let outcome = run_ad_templates(&AdTemplatesCommand::Check(AdTemplatesCheckArgs {
            config,
            path_or_url: "/sports/game".to_string(),
            expected_slots: vec!["atf".to_string()],
            expect_no_slots: false,
            allow_extra_slots: false,
        }))
        .expect("assertion drift should not be a tool error");

        assert_eq!(outcome, RunOutcome::AssertionFailed);
    }

    #[test]
    fn http_method_parser_normalizes_standard_methods() {
        assert_eq!(
            parse_http_method("get").expect("should parse lowercase GET"),
            Method::GET,
            "lowercase GET must evaluate the same runtime gate as uppercase GET"
        );
    }
}
