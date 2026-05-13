use edgezero_core::app::Hooks as _;
use trusted_server_adapter_axum::app::TrustedServerApp;

fn main() {
    // When PORT is set, use a dynamic address so integration tests can allocate
    // a fresh OS port each run and avoid TIME_WAIT flakiness. The standard
    // `run_app` path is kept for normal development (reads config from axum.toml).
    if let Some(port) = port_from_env() {
        let _ = simple_logger::SimpleLogger::new().init();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        log::info!("Listening on http://{addr}");
        let config = edgezero_adapter_axum::AxumDevServerConfig {
            addr,
            enable_ctrl_c: true,
        };
        let router = TrustedServerApp::routes();
        if let Err(err) = edgezero_adapter_axum::AxumDevServer::with_config(router, config).run() {
            log::error!("trusted-server-adapter-axum failed: {err}");
            std::process::exit(1);
        }
    } else {
        let addr = edgezero_adapter_axum::AxumDevServerConfig::default().addr;
        let _ = simple_logger::SimpleLogger::new().init();
        log::info!("Listening on http://{addr}");
        if let Err(err) =
            edgezero_adapter_axum::run_app::<TrustedServerApp>(include_str!("../axum.toml"))
        {
            log::error!("trusted-server-adapter-axum failed: {err}");
            std::process::exit(1);
        }
    }
}

/// Read a port number from the `PORT` environment variable.
///
/// Returns `None` when the variable is unset or cannot be parsed as `u16`.
fn port_from_env() -> Option<u16> {
    let raw = std::env::var("PORT").ok()?;
    match raw.parse() {
        Ok(port) => Some(port),
        Err(e) => {
            log::warn!("PORT env var '{raw}' is not a valid u16: {e}; falling back to axum.toml");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
