use std::convert::Infallible;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use edgezero_adapter_axum::dev_server::AxumDevServerConfig;
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use log::LevelFilter;
use tokio::net::TcpListener;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::signal;
use tower::{Service, ServiceExt as _, service_fn};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::timing::TimingService;

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

    let service = TrustedServerApp::dev_server_service();
    if let Err(err) = run(service, config) {
        log::error!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

/// Runs the Axum dev server with the request-phase timing terminal layer
/// ([`trusted_server_adapter_axum::timing::TimingService`]) wrapped around
/// `EdgeZeroAxumService`, ahead of `axum::serve`.
///
/// This does not use `edgezero_adapter_axum::dev_server::AxumDevServer::run`:
/// that helper only accepts a bare [`RouterService`](edgezero_core::router::RouterService) and builds its own
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
    service: TimingService<EdgeZeroAxumService>,
    config: AxumDevServerConfig,
) -> std::io::Result<()> {
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to build tokio runtime: {error}"),
            )
        })?;
    runtime.block_on(serve(service, config))
}

async fn serve(
    service: TimingService<EdgeZeroAxumService>,
    config: AxumDevServerConfig,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.addr).await.map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("failed to bind dev server to {}: {error}", config.addr),
        )
    })?;

    log::info!("Listening on http://{}", listener.local_addr()?);

    let make_service =
        dev_server_router(service).into_make_service_with_connect_info::<SocketAddr>();

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
    .map_err(|error| std::io::Error::new(error.kind(), format!("axum server error: {error}")))
}

fn dev_server_router<S>(service: S) -> Router
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send + 'static,
{
    Router::new().fallback_service(service_fn(move |req| service.clone().oneshot(req)))
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
    use std::future::{Ready, ready};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use super::*;

    struct ReadinessSensitiveService {
        polled: bool,
        readied: bool,
        pending_polls: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    impl Clone for ReadinessSensitiveService {
        fn clone(&self) -> Self {
            Self {
                polled: false,
                readied: false,
                pending_polls: Arc::clone(&self.pending_polls),
                calls: Arc::clone(&self.calls),
            }
        }
    }

    impl Service<Request<Body>> for ReadinessSensitiveService {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if !self.polled {
                self.polled = true;
                self.pending_polls.fetch_add(1, Ordering::Relaxed);
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            self.readied = true;
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: Request<Body>) -> Self::Future {
            assert!(
                self.readied,
                "should wait for this instance to become ready"
            );
            self.readied = false;
            self.calls.fetch_add(1, Ordering::Relaxed);
            ready(Ok(Response::builder()
                .header("cache-control", "private")
                .body(Body::empty())
                .expect("should build a private response")))
        }
    }

    #[tokio::test]
    async fn dev_server_bridge_waits_for_readiness_on_timed_and_health_paths() {
        for path in ["/private", "/health"] {
            for enabled in [true, false] {
                let pending_polls = Arc::new(AtomicUsize::new(0));
                let calls = Arc::new(AtomicUsize::new(0));
                let router = dev_server_router(TimingService::new(
                    ReadinessSensitiveService {
                        polled: false,
                        readied: false,
                        pending_polls: Arc::clone(&pending_polls),
                        calls: Arc::clone(&calls),
                    },
                    enabled,
                ));

                for expected_calls in 1..=2 {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::builder()
                                .uri(path)
                                .body(Body::empty())
                                .expect("should build a bridge request"),
                        )
                        .await
                        .expect("should serve the request after readiness");

                    assert_eq!(calls.load(Ordering::Relaxed), expected_calls);
                    assert_eq!(pending_polls.load(Ordering::Relaxed), expected_calls);
                    assert_eq!(
                        response.headers().contains_key("server-timing"),
                        enabled && path != "/health",
                        "should preserve timing configuration and health exclusion"
                    );
                }
            }
        }
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
