use edgezero_adapter_axum::dev_server::{
    AxumRunOptions, run_production_app_with_options_and_initializer,
};
use edgezero_adapter_axum::geo::NativeGeoConfigError;
use error_stack::Report;
use log::LevelFilter;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::native_geo::{NativeGeoProfileError, load_selection};
use trusted_server_adapter_axum::startup::initialize_native;

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

    let options = match native_options() {
        Ok(options) => options,
        Err(error) => {
            log::error!("Trusted Server native hosting configuration failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(err) = run_production_app_with_options_and_initializer::<TrustedServerApp, _>(
        options,
        |app, stores| {
            Box::pin(async move {
                initialize_native(app, stores).await.inspect_err(|error| {
                    // RC classifies callback errors without their details. This safe
                    // application diagnostic tells operators which input to fix.
                    log::error!("Trusted Server initialization failed: {error}");
                })
            })
        },
    ) {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Typed startup failures containing only bounded errors or redacted SDK text.
#[derive(Debug, derive_more::Display)]
enum NativeHostingError {
    #[display("{_0}")]
    GeoProfile(NativeGeoProfileError),
    #[display("{_0}")]
    Environment(String),
    #[display("{_0}")]
    GeoCapability(NativeGeoConfigError),
}

impl core::error::Error for NativeHostingError {}

/// Capture hosting inputs and validate the final provider selection before startup.
fn native_options() -> Result<AxumRunOptions, Report<NativeHostingError>> {
    let selection = load_selection().map_err(|error| {
        let context = NativeHostingError::GeoProfile(*error.current_context());
        error.change_context(context)
    })?;
    // The SDK guarantees redacted setting diagnostics. Retain that message,
    // not the SDK source-error graph or environment values.
    let options = AxumRunOptions::from_env()
        .map_err(|error| Report::new(NativeHostingError::Environment(error.to_string())))?
        .with_geo(selection);
    options.geo_capability().map_err(|error| {
        Report::new(error).change_context(NativeHostingError::GeoCapability(error))
    })?;
    Ok(options)
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

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_adapter_axum::native_geo::PROFILE_FILE_ENV;

    #[test]
    fn crate_compiles() {}

    #[test]
    fn hosting_error_preserves_redacted_sdk_diagnostic() {
        temp_env::with_vars(
            [
                (PROFILE_FILE_ENV, None),
                ("EDGEZERO__ADAPTER__PORT", Some("private-invalid-port")),
            ],
            || {
                let error = native_options()
                    .err()
                    .expect("should reject an invalid hosting port");
                assert!(
                    matches!(error.current_context(), NativeHostingError::Environment(_)),
                    "should retain a typed hosting-environment failure"
                );
                assert_eq!(
                    error.to_string(),
                    "startup settings: PORT: invalid nonzero port",
                    "should preserve the SDK setting name and safe failure diagnostic"
                );
                assert!(
                    !format!("{error:?}").contains("private-invalid-port"),
                    "should not retain the raw environment value in report frames"
                );
            },
        );
    }

    #[test]
    fn profile_error_keeps_its_category_without_retaining_inputs() {
        let directory = tempfile::tempdir().expect("should create private profile directory");
        let path = directory.path().join("private-profile.toml");
        std::fs::write(&path, "private malformed profile input")
            .expect("should write malformed fixture profile");
        temp_env::with_var(PROFILE_FILE_ENV, Some(path.as_os_str()), || {
            let error = native_options()
                .err()
                .expect("should reject a malformed selected profile");
            assert!(
                matches!(
                    error.current_context(),
                    NativeHostingError::GeoProfile(NativeGeoProfileError::InvalidProfile)
                ),
                "should retain the typed profile category at the hosting boundary"
            );
            assert_eq!(
                error.to_string(),
                "native Geo profile is invalid",
                "should preserve the existing profile diagnostic"
            );
            let diagnostic = format!("{error:?}");
            assert!(
                !diagnostic.contains(path.to_str().expect("should have fixture path")),
                "should not attach the private profile path"
            );
            assert!(
                !diagnostic.contains("private malformed profile input"),
                "should not attach parser input to the hosting report"
            );
        });
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
