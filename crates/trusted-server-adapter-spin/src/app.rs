use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{AuctionOrchestrator, build_orchestrator};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    PublisherResponse, handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware};
use crate::platform::build_runtime_services;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = get_settings()?;
    let orchestrator = build_orchestrator(&settings)?;
    let registry = IntegrationRegistry::new(&settings)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
    }))
}

// ---------------------------------------------------------------------------
// Publisher response helper
// ---------------------------------------------------------------------------

/// Collapse a [`PublisherResponse`] into a plain [`Response`].
///
/// Buffers streaming and pass-through variants in memory (acceptable for a
/// Spin invocation which processes one request at a time).
fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    settings: &Settings,
    registry: &IntegrationRegistry,
) -> Result<Response, Report<TrustedServerError>> {
    match publisher_response {
        PublisherResponse::Buffered(response) => Ok(response),
        PublisherResponse::Stream {
            mut response,
            body,
            params,
        } => {
            let mut output = Vec::new();
            stream_publisher_body(body, &mut output, &params, settings, registry)?;
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                edgezero_core::http::HeaderValue::from(output.len() as u64),
            );
            *response.body_mut() = edgezero_core::body::Body::from(output);
            Ok(response)
        }
        PublisherResponse::PassThrough { mut response, body } => {
            *response.body_mut() = body;
            Ok(response)
        }
    }
}

// ---------------------------------------------------------------------------
// Spin host extraction
// ---------------------------------------------------------------------------

// Extracts "host[:port]" from a full URL string (e.g. "http://localhost:3000/path" → "localhost:3000").
// Used to populate the missing Host header from Spin's spin-full-url synthetic header.
fn host_from_spin_url(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_spin_url_extracts_localhost_with_port() {
        assert_eq!(
            host_from_spin_url("http://localhost:3000/some/path"),
            Some("localhost:3000".to_string()),
            "should extract host:port from http URL"
        );
    }

    #[test]
    fn host_from_spin_url_extracts_production_domain() {
        assert_eq!(
            host_from_spin_url("https://www.publisher.com/cars/"),
            Some("www.publisher.com".to_string()),
            "should extract domain without port from https URL"
        );
    }

    #[test]
    fn host_from_spin_url_handles_root_path() {
        assert_eq!(
            host_from_spin_url("http://127.0.0.1:3000/"),
            Some("127.0.0.1:3000".to_string()),
            "should extract host from root path URL"
        );
    }

    #[test]
    fn host_from_spin_url_rejects_no_scheme() {
        assert_eq!(
            host_from_spin_url("localhost:3000/path"),
            None,
            "should return None when no scheme separator"
        );
    }
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`].
pub(crate) fn http_error(report: &Report<TrustedServerError>) -> Response {
    let root_error = report.current_context();
    log::error!("Error occurred: {:?}", report);

    let body = edgezero_core::body::Body::from(format!("{}\n", root_error.user_message()));
    let mut response = Response::new(body);
    *response.status_mut() = root_error.status_code();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

// ---------------------------------------------------------------------------
// Startup error fallback
// ---------------------------------------------------------------------------

/// Returns a [`RouterService`] that responds to every route with the startup error.
fn startup_error_router(e: &Report<TrustedServerError>) -> RouterService {
    let message = Arc::new(format!("{}\n", e.current_context().user_message()));
    let status = e.current_context().status_code();

    let make = move |msg: Arc<String>| {
        move |_ctx: RequestContext| {
            let body = edgezero_core::body::Body::from((*msg).clone());
            let mut resp = Response::new(body);
            *resp.status_mut() = status;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            async move { Ok::<Response, EdgeError>(resp) }
        }
    };

    RouterService::builder()
        .middleware(FinalizeResponseMiddleware::new(Arc::new(
            Settings::default(),
        )))
        .get("/", make(Arc::clone(&message)))
        .post("/", make(Arc::clone(&message)))
        .get("/{*rest}", make(Arc::clone(&message)))
        .post("/{*rest}", make(Arc::clone(&message)))
        .build()
}

// ---------------------------------------------------------------------------
// TrustedServerApp
// ---------------------------------------------------------------------------

/// `EdgeZero` [`Hooks`] implementation for the Trusted Server application.
pub struct TrustedServerApp;

impl Hooks for TrustedServerApp {
    fn name() -> &'static str {
        "TrustedServer"
    }

    fn routes() -> RouterService {
        let state = match build_state() {
            Ok(s) => s,
            Err(ref e) => {
                log::error!("failed to build application state: {:?}", e);
                return startup_error_router(e);
            }
        };

        // /.well-known/trusted-server.json
        let s = Arc::clone(&state);
        let discovery_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_trusted_server_discovery(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /verify-signature
        let s = Arc::clone(&state);
        let verify_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_verify_signature(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /admin/keys/rotate
        let s = Arc::clone(&state);
        let rotate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_rotate_key(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /admin/keys/deactivate
        let s = Arc::clone(&state);
        let deactivate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_deactivate_key(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /auction
        let s = Arc::clone(&state);
        let auction_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_auction(&s.settings, &s.orchestrator, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // GET /first-party/proxy
        let s = Arc::clone(&state);
        let fp_proxy_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /first-party/click
        let s = Arc::clone(&state);
        let fp_click_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_click(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // GET /first-party/sign
        let s = Arc::clone(&state);
        let fp_sign_get_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy_sign(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // POST /first-party/sign
        let s = Arc::clone(&state);
        let fp_sign_post_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy_sign(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /first-party/proxy-rebuild
        let s = Arc::clone(&state);
        let fp_rebuild_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(
                    handle_first_party_proxy_rebuild(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // Shared fallback dispatch: routes to tsjs (GET only), integration proxy, or publisher.
        async fn dispatch(
            state: Arc<AppState>,
            ctx: RequestContext,
            allow_tsjs: bool,
        ) -> Result<Response, EdgeError> {
            let services = build_runtime_services(&ctx);
            let mut req = ctx.into_request();

            // Spin's WASI HTTP bridge does not surface the incoming Host header via
            // IncomingRequest::headers() — the host is only accessible through the
            // spin-full-url synthetic header injected by the Spin runtime. Without a
            // Host header, extract_request_host() returns "" which causes
            // classify_response_route to fall back to BufferedUnmodified and skip the
            // HTML processor entirely (no URL rewriting, no TSJS injection).
            if req.headers().get(header::HOST).is_none() {
                if let Some(host) = req
                    .headers()
                    .get("spin-full-url")
                    .and_then(|v| v.to_str().ok())
                    .and_then(host_from_spin_url)
                {
                    if let Ok(hval) = HeaderValue::from_str(&host) {
                        req.headers_mut().insert(header::HOST, hval);
                    }
                }
            }

            let path = req.uri().path().to_owned();
            let method = req.method().clone();

            let result = if allow_tsjs && path.starts_with("/static/tsjs=") {
                handle_tsjs_dynamic(&req, &state.registry)
            } else if state.registry.has_route(&method, &path) {
                state
                    .registry
                    .handle_proxy(&method, &path, &state.settings, &services, req)
                    .await
                    .unwrap_or_else(|| {
                        Err(Report::new(TrustedServerError::BadRequest {
                            message: format!("Unknown integration route: {path}"),
                        }))
                    })
            } else {
                handle_publisher_request(&state.settings, &state.registry, &services, req)
                    .await
                    .and_then(|pr| resolve_publisher_response(pr, &state.settings, &state.registry))
            };

            Ok(result.unwrap_or_else(|e| http_error(&e)))
        }

        // GET /{*rest} — tsjs, integration proxy, or publisher fallback
        let s = Arc::clone(&state);
        let get_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            dispatch(s, ctx, true)
        };

        // POST /{*rest} — integration proxy or publisher origin fallback
        let s = Arc::clone(&state);
        let post_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            dispatch(s, ctx, false)
        };

        RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))
            .get("/.well-known/trusted-server.json", discovery_handler)
            .post("/verify-signature", verify_handler)
            .post("/admin/keys/rotate", rotate_handler)
            .post("/admin/keys/deactivate", deactivate_handler)
            .post("/auction", auction_handler)
            .get("/first-party/proxy", fp_proxy_handler)
            .get("/first-party/click", fp_click_handler)
            .get("/first-party/sign", fp_sign_get_handler)
            .post("/first-party/sign", fp_sign_post_handler)
            .post("/first-party/proxy-rebuild", fp_rebuild_handler)
            .get("/", get_fallback.clone())
            .post("/", post_fallback.clone())
            .get("/{*rest}", get_fallback)
            .post("/{*rest}", post_fallback)
            .build()
    }
}
