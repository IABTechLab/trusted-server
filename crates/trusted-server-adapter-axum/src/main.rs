#[cfg(not(feature = "aps-runner-proxy-integration-test"))]
use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_core::app::Hooks as _;
use trusted_server_adapter_axum::app::TrustedServerApp;

#[cfg(not(feature = "aps-runner-proxy-integration-test"))]
#[allow(clippy::print_stderr)]
fn main() {
    if let Err(e) = simple_logger::SimpleLogger::new().init() {
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

#[cfg(feature = "aps-runner-proxy-integration-test")]
#[tokio::main]
#[allow(clippy::print_stderr)]
async fn main() {
    use axum::Router;
    use axum::routing::any;
    use edgezero_adapter_axum::service::EdgeZeroAxumService;

    if let Err(e) = simple_logger::SimpleLogger::new().init() {
        eprintln!("warning: logger init failed: {e}");
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port_from_env().unwrap_or(8787)));
    let dispatcher =
        trusted_server_adapter_axum::app::ReservedApsDispatcher::from_startup_settings()
            .expect("APS feature artifact should build its reserved dispatcher");
    let reserved = any(move |request: axum::http::Request<axum::body::Body>| {
        let dispatcher = dispatcher.clone();
        async move {
            let response = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let request = match edgezero_adapter_axum::request::into_core_request(request)
                        .await
                    {
                        Ok(request) => request,
                        Err(error) => {
                            log::warn!("reserved APS request conversion failed: {error:?}");
                            return Err(axum::http::StatusCode::BAD_REQUEST);
                        }
                    };
                    match dispatcher.dispatch(request).await {
                        Some(response) => Ok(response),
                        None => {
                            log::error!(
                                "reserved APS entry route reached a request outside its route family"
                            );
                            Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        }
                    }
                })
            });
            match response {
                Ok(response) => edgezero_adapter_axum::response::into_axum_response(response),
                Err(status) => axum::response::IntoResponse::into_response(status),
            }
        }
    });
    let app = Router::new()
        .route("/integrations/aps", reserved.clone())
        .route("/integrations/aps/{*rest}", reserved)
        .fallback_service(EdgeZeroAxumService::new(TrustedServerApp::routes()));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("APS feature artifact should bind its configured address");
    log::info!("Listening on http://{addr}");
    if let Err(error) = axum::serve(listener, app).await {
        log::error!("trusted-server-adapter-axum failed: {error}");
    }
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
    #[test]
    fn crate_compiles() {}
}
