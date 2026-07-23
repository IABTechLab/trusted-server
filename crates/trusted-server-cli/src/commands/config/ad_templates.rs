use std::collections::BTreeSet;
use std::io::{self, Write};

use crate::ad_templates::expected::normalize_path_or_url;
use crate::app_config::{AppConfigArgs, load_settings};
use clap::{Args, Subcommand};
use trusted_server_core::auction::types::MediaType;
use trusted_server_core::creative_opportunities::{
    AdStackGateInput, CreativeOpportunityFormat, CreativeOpportunitySlot, RuntimeAdStackExpected,
    evaluate_ad_stack_gate, match_slots,
};

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
    #[arg(long)]
    pub allow_extra_slots: bool,
}

#[derive(Debug, Args)]
pub struct AdTemplatesExplainArgs {
    #[command(flatten)]
    pub config: AppConfigArgs,
    /// Page path or full URL to evaluate.
    pub path_or_url: String,
    /// HTTP method to model.
    #[arg(long, default_value = "GET")]
    pub method: String,
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
    /// Model Fastly `edgezero_enabled=true`.
    #[arg(long)]
    pub edgezero_enabled: bool,
}

/// Run an ad-template CLI command.
///
/// # Errors
///
/// Returns a user-facing string when config loading, matching, or assertion
/// checks fail.
pub fn run_ad_templates(args: &AdTemplatesCommand) -> Result<(), String> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    run_ad_templates_with_writer(args, &mut out)
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
    writeln!(out, "gam_network_id: {}", config.gam_network_id).map_err(output_error)?;
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
            loaded.settings.auction.providers.join(", ")
        }
    )
    .map_err(output_error)?;

    if config.slot.is_empty() {
        writeln!(out, "status: disabled because no slots are configured").map_err(output_error)?;
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

    if !config.slot.is_empty() {
        writeln!(
            out,
            "edgezero: configured slots currently require Fastly legacy fallback"
        )
        .map_err(output_error)?;
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

    write_match_result(out, &path, &matched, &config.gam_network_id, args.details)
}

fn run_check(args: &AdTemplatesCheckArgs, out: &mut dyn Write) -> Result<(), String> {
    if args.expect_no_slots && !args.expected_slots.is_empty() {
        return Err("--expect-no-slots cannot be combined with --expected-slot".to_string());
    }
    if !args.expect_no_slots && args.expected_slots.is_empty() {
        return Err("provide --expected-slot at least once or pass --expect-no-slots".to_string());
    }

    let loaded = load_settings(&args.config)?;
    let path = normalize_path_or_url(&args.path_or_url)?;
    let matched = loaded
        .settings
        .creative_opportunities
        .as_ref()
        .map(|config| match_slots(&config.slot, &path))
        .unwrap_or_default();
    let actual: BTreeSet<&str> = matched.iter().map(|slot| slot.id.as_str()).collect();

    if args.expect_no_slots {
        if actual.is_empty() {
            writeln!(out, "{path}: OK, no slots matched").map_err(output_error)?;
            return Ok(());
        }
        return Err(format!(
            "{path}: expected no slots, matched {}",
            join_set(&actual)
        ));
    }

    let expected: BTreeSet<&str> = args.expected_slots.iter().map(String::as_str).collect();
    let missing: BTreeSet<&str> = expected.difference(&actual).copied().collect();
    let extra: BTreeSet<&str> = actual.difference(&expected).copied().collect();

    if missing.is_empty() && (args.allow_extra_slots || extra.is_empty()) {
        writeln!(out, "{path}: OK, matched {}", join_set(&actual)).map_err(output_error)?;
        return Ok(());
    }

    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("missing {}", join_set(&missing)));
    }
    if !args.allow_extra_slots && !extra.is_empty() {
        problems.push(format!("unexpected {}", join_set(&extra)));
    }
    Err(format!("{path}: {}", problems.join("; ")))
}

fn run_explain(args: &AdTemplatesExplainArgs, out: &mut dyn Write) -> Result<(), String> {
    let loaded = load_settings(&args.config)?;
    let path = normalize_path_or_url(&args.path_or_url)?;
    writeln!(out, "path: {path}").map_err(output_error)?;

    let Some(config) = &loaded.settings.creative_opportunities else {
        writeln!(out, "creative_opportunities: not configured").map_err(output_error)?;
        writeln!(out, "server-side ad stack: no").map_err(output_error)?;
        return Ok(());
    };

    let matched = match_slots(&config.slot, &path);
    write_match_result(out, &path, &matched, &config.gam_network_id, true)?;

    let method_pass = args.method.eq_ignore_ascii_case("GET");
    let navigation_pass = !args.non_navigation;
    let prefetch_pass = !args.prefetch;
    let bot_pass = !args.bot;
    let consent_pass = !args.consent_denied;
    let auction_enabled = loaded.settings.auction.enabled;
    let providers_configured = !loaded.settings.auction.providers.is_empty();
    let has_matches = !matched.is_empty();

    write_gate(out, "method GET", method_pass)?;
    write_gate(out, "navigation", navigation_pass)?;
    write_gate(out, "not prefetch", prefetch_pass)?;
    write_gate(out, "not bot", bot_pass)?;
    write_gate(out, "consent allows auction", consent_pass)?;
    write_gate(out, "auction.enabled", auction_enabled)?;
    write_gate(out, "auction providers configured", providers_configured)?;
    write_gate(out, "matched slots", has_matches)?;

    // Share the runtime ad-stack decision with `publisher.rs` so explain cannot
    // drift from the live gate. The "auction providers configured" gate is an
    // explain-only supplementary check the runtime helper intentionally omits.
    let gate = evaluate_ad_stack_gate(AdStackGateInput {
        method_get: method_pass,
        navigation: navigation_pass,
        prefetch: args.prefetch,
        bot: args.bot,
        matched_slots: has_matches,
        consent_allows_auction: Some(consent_pass),
        auction_enabled,
    });
    let runs_ad_stack = gate.expected == RuntimeAdStackExpected::Yes && providers_configured;
    writeln!(
        out,
        "server-side ad stack: {}",
        if runs_ad_stack { "yes" } else { "no" }
    )
    .map_err(output_error)?;

    if args.edgezero_enabled && !config.slot.is_empty() {
        writeln!(
            out,
            "edgezero: configured slots require Fastly legacy fallback until buffered EdgeZero ad-template injection is wired"
        )
        .map_err(output_error)?;
    }

    Ok(())
}

fn write_match_result(
    out: &mut dyn Write,
    path: &str,
    matched: &[&CreativeOpportunitySlot],
    gam_network_id: &str,
    details: bool,
) -> Result<(), String> {
    if matched.is_empty() {
        writeln!(out, "{path}: no slots matched").map_err(output_error)?;
        return Ok(());
    }

    let ids = matched
        .iter()
        .map(|slot| slot.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "{path}: matched {ids}").map_err(output_error)?;

    if details {
        for slot in matched {
            writeln!(out, "- {}", format_slot(slot, gam_network_id)).map_err(output_error)?;
        }
    }

    Ok(())
}

fn write_gate(out: &mut dyn Write, label: &str, pass: bool) -> Result<(), String> {
    writeln!(out, "gate {label}: {}", if pass { "pass" } else { "block" }).map_err(output_error)
}

fn format_slot(slot: &CreativeOpportunitySlot, gam_network_id: &str) -> String {
    let formats = slot
        .formats
        .iter()
        .map(format_format)
        .collect::<Vec<_>>()
        .join(", ");
    let providers = format_providers(slot);
    format!(
        "{} div={} gam={} patterns=[{}] formats=[{}] providers=[{}]",
        slot.id,
        slot.resolved_div_id(),
        slot.resolved_gam_unit_path(gam_network_id),
        slot.page_patterns.join(", "),
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

fn join_set(set: &BTreeSet<&str>) -> String {
    if set.is_empty() {
        return "(none)".to_string();
    }
    set.iter().copied().collect::<Vec<_>>().join(", ")
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
    fn explain_reports_runtime_gates_and_edgezero_fallback() {
        let config_text = config_with_slots()
            .replace("[auction]\nenabled = false", "[auction]\nenabled = true")
            .replace("providers = []", "providers = [\"prebid\"]");
        let (_temp, config) = project_with_config(&config_text);
        let mut out = Vec::new();

        run_ad_templates_with_writer(
            &AdTemplatesCommand::Explain(AdTemplatesExplainArgs {
                config,
                path_or_url: "/news/story".to_string(),
                method: "GET".to_string(),
                non_navigation: false,
                prefetch: false,
                bot: false,
                consent_denied: false,
                edgezero_enabled: true,
            }),
            &mut out,
        )
        .expect("should explain path");

        let output = String::from_utf8(out).expect("should be utf8");
        assert!(
            output.contains("server-side ad stack: yes"),
            "should report ad stack enabled"
        );
        assert!(
            output.contains("configured slots require Fastly legacy fallback"),
            "should report EdgeZero fallback"
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
        assert!(
            output.contains("edgezero: configured slots currently require Fastly legacy fallback"),
            "should report the EdgeZero legacy-fallback note"
        );
    }
}
