//! Full `EdgeZero` application wiring for Trusted Server.
//!
//! Registers all routes from the legacy [`crate::route_request`] into a
//! [`RouterService`], attaches [`FinalizeResponseMiddleware`] (outermost) and
//! [`AuthMiddleware`] (inner), and builds the [`AppState`] once per Wasm
//! instance.
//!
//! # Route inventory
//!
//! | Method | Path pattern | Handler |
//! |--------|-------------|---------|
//! | GET | `/.well-known/trusted-server.json` | [`handle_trusted_server_discovery`] |
//! | POST | `/verify-signature` | [`handle_verify_signature`] |
//! | POST | `/admin/keys/rotate` | [`handle_rotate_key`] |
//! | POST | `/admin/keys/deactivate` | [`handle_deactivate_key`] |
//! | POST | `/auction` | [`handle_auction`] |
//! | GET | `/first-party/proxy` | [`handle_first_party_proxy`] |
//! | GET | `/first-party/click` | [`handle_first_party_click`] |
//! | GET | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/proxy-rebuild` | [`handle_first_party_proxy_rebuild`] |
//! | GET | `/` and `/{*rest}` | tsjs (if `/static/tsjs=` prefix), integration proxy, or publisher fallback |
//! | POST, HEAD, OPTIONS, PUT, PATCH, DELETE | `/` and `/{*rest}` | integration proxy or publisher fallback |
//!
//! # Startup error handling
//!
//! When [`build_state`] fails, [`startup_error_router`] returns a minimal router
//! that responds to all routes with the startup error. This router does **not**
//! attach middleware — startup errors are returned without geo or TS headers.

use core::future::Future;
use std::sync::Arc;

use edgezero_adapter_fastly::FastlyRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{header, HeaderValue, Method, Request, Response};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::{ClientInfo, PlatformKvStore, RuntimeServices};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware};
use crate::platform::{
    FastlyPlatformBackend, FastlyPlatformConfigStore, FastlyPlatformGeo, FastlyPlatformHttpClient,
    FastlyPlatformSecretStore, UnavailableKvStore,
};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once per Wasm instance and shared for its lifetime.
///
/// In Fastly Compute each request spawns a new Wasm instance, so this struct is
/// effectively per-request. It holds pre-parsed settings and all service handles.
pub(crate) struct AppState {
    pub(crate) settings: Arc<Settings>,
    pub(crate) orchestrator: Arc<AuctionOrchestrator>,
    pub(crate) registry: Arc<IntegrationRegistry>,
    pub(crate) kv_store: Arc<dyn PlatformKvStore>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
pub(crate) fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = get_settings()?;

    let orchestrator = build_orchestrator(&settings)?;

    let registry = IntegrationRegistry::new(&settings)?;

    let kv_store = Arc::new(UnavailableKvStore) as Arc<dyn PlatformKvStore>;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        kv_store,
    }))
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Construct per-request [`RuntimeServices`] from the `EdgeZero` request context.
///
/// Extracts the client IP address from the [`FastlyRequestContext`] extension
/// inserted by `edgezero_adapter_fastly::dispatch`. TLS metadata is not
/// available through the `EdgeZero` context so those fields are left empty.
fn build_per_request_services(state: &AppState, ctx: &RequestContext) -> RuntimeServices {
    let client_ip = FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip);

    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(Arc::clone(&state.kv_store))
        .backend(Arc::new(FastlyPlatformBackend))
        .http_client(Arc::new(FastlyPlatformHttpClient))
        .geo(Arc::new(FastlyPlatformGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

fn publisher_fallback_methods() -> [Method; 7] {
    [
        Method::GET,
        Method::POST,
        Method::HEAD,
        Method::OPTIONS,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ]
}

fn uses_dynamic_tsjs_fallback(method: &Method, path: &str) -> bool {
    *method == Method::GET && path.starts_with("/static/tsjs=")
}

async fn execute_handler<F, Fut>(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: F,
) -> Result<Response, EdgeError>
where
    F: FnOnce(Arc<AppState>, RuntimeServices, Request) -> Fut,
    Fut: Future<Output = Result<Response, Report<TrustedServerError>>>,
{
    let services = build_per_request_services(&state, &ctx);
    let req = ctx.into_request();

    Ok(handler(state, services, req)
        .await
        .unwrap_or_else(|e| http_error(&e)))
}

async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if uses_dynamic_tsjs_fallback(&method, &path) {
        return handle_tsjs_dynamic(&req, &state.registry);
    }

    if state.registry.has_route(&method, &path) {
        return state
            .registry
            .handle_proxy(&method, &path, &state.settings, services, req)
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            });
    }

    handle_publisher_request(&state.settings, &state.registry, services, req)
        .await
        .and_then(|pub_response| {
            crate::resolve_publisher_response(pub_response, &state.settings, &state.registry)
        })
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`],
/// mirroring [`crate::http_error_response`] exactly.
///
/// The near-identical function in `main.rs` is intentional: the legacy path
/// uses fastly HTTP types while this path uses `edgezero_core` types.
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

/// Returns a [`RouterService`] that responds to every registered route with the startup error.
///
/// Called when [`build_state`] fails so that request handling degrades to a
/// structured HTTP error response rather than an unrecoverable panic.
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

    let mut router = RouterService::builder();
    for method in publisher_fallback_methods() {
        router = router.route("/", method.clone(), make(Arc::clone(&message)));
        router = router.route("/{*rest}", method, make(Arc::clone(&message)));
    }
    router.build()
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

        // Each named route only selects its core handler; the request/context
        // scaffolding and Report -> HTTP mapping live in execute_handler().
        let s = Arc::clone(&state);
        let discovery_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_trusted_server_discovery(&state.settings, &services, req)
            })
        };

        let s = Arc::clone(&state);
        let verify_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_verify_signature(&state.settings, &services, req)
            })
        };

        let s = Arc::clone(&state);
        let rotate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_rotate_key(&state.settings, &services, req)
            })
        };

        let s = Arc::clone(&state);
        let deactivate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_deactivate_key(&state.settings, &services, req)
            })
        };

        let s = Arc::clone(&state);
        let auction_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_auction(&state.settings, &state.orchestrator, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fp_proxy_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_first_party_proxy(&state.settings, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fp_click_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_first_party_click(&state.settings, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fp_sign_get_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_first_party_proxy_sign(&state.settings, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fp_sign_post_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_first_party_proxy_sign(&state.settings, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fp_rebuild_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                handle_first_party_proxy_rebuild(&state.settings, &services, req).await
            })
        };

        let s = Arc::clone(&state);
        let fallback_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            execute_handler(s, ctx, |state, services, req| async move {
                dispatch_fallback(&state, &services, req).await
            })
        };

        let mut router = RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(
                Arc::clone(&state.settings),
                Arc::new(FastlyPlatformGeo),
            ))
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
            .post("/first-party/proxy-rebuild", fp_rebuild_handler);

        // matchit's `/{*rest}` does not match the bare root `/` — register
        // explicit root routes so `/` reaches the publisher fallback too.
        for method in publisher_fallback_methods() {
            router = router.route("/", method.clone(), fallback_handler.clone());
            router = router.route("/{*rest}", method, fallback_handler.clone());
        }

        router.build()
    }
}

#[cfg(test)]
mod tests {
    use super::{startup_error_router, TrustedServerApp};

    use edgezero_core::app::Hooks as _;
    use edgezero_core::body::Body;
    use edgezero_core::http::{header, request_builder, Method, StatusCode};
    use error_stack::Report;
    use futures::executor::block_on;
    use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
    use trusted_server_core::error::TrustedServerError;

    fn empty_request(method: Method, uri: &str) -> edgezero_core::http::Request {
        request_builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("should build request")
    }

    #[test]
    fn startup_error_router_handles_head_and_options() {
        let report = Report::new(TrustedServerError::BadRequest {
            message: "startup failed".to_string(),
        });
        let router = startup_error_router(&report);

        let head_response = block_on(router.oneshot(empty_request(Method::HEAD, "/")));
        let options_response = block_on(router.oneshot(empty_request(Method::OPTIONS, "/any")));

        assert_eq!(
            head_response.status(),
            StatusCode::BAD_REQUEST,
            "HEAD should use the degraded startup-error response"
        );
        assert_eq!(
            options_response.status(),
            StatusCode::BAD_REQUEST,
            "OPTIONS should use the degraded startup-error response"
        );
        assert_eq!(
            head_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8"),
            "startup errors should stay plain-text for HEAD requests"
        );
        assert_eq!(
            options_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8"),
            "startup errors should stay plain-text for OPTIONS requests"
        );
    }

    #[test]
    fn dynamic_tsjs_fallback_is_get_only() {
        assert!(
            super::uses_dynamic_tsjs_fallback(&Method::GET, "/static/tsjs=tsjs-unified.js"),
            "GET should use the dynamic tsjs shortcut"
        );
        assert!(
            !super::uses_dynamic_tsjs_fallback(&Method::HEAD, "/static/tsjs=tsjs-unified.js"),
            "HEAD should fall through to the publisher/integration fallback"
        );
        assert!(
            !super::uses_dynamic_tsjs_fallback(&Method::OPTIONS, "/static/tsjs=tsjs-unified.js"),
            "OPTIONS should fall through to the publisher/integration fallback"
        );
    }

    // ---------------------------------------------------------------------------
    // Full EdgeZero dispatch-path tests
    // ---------------------------------------------------------------------------

    #[test]
    fn dispatch_auth_rejected_401_carries_finalize_headers() {
        // Verifies FinalizeResponseMiddleware is outermost: an auth-rejected 401
        // must still carry standard TS headers before reaching the client.
        //
        // The embedded trusted-server.toml protects `^/admin` with basic-auth.
        // Sending the request without an Authorization header causes AuthMiddleware
        // to short-circuit with a 401, which then bubbles through
        // FinalizeResponseMiddleware for header injection.
        //
        // This is safe to run without Viceroy: enforce_basic_auth is pure Rust
        // (reads settings + request headers only) and FastlyPlatformGeo.lookup(None)
        // short-circuits without calling any Fastly ABI.
        let router = TrustedServerApp::routes();
        let req = request_builder()
            .method(Method::POST)
            .uri("/admin/keys/rotate")
            .body(Body::empty())
            .expect("should build test request");

        let response = block_on(router.oneshot(req));

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "request without credentials should be rejected"
        );
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_GEO_INFO_AVAILABLE)
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "FinalizeResponseMiddleware must run even for auth-rejected responses"
        );
    }

    #[test]
    fn dispatch_unregistered_method_returns_405_at_router_level() {
        // Documents the known router-level behavior for unregistered HTTP methods:
        // the RouterService returns 405 before the middleware chain runs, so
        // FinalizeResponseMiddleware does not inject TS headers at this layer.
        //
        // The full-system guarantee (TS headers on ALL responses) is maintained
        // by the entry-point finalize wrap in main.rs, which is idempotent for
        // requests that did run through the middleware chain.
        let router = TrustedServerApp::routes();
        let req = request_builder()
            .method(Method::from_bytes(b"TRACE").expect("should parse TRACE"))
            .uri("/")
            .body(Body::empty())
            .expect("should build TRACE request");

        let response = block_on(router.oneshot(req));

        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "unregistered method should return 405 from the router layer"
        );
        assert!(
            response
                .headers()
                .get(HEADER_X_GEO_INFO_AVAILABLE)
                .is_none(),
            "router-level 405 bypasses FinalizeResponseMiddleware; main.rs entry-point covers this"
        );
    }
}
