use edgezero_adapter_axum::dev_server::AxumDevServerConfig;
use edgezero_core::app::Hooks as _;
use trusted_server_adapter_axum::app::TrustedServerApp;

#[tokio::main]
#[allow(clippy::print_stderr)]
async fn main() {
    use axum::Router;
    use axum::routing::any;
    use edgezero_adapter_axum::service::EdgeZeroAxumService;

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

    let dispatcher =
        trusted_server_adapter_axum::app::ReservedApsDispatcher::from_startup_settings()
            .expect("should build the reserved APS dispatcher");
    let reserved = any(move |request: axum::http::Request<axum::body::Body>| {
        let dispatcher = dispatcher.clone();
        async move {
            // The core reserved dispatcher is intentionally `?Send`, while this
            // native-only development adapter runs on Tokio's multi-threaded
            // executor. Keep that bridge explicit: a runner request can occupy
            // this blocking-pool thread for its bounded five-second budget.
            let response = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let request =
                        match edgezero_adapter_axum::request::into_core_request(request).await {
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
                                "reserved APS entry route reached a request outside its family"
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
    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .expect("should bind the configured address");
    log::info!("Listening on http://{}", config.addr);
    let server = axum::serve(listener, app);
    let result = if config.enable_ctrl_c {
        server
            .with_graceful_shutdown(async {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    log::error!("failed to install Ctrl-C handler: {error}");
                }
            })
            .await
    } else {
        server.await
    };
    if let Err(error) = result {
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
