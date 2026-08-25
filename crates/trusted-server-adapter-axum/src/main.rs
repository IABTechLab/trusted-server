use std::net::SocketAddr;

use axum::Router;
use edgezero_adapter_axum::dev_server::AxumDevServerConfig;
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use edgezero_core::router::RouterService;
use tokio::net::TcpListener;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::signal;
use tower::Service as _;
use tower::service_fn;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::timing::TimingService;

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
    let (router, server_timing_enabled) = TrustedServerApp::routes_with_server_timing_flag();
    if let Err(err) = run(router, server_timing_enabled, config) {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Runs the Axum dev server with the request-phase timing terminal layer
/// ([`trusted_server_adapter_axum::timing::TimingService`]) wrapped around
/// `EdgeZeroAxumService`, ahead of `axum::serve`.
///
/// This does not use `edgezero_adapter_axum::dev_server::AxumDevServer::run`:
/// that helper only accepts a bare [`RouterService`] and builds its own
/// `EdgeZeroAxumService` and `axum::Router` internally, with no seam for an
/// outer service wrapper. Router-generated 404/405 responses bypass
/// `RouterBuilder::middleware` (see `trusted_server_adapter_axum::timing`),
/// so the freeze point has to wrap the tower `Service` boundary itself.
/// Driving `axum::serve` directly here mirrors that helper's own internal
/// bind/wrap/serve/shutdown sequence closely enough to keep behavior
/// identical for callers (`PORT` env var, ctrl-c graceful shutdown).
///
/// # Errors
///
/// Returns an error if the Tokio runtime fails to start, the listener fails
/// to bind, or the underlying serve loop errors.
fn run(
    router: RouterService,
    server_timing_enabled: bool,
    config: AxumDevServerConfig,
) -> std::io::Result<()> {
    let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
    runtime.block_on(serve(router, server_timing_enabled, config))
}

async fn serve(
    router: RouterService,
    server_timing_enabled: bool,
    config: AxumDevServerConfig,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.addr).await?;

    let service = TimingService::new(EdgeZeroAxumService::new(router), server_timing_enabled);
    let axum_router = Router::new().fallback_service(service_fn(move |req| {
        let mut svc = service.clone();
        async move { svc.call(req).await }
    }));
    let make_service = axum_router.into_make_service_with_connect_info::<SocketAddr>();

    let server = axum::serve(listener, make_service);
    if config.enable_ctrl_c {
        server
            .with_graceful_shutdown(async {
                let _ctrl_c = signal::ctrl_c().await;
            })
            .await
    } else {
        server.await
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
