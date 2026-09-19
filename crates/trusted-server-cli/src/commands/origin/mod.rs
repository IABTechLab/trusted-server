//! `ts origin` — questions about a publisher origin's behaviour.

pub mod probe;
pub mod report;

use clap::Subcommand;

use crate::error::CliResult;

/// Subcommands under `ts origin`.
#[derive(Debug, Subcommand)]
pub enum OriginCommand {
    /// Check whether an origin's responses may be shared between readers.
    ProbeShareability(ProbeShareabilityArgs),
}

/// Arguments for `ts origin probe-shareability`.
#[derive(Debug, clap::Args)]
pub struct ProbeShareabilityArgs {
    /// URL to probe. Repeat for several pages; one clean URL is not a statement about
    /// the origin.
    #[arg(long, required = true)]
    pub url: Vec<String>,

    /// How many extra times to repeat the self-identity comparison.
    #[arg(long, default_value_t = 3)]
    pub repeat: u32,

    /// Extra cookie to send in the cookie arm, as `name=value`. Repeatable.
    ///
    /// The probe always sends a representative Trusted Server cookie set; use this to add
    /// publisher cookies a real reader would also carry.
    #[arg(long)]
    pub cookie: Vec<String>,

    /// Request header the origin is configured to vary on, beyond `rsc`. Repeatable.
    ///
    /// Mirror `creative_opportunities.template_cache_vary` here. Each additional header
    /// is compared independently as absent versus `1`, both with and without RSC;
    /// built-in axes are not repeated.
    #[arg(long = "vary-header")]
    pub vary_header: Vec<String>,

    /// Cookie every request carries, as `name=value`, to get past a bot wall.
    ///
    /// Distinct from `--cookie`: this one is sent on *every* arm including the baseline,
    /// because without it a protected origin answers each arm with a challenge page and
    /// the probe would report on those instead of on the origin. It is not part of what
    /// the cookie axis varies.
    #[arg(long = "admission-cookie")]
    pub admission_cookie: Option<String>,

    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

/// Run an `ts origin` subcommand.
///
/// # Errors
///
/// Returns an error when an origin cannot be reached, or when the probe's verdict is that
/// the origin is not shareable — the caller turns that into a non-zero exit so the command
/// can gate a deploy.
pub fn run(command: OriginCommand, out: &mut impl std::io::Write) -> CliResult<()> {
    match command {
        OriginCommand::ProbeShareability(args) => run_probe(&args, out),
    }
}

fn run_probe(args: &ProbeShareabilityArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    for cookie in &args.cookie {
        if !cookie.contains('=') {
            return crate::error::cli_error(format!("--cookie expects name=value, got {cookie:?}"));
        }
    }

    if let Some(cookie) = args.admission_cookie.as_deref()
        && !cookie.contains('=')
    {
        return crate::error::cli_error(format!(
            "--admission-cookie expects name=value, got {cookie:?}"
        ));
    }

    let report = probe::probe_urls(
        &args.url,
        args.repeat,
        &args.cookie,
        &args.vary_header,
        args.admission_cookie.as_deref(),
    )?;

    let rendered = if args.json {
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to render the probe report as JSON: {error}"))?
    } else {
        report.render_text()
    };
    writeln!(out, "{rendered}").map_err(|error| format!("failed to write the report: {error}"))?;

    if report.passed() {
        Ok(())
    } else {
        // A failing verdict is the answer, not a malfunction — but it must not exit zero.
        // The gate this probe guards is decided before the origin responds, so this
        // result is the only thing standing between it and cross-serving.
        crate::error::cli_error(
            "origin is not safe to share: do not enable origin_readthrough_enabled or origin_is_cookie_independent",
        )
    }
}
