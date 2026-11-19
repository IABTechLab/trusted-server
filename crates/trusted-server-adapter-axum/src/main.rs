use edgezero_adapter_axum::dev_server::run_production_app_with_initializer;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::startup::initialize_native;

/// Runs Trusted Server through `EdgeZero`'s strict Axum production startup.
///
/// `EdgeZero` owns startup: it installs the logger at
/// `EDGEZERO__LOGGING__LEVEL`, reads the ingress limits from
/// `EDGEZERO__ADAPTER__INGRESS__*` and the listener address from
/// `EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT`, and prepares the
/// declared config and secret stores from the selected providers. The
/// initializer then loads and validates the settings from those stores and
/// installs the router. A settings, store, ingress or bind failure returns
/// before any request is served, and the process exits non-zero.
fn main() {
    if let Err(error) = run_production_app_with_initializer::<TrustedServerApp, _>(|app, stores| {
        Box::pin(async move {
            initialize_native(app, stores).await.inspect_err(|error| {
                // RC classifies callback errors without their details. This safe
                // application diagnostic tells operators which input to fix.
                log::error!("Trusted Server initialization failed: {error}");
            })
        })
    }) {
        report_startup_failure(format_args!(
            "trusted-server-adapter-axum failed: {error:#}"
        ));
        std::process::exit(1);
    }
}

/// Reports a fatal startup failure.
///
/// `EdgeZero` validates the hosting settings, such as the listener address,
/// the log level and the ingress limits, before it installs the logger. A
/// failure there finds no logger, so the message goes to standard error
/// instead of being dropped.
#[allow(clippy::print_stderr)]
fn report_startup_failure(message: core::fmt::Arguments<'_>) {
    if log::log_enabled!(log::Level::Error) {
        log::error!("{message}");
    } else {
        eprintln!("{message}");
    }
}
