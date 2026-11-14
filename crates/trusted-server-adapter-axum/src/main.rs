use edgezero_adapter_axum::dev_server::run_app;
use trusted_server_adapter_axum::app::TrustedServerApp;

/// Runs Trusted Server on `EdgeZero`'s Axum dev server.
///
/// `EdgeZero` owns startup: it installs the logger at
/// `EDGEZERO__LOGGING__LEVEL`, reads the ingress limits from
/// `EDGEZERO__ADAPTER__INGRESS__*` and the listener address from
/// `EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT`, then builds the app
/// and serves it. A settings, ingress or bind failure returns before any
/// request is served, and the process exits non-zero.
fn main() {
    if let Err(error) = run_app::<TrustedServerApp>() {
        log::error!("trusted-server-adapter-axum failed: {error:#}");
        std::process::exit(1);
    }
}
