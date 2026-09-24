//! `ts origin` — questions about a publisher origin's behaviour.

pub mod probe;
pub mod report;

use clap::Subcommand;

use crate::error::CliResult;

/// Subcommands under `ts origin`.
#[derive(Debug, Subcommand)]
pub enum OriginCommand {
    /// Check whether an origin's responses may be shared between readers.
    ///
    /// Cookies are read from the environment, never from the command line, because they
    /// are credentials: set `TRUSTED_SERVER_PROBE_COOKIES` to the publisher cookies a real
    /// reader carries (`name=value; name=value`), and
    /// `TRUSTED_SERVER_PROBE_ADMISSION_COOKIE` to a cookie that gets past a bot wall. Every --url must be HTTPS; plain HTTP is
    /// accepted only for a loopback development origin.
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

    /// Extra cookies sent in the cookie arm, read from
    /// [`PROBE_COOKIES_ENVIRONMENT_VARIABLE`].
    ///
    /// Never a flag. The report asks for the cookies of a genuine authenticated session,
    /// and an argument is visible to every other process on the host through `ps` and
    /// lands in shell history. The probe always sends a representative Trusted Server
    /// cookie set; the environment adds publisher cookies a real reader would also carry.
    #[arg(skip)]
    pub cookie: Vec<String>,

    /// Request header the origin is configured to vary on, beyond `rsc`. Repeatable.
    ///
    /// Mirror `creative_opportunities.template_cache_vary` here. Each additional header
    /// is compared independently as absent versus `1`, both with and without RSC;
    /// built-in axes are not repeated.
    #[arg(long = "vary-header")]
    pub vary_header: Vec<String>,

    /// Cookie every request carries to get past a bot wall, read from
    /// [`PROBE_ADMISSION_COOKIE_ENVIRONMENT_VARIABLE`].
    ///
    /// Also environment-only: a bot-wall admission token is a credential. Distinct from
    /// the cookie arm's extras: this one is sent on *every* arm including the baseline,
    /// because without it a protected origin answers each arm with a challenge page and
    /// the probe would report on those instead of on the origin. It is not part of what
    /// the cookie axis varies. These runs are diagnostic only and always fail the safety
    /// gate because cookieless responses are untested. Rerun without it against the
    /// origin before enabling caching.
    #[arg(skip)]
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
        OriginCommand::ProbeShareability(mut args) => {
            load_cookies_from_environment(&mut args);
            run_probe(&args, out)
        }
    }
}

/// Environment variable carrying the cookie arm's extra cookies, as one cookie header
/// value: `name=value; name=value`.
///
/// Read from the environment and never accepted as a flag, for the same reason as the
/// admin password in `ts cache purge`: an argument is visible to every other process on
/// the host through `ps`, and lands in shell history. The report asks operators to probe
/// with a genuine session's cookies, so these values are credentials.
pub const PROBE_COOKIES_ENVIRONMENT_VARIABLE: &str = "TRUSTED_SERVER_PROBE_COOKIES";

/// Environment variable carrying the bot-wall admission cookie, as `name=value`.
pub const PROBE_ADMISSION_COOKIE_ENVIRONMENT_VARIABLE: &str =
    "TRUSTED_SERVER_PROBE_ADMISSION_COOKIE";

/// Split a cookie header value into its `name=value` pairs.
fn split_cookie_header(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Fold the environment's cookies into the parsed arguments.
///
/// Appends rather than replaces so a caller that constructed the arguments directly — the
/// integration suite — keeps what it set.
fn load_cookies_from_environment(args: &mut ProbeShareabilityArgs) {
    load_cookies(args, |name| std::env::var(name).ok());
}

fn load_cookies(args: &mut ProbeShareabilityArgs, environment: impl Fn(&str) -> Option<String>) {
    if let Some(value) = environment(PROBE_COOKIES_ENVIRONMENT_VARIABLE) {
        args.cookie.extend(split_cookie_header(&value));
    }
    if args.admission_cookie.is_none()
        && let Some(value) = environment(PROBE_ADMISSION_COOKIE_ENVIRONMENT_VARIABLE)
    {
        let value = value.trim();
        if !value.is_empty() {
            args.admission_cookie = Some(value.to_owned());
        }
    }
}

fn run_probe(args: &ProbeShareabilityArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    // Transport first: every later step sends the cookies below to these URLs, so a URL
    // that cannot carry a credential safely must be refused before one is read.
    for url in &args.url {
        let parsed = reqwest::Url::parse(url)
            .map_err(|error| format!("--url must be an absolute URL, got {url:?}: {error}"))?;
        crate::url_guard::require_credential_safe_transport(&parsed, "--url")?;
    }

    for cookie in &args.cookie {
        if !cookie.contains('=') {
            return crate::error::cli_error(format!(
                "{PROBE_COOKIES_ENVIRONMENT_VARIABLE} expects name=value pairs separated by \
                 `;`, got {cookie:?}"
            ));
        }
    }

    if let Some(cookie) = args.admission_cookie.as_deref()
        && !cookie.contains('=')
    {
        return crate::error::cli_error(format!(
            "{PROBE_ADMISSION_COOKIE_ENVIRONMENT_VARIABLE} expects name=value, got {cookie:?}"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_header_splits_trims_and_ignores_empty_segments() {
        assert_eq!(
            split_cookie_header(" ; session=example ; ; preference=a=b; "),
            ["session=example", "preference=a=b"],
            "should preserve cookie pairs while removing empty segments"
        );
        assert!(
            split_cookie_header(" ; ; ").is_empty(),
            "should ignore empty cookies"
        );
    }

    #[test]
    fn environment_appends_cookies_and_preserves_explicit_admission() {
        let mut args = ProbeShareabilityArgs {
            url: vec![],
            repeat: 1,
            cookie: vec!["caller=example".to_owned()],
            vary_header: vec![],
            admission_cookie: Some("admission=caller".to_owned()),
            json: false,
        };
        load_cookies(&mut args, |name| {
            Some(if name == PROBE_COOKIES_ENVIRONMENT_VARIABLE {
                " session=environment; ; preference=example ".to_owned()
            } else {
                " admission=environment ".to_owned()
            })
        });
        assert_eq!(
            args.cookie,
            [
                "caller=example",
                "session=environment",
                "preference=example"
            ],
            "should append parsed environment cookies"
        );
        assert_eq!(
            args.admission_cookie.as_deref(),
            Some("admission=caller"),
            "should preserve explicit admission cookie"
        );
        args.admission_cookie = None;
        load_cookies(&mut args, |_| None);
        assert!(
            args.admission_cookie.is_none(),
            "should tolerate an absent environment"
        );
        load_cookies(&mut args, |_| Some("  ".to_owned()));
        assert!(
            args.admission_cookie.is_none(),
            "should ignore blank admission cookies"
        );
        load_cookies(&mut args, |name| {
            (name == PROBE_ADMISSION_COOKIE_ENVIRONMENT_VARIABLE)
                .then(|| " admission=environment ".to_owned())
        });
        assert_eq!(
            args.admission_cookie.as_deref(),
            Some("admission=environment"),
            "should trim the environment admission cookie"
        );
    }
}
