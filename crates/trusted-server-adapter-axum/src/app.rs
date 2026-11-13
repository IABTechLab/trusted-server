use core::future::Future;
use std::sync::Arc;

use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_core::app::{App, Hooks, StoreMetadata, StoresMetadata};
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, RequestParts, Response, StatusCode, header,
};
use edgezero_core::outbound::ResponseDecoding;
use edgezero_core::router::{RouterBuilder, RouterService};
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
use trusted_server_core::edge_app::install_edge_policies;
use trusted_server_core::error::{TrustedServerError, http_error};
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, PublisherOriginOptions,
    buffer_publisher_response_async, handle_page_bids, handle_publisher_request,
    handle_tsjs_dynamic, page_bids_preflight_denied, publisher_input_cap, publisher_owner_error,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;

use trusted_server_core::platform::{ClientInfo, HttpClient, RuntimeServices};
use trusted_server_core::trace::{TraceMetadata, TracePreDispatchHook};

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::build_runtime_services;
use crate::startup::NativeStoreSnapshot;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    settings: Arc<Settings>,
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
fn build_state_with_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_services(settings, None)
}

fn build_state_with_services(
    settings: Settings,
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(&settings)?);
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
    fn services_for_request(&self, ctx: &RequestContext) -> Result<RuntimeServices, EdgeError> {
        self.services
            .clone()
            .map_or_else(|| build_runtime_services(ctx), Ok)
    }
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

async fn execute_handler<F, Fut>(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: F,
) -> Result<Response, EdgeError>
where
    F: FnOnce(Arc<AppState>, RuntimeServices, Request) -> Fut,
    Fut: Future<Output = Result<Response, Report<TrustedServerError>>>,
{
    let services = state.services_for_request(&ctx)?;
    let mut req = ctx.into_request()?;
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
/// users. Geo comes from the immutable managed request snapshot. Off, untrusted
/// or unavailable results remain Unknown unless real consent supplies it. A
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
        integration_registry: &state.registry,
        orchestrator: &state.orchestrator,
        slots: state.settings.creative_opportunity_slots(),
        registry: None,
    };
    // Axum has no runtime memory limit of its own, so the publisher setting
    // alone bounds the origin body this adapter collects before egress.
    let input_cap = publisher_input_cap(&state.settings, u64::MAX);
    let (publisher_response, owner) = handle_publisher_request(
        &state.settings,
        services,
        None,
        &mut ec_context,
        auction,
        req,
        PublisherOriginOptions {
            edge_header: EdgeCacheHeader::SMaxageFallback,
            input_cap: Some(input_cap),
            response_decoding: ResponseDecoding::Original,
        },
    )
    .await?;
    // Async finalize so the dispatched auction is collected and its bids are
    // injected before `</body>` (the sync buffer path would drop them).
    let response = buffer_publisher_response_async(
        publisher_response,
        &method,
        &state.settings,
        &state.registry,
        &state.orchestrator,
        services,
        Some(input_cap),
    )
    .await?;
    owner.attach(response).map_err(publisher_owner_error)
}

fn fallback_handler(
    state: Arc<AppState>,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(execute_handler(
            state,
            ctx,
            |state, services, req| async move { dispatch_fallback(&state, &services, req).await },
        ))
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
    state: Arc<AppState>,
    handler: NamedRouteHandler,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(execute_handler(
            state,
            ctx,
            move |state, services, req| async move {
                match handler {
                    NamedRouteHandler::TrustedServerDiscovery => {
                        handle_trusted_server_discovery(&state.settings, &services, req)
                    }
                    NamedRouteHandler::VerifySignature => {
                        handle_verify_signature(&state.settings, &services, req).await
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
                        // Config/secret-store writes are backed by read-only env vars on the
                        // Axum dev server. Returning 501 is clearer than failing on the first
                        // store write.
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
                                integration_registry: &state.registry,
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
// Publisher fallback methods
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

// ---------------------------------------------------------------------------
// TrustedServerApp
// ---------------------------------------------------------------------------

/// `EdgeZero` [`Hooks`] implementation for the Trusted Server application.
pub struct TrustedServerApp;

impl Hooks for TrustedServerApp {
    fn configure(app: &mut App) -> Result<(), EdgeError> {
        install_edge_policies(app)
    }

    fn name() -> &'static str {
        "TrustedServer"
    }

    fn routes() -> Result<RouterService, EdgeError> {
        // The prepared-store initializer installs the complete router before listening.
        Ok(RouterService::builder().build())
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
    /// Testing seam: integration tests use this to drive the router with
    /// known-good settings instead of the baked `get_settings()` result,
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
        Ok(router_builder(&state).build())
    }

    /// Build the full application router from explicit settings, with `client`
    /// in router state.
    ///
    /// Testing seam: `EdgeZero` hands a client in router state to every
    /// request in place of the adapter's own, so an app served from this
    /// router on `EdgeZero`'s Axum test server starts no real transport.
    /// Production installs no client in router state.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or the integration
    /// registry fail to initialise.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let client = HttpClient::with_client(ScriptedHttpClient::forbidding());
    /// let router = TrustedServerApp::routes_with_settings_and_outbound_client(settings, client)?;
    /// ```
    pub fn routes_with_settings_and_outbound_client(
        settings: Settings,
        client: HttpClient,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_settings(settings)?;
        Ok(router_builder(&state).with_state(client).build())
    }

    /// Build the full router with explicit settings and runtime services.
    ///
    /// Each request receives a clone of the supplied services, allowing callers
    /// to exercise production routes with deterministic platform dependencies.
    /// The supplied client metadata applies to every request to this router.
    /// The services' outbound client also goes into router state, so an app
    /// served from this router starts no real `EdgeZero` transport.
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
        let client = services.outbound_client().clone();
        let state = build_state_with_services(settings, Some(services))?;
        Ok(router_builder(&state).with_state(client).build())
    }
}

/// Registers every route, middleware, and hook for `state` on a router builder.
fn router_builder(state: &Arc<AppState>) -> RouterBuilder {
    let fallback = fallback_handler(Arc::clone(state));
    let trace_state = Arc::clone(state);
    let forwarder_settings = Arc::clone(&state.settings);

    let mut router = RouterService::builder()
        .with_state(Arc::new(NativeStoreSnapshot::empty()))
        .received_head_preparer(move |headers, extensions| {
            if extensions
                .get::<trusted_server_core::forwarder::ForwarderPreparation>()
                .is_none()
            {
                extensions.insert(
                    trusted_server_core::forwarder::ForwarderPreparation::capture(
                        headers,
                        &forwarder_settings,
                    ),
                );
            }
        })
        .pre_dispatch_hook_arc(Arc::new(TracePreDispatchHook::new(
            Arc::clone(&state.settings),
            Arc::new(move |request| trace_metadata(&trace_state, request)),
        )))
        // Outermost middleware: strips the configured trusted-client-IP
        // headers before anything else sees the request. Must stay first —
        // any middleware registered ahead of it would observe the
        // shared-secret authentication header.
        .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))
        .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
        .middleware(AuthMiddleware::new(Arc::clone(&state.settings)));

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
                named_route_handler(Arc::clone(state), route.handler),
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

    router
}

fn trace_metadata(state: &AppState, request: &RequestParts) -> TraceMetadata {
    if let Some(services) = &state.services {
        let client_info = services.client_info().clone();
        let geo = client_info.client_ip.and_then(|client_ip| {
            services.geo().lookup(Some(client_ip)).unwrap_or_else(|_| {
                log::warn!("trace_geo_unavailable");
                None
            })
        });
        return TraceMetadata { client_info, geo };
    }
    TraceMetadata {
        client_info: ClientInfo {
            client_ip: request
                .extensions
                .get::<AxumRequestContext>()
                .and_then(|context| context.remote_addr)
                .map(|peer| peer.ip()),
            ..ClientInfo::default()
        },
        geo: None,
    }
}
#[cfg(test)]
mod trace_dispatch_tests {
    use super::*;
    use edgezero_core::ingress::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use futures::executor::block_on;
    use trusted_server_core::forwarder::{
        FORWARDER_AUTH_HEADER, ForwarderPreparation, prepare_trusted_forwarder,
    };
    use trusted_server_core::http_util::RequestInfo;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::{TrustedClientIpConfig, TrustedForwarderConfig};
    use trusted_server_core::trace::TraceTerminalResponse;

    fn trace_settings(enabled: bool) -> Settings {
        Settings::from_toml(&format!(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "example-user"
            password = "example-password"
            [publisher]
            domain = "publisher.example.com"
            cookie_domain = ".publisher.example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "fictional-proxy-secret"
            [ec]
            passphrase = "fictional-passphrase-at-least-32-bytes"
            [request_signing]
            enabled = false
            config_store_id = "fictional-config"
            secret_store_id = "fictional-secrets"
            [integrations.gpt_diagnostics]
            enabled = true
            trace_page_enabled = {enabled}
        "#
        ))
        .expect("should parse trace settings")
    }

    fn forwarder_settings() -> Settings {
        let mut settings = trace_settings(true);
        settings.trusted_forwarder = Some(TrustedForwarderConfig {
            auth_header: FORWARDER_AUTH_HEADER.to_owned(),
            shared_secret: Redacted::new("fictional-forwarder-secret-0123456789".to_owned()),
        });
        settings
    }

    #[test]
    fn received_head_preparation_preserves_authenticated_origin_after_sanitation() {
        let settings = forwarder_settings();
        let app = App::new(
            TrustedServerApp::routes_with_settings(settings.clone())
                .expect("should build the actual application router"),
        );
        for (credential, duplicate, authority, accepted) in [
            (
                Some("fictional-forwarder-secret-0123456789"),
                None,
                "publisher.example.com:8443",
                true,
            ),
            (None, None, "publisher.example.com:8443", false),
            (
                Some("fictional-wrong-secret"),
                None,
                "publisher.example.com:8443",
                false,
            ),
            (
                Some("fictional-forwarder-secret-0123456789"),
                Some(FORWARDER_AUTH_HEADER),
                "publisher.example.com:8443",
                false,
            ),
            (
                Some("fictional-forwarder-secret-0123456789"),
                Some("x-forwarded-host"),
                "publisher.example.com:8443",
                false,
            ),
            (
                Some("fictional-forwarder-secret-0123456789"),
                Some("x-forwarded-proto"),
                "publisher.example.com:8443",
                false,
            ),
            (
                Some("fictional-forwarder-secret-0123456789"),
                None,
                "other.example.com:8443",
                false,
            ),
        ] {
            let mut request = edgezero_core::http::request_builder()
                .uri("http://upstream.example.com/path?example=1")
                .header(header::HOST, "upstream.example.com")
                .header(header::ORIGIN, "https://browser.example.com")
                .header("x-forwarded-host", authority)
                .header("x-forwarded-proto", "https")
                .body(edgezero_core::body::Body::empty())
                .expect("should build a fictional forwarded request");
            if let Some(credential) = credential {
                request
                    .headers_mut()
                    .insert(FORWARDER_AUTH_HEADER, HeaderValue::from_static(credential));
            }
            if let Some(name) = duplicate {
                let value = request.headers()[name].clone();
                request.headers_mut().append(name, value);
            }
            request.extensions_mut().insert(RequestIngress::new(
                CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                Some(
                    InboundOrigin::parse("http", "upstream.example.com", OriginSource::RuntimeUri)
                        .expect("should construct independent transport evidence"),
                ),
                HeaderFidelity::default(),
            ));
            let (mut parts, body) = request.into_parts();
            let original_headers = parts.headers.clone();
            app.prepare_received_head(&parts.headers, &mut parts.extensions);
            assert!(
                parts.extensions.get::<ForwarderPreparation>().is_some(),
                "should freeze an accepted or rejected decision before native sanitation"
            );
            assert!(
                parts.headers == original_headers,
                "should not mutate received headers"
            );
            for name in [
                FORWARDER_AUTH_HEADER,
                "x-forwarded-host",
                "x-forwarded-proto",
            ] {
                parts.headers.remove(name);
            }
            prepare_trusted_forwarder(&mut parts, &settings);
            let request = Request::from_parts(parts, body);
            let info = RequestInfo::from_request(&request, &ClientInfo::default())
                .expect("should resolve prepared public metadata");
            assert_eq!(
                info.host,
                if accepted {
                    "publisher.example.com:8443"
                } else {
                    "upstream.example.com"
                }
            );
            assert_eq!(info.scheme, if accepted { "https" } else { "http" });
            assert_eq!(request.uri(), "http://upstream.example.com/path?example=1");
            assert_eq!(request.headers()[header::HOST], "upstream.example.com");
            assert_eq!(
                request.headers()[header::ORIGIN],
                "https://browser.example.com"
            );
            let ingress = request
                .extensions()
                .get::<RequestIngress>()
                .expect("should retain transport evidence");
            assert_eq!(
                ingress
                    .origin()
                    .expect("should retain transport origin")
                    .authority(),
                "upstream.example.com"
            );
            assert_eq!(
                ingress.header_fidelity(&header::COOKIE),
                HeaderFidelity::default()
            );
        }
    }

    #[test]
    fn received_head_preparation_does_not_upgrade_an_existing_rejection() {
        let settings = forwarder_settings();
        let app = App::new(
            TrustedServerApp::routes_with_settings(settings.clone())
                .expect("should build the actual application router"),
        );
        let request = edgezero_core::http::request_builder()
            .uri("http://upstream.example.com/path")
            .header(
                FORWARDER_AUTH_HEADER,
                "fictional-forwarder-secret-0123456789",
            )
            .header("x-forwarded-host", "publisher.example.com:8443")
            .header("x-forwarded-proto", "https")
            .body(edgezero_core::body::Body::empty())
            .expect("should build a fictional forwarded request");
        let (mut parts, body) = request.into_parts();
        parts.extensions.insert(ForwarderPreparation::default());
        app.prepare_received_head(&parts.headers, &mut parts.extensions);
        app.prepare_received_head(&parts.headers, &mut parts.extensions);
        prepare_trusted_forwarder(&mut parts, &settings);
        let request = Request::from_parts(parts, body);
        let info = RequestInfo::from_request(&request, &ClientInfo::default())
            .expect("should retain the original rejected decision");
        assert_eq!(info.host, "upstream.example.com");
        assert_eq!(info.scheme, "http");
        assert!(!request.headers().contains_key(FORWARDER_AUTH_HEADER));
    }

    fn router(enabled: bool) -> RouterService {
        TrustedServerApp::routes_with_settings(trace_settings(enabled))
            .expect("should build adapter routes")
    }

    #[tokio::test]
    async fn trace_dispatch_reserves_all_methods_before_ordinary_lifecycle() {
        let router = router(true);
        for (method, path, status) in [
            (Method::GET, "/_ts/trace/state", StatusCode::OK),
            (Method::HEAD, "/_ts/trace", StatusCode::OK),
            (Method::GET, "/_ts/trace/assets/v1.js", StatusCode::OK),
            (Method::POST, "/_ts/trace/enable", StatusCode::FORBIDDEN),
            (
                Method::PATCH,
                "/_ts/trace/state",
                StatusCode::METHOD_NOT_ALLOWED,
            ),
            (
                Method::from_bytes(b"EXAMPLE-METHOD").expect("should parse extension method"),
                "/_ts/trace",
                StatusCode::METHOD_NOT_ALLOWED,
            ),
            (Method::GET, "/_ts/trace/extra", StatusCode::NOT_FOUND),
            (Method::GET, "/%5Fts/trace", StatusCode::BAD_REQUEST),
        ] {
            let head = method == Method::HEAD;
            let request = edgezero_core::http::request_builder()
                .method(method)
                .uri(format!("https://publisher.example.com{path}"))
                .body(edgezero_core::body::Body::empty())
                .expect("should build trace request");
            let response =
                block_on(router.oneshot(request)).expect("should return local trace policy");
            assert_eq!(response.status(), status, "should bypass ordinary dispatch");
            assert!(
                response
                    .extensions()
                    .get::<TraceTerminalResponse>()
                    .is_some(),
                "should mark terminal trace responses"
            );
            assert!(
                !response.headers().contains_key(header::SET_COOKIE),
                "should never manufacture a mutation"
            );
            if head {
                assert_eq!(
                    response
                        .into_body()
                        .into_bytes()
                        .expect("should buffer HEAD")
                        .len(),
                    0,
                    "should remove HEAD bodies"
                );
            }
        }
        let response = block_on(
            self::router(false).oneshot(
                edgezero_core::http::request_builder()
                    .uri("/_ts/trace/state")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build disabled request"),
            ),
        )
        .expect("should reserve disabled namespace");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should hide disabled feature"
        );
        assert!(
            response
                .extensions()
                .get::<TraceTerminalResponse>()
                .is_some(),
            "should harden disabled feature"
        );
    }

    #[tokio::test]
    async fn trace_setup_honors_authenticated_client_ip_and_preserves_runtime_fallback() {
        for (ip_header, auth_header) in [
            ("x-example-client-ip", "x-example-client-auth"),
            ("fastly-client-ip", "x-example-client-auth"),
            ("x-example-client-ip", "x-ts-forwarder-auth"),
            ("x-forwarded-host", "x-example-client-auth"),
        ] {
            for (forwarded_ip, auth, duplicate, configured, expected) in [
                (
                    "198.51.100.7",
                    Some("fictional-client-secret-0123456789"),
                    None,
                    true,
                    "198.51.100.0/24",
                ),
                (
                    "2001:db8:abcd::7",
                    Some("fictional-client-secret-0123456789"),
                    None,
                    true,
                    "2001:db8:abcd::/48",
                ),
                ("198.51.100.7", None, None, true, "192.0.2.0/24"),
                (
                    "198.51.100.7",
                    Some("fictional-wrong-secret"),
                    None,
                    true,
                    "192.0.2.0/24",
                ),
                (
                    "198.51.100.7",
                    Some("fictional-client-secret-0123456789"),
                    Some(auth_header),
                    true,
                    "192.0.2.0/24",
                ),
                (
                    "198.51.100.7",
                    Some("fictional-client-secret-0123456789"),
                    Some(ip_header),
                    true,
                    "192.0.2.0/24",
                ),
                (
                    "198.51.100.7, 203.0.113.7",
                    Some("fictional-client-secret-0123456789"),
                    None,
                    true,
                    "192.0.2.0/24",
                ),
                (
                    "198.51.100.7",
                    Some("fictional-client-secret-0123456789"),
                    None,
                    false,
                    "192.0.2.0/24",
                ),
            ] {
                let mut settings = trace_settings(true);
                if configured {
                    settings.trusted_client_ip = Some(TrustedClientIpConfig {
                        ip_header: ip_header.to_owned(),
                        auth_header: auth_header.to_owned(),
                        shared_secret: Redacted::new(
                            "fictional-client-secret-0123456789".to_owned(),
                        ),
                    });
                }
                let router = TrustedServerApp::routes_with_settings(settings)
                    .expect("should build configured trace routes");
                let mut request = edgezero_core::http::request_builder()
                    .uri("https://publisher.example.com/_ts/trace")
                    .header(ip_header, forwarded_ip)
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build trace setup request");
                if let Some(auth) = auth {
                    request
                        .headers_mut()
                        .insert(auth_header, HeaderValue::from_static(auth));
                }
                if let Some(name) = duplicate {
                    let value = request
                        .headers()
                        .get(name)
                        .expect("should have original header")
                        .clone();
                    request.headers_mut().append(name, value);
                }
                AxumRequestContext::insert(
                    &mut request,
                    AxumRequestContext {
                        remote_addr: Some(
                            "192.0.2.99:4321"
                                .parse()
                                .expect("should parse peer address"),
                        ),
                    },
                );
                let response =
                    block_on(router.oneshot(request)).expect("should render trace setup");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "should accept setup request"
                );
                let bytes = response
                    .into_body()
                    .into_bytes()
                    .expect("should buffer setup");
                let html = String::from_utf8(bytes.to_vec()).expect("should render UTF8 setup");
                assert!(
                    html.contains(expected),
                    "should mask resolved client IP for {ip_header} with {forwarded_ip} and duplicate {duplicate:?}"
                );
                assert!(
                    !html.contains("fictional-client-secret"),
                    "should omit credentials from setup"
                );
            }
        }
    }
}
