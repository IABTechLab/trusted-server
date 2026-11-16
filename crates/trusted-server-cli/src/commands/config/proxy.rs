//! Read-only creative proxy policy explanation using the runtime matcher.

use std::io::{self, Write};

use clap::{Args, Subcommand};
use serde::Serialize;
use trusted_server_core::config::validate_settings_for_deploy;
use url::Url;

use crate::ad_templates::output::escape_terminal_text;
use crate::app_config::{AppConfigArgs, load_settings};
use crate::run::RunOutcome;

/// Creative proxy configuration diagnostics.
#[derive(Debug, Subcommand)]
pub enum ProxyCommand {
    /// Explain hypothetical creative fetch and identity policy without fetching.
    Explain(ProxyExplainArgs),
}

/// Inputs for a read-only proxy policy explanation.
#[derive(Debug, Args)]
pub struct ProxyExplainArgs {
    #[command(flatten)]
    config: AppConfigArgs,
    /// Absolute HTTP or HTTPS target URL; query and fragment are never printed.
    url: String,
    /// Print the stable version 1 JSON decision.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct Explanation<'a> {
    version: u8,
    route: &'static str,
    fetch_allowed: bool,
    rule_id: Option<&'a str>,
    identity_eligibility: &'static str,
    headers: HeaderExplanation,
    upstream_cache: &'static str,
    downstream_cache: &'static str,
    redirects: RedirectExplanation,
}

#[derive(Serialize)]
struct HeaderExplanation {
    profile: &'static str,
    forwarded: [&'static str; 2],
    user_agent: &'static str,
    accept_encoding: &'static str,
    cors: &'static str,
}

#[derive(Serialize)]
struct RedirectExplanation {
    policy: &'static str,
    terminal_location: &'static str,
}

/// Explain creative policy without fetching a URL or evaluating real consent.
///
/// # Errors
///
/// Returns an error when the URL or effective configuration is invalid, or output
/// cannot be written.
///
/// # Examples
///
/// ```text
/// ts config proxy explain https://measurement.example.com/event --json
/// ```
pub fn run_proxy(command: &ProxyCommand) -> Result<RunOutcome, String> {
    let ProxyCommand::Explain(args) = command;
    let target = Url::parse(&args.url).map_err(|_| "invalid proxy target URL".to_owned())?;
    if !matches!(target.scheme(), "http" | "https") || target.host_str().is_none() {
        return Err("proxy target must be an absolute HTTP or HTTPS URL with a host".to_owned());
    }
    if !target.username().is_empty() || target.password().is_some() {
        return Err("proxy target must not contain userinfo".to_owned());
    }
    let mut loaded = load_settings(&args.config)?;
    validate_settings_for_deploy(&loaded.settings)
        .map_err(|error| format!("invalid proxy configuration: {error}"))?;
    loaded
        .settings
        .proxy
        .prepare_runtime()
        .map_err(|error| format!("invalid proxy configuration: {error}"))?;
    let decision = loaded
        .settings
        .proxy
        .identity_policy()
        .evaluate(&target, &loaded.settings.proxy.allowed_domains);
    let fetch_allowed = decision.fetch_allowed;
    let endpoint = decision.endpoint;
    let explanation = Explanation {
        version: 1,
        route: "creative",
        fetch_allowed,
        rule_id: endpoint.map(|endpoint| endpoint.id.as_str()),
        identity_eligibility: if endpoint.is_some() {
            "conditional"
        } else {
            "denied"
        },
        headers: HeaderExplanation {
            profile: "creative_resource",
            forwarded: ["accept", "accept-language"],
            user_agent: "TrustedServer/1.0",
            accept_encoding: "platform_supported",
            cors: "strip_upstream_grants",
        },
        upstream_cache: if endpoint.is_some() {
            "bypass"
        } else {
            "platform_default"
        },
        downstream_cache: if endpoint.is_some() {
            "private_no_store"
        } else {
            "origin"
        },
        redirects: RedirectExplanation {
            policy: "strict_creative",
            terminal_location: "reject",
        },
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &explanation)
            .map_err(|error| format!("failed to write proxy explanation: {error}"))?;
        writeln!(out).map_err(|error| output_error(&error))?;
    } else {
        let destination = format!("{}{}", target.origin().ascii_serialization(), target.path());
        writeln!(
            out,
            "creative target: {}",
            escape_terminal_text(&destination)
        )
        .map_err(|error| output_error(&error))?;
        writeln!(out, "fetch allowed: {}", explanation.fetch_allowed)
            .map_err(|error| output_error(&error))?;
        let reason = match explanation.rule_id {
            Some(id) => format!(
                "{id} (conditional on canonical existing cookie EC and current-request consent)"
            ),
            None if !fetch_allowed => "default-deny: fetch policy blocks this target".to_owned(),
            None => "default-deny: no identity endpoint matches".to_owned(),
        };
        writeln!(out, "identity: {}", escape_terminal_text(&reason))
            .map_err(|error| output_error(&error))?;
        writeln!(out, "headers: creative_resource; accept, accept-language; fixed TrustedServer/1.0 user agent; platform-supported encodings; upstream CORS grants stripped").map_err(|error| output_error(&error))?;
        writeln!(
            out,
            "upstream cache: {}; downstream cache: {}",
            explanation.upstream_cache, explanation.downstream_cache
        )
        .map_err(|error| output_error(&error))?;
        writeln!(
            out,
            "redirects: strict_creative; reject terminal Location; re-evaluate each destination"
        )
        .map_err(|error| output_error(&error))?;
    }
    Ok(RunOutcome::Success)
}

fn output_error(error: &io::Error) -> String {
    format!("failed to write proxy explanation: {error}")
}
