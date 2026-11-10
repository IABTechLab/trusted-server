use std::net::{Ipv4Addr, SocketAddr};

use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_core::app::Hooks as _;
use log::LevelFilter;
use trusted_server_adapter_axum::app::TrustedServerApp;

/// Environment variable that sets the maximum log level.
const LOG_LEVEL_ENV: &str = "RUST_LOG";

/// Log level used when `RUST_LOG` is unset or unrecognized.
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::Info;

/// Environment variable carrying a full listen socket address.
const LISTEN_ADDR_ENV: &str = "TRUSTED_SERVER_LISTEN_ADDR";

/// Environment variable carrying a loopback listen port.
const PORT_ENV: &str = "PORT";

#[allow(clippy::print_stderr)]
fn main() {
    // `var_os` keeps a set but non-UTF-8 value visible, so it is rejected
    // with a warning instead of silently falling back.
    let configured_level =
        std::env::var_os(LOG_LEVEL_ENV).map(|value| value.to_string_lossy().into_owned());
    let level = parse_log_level(configured_level.as_deref()).unwrap_or_else(|value| {
        eprintln!(
            "warning: {LOG_LEVEL_ENV}={value:?} is not a single log level; using {}",
            DEFAULT_LOG_LEVEL.as_str().to_ascii_lowercase()
        );
        DEFAULT_LOG_LEVEL
    });
    if let Err(e) = simple_logger::SimpleLogger::new().with_level(level).init() {
        eprintln!("warning: logger init failed: {e}");
    }

    let listen =
        std::env::var_os(LISTEN_ADDR_ENV).map(|value| value.to_string_lossy().into_owned());
    let port = std::env::var_os(PORT_ENV).map(|value| value.to_string_lossy().into_owned());
    let config = match listen_address(listen.as_deref(), port.as_deref()) {
        Ok(Some(addr)) => AxumDevServerConfig {
            addr,
            enable_ctrl_c: true,
        },
        Ok(None) => AxumDevServerConfig::default(),
        // Silently falling back to another address would surprise tooling
        // that expects the server where it asked for it.
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };

    log::info!("Listening on http://{}", config.addr);
    let router = TrustedServerApp::routes();
    if let Err(err) = AxumDevServer::with_config(router, config).run() {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Parses an optional `RUST_LOG` value into the logger's maximum level.
///
/// Absent and blank values resolve to [`DEFAULT_LOG_LEVEL`]. Any other value
/// must be a single level name (`trace`, `debug`, `info`, `warn`, `error`, or
/// `off`), matched case-insensitively after trimming whitespace. The quiet
/// default keeps trace-level payload and consent-string logging out of
/// standard output unless an operator explicitly asks for it.
///
/// # Errors
///
/// Returns the trimmed value when it is not a single level name, such as the
/// per-module filter `trusted_server=trace`, so the caller can warn before
/// falling back to [`DEFAULT_LOG_LEVEL`].
fn parse_log_level(configured: Option<&str>) -> Result<LevelFilter, &str> {
    match configured.map(str::trim) {
        None | Some("") => Ok(DEFAULT_LOG_LEVEL),
        Some(trimmed) => trimmed.parse().map_err(|_| trimmed),
    }
}

/// Selects the listen address from `TRUSTED_SERVER_LISTEN_ADDR` and `PORT`.
///
/// `TRUSTED_SERVER_LISTEN_ADDR` takes a full socket address such as
/// `0.0.0.0:8080` or `[::]:8080`, which a container needs to accept outside
/// connections. Otherwise `PORT` binds the loopback address on that port, so
/// integration tests can allocate a fresh OS port each run. With neither set,
/// `Ok(None)` keeps the adapter's default loopback address.
///
/// # Errors
///
/// Returns a message naming the variable when a set value is not a valid
/// socket address or port.
fn listen_address(listen: Option<&str>, port: Option<&str>) -> Result<Option<SocketAddr>, String> {
    if let Some(raw) = listen {
        return raw.parse().map(Some).map_err(|error| {
            format!("{LISTEN_ADDR_ENV}={raw:?} is not a valid socket address: {error}")
        });
    }
    let Some(raw) = port else {
        return Ok(None);
    };
    raw.parse::<u16>()
        .map(|port| Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port))))
        .map_err(|error| format!("{PORT_ENV}={raw:?} is not a valid u16: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_compiles() {}

    #[test]
    fn listen_address_prefers_the_full_socket_address() {
        assert_eq!(
            listen_address(Some("0.0.0.0:8080"), Some("9000")),
            Ok(Some(SocketAddr::from(([0, 0, 0, 0], 8080)))),
            "should bind every interface when the full address is set"
        );
        assert_eq!(
            listen_address(Some("[::]:8080"), None),
            Ok(Some(
                "[::]:8080".parse().expect("should parse IPv6 fixture")
            )),
            "should accept an IPv6 listen address"
        );
    }

    #[test]
    fn listen_address_keeps_port_on_loopback_and_default_when_unset() {
        assert_eq!(
            listen_address(None, Some("9000")),
            Ok(Some(SocketAddr::from(([127, 0, 0, 1], 9000)))),
            "should keep PORT on the loopback address"
        );
        assert_eq!(
            listen_address(None, None),
            Ok(None),
            "should keep the adapter default when nothing is set"
        );
    }

    #[test]
    fn listen_address_rejects_invalid_values_naming_the_variable() {
        for (listen, port, variable) in [
            (Some("0.0.0.0"), None, LISTEN_ADDR_ENV),
            (Some(""), Some("9000"), LISTEN_ADDR_ENV),
            (None, Some("70000"), PORT_ENV),
        ] {
            let message =
                listen_address(listen, port).expect_err("should reject an unusable listen setting");
            assert!(
                message.starts_with(variable),
                "should name {variable} in the error, got: {message}"
            );
        }
    }

    #[test]
    fn parse_log_level_defaults_to_info_when_unset_or_blank() {
        assert_eq!(
            parse_log_level(None),
            Ok(LevelFilter::Info),
            "should default to info when RUST_LOG is unset"
        );
        assert_eq!(
            parse_log_level(Some("")),
            Ok(LevelFilter::Info),
            "should treat an empty value as unset"
        );
        assert_eq!(
            parse_log_level(Some("  ")),
            Ok(LevelFilter::Info),
            "should treat a whitespace-only value as unset"
        );
    }

    #[test]
    fn parse_log_level_honors_explicit_levels() {
        assert_eq!(
            parse_log_level(Some("info")),
            Ok(LevelFilter::Info),
            "should suppress trace when RUST_LOG is info"
        );
        assert_eq!(
            parse_log_level(Some("trace")),
            Ok(LevelFilter::Trace),
            "should enable trace when RUST_LOG is trace"
        );
        assert_eq!(
            parse_log_level(Some(" DEBUG ")),
            Ok(LevelFilter::Debug),
            "should accept case-insensitive levels with surrounding whitespace"
        );
        assert_eq!(
            parse_log_level(Some("off")),
            Ok(LevelFilter::Off),
            "should allow disabling logging"
        );
    }

    #[test]
    fn parse_log_level_rejects_unrecognized_values() {
        assert_eq!(
            parse_log_level(Some("trace,hyper=info")),
            Err("trace,hyper=info"),
            "should reject module-filter syntax it does not support"
        );
        assert_eq!(
            parse_log_level(Some(" verbose ")),
            Err("verbose"),
            "should reject an unknown level name and return it trimmed"
        );
    }
}
