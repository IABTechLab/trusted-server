//! Fastly-specific logger wiring for the trusted-server adapter.

use chrono::{SecondsFormat, Utc};
use log_fastly::Logger;

/// Extracts the final `::` segment from a Rust module path for use as a log label.
///
/// When the input has no `::` separator, returns the full target. When the
/// separator is at the trailing position (e.g. `"foo::"`), returns the head
/// segment (`"foo"`) to avoid emitting an empty label.
fn target_label(target: &str) -> &str {
    match target.rsplit_once("::") {
        Some((head, "")) => head,
        Some((_, last)) => last,
        None => target,
    }
}

/// Environment variable that explicitly overrides the Fastly logger's maximum level.
///
/// Production ships at `Info`. `fastly compute serve`/Viceroy does not propagate
/// arbitrary shell environment variables into the Compute guest, so this override
/// only takes effect where the guest environment is populated deliberately (e.g.
/// the integration-test harness). Routine local validation does not depend on it —
/// see [`init_logger`], which auto-raises the level under Viceroy via the
/// [`LOCAL_HOSTNAME_ENV`] signal. The level stays at the safe default when this
/// variable is unset or unparseable.
const LOG_LEVEL_ENV: &str = "EDGEZERO_LOG_LEVEL";

/// Fastly-provided hostname environment variable, visible to guest code.
///
/// Viceroy (`fastly compute serve`) reports [`LOCAL_HOSTNAME`]; production cache
/// nodes report their real hostname. [`init_logger`] reads it to raise the log
/// level for local route-decision observability without affecting production.
///
/// Fastly-specific by design: this signal exists *because* Compute exposes a
/// guest-visible hostname that is fixed to `localhost` only under the simulator.
/// It must not be copied verbatim into the axum/spin/cloudflare adapters, where
/// it carries no such meaning and would mis-detect the runtime environment.
const LOCAL_HOSTNAME_ENV: &str = "FASTLY_HOSTNAME";

/// Hostname value Viceroy reports for [`LOCAL_HOSTNAME_ENV`] in local runs.
const LOCAL_HOSTNAME: &str = "localhost";

/// Resolves the logger's maximum level from an optional configured value,
/// falling back to `Info` when it is absent or not a recognised level filter.
fn resolve_max_level(configured: Option<&str>) -> log::LevelFilter {
    configured
        .and_then(|value| value.trim().parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info)
}

/// Detects the maximum log level from explicit config and the Fastly hostname.
///
/// Explicit `EDGEZERO_LOG_LEVEL` input wins, including values that lower the
/// level. Without an explicit value, Viceroy's `FASTLY_HOSTNAME=localhost`
/// auto-raises local runs to `Debug`; production hostnames and missing hostnames
/// stay at the safe `Info` default.
fn detect_max_level(explicit: Option<&str>, hostname: Option<&str>) -> log::LevelFilter {
    let configured = explicit.or_else(|| (hostname == Some(LOCAL_HOSTNAME)).then_some("debug"));
    resolve_max_level(configured)
}

/// Initialises the Fastly-backed `fern` logger and installs it as the global logger.
///
/// Log records are forwarded to the `tslog` Fastly endpoint and echoed to stdout.
/// Each line is prefixed with an RFC 3339 timestamp, level, and the final segment
/// of the record's target module path.
///
/// The maximum level defaults to `Info`. Under Viceroy (`fastly compute serve`)
/// it is auto-raised to `debug` so route-decision lines are observable locally,
/// detected via the [`LOCAL_HOSTNAME_ENV`] signal. An explicit [`LOG_LEVEL_ENV`]
/// value takes precedence where the guest environment is populated — the value is
/// used as-is, so `error`/`off` lowers it just as `debug` raises it; see
/// [`resolve_max_level`].
///
/// # Panics
///
/// Panics if the Fastly logger cannot be built or if the global logger has already
/// been set.
pub(crate) fn init_logger() {
    let explicit = std::env::var(LOG_LEVEL_ENV).ok();
    let hostname = std::env::var(LOCAL_HOSTNAME_ENV).ok();
    let max_level = detect_max_level(explicit.as_deref(), hostname.as_deref());

    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(max_level)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
        .level(max_level)
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} [{}] {}",
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                record.level(),
                target_label(record.target()),
                message
            ));
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("should initialize logger");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_label_extracts_correct_segment() {
        assert_eq!(
            target_label("trusted_server_adapter_fastly::proxy"),
            "proxy",
            "should handle standard single-separator case"
        );
        assert_eq!(
            target_label("foo::bar::baz"),
            "baz",
            "should handle multiple separators"
        );
        assert_eq!(
            target_label("no_separators_here"),
            "no_separators_here",
            "should handle inputs without ::"
        );
        assert_eq!(target_label(""), "", "should handle empty strings");
        assert_eq!(
            target_label("trailing::"),
            "trailing",
            "should strip separator when trailing segment is empty"
        );
    }

    #[test]
    fn resolve_max_level_defaults_to_info_when_unset() {
        assert_eq!(
            resolve_max_level(None),
            log::LevelFilter::Info,
            "should default to Info when the override is unset"
        );
    }

    #[test]
    fn resolve_max_level_raises_to_debug_for_viceroy_validation() {
        assert_eq!(
            resolve_max_level(Some("debug")),
            log::LevelFilter::Debug,
            "should raise to Debug so route-decision lines are observable locally"
        );
        assert_eq!(
            resolve_max_level(Some("  DEBUG  ")),
            log::LevelFilter::Debug,
            "should accept case-insensitive, surrounding-whitespace values"
        );
    }

    #[test]
    fn resolve_max_level_falls_back_to_info_on_unrecognised_value() {
        assert_eq!(
            resolve_max_level(Some("not-a-level")),
            log::LevelFilter::Info,
            "should keep the safe Info default for an unparseable override"
        );
    }

    #[test]
    fn detect_max_level_explicit_override_wins_over_local_hostname() {
        assert_eq!(
            detect_max_level(Some("error"), Some(LOCAL_HOSTNAME)),
            log::LevelFilter::Error,
            "explicit override should win over Viceroy auto-debug"
        );
    }

    #[test]
    fn detect_max_level_auto_debugs_only_for_viceroy_hostname() {
        assert_eq!(
            detect_max_level(None, Some(LOCAL_HOSTNAME)),
            log::LevelFilter::Debug,
            "Viceroy localhost hostname should auto-raise local validation to Debug"
        );
        assert_eq!(
            detect_max_level(None, Some("cache-lax1234-LAX")),
            log::LevelFilter::Info,
            "production Fastly hostnames must stay at Info"
        );
        assert_eq!(
            detect_max_level(None, None),
            log::LevelFilter::Info,
            "missing hostname should stay at Info"
        );
    }
}
