use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_core::addr::{BindAddrResolution, resolve_bind_addr};
use edgezero_core::app::Hooks as _;
use log::LevelFilter;
use trusted_server_adapter_axum::app::TrustedServerApp;

/// Environment variable that sets the maximum log level.
const LOG_LEVEL_ENV: &str = "RUST_LOG";

/// Log level used when `RUST_LOG` is unset or unrecognized.
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::Info;

/// `EdgeZero` environment variable carrying the listen IP address.
const HOST_ENV: &str = "EDGEZERO__ADAPTER__HOST";

/// `EdgeZero` environment variable carrying the listen port.
const EDGEZERO_PORT_ENV: &str = "EDGEZERO__ADAPTER__PORT";

/// Environment variable carrying a listen port for local tooling and tests.
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

    let env = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
    let (host, edgezero_port, port) = (env(HOST_ENV), env(EDGEZERO_PORT_ENV), env(PORT_ENV));
    let resolution = match bind_address(host.as_deref(), edgezero_port.as_deref(), port.as_deref())
    {
        Ok(resolution) => resolution,
        // Silently falling back to another port would surprise tooling
        // that expects the server where it asked for it.
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };
    for warning in &resolution.warnings {
        log::warn!("{warning}");
    }
    let config = AxumDevServerConfig {
        addr: resolution.addr,
        enable_ctrl_c: true,
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

/// Resolves the listen address with `EdgeZero`'s precedence.
///
/// `EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT` win, so a container
/// can listen on `0.0.0.0`. `PORT` is the port fallback local tooling uses to
/// allocate a fresh port per run. Otherwise the address is `127.0.0.1:8787`.
/// An invalid `EdgeZero` value falls back with a warning, as `EdgeZero` does.
/// `PORT` is read only when it is the fallback, so a valid
/// `EDGEZERO__ADAPTER__PORT` makes any `PORT` irrelevant.
///
/// # Errors
///
/// Returns a message naming `PORT` when it is the fallback and is not a
/// nonzero port number.
fn bind_address(
    host: Option<&str>,
    edgezero_port: Option<&str>,
    port: Option<&str>,
) -> Result<BindAddrResolution, String> {
    let edgezero_port_valid =
        edgezero_port.is_some_and(|raw| raw.parse::<u16>().is_ok_and(|value| value != 0));
    let fallback_port = match port {
        Some(raw) if !edgezero_port_valid => match raw.parse::<u16>() {
            Ok(0) => {
                return Err(format!(
                    "{PORT_ENV}=\"0\" is not supported (would bind to a random OS port)"
                ));
            }
            Ok(value) => Some(value),
            Err(error) => return Err(format!("{PORT_ENV}={raw:?} is not a valid u16: {error}")),
        },
        _ => None,
    };
    Ok(resolve_bind_addr(host, edgezero_port, None, fallback_port))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    #[test]
    fn crate_compiles() {}

    #[test]
    fn bind_address_uses_edgezero_host_and_port_first() {
        let resolution = bind_address(Some("0.0.0.0"), Some("8080"), Some("9000"))
            .expect("should accept valid listen settings");
        assert_eq!(
            resolution.addr,
            SocketAddr::from(([0, 0, 0, 0], 8080)),
            "should bind every interface on the EdgeZero port"
        );
        assert!(
            resolution.warnings.is_empty(),
            "should not warn on valid values"
        );
        let resolution =
            bind_address(Some("::"), None, Some("9000")).expect("should accept an IPv6 host");
        assert_eq!(
            resolution.addr,
            "[::]:9000".parse().expect("should parse IPv6 fixture"),
            "should fall back to PORT when no EdgeZero port is set"
        );
    }

    #[test]
    fn bind_address_keeps_port_and_default_on_loopback() {
        let resolution = bind_address(None, None, Some("9000")).expect("should accept PORT");
        assert_eq!(
            resolution.addr,
            SocketAddr::from(([127, 0, 0, 1], 9000)),
            "should keep PORT on the loopback address"
        );
        let resolution = bind_address(None, None, None).expect("should accept no settings");
        assert_eq!(
            resolution.addr,
            SocketAddr::from(([127, 0, 0, 1], 8787)),
            "should keep the EdgeZero loopback default"
        );
    }

    #[test]
    fn bind_address_warns_on_invalid_edgezero_values_and_rejects_invalid_port() {
        let resolution = bind_address(Some("localhost"), Some("0"), None)
            .expect("should fall back from invalid EdgeZero values");
        assert_eq!(
            resolution.addr,
            SocketAddr::from(([127, 0, 0, 1], 8787)),
            "should fall back to the loopback default"
        );
        assert!(
            resolution
                .warnings
                .iter()
                .any(|warning| warning.starts_with(HOST_ENV))
                && resolution
                    .warnings
                    .iter()
                    .any(|warning| warning.starts_with(EDGEZERO_PORT_ENV)),
            "should warn naming each invalid EdgeZero variable, got: {:?}",
            resolution.warnings
        );
        let resolution = bind_address(Some("0.0.0.0"), Some("8080"), Some("not-a-port"))
            .expect("should ignore PORT when the EdgeZero port is valid");
        assert_eq!(
            resolution.addr,
            SocketAddr::from(([0, 0, 0, 0], 8080)),
            "should not read a platform-injected PORT inside the image"
        );
        let message = bind_address(None, None, Some("0"))
            .expect_err("should reject PORT=0 instead of binding a random port");
        assert!(
            message.starts_with(PORT_ENV),
            "should name PORT for the zero port, got: {message}"
        );
        let message = bind_address(Some("0.0.0.0"), None, Some("70000"))
            .expect_err("should reject an invalid PORT");
        assert!(
            message.starts_with(PORT_ENV),
            "should name PORT in the error, got: {message}"
        );
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
