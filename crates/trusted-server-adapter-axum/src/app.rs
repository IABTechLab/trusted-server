use core::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::app::{Hooks, StoreMetadata, StoresMetadata};
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, Response, StatusCode, header,
};
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::config_payload::{DEFAULT_CONFIG_STORE_ID, DEFAULT_SECRET_STORE_ID};
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::admin::{
    admin_ec_lookup_not_supported, deny_admin_diagnostic_fallback, handle_admin_eids_lookup,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, buffer_publisher_response_async,
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, page_bids_preflight_denied,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;

use trusted_server_core::platform::RuntimeServices;

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::{apply_native_ingress, build_runtime_services};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    pub(crate) settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    services: Option<RuntimeServices>,
}

/// Build the application state from explicit settings.
///
/// # Errors
///
/// Returns an error when the auction orchestrator or the integration
/// registry fail to initialise.
pub(crate) fn build_state_with_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_services(settings, None)
}

fn build_state_with_services(
    settings: Settings,
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(&settings)?);
    plan.validate_for_target(trusted_server_core::platform::AuctionTargetId::Axum)?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)?;
    let registry = IntegrationRegistry::with_plan(&settings, plan)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        services,
    }))
}

impl AppState {
    pub(crate) fn from_request(ctx: &RequestContext) -> Result<Arc<Self>, EdgeError> {
        ctx.extensions().get::<Arc<Self>>().cloned().ok_or_else(|| {
            EdgeError::service_unavailable("Trusted Server application state is not initialized")
        })
    }

    fn services_for_request(&self, ctx: &RequestContext) -> Result<RuntimeServices, EdgeError> {
        if let Some(services) = &self.services {
            return Ok(services.clone());
        }
        build_runtime_services(ctx).map_err(|error| {
            log::error!("native runtime services unavailable: {error:?}");
            EdgeError::service_unavailable("initialized native runtime services are unavailable")
        })
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

/// Builds the local `404 Not Found` returned for legacy `/admin/keys/*`
/// aliases on the Axum dev server.
///
/// These non-`/_ts` aliases are not matched by the `^/_ts/admin` basic-auth
/// handler, so they fail closed locally rather than fall through to the
/// publisher fallback — which would forward the caller's `Authorization` header
/// and key-management payload to the origin, leaking admin credentials.
fn legacy_admin_alias_denied() -> Response {
    let mut response = Response::new(edgezero_core::body::Body::from("Not found\n"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

// ---------------------------------------------------------------------------
// Shared handler executor
// ---------------------------------------------------------------------------

async fn execute_handler<F, Fut>(ctx: RequestContext, handler: F) -> Result<Response, EdgeError>
where
    F: FnOnce(Arc<AppState>, RuntimeServices, Request) -> Fut,
    Fut: Future<Output = Result<Response, Report<TrustedServerError>>>,
{
    let state = AppState::from_request(&ctx)?;
    let services = state.services_for_request(&ctx)?;
    if ctx.parts().method == Method::POST {
        let limit = match ctx.parts().uri.path() {
            "/verify-signature" => {
                Some(trusted_server_core::request_signing::endpoints::VERIFY_MAX_BODY_BYTES)
            }
            "/auction" => Some(trusted_server_core::auction::endpoints::MAX_AUCTION_BODY_SIZE),
            "/first-party/sign" => Some(trusted_server_core::proxy::SIGN_MAX_BODY_BYTES),
            "/first-party/proxy-rebuild" => {
                Some(trusted_server_core::proxy::REBUILD_MAX_BODY_BYTES)
            }
            _ => None,
        };
        if let Some(limit) = limit {
            ctx.body_bytes(limit).await?;
        }
    }
    let mut req = ctx.into_request()?;
    apply_native_ingress(&mut req)?;
    if let Err(error) = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
        &state.settings,
        &mut req,
    ) {
        return Ok(http_error(&error));
    }
    Ok(handler(state, services, req)
        .await
        .unwrap_or_else(|e| http_error(&e)))
}

// ---------------------------------------------------------------------------
// EC context
// ---------------------------------------------------------------------------

/// Builds the geo-aware [`EcContext`] for consent-gated endpoints (`/auction`,
/// `/_ts/page-bids`, and the publisher fallback).
///
/// Mirrors the Fastly entry point: `EcContext::default()` leaves jurisdiction
/// Unknown, which fails the auction consent gate closed even for consented
/// users. Geo comes from the platform (a no-op on the local Axum dev server, so
/// jurisdiction stays Unknown there unless the request carries TCF consent). A
/// malformed consent string is logged and falls back to the default
/// (fail-closed) context rather than being silently swallowed.
fn build_ec_context(state: &AppState, services: &RuntimeServices, req: &Request) -> EcContext {
    let geo_info = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    EcContext::read_from_request_with_geo(&state.settings, req, services, geo_info.as_ref())
        .unwrap_or_else(|e| {
            log::warn!("EC context read failed: {e:?}");
            EcContext::default()
        })
}

// ---------------------------------------------------------------------------
// Fallback dispatcher (tsjs / integration proxy / publisher)
// ---------------------------------------------------------------------------

async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    if let Some(response) = deny_admin_diagnostic_fallback(&req) {
        return Ok(response);
    }

    trusted_server_core::integrations::gpt_diagnostics::prepare_request(&state.settings, &mut req)?;
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if method == Method::GET && path.starts_with("/static/tsjs=") {
        return handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SMaxageFallback);
    }

    if state.registry.has_route(&method, &path) {
        let mut ec_context = EcContext::default();
        return state
            .registry
            .handle_proxy(ProxyDispatchInput {
                method: &method,
                path: &path,
                settings: &state.settings,
                kv: None,
                ec_context: &mut ec_context,
                services,
                req,
            })
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            });
    }

    // Run the server-side auction with the configured creative-opportunity
    // slots; `handle_publisher_request` matches them against the request path.
    let mut ec_context = build_ec_context(state, services, &req);
    let auction = AuctionDispatch {
        orchestrator: &state.orchestrator,
        slots: state.settings.creative_opportunity_slots(),
        registry: None,
    };
    let publisher_response = handle_publisher_request(
        &state.settings,
        services,
        None,
        &mut ec_context,
        auction,
        req,
        EdgeCacheHeader::SMaxageFallback,
    )
    .await?;
    // Async finalize so the dispatched auction is collected and its bids are
    // injected before `</body>` (the sync buffer path would drop them).
    buffer_publisher_response_async(
        publisher_response,
        &method,
        &state.settings,
        &state.registry,
        &state.orchestrator,
        services,
    )
    .await
}

fn fallback_handler() -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        Box::pin(execute_handler(ctx, |state, services, req| async move {
            dispatch_fallback(&state, &services, req).await
        }))
    }
}

// ---------------------------------------------------------------------------
// Named route table
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum NamedRouteHandler {
    TrustedServerDiscovery,
    VerifySignature,
    AdminNotSupported,
    CachePurgeNotSupported,
    AdminEcNotSupported,
    AdminEidsLookup,
    /// Legacy `/admin/keys/*` aliases — denied locally with 404 so they never
    /// reach the publisher fallback (which would leak admin credentials).
    LegacyAdminDenied,
    Auction,
    PageBids,
    FirstPartyProxy,
    FirstPartyClick,
    FirstPartySign,
    FirstPartyProxyRebuild,
}

struct NamedRoute {
    path: &'static str,
    primary_methods: &'static [Method],
    handler: NamedRouteHandler,
}

const LEGACY_ADMIN_DENY_METHODS: &[Method] = &[
    Method::GET,
    Method::POST,
    Method::HEAD,
    Method::OPTIONS,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
];

fn named_routes() -> [NamedRoute; 17] {
    [
        NamedRoute {
            path: "/.well-known/trusted-server.json",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::TrustedServerDiscovery,
        },
        NamedRoute {
            path: "/verify-signature",
            primary_methods: &[Method::POST],
            handler: NamedRouteHandler::VerifySignature,
        },
        // Canonical admin key routes. These match `Settings::ADMIN_ENDPOINTS`
        // and the production basic-auth handler regex (`^/_ts/admin`), so they
        // are auth-gated under a production-shaped config.
        NamedRoute {
            path: "/_ts/admin/keys/rotate",
            primary_methods: &[Method::POST],
            handler: NamedRouteHandler::AdminNotSupported,
        },
        NamedRoute {
            path: "/_ts/admin/keys/deactivate",
            primary_methods: &[Method::POST],
            handler: NamedRouteHandler::AdminNotSupported,
        },
        // Every method, for the same reason as the Fastly adapter: a method this route
        // does not claim falls through to the publisher with the caller's `Authorization`
        // header still attached.
        NamedRoute {
            path: "/_ts/admin/cache/purge",
            primary_methods: LEGACY_ADMIN_DENY_METHODS,
            handler: NamedRouteHandler::CachePurgeNotSupported,
        },
        // Admin EC lookup routes. Registered explicitly (like the key routes
        // above) so they never fall through to the publisher fallback, and
        // they match `Settings::ADMIN_ENDPOINTS` for auth coverage.
        NamedRoute {
            path: "/_ts/admin/ec",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::AdminEcNotSupported,
        },
        NamedRoute {
            path: "/_ts/admin/ec/{id}",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::AdminEcNotSupported,
        },
        // Admin EIDs echo: pure request inspection (no KV), so the dev
        // server serves the real handler.
        NamedRoute {
            path: "/_ts/admin/eids",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::AdminEidsLookup,
        },
        // The legacy non-`/_ts` aliases (`/admin/keys/*`) are denied locally with
        // a 404, matching the Fastly and Cloudflare adapters: the production
        // basic-auth handler regex `^/_ts/admin` does not match them, and letting
        // any publisher-fallback method fall through would forward the caller's
        // `Authorization` header and key-management payload to the origin,
        // leaking admin credentials.
        NamedRoute {
            path: "/admin/keys/rotate",
            primary_methods: LEGACY_ADMIN_DENY_METHODS,
            handler: NamedRouteHandler::LegacyAdminDenied,
        },
        NamedRoute {
            path: "/admin/keys/deactivate",
            primary_methods: LEGACY_ADMIN_DENY_METHODS,
            handler: NamedRouteHandler::LegacyAdminDenied,
        },
        NamedRoute {
            path: "/auction",
            primary_methods: &[Method::POST],
            handler: NamedRouteHandler::Auction,
        },
        // GET runs the SPA re-auction; OPTIONS is denied in-handler as a CORS
        // preflight guard for this side-effecting endpoint.
        NamedRoute {
            path: PAGE_BIDS_PATH,
            primary_methods: &[Method::GET, Method::OPTIONS],
            handler: NamedRouteHandler::PageBids,
        },
        // Deprecated double-underscore alias, kept so tsjs bundles served before
        // the `/_ts/page-bids` rename keep getting ads on SPA navigations until
        // they age out of browser caches. See `PAGE_BIDS_LEGACY_PATH`.
        NamedRoute {
            path: PAGE_BIDS_LEGACY_PATH,
            primary_methods: &[Method::GET, Method::OPTIONS],
            handler: NamedRouteHandler::PageBids,
        },
        NamedRoute {
            path: "/first-party/proxy",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::FirstPartyProxy,
        },
        NamedRoute {
            path: "/first-party/click",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::FirstPartyClick,
        },
        NamedRoute {
            path: "/first-party/sign",
            primary_methods: &[Method::GET, Method::POST],
            handler: NamedRouteHandler::FirstPartySign,
        },
        NamedRoute {
            path: "/first-party/proxy-rebuild",
            // GET serves the click guard's navigation fallback: the creative
            // iframe is an opaque origin (sandbox without `allow-same-origin`),
            // so its JSON POST is blocked by CORS and the guard navigates here
            // for a 302 instead.
            primary_methods: &[Method::GET, Method::POST],
            handler: NamedRouteHandler::FirstPartyProxyRebuild,
        },
    ]
}

fn named_route_handler(
    handler: NamedRouteHandler,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        Box::pin(execute_handler(
            ctx,
            move |state, services, req| async move {
                match handler {
                    NamedRouteHandler::TrustedServerDiscovery => {
                        handle_trusted_server_discovery(&state.settings, &services, req)
                    }
                    NamedRouteHandler::VerifySignature => {
                        handle_verify_signature(&state.settings, &services, req)
                    }
                    NamedRouteHandler::CachePurgeNotSupported => {
                        // The Axum dev server has no template cache to purge. 501 rather
                        // than a fallthrough 404, so a CMS webhook can tell "not supported
                        // here" from "endpoint does not exist".
                        let body = edgezero_core::body::Body::from(
                            "Template cache purge is not supported on the Axum dev server.\n\
                             Use the Fastly adapter (via Viceroy or deployed) to purge.\n",
                        );
                        let mut resp = Response::new(body);
                        *resp.status_mut() = StatusCode::NOT_IMPLEMENTED;
                        resp.headers_mut().insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/plain; charset=utf-8"),
                        );
                        Ok(resp)
                    }
                    NamedRouteHandler::AdminNotSupported => {
                        // Native snapshots are immutable. Returning 501 is clearer
                        // than failing on the first management write.
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
                        Ok(resp)
                    }
                    NamedRouteHandler::AdminEcNotSupported => {
                        // The EC identity graph is Fastly KV backed; the Axum
                        // dev server has no store to read.
                        Ok(admin_ec_lookup_not_supported())
                    }
                    NamedRouteHandler::AdminEidsLookup => {
                        let partner_registry =
                            PartnerRegistry::from_config(&state.settings.ec.partners)?;
                        handle_admin_eids_lookup(&partner_registry, &req)
                    }
                    NamedRouteHandler::LegacyAdminDenied => Ok(legacy_admin_alias_denied()),
                    NamedRouteHandler::Auction => {
                        // Build the geo-aware EC context so the auction consent
                        // gate sees the caller's jurisdiction — `EcContext::default()`
                        // fails it closed for consented users.
                        let mut ec_context = build_ec_context(&state, &services, &req);
                        handle_auction(
                            &state.settings,
                            &state.orchestrator,
                            None,
                            None,
                            &mut ec_context,
                            &services,
                            req,
                        )
                        .await
                    }
                    NamedRouteHandler::PageBids => {
                        // SPA re-auction endpoint. `OPTIONS` is a CORS preflight
                        // for this side-effecting GET and is always denied so the
                        // GET handler's `X-TSJS-Page-Bids` gate stays trustworthy.
                        if req.method() == Method::OPTIONS {
                            Ok(page_bids_preflight_denied())
                        } else {
                            let mut ec_context = build_ec_context(&state, &services, &req);
                            let auction = AuctionDispatch {
                                orchestrator: &state.orchestrator,
                                slots: state.settings.creative_opportunity_slots(),
                                registry: None,
                            };
                            handle_page_bids(
                                &state.settings,
                                &services,
                                None,
                                auction,
                                &mut ec_context,
                                req,
                            )
                            .await
                        }
                    }
                    NamedRouteHandler::FirstPartyProxy => {
                        handle_first_party_proxy(&state.settings, &services, req).await
                    }
                    NamedRouteHandler::FirstPartyClick => {
                        handle_first_party_click(&state.settings, &services, req).await
                    }
                    NamedRouteHandler::FirstPartySign => {
                        handle_first_party_proxy_sign(&state.settings, &services, req).await
                    }
                    NamedRouteHandler::FirstPartyProxyRebuild => {
                        handle_first_party_proxy_rebuild(&state.settings, &services, req).await
                    }
                }
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Startup error fallback
// ---------------------------------------------------------------------------

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

/// Only explicit-settings test routers capture state. Production state is
/// inserted into the framework App by the prepared-store initializer.
struct ExplicitStateMiddleware(Arc<AppState>);

#[async_trait(?Send)]
impl Middleware for ExplicitStateMiddleware {
    async fn handle(&self, mut ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        ctx.extensions_mut().insert(Arc::clone(&self.0));
        next.run(ctx).await
    }
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
        build_router(None)
    }

    fn stores() -> StoresMetadata {
        StoresMetadata {
            config: Some(StoreMetadata {
                default: DEFAULT_CONFIG_STORE_ID,
                ids: &[DEFAULT_CONFIG_STORE_ID],
            }),
            secrets: Some(StoreMetadata {
                default: DEFAULT_SECRET_STORE_ID,
                ids: &[DEFAULT_SECRET_STORE_ID],
            }),
            kv: None,
        }
    }

    fn owns_logging() -> bool {
        true
    }
}

impl TrustedServerApp {
    /// Build the full application router from explicit settings.
    ///
    /// Testing seam with unavailable network/store dependencies. Tests needing
    /// those dependencies must use [`Self::routes_with_settings_and_services`].
    /// This does not load prepared stores or read legacy environment settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or the integration
    /// registry fail to initialise.
    pub fn routes_with_settings(
        settings: Settings,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_services(
            settings,
            Some(crate::startup::explicit_settings_services()),
        )?;
        Ok(build_router(Some(state)))
    }

    /// Build the full router with explicit settings and runtime services.
    ///
    /// Each request receives a clone of the supplied services, allowing callers
    /// to exercise production routes with deterministic platform dependencies.
    /// The supplied client metadata applies to every request to this router.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or integration registry
    /// cannot be initialized.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let router = TrustedServerApp::routes_with_settings_and_services(settings, services)?;
    /// ```
    pub fn routes_with_settings_and_services(
        settings: Settings,
        services: RuntimeServices,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_services(settings, Some(services))?;
        Ok(build_router(Some(state)))
    }
}

fn build_router(state: Option<Arc<AppState>>) -> RouterService {
    let fallback = fallback_handler();
    let mut router = RouterService::builder();
    if let Some(state) = state {
        router = router.middleware(ExplicitStateMiddleware(state));
    }
    // The test-only injector observes no headers. Sanitation remains the first
    // request-processing middleware so trust credentials cannot leak inward.
    router = router
        .middleware(SanitizeRequestMiddleware::from_request())
        .middleware(FinalizeResponseMiddleware::from_request())
        .middleware(AuthMiddleware::from_request());

    router = router.route("/health", Method::GET, |_ctx: RequestContext| async {
        Ok::<Response, EdgeError>(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
                .body(edgezero_core::body::Body::from("ok"))
                .expect("should build health response"),
        )
    });

    for route in named_routes() {
        for method in route.primary_methods {
            router = router.route(
                route.path,
                method.clone(),
                named_route_handler(route.handler),
            );
        }
        for method in publisher_fallback_methods() {
            if !route.primary_methods.contains(&method) {
                router = router.route(route.path, method, fallback.clone());
            }
        }
    }

    for method in publisher_fallback_methods() {
        router = router.route("/", method.clone(), fallback.clone());
        router = router.route("/{*rest}", method, fallback.clone());
    }

    router.build()
}
