use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Method, Request, Response, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{AuctionOrchestrator, build_orchestrator};
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    PublisherResponse, buffer_publisher_response, handle_publisher_request, handle_tsjs_dynamic,
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
    build_state_with_settings(settings)
}

/// Build the application state from explicit settings.
///
/// # Errors
///
/// Returns an error when the auction orchestrator or the integration
/// registry fail to initialise.
fn build_state_with_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let orchestrator = build_orchestrator(&settings)?;
    let registry = IntegrationRegistry::new(&settings)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
    }))
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

fn build_per_request_services(ctx: &RequestContext) -> RuntimeServices {
    build_runtime_services(ctx)
}

// ---------------------------------------------------------------------------
// Handler factory
// ---------------------------------------------------------------------------

/// Wraps a core handler function in the standard request-scoped boilerplate:
/// build `RuntimeServices`, extract the `Request`, invoke the handler, and
/// convert any error into an HTTP error response.
///
/// Accepts both sync (`|s, svc, req| { ... }`) and async
/// (`|s, svc, req| async move { ... }`) closures.
type BoxedHandlerFuture = Pin<Box<dyn Future<Output = Result<Response, EdgeError>>>>;

fn make_handler<F, Fut>(
    state: Arc<AppState>,
    f: F,
) -> impl Fn(RequestContext) -> BoxedHandlerFuture + Clone + 'static
where
    F: Fn(Arc<AppState>, RuntimeServices, Request) -> Fut + Clone + 'static,
    Fut: Future<Output = Result<Response, Report<TrustedServerError>>> + 'static,
{
    move |ctx: RequestContext| {
        let s = Arc::clone(&state);
        let f = f.clone();
        Box::pin(async move {
            let services = build_per_request_services(&ctx);
            let req = ctx.into_request();
            Ok(f(s, services, req).await.unwrap_or_else(|e| http_error(&e)))
        })
    }
}

// ---------------------------------------------------------------------------
// Publisher response helper
// ---------------------------------------------------------------------------

/// Collapse a [`PublisherResponse`] into a plain [`Response`].
///
/// Delegates to the shared [`buffer_publisher_response`], which enforces
/// `settings.publisher.max_buffered_body_bytes`, then removes any
/// `Transfer-Encoding` header since the buffered body is no longer chunked.
fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    settings: &Settings,
    registry: &IntegrationRegistry,
) -> Result<Response, Report<TrustedServerError>> {
    let mut response = buffer_publisher_response(publisher_response, settings, registry)?;
    response.headers_mut().remove(header::TRANSFER_ENCODING);
    Ok(response)
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

/// HTTP methods the publisher fallback proxies, mirroring the Axum/Fastly
/// adapters so a transparent edge proxy handles HEAD, CORS preflights, and
/// non-GET/POST API calls rather than rejecting them.
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

    let mut router = RouterService::builder().middleware(FinalizeResponseMiddleware::new(
        Arc::new(Settings::default()),
    ));
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

        build_router(&state)
    }
}

impl TrustedServerApp {
    /// Build the full application router from explicit settings.
    ///
    /// Testing seam: cross-adapter parity tests use this to drive the router
    /// with known-good settings instead of the baked `get_settings()` result,
    /// whose embedded placeholder secrets fail validation by design.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or the integration
    /// registry fail to initialise.
    pub fn routes_with_settings(
        settings: Settings,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_settings(settings)?;
        Ok(build_router(&state))
    }
}

fn build_router(state: &Arc<AppState>) -> RouterService {
    {
        let state = Arc::clone(state);

        // Shared fallback dispatch: routes to tsjs (GET only), integration proxy, or publisher.
        async fn dispatch(
            state: Arc<AppState>,
            ctx: RequestContext,
        ) -> Result<Response, EdgeError> {
            let services = build_per_request_services(&ctx);
            let req = ctx.into_request();
            let path = req.uri().path().to_owned();
            let method = req.method().clone();
            // tsjs assets are served for GET only, matching the Axum/Fastly adapters.
            let allow_tsjs = method == Method::GET;

            let result = if allow_tsjs && path.starts_with("/static/tsjs=") {
                handle_tsjs_dynamic(&req, &state.registry)
            } else if state.registry.has_route(&method, &path) {
                let mut ec_context = EcContext::default();
                state
                    .registry
                    .handle_proxy(ProxyDispatchInput {
                        method: &method,
                        path: &path,
                        settings: &state.settings,
                        kv: None,
                        ec_context: &mut ec_context,
                        services: &services,
                        req,
                    })
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

        let fallback = {
            let s = Arc::clone(&state);
            move |ctx: RequestContext| {
                let s = Arc::clone(&s);
                dispatch(s, ctx)
            }
        };

        let mut router = RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))
            .get(
                "/.well-known/trusted-server.json",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_trusted_server_discovery(&s.settings, &services, req)
                }),
            )
            .post(
                "/verify-signature",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_verify_signature(&s.settings, &services, req)
                }),
            )
            // Canonical admin key routes. These match `Settings::ADMIN_ENDPOINTS`
            // and the production basic-auth handler regex (`^/_ts/admin`), so they
            // are auth-gated under a production-shaped config.
            //
            // The legacy non-`/_ts` aliases (`/admin/keys/*`) are deliberately not
            // registered: the production handler regex `^/_ts/admin` does not match
            // them, so registering them as admin routes would expose the key
            // handlers to unauthenticated callers. Unrouted, they fall through to
            // the publisher fallback like any other unknown path.
            .post(
                "/_ts/admin/keys/rotate",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_rotate_key(&s.settings, &services, req)
                }),
            )
            .post(
                "/_ts/admin/keys/deactivate",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_deactivate_key(&s.settings, &services, req)
                }),
            )
            .post(
                "/auction",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    let ec_context = EcContext::default();
                    handle_auction(
                        &s.settings,
                        &s.orchestrator,
                        None,
                        None,
                        &ec_context,
                        &services,
                        req,
                    )
                    .await
                }),
            )
            .get(
                "/first-party/proxy",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy(&s.settings, &services, req).await
                }),
            )
            .get(
                "/first-party/click",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_click(&s.settings, &services, req).await
                }),
            )
            .get(
                "/first-party/sign",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_sign(&s.settings, &services, req).await
                }),
            )
            .post(
                "/first-party/sign",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_sign(&s.settings, &services, req).await
                }),
            )
            .post(
                "/first-party/proxy-rebuild",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_rebuild(&s.settings, &services, req).await
                }),
            );

        for method in publisher_fallback_methods() {
            router = router.route("/", method.clone(), fallback.clone());
            router = router.route("/{*rest}", method, fallback.clone());
        }

        router.build()
    }
}
