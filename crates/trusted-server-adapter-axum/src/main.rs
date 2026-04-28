use edgezero_core::app::Hooks as _;
use trusted_server_adapter_axum::app::TrustedServerApp;

fn main() {
    // When PORT is set, use a dynamic address so integration tests can allocate
    // a fresh OS port each run and avoid TIME_WAIT flakiness. The standard
    // `run_app` path is kept for normal development (reads config from axum.toml).
    if let Some(port) = port_from_env() {
        let _ = simple_logger::SimpleLogger::new().init();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let config = edgezero_adapter_axum::AxumDevServerConfig {
            addr,
            enable_ctrl_c: true,
        };
        let router = TrustedServerApp::routes();
        if let Err(err) = edgezero_adapter_axum::AxumDevServer::with_config(router, config).run() {
            log::error!("trusted-server-adapter-axum failed: {err}");
            std::process::exit(1);
        }
    } else if let Err(err) =
        edgezero_adapter_axum::run_app::<TrustedServerApp>(include_str!("../axum.toml"))
    {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Read a port number from the `PORT` environment variable.
///
/// Returns `None` when the variable is unset or cannot be parsed as `u16`.
fn port_from_env() -> Option<u16> {
    std::env::var("PORT").ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
