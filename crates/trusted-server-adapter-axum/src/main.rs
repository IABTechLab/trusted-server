use edgezero_adapter_axum::dev_server::{
    AxumRunOptions, run_production_app_with_options_and_initializer,
};
use edgezero_adapter_axum::geo::NativeGeoConfigError;
use error_stack::Report;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::native_geo::{NativeGeoProfileError, load_selection};
use trusted_server_adapter_axum::startup::initialize_native;

/// Runs Trusted Server through `EdgeZero`'s strict Axum production startup.
///
/// `EdgeZero` owns startup: it installs the logger at
/// `EDGEZERO__LOGGING__LEVEL`, reads the ingress limits from
/// `EDGEZERO__ADAPTER__INGRESS__*` and the listener address from
/// `EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT`, and prepares the
/// declared config and secret stores from the selected providers. The
/// initializer then loads and validates the settings from those stores and
/// installs the router. A failure before the initializer installs a router,
/// including a hosting, Geo, provider preparation, config extraction limit,
/// ingress or bind failure, returns before any request is served, and the
/// process exits non-zero. A store read, settings or state failure in the
/// initializer installs the failure router instead, which keeps `/health` live
/// and answers `/ready` and every supported route with `503`.
fn main() {
    let options = match native_options() {
        Ok(options) => options,
        Err(error) => {
            report_startup_failure(format_args!(
                "Trusted Server native hosting configuration failed: {error}"
            ));
            std::process::exit(1);
        }
    };
    if let Err(error) = run_production_app_with_options_and_initializer::<TrustedServerApp, _>(
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
        report_startup_failure(format_args!(
            "trusted-server-adapter-axum failed: {error:#}"
        ));
        std::process::exit(1);
    }
}

/// Reports a fatal startup failure.
///
/// `EdgeZero` validates the hosting settings, such as the listener address,
/// the log level and the ingress limits, before it installs the logger, and
/// the hosting options are captured before the runner starts. A failure there
/// finds no logger, so the message goes to standard error instead of being
/// dropped.
#[allow(clippy::print_stderr)]
fn report_startup_failure(message: core::fmt::Arguments<'_>) {
    if log::log_enabled!(log::Level::Error) {
        log::error!("{message}");
    } else {
        eprintln!("{message}");
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
        .map_err(|error| Report::new(NativeHostingError::Environment(error.to_string())))?;
    let options = match selection {
        Some(selection) => options.with_geo(selection),
        None => options,
    };
    options
        .geo_capability_for::<TrustedServerApp>()
        .map_err(|error| {
            Report::new(error).change_context(NativeHostingError::GeoCapability(error))
        })?;
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_adapter_axum::native_geo::PROFILE_FILE_ENV;

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
}
