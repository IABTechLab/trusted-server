use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_core::addr::resolve_bind_addr;
use edgezero_core::app::Hooks as _;
use log::LevelFilter;
use trusted_server_adapter_axum::app::TrustedServerApp;

/// Environment variable that sets the maximum log level.
const LOG_LEVEL_ENV: &str = "RUST_LOG";

/// Log level used when `RUST_LOG` is unset or unrecognized.
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::Info;

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

    // The default is loopback; runtime overrides are passed directly to EdgeZero.
    let host = std::env::var_os("EDGEZERO__ADAPTER__HOST");
    let port = std::env::var_os("PORT");
    let adapter_port = std::env::var_os("EDGEZERO__ADAPTER__PORT");
    let addr = parse_bind_address(host.as_deref(), port.as_deref(), adapter_port.as_deref())
        .unwrap_or_else(|error| {
            // Write directly so the refusal stays visible when `RUST_LOG=off`
            // or logger initialization failed.
            eprintln!("error: invalid runtime bind address: {error}");
            std::process::exit(1);
        });
    let config = AxumDevServerConfig {
        addr,
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

/// Resolve runtime binding using `EdgeZero` without embedding deployment configuration.
///
/// `EDGEZERO__ADAPTER__PORT` is primary; legacy `PORT` must agree when both
/// are set. Ports must be 1–65535; the default is loopback port 8787.
///
/// # Errors
///
/// Rejects non-Unicode values, conflicting ports and every `EdgeZero` resolver
/// warning, including malformed addresses and port zero, rather than falling back.
fn parse_bind_address(
    host: Option<&std::ffi::OsStr>,
    port: Option<&std::ffi::OsStr>,
    adapter_port: Option<&std::ffi::OsStr>,
) -> Result<std::net::SocketAddr, String> {
    let [host, port, adapter_port] = [
        ("EDGEZERO__ADAPTER__HOST", host),
        ("PORT", port),
        ("EDGEZERO__ADAPTER__PORT", adapter_port),
    ]
    .map(|(name, value)| {
        value
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| format!("{name} must be Unicode; startup refused"))
            })
            .transpose()
    });
    let host = host?;
    let port = port?;
    let adapter_port = adapter_port?;
    for (name, value) in [("PORT", port), ("EDGEZERO__ADAPTER__PORT", adapter_port)] {
        if let Some(value) = value
            && !resolve_bind_addr(None, Some(value), None, None)
                .warnings
                .is_empty()
        {
            return Err(format!(
                "{name}={value:?} must be a port in 1–65535; startup refused"
            ));
        }
    }
    let primary = resolve_bind_addr(host, adapter_port.or(port), None, None);
    if !primary.warnings.is_empty() {
        return Err(format!(
            "EDGEZERO__ADAPTER__HOST={:?} must be a valid IP address; startup refused",
            host.unwrap_or_default()
        ));
    }
    if let Some(port) = port {
        let legacy = resolve_bind_addr(None, Some(port), None, None);
        if legacy.addr.port() != primary.addr.port() {
            return Err(
                "PORT and EDGEZERO__ADAPTER__PORT must agree when both are set; startup refused"
                    .to_owned(),
            );
        }
    }
    Ok(primary.addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt as _;

    #[test]
    fn crate_compiles() {}

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

    #[test]
    fn adapter_port_alias_is_validated_alongside_port() {
        let port = std::ffi::OsStr::new("8080");
        for (legacy, adapter) in [
            (None, Some(port)),
            (Some(port), None),
            (Some(port), Some(port)),
        ] {
            assert_eq!(
                parse_bind_address(None, legacy, adapter)
                    .expect("should accept matching runtime ports")
                    .port(),
                8080,
                "should resolve either runtime port name"
            );
        }
        assert!(
            parse_bind_address(None, Some(port), Some(std::ffi::OsStr::new("8081"))).is_err(),
            "should reject conflicting port names"
        );
        for invalid in ["0", "", " ", " 80", "invalid", "65536"] {
            let invalid = std::ffi::OsStr::new(invalid);
            assert!(
                parse_bind_address(None, Some(port), Some(invalid)).is_err(),
                "should reject invalid adapter port even with valid PORT"
            );
            assert!(
                parse_bind_address(None, Some(invalid), Some(port)).is_err(),
                "should reject invalid PORT even with valid adapter port"
            );
        }
        #[cfg(unix)]
        assert!(
            parse_bind_address(None, Some(port), Some(std::ffi::OsStr::from_bytes(b"\xff")))
                .is_err(),
            "should reject non-Unicode adapter port"
        );
    }

    #[test]
    fn bind_address_errors_identify_the_input_and_refuse_startup() {
        for invalid in ["0", "invalid", "65536"] {
            let invalid = std::ffi::OsStr::new(invalid);
            let valid = std::ffi::OsStr::new("8080");
            for (legacy, adapter, variable) in [
                (Some(invalid), None, "PORT"),
                (None, Some(invalid), "EDGEZERO__ADAPTER__PORT"),
                (Some(invalid), Some(valid), "PORT"),
                (Some(valid), Some(invalid), "EDGEZERO__ADAPTER__PORT"),
            ] {
                let error = parse_bind_address(None, legacy, adapter)
                    .expect_err("should refuse invalid ports");
                assert!(
                    error.contains(&format!("{variable}=")),
                    "should identify the invalid input: {error}"
                );
                if variable == "PORT" {
                    assert!(
                        !error.contains("EDGEZERO__ADAPTER__PORT"),
                        "should not attribute legacy PORT to the adapter variable: {error}"
                    );
                }
                assert!(
                    error.contains("startup refused"),
                    "should state refusal: {error}"
                );
                assert!(
                    !error.contains("falling back"),
                    "should not promise fallback: {error}"
                );
            }
        }
        let error = parse_bind_address(Some(std::ffi::OsStr::new("localhost")), None, None)
            .expect_err("should refuse a hostname");
        assert!(
            error.contains("EDGEZERO__ADAPTER__HOST="),
            "should identify the host input"
        );
        assert!(
            error.contains("startup refused"),
            "should state host refusal"
        );
        assert!(
            !error.contains("falling back"),
            "should not promise host fallback"
        );
    }

    #[test]
    fn bind_address_rejects_zero_and_accepts_port_boundaries() {
        let zero = std::ffi::OsStr::new("0");
        assert!(
            parse_bind_address(None, Some(zero), None).is_err(),
            "should reject legacy port zero"
        );
        assert!(
            parse_bind_address(None, None, Some(zero)).is_err(),
            "should reject primary port zero"
        );
        for value in ["1", "65535"] {
            let port = std::ffi::OsStr::new(value);
            assert!(
                parse_bind_address(None, Some(port), Some(port)).is_ok(),
                "should accept port bounds"
            );
        }
    }

    #[test]
    fn bind_address_defaults_and_runtime_overrides() {
        assert_eq!(
            parse_bind_address(None, None, None).expect("should default"),
            "127.0.0.1:8787".parse().expect("should parse fixture"),
            "should retain loopback default"
        );
        assert_eq!(
            parse_bind_address(
                Some(std::ffi::OsStr::new("0.0.0.0")),
                Some(std::ffi::OsStr::new("8080")),
                None
            )
            .expect("should override")
            .to_string(),
            "0.0.0.0:8080",
            "should bind all interfaces at runtime"
        );
        assert_eq!(
            parse_bind_address(Some(std::ffi::OsStr::new("::1")), None, None)
                .expect("should accept IPv6")
                .to_string(),
            "[::1]:8787",
            "should accept IPv6"
        );
        assert_eq!(
            parse_bind_address(None, Some(std::ffi::OsStr::new("9000")), None)
                .expect("should preserve PORT")
                .port(),
            9000,
            "should honor existing port setting"
        );
    }

    #[test]
    fn bind_address_rejects_invalid_explicit_values() {
        for host in ["", "localhost", "0.0.0.0:8080", " 0.0.0.0"] {
            assert!(
                parse_bind_address(Some(std::ffi::OsStr::new(host)), None, None).is_err(),
                "should reject invalid host"
            );
        }
        for port in ["", "invalid", "65536", " 80"] {
            assert!(
                parse_bind_address(None, Some(std::ffi::OsStr::new(port)), None).is_err(),
                "should reject invalid port"
            );
        }
        #[cfg(unix)]
        {
            assert!(
                parse_bind_address(Some(std::ffi::OsStr::from_bytes(b"\xff")), None, None).is_err(),
                "should reject non-Unicode host"
            );
            assert!(
                parse_bind_address(None, Some(std::ffi::OsStr::from_bytes(b"\xff")), None).is_err(),
                "should reject non-Unicode port"
            );
        }
    }
}
