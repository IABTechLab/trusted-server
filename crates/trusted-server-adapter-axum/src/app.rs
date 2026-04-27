use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response, StatusCode, header};
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
use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
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

        // /admin/keys/rotate and /admin/keys/deactivate
        //
        // Config/secret-store writes are not supported on the Axum dev server
        // (backed by read-only env vars). Exposing these routes and returning 500
        // on the first store write is misleading, so we explicitly return 501.
        let admin_not_supported = |_ctx: RequestContext| async {
            let body = edgezero_core::body::Body::from(
                "Admin key management is not supported on the Axum dev server.\n\
                 Use the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n",
            );
            let mut resp = Response::new(body);
            *resp.status_mut() = StatusCode::NOT_IMPLEMENTED;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            Ok::<Response, EdgeError>(resp)
        };
        let rotate_handler = admin_not_supported;
        let deactivate_handler = admin_not_supported;

        // /auction
        let s = Arc::clone(&state);
        let auction_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(
                    handle_auction(&s.settings, &s.orchestrator, &services, None, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // /first-party/proxy
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

        // GET /{*rest} — tsjs, integration proxy, or publisher fallback
        let s = Arc::clone(&state);
        let get_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                let path = req.uri().path().to_string();
                let method = req.method().clone();

                let result = if path.starts_with("/static/tsjs=") {
                    handle_tsjs_dynamic(&req, &s.registry)
                } else if s.registry.has_route(&method, &path) {
                    s.registry
                        .handle_proxy(&method, &path, &s.settings, &services, req)
                        .await
                        .unwrap_or_else(|| {
                            Err(Report::new(TrustedServerError::BadRequest {
                                message: format!("Unknown integration route: {path}"),
                            }))
                        })
                } else {
                    handle_publisher_request(&s.settings, &s.registry, &services, None, req).await
                };

                Ok(result.unwrap_or_else(|e| http_error(&e)))
            }
        };

        // POST /{*rest} — integration proxy or publisher origin fallback
        let s = Arc::clone(&state);
        let post_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                let path = req.uri().path().to_string();
                let method = req.method().clone();

                let result = if s.registry.has_route(&method, &path) {
                    s.registry
                        .handle_proxy(&method, &path, &s.settings, &services, req)
                        .await
                        .unwrap_or_else(|| {
                            Err(Report::new(TrustedServerError::BadRequest {
                                message: format!("Unknown integration route: {path}"),
                            }))
                        })
                } else {
                    handle_publisher_request(&s.settings, &s.registry, &services, None, req).await
                };

                Ok(result.unwrap_or_else(|e| http_error(&e)))
            }
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
