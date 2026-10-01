use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_core::app::Hooks as _;
use log::LevelFilter;
use trusted_server_adapter_axum::app::TrustedServerApp;

/// Environment variable that sets the maximum log level.
const LOG_LEVEL_ENV: &str = "RUST_LOG";

/// Log level used when `RUST_LOG` is unset or unrecognized.
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::Info;

#[allow(clippy::print_stderr)]
fn main() {
    let configured_level = std::env::var(LOG_LEVEL_ENV).ok();
    if let Some(value) = unrecognized_log_level(configured_level.as_deref()) {
        eprintln!(
            "warning: {LOG_LEVEL_ENV}={value:?} is not a single log level; using {}",
            DEFAULT_LOG_LEVEL.as_str().to_ascii_lowercase()
        );
    }
    let level = resolve_max_level(configured_level.as_deref());
    if let Err(e) = simple_logger::SimpleLogger::new().with_level(level).init() {
        eprintln!("warning: logger init failed: {e}");
    }

    let config = match port_from_env() {
        // When PORT is set, bind to a specific address so integration tests
        // can allocate a fresh OS port each run and avoid TIME_WAIT flakiness.
        Some(port) => AxumDevServerConfig {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            enable_ctrl_c: true,
        },
        // Normal development path: read bind address from axum.toml.
        None => AxumDevServerConfig::default(),
    };

    log::info!("Listening on http://{}", config.addr);
    let router = TrustedServerApp::routes();
    if let Err(err) = AxumDevServer::with_config(router, config).run() {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Resolves the logger's maximum level from an optional `RUST_LOG` value.
///
/// Falls back to `Info` when the value is absent or is not a single level
/// name (such as `trace`, `debug`, `info`, `warn`, `error`, or `off`). The
/// quiet default keeps trace-level payload and consent-string logging out of
/// standard output unless an operator explicitly asks for it.
fn resolve_max_level(configured: Option<&str>) -> LevelFilter {
    configured
        .and_then(|value| value.trim().parse::<LevelFilter>().ok())
        .unwrap_or(DEFAULT_LOG_LEVEL)
}

/// Returns the configured `RUST_LOG` value when it is set but ignored.
///
/// A value is ignored when it is non-blank and does not parse as a single
/// level name, such as the per-module filter `trusted_server=trace`. Blank
/// values are treated as unset and return `None`.
fn unrecognized_log_level(configured: Option<&str>) -> Option<&str> {
    configured.filter(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty() && trimmed.parse::<LevelFilter>().is_err()
    })
}

/// Read a port number from the `PORT` environment variable.
///
/// Returns `None` when the variable is unset. Exits non-zero if the value
/// is set but cannot be parsed — silently falling back to a different port
/// would surprise tooling that expects the server at the requested address.
#[allow(clippy::print_stderr)]
fn port_from_env() -> Option<u16> {
    let raw = std::env::var("PORT").ok()?;
    match raw.parse() {
        Ok(port) => Some(port),
        Err(e) => {
            eprintln!("error: PORT env var '{raw}' is not a valid u16: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_compiles() {}

    #[test]
    fn resolve_max_level_defaults_to_info_when_unset() {
        assert_eq!(
            resolve_max_level(None),
            LevelFilter::Info,
            "should default to info when RUST_LOG is unset"
        );
    }

    #[test]
    fn resolve_max_level_honors_explicit_levels() {
        assert_eq!(
            resolve_max_level(Some("info")),
            LevelFilter::Info,
            "should suppress trace when RUST_LOG is info"
        );
        assert_eq!(
            resolve_max_level(Some("trace")),
            LevelFilter::Trace,
            "should enable trace when RUST_LOG is trace"
        );
        assert_eq!(
            resolve_max_level(Some(" DEBUG ")),
            LevelFilter::Debug,
            "should accept case-insensitive levels with surrounding whitespace"
        );
        assert_eq!(
            resolve_max_level(Some("off")),
            LevelFilter::Off,
            "should allow disabling logging"
        );
    }

    #[test]
    fn resolve_max_level_falls_back_to_info_for_unrecognized_values() {
        assert_eq!(
            resolve_max_level(Some("trusted_server=trace")),
            LevelFilter::Info,
            "should fall back to info for module-filter syntax it does not support"
        );
        assert_eq!(
            resolve_max_level(Some("")),
            LevelFilter::Info,
            "should fall back to info for an empty value"
        );
    }

    #[test]
    fn unrecognized_log_level_reports_ignored_values() {
        assert_eq!(
            unrecognized_log_level(Some("trace,hyper=info")),
            Some("trace,hyper=info"),
            "should report a module-filter value that falls back to the default"
        );
        assert_eq!(
            unrecognized_log_level(Some("verbose")),
            Some("verbose"),
            "should report an unknown level name"
        );
    }

    #[test]
    fn unrecognized_log_level_ignores_unset_blank_and_valid_values() {
        assert_eq!(
            unrecognized_log_level(None),
            None,
            "should not report an unset value"
        );
        assert_eq!(
            unrecognized_log_level(Some("  ")),
            None,
            "should treat a blank value as unset"
        );
        assert_eq!(
            unrecognized_log_level(Some(" Debug ")),
            None,
            "should not report a valid level"
        );
    }
}
