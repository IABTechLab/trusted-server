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

/// Environment variable that overrides the Fastly logger's maximum level.
///
/// Production ships at `Info`; this override exists for local pre-production
/// validation under Viceroy, where raising the level to `debug` makes the
/// `should_route_to_edgezero` route-decision lines observable. Production Fastly
/// Compute does not surface arbitrary process environment variables, so the
/// override is effectively local-only and the level stays at the safe default
/// when the variable is unset or unparseable.
///
/// Fastly-only by design: this knob is safe here *because* Compute hides runtime
/// env vars. It must not be copied verbatim into the axum/spin/cloudflare
/// adapters, which run where env vars are readable — there it would reintroduce
/// the per-request production debug flood this path deliberately avoids.
const LOG_LEVEL_ENV: &str = "EDGEZERO_LOG_LEVEL";

/// Resolves the logger's maximum level from an optional configured value,
/// falling back to `Info` when it is absent or not a recognised level filter.
fn resolve_max_level(configured: Option<&str>) -> log::LevelFilter {
    configured
        .and_then(|value| value.trim().parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info)
}

/// Initialises the Fastly-backed `fern` logger and installs it as the global logger.
///
/// Log records are forwarded to the `tslog` Fastly endpoint and echoed to stdout.
/// Each line is prefixed with an RFC 3339 timestamp, level, and the final segment
/// of the record's target module path.
///
/// The maximum level defaults to `Info`. Setting the [`LOG_LEVEL_ENV`]
/// environment variable (e.g. `EDGEZERO_LOG_LEVEL=debug`) overrides it for local
/// Viceroy validation — the value is used as-is, so `error`/`off` lowers it just
/// as `debug` raises it; see [`resolve_max_level`].
///
/// # Panics
///
/// Panics if the Fastly logger cannot be built or if the global logger has already
/// been set.
pub(crate) fn init_logger() {
    let configured = std::env::var(LOG_LEVEL_ENV).ok();
    let max_level = resolve_max_level(configured.as_deref());

    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(max_level)
        .build()
        .expect("should build Logger");

    fern::Dispatch::new()
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
}
