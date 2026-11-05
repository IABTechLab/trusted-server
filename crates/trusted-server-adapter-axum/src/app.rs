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
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};
use trusted_server_core::ec::admin::{
    admin_ec_lookup_not_supported, deny_admin_diagnostic_fallback, handle_admin_eids_lookup,
};
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::kv_backend::EcKvStore;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::{EcContext, EidSyncSource};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{
    IntegrationRegistry, ProxyDispatchInput, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome,
};
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
    #[cfg(feature = "lab-store-check")]
    pub(crate) config_sha256: Option<String>,
    #[cfg(feature = "lab-store-check")]
    pub(crate) public_origin: Option<crate::lab_public_origin::LabPublicOrigin>,
    partners: PartnerRegistry,
    ec_graph: Option<Arc<dyn Fn() -> KvIdentityGraph + Send + Sync>>,
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
    #[cfg(feature = "lab-store-check")]
    if settings
        .handler_for_path(crate::lab_store_check::PATH)?
        .is_none()
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "lab store diagnostic requires existing admin handler coverage".to_owned(),
        }));
    }
    settings
        .ec
        .resolve_storage(trusted_server_core::ec::storage::EcRuntime::Native)?;
    let plan = Arc::new(compile_auction_plan(&settings)?);
    plan.validate_for_target(trusted_server_core::platform::AuctionTargetId::Axum)?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)?;
    let registry = IntegrationRegistry::with_plan(&settings, plan)?;
    let partners = PartnerRegistry::from_config(&settings.ec.partners)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        services,
        #[cfg(feature = "lab-store-check")]
        config_sha256: None,
        #[cfg(feature = "lab-store-check")]
        public_origin: None,
        partners,
        ec_graph: None,
    }))
}

impl AppState {
    pub(crate) fn from_request(ctx: &RequestContext) -> Result<Arc<Self>, EdgeError> {
        ctx.extensions().get::<Arc<Self>>().cloned().ok_or_else(|| {
            EdgeError::service_unavailable("Trusted Server application state is not initialized")
        })
    }

    pub(crate) fn set_ec_store<S>(&mut self, store: S)
    where
        S: EcKvStore + Clone + Send + Sync + 'static,
    {
        self.ec_graph = Some(Arc::new(move || KvIdentityGraph::new(store.clone())));
    }

    fn identity_graph(&self) -> Option<KvIdentityGraph> {
        self.ec_graph.as_ref().map(|factory| factory())
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
    #[cfg(feature = "lab-store-check")]
    if ctx.parts().method == Method::PUT && ctx.parts().uri.path() == crate::lab_store_check::PATH {
        ctx.body_bytes(1_048_576).await?;
    }
    if ctx.parts().method == Method::POST {
        let limit = match ctx.parts().uri.path() {
            trusted_server_core::ec::consent_update::CONSENT_UPDATE_PATH => {
                Some(trusted_server_core::ec::consent_update::MAX_CONSENT_UPDATE_BODY_BYTES)
            }
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
    apply_request_ingress(&state, &mut req)?;
    if req.uri().path() == trusted_server_core::ec::consent_update::CONSENT_UPDATE_PATH {
        req = match trusted_server_core::ec::consent_update::prepare_consent_update(&services, req)
            .await
        {
            Ok(req) => req,
            Err(response) => return Ok(*response),
        };
    }
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

fn apply_request_ingress(_state: &AppState, request: &mut Request) -> Result<(), EdgeError> {
    #[cfg(feature = "lab-store-check")]
    if let Some(origin) = &_state.public_origin {
        return origin.apply(request);
    }
    apply_native_ingress(request)
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
/// malformed EC cookie fails closed before dispatch rather than silently
/// substituting a context. Consent decoding remains owned by core.
fn build_ec_context(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcContext, Report<TrustedServerError>> {
    let geo_info = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    EcContext::read_from_request_with_geo(&state.settings, req, services, geo_info.as_ref())
}

/// Native has no trusted TLS/JA4 fingerprint from CloudFront/ALB. Classify only
/// obvious browser traffic from UA, then require checked client metadata and
/// shared navigation/consent gates before issuance. This is a heuristic, not
/// authentication, and forwarded fingerprint headers are deliberately ignored.
fn native_browser_signals(req: &Request) -> (DeviceSignals, bool) {
    let ua = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let signals = DeviceSignals::derive(ua, None, None);
    let browser = signals.platform_class.is_some()
        && ["Firefox/", "Chrome/", "Safari/"]
            .iter()
            .any(|token| ua.contains(token))
        && !ua.to_ascii_lowercase().contains("bot");
    (signals, browser)
}

fn cookie_value(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_owned())
}

struct NativeEcRequest {
    context: EcContext,
    graph: Option<KvIdentityGraph>,
    browser: bool,
    eids_cookie: Option<String>,
    sharedid_cookie: Option<String>,
}

impl NativeEcRequest {
    fn read(
        state: &AppState,
        services: &RuntimeServices,
        req: &Request,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut context = build_ec_context(state, services, req)?;
        let (signals, browser) = native_browser_signals(req);
        context.set_device_signals(signals);
        Ok(Self {
            context,
            graph: state.identity_graph(),
            browser,
            eids_cookie: cookie_value(req, COOKIE_TS_EIDS),
            sharedid_cookie: cookie_value(req, COOKIE_SHAREDID),
        })
    }

    async fn finalize(mut self, state: &AppState, mut response: Response) -> Response {
        // Revocation must remain writable even for a hardened/non-browser UA.
        let graph = self
            .graph
            .as_ref()
            .filter(|_| self.browser || ec_consent_withdrawn(self.context.consent()));
        ec_finalize_response(
            &state.settings,
            &mut self.context,
            graph,
            &state.partners,
            self.eids_cookie.as_deref(),
            self.sharedid_cookie.as_deref(),
            &mut response,
        )
        .await;
        response
    }
}

#[derive(Clone, Copy)]
enum EcRoute {
    Publisher,
    Auction,
    PageBids,
    ConsentUpdate,
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

    dispatch_ec_route(state, services, req, EcRoute::Publisher).await
}

async fn dispatch_ec_route(
    state: &AppState,
    services: &RuntimeServices,
    mut req: Request,
    route: EcRoute,
) -> Result<Response, Report<TrustedServerError>> {
    let mut ec = NativeEcRequest::read(state, services, &req)?;
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let effects = match state
        .registry
        .filter_request(RequestFilterRegistryInput {
            settings: &state.settings,
            services,
            req: &mut req,
            geo_info: ec.context.geo_info(),
        })
        .await
    {
        Ok(RequestFilterRegistryOutcome::Continue(effects)) => effects,
        Ok(RequestFilterRegistryOutcome::Respond { response, effects }) => {
            let mut response = ec.finalize(state, *response).await;
            effects.apply_to_response(&mut response);
            return Ok(response);
        }
        Err(error) => return Ok(ec.finalize(state, http_error(&error)).await),
    };
    // Borrows of the graph and context remain local to this request. Neither
    // the non-Send graph nor a response snapshot enters shared App extensions.
    let graph = ec.graph.as_ref().filter(|_| ec.browser);
    let auction = AuctionDispatch {
        orchestrator: &state.orchestrator,
        slots: state.settings.creative_opportunity_slots(),
        registry: Some(&state.partners),
    };
    let result = match route {
        EcRoute::ConsentUpdate => Ok(
            trusted_server_core::ec::consent_update::handle_consent_update(&req, ec.graph.as_ref()),
        ),
        EcRoute::Auction => {
            handle_auction(
                &state.settings,
                &state.orchestrator,
                graph,
                Some(&state.partners),
                &mut ec.context,
                services,
                req,
            )
            .await
        }
        EcRoute::PageBids => {
            handle_page_bids(
                &state.settings,
                services,
                graph,
                auction,
                &mut ec.context,
                req,
            )
            .await
        }
        EcRoute::Publisher if state.registry.has_route(&method, &path) => state
            .registry
            .handle_proxy(ProxyDispatchInput {
                method: &method,
                path: &path,
                settings: &state.settings,
                kv: graph,
                allow_generation: false,
                ec_context: &mut ec.context,
                services,
                req,
            })
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            }),
        EcRoute::Publisher => {
            let navigation = method == Method::GET
                && is_navigation_request(&req)
                && state.settings.asset_route_for_path(&path).is_none();
            if navigation {
                ec.context.set_eid_sync_source(EidSyncSource::Navigation);
            }
            let eligible = navigation && ec.browser && graph.is_some();
            if eligible
                && let Err(error) = ec.context.generate_if_needed(&state.settings, graph).await
            {
                log::warn!("native EC generation failed: {error:?}");
            }
            match handle_publisher_request(
                &state.settings,
                services,
                graph,
                &mut ec.context,
                auction,
                req,
                EdgeCacheHeader::SMaxageFallback,
            )
            .await
            {
                Ok(publisher_response) => {
                    ec.context.set_recovery_eligible(eligible);
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
                Err(error) => Err(error),
            }
        }
    };
    // Convert handled route errors before finalization, so withdrawal is not
    // lost when an origin/auction fails. There is exactly one finalize call.
    let mut response = ec
        .finalize(state, result.unwrap_or_else(|error| http_error(&error)))
        .await;
    effects.apply_to_response(&mut response);
    Ok(response)
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
    #[cfg(feature = "lab-store-check")]
    LabStoreCheck,
    /// Legacy `/admin/keys/*` aliases — denied locally with 404 so they never
    /// reach the publisher fallback (which would leak admin credentials).
    LegacyAdminDenied,
    Auction,
    PageBids,
    ConsentUpdate,
    Identify,
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

fn named_routes() -> Vec<NamedRoute> {
    vec![
        #[cfg(feature = "lab-store-check")]
        NamedRoute {
            path: "/_ts/admin/lab/store-check",
            primary_methods: LEGACY_ADMIN_DENY_METHODS,
            handler: NamedRouteHandler::LabStoreCheck,
        },
        NamedRoute {
            path: trusted_server_core::ec::consent_update::CONSENT_UPDATE_PATH,
            primary_methods: &[
                Method::GET,
                Method::POST,
                Method::HEAD,
                Method::OPTIONS,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::TRACE,
                Method::CONNECT,
            ],
            handler: NamedRouteHandler::ConsentUpdate,
        },
        NamedRoute {
            path: "/_ts/api/v1/identify",
            primary_methods: &[Method::GET, Method::OPTIONS],
            handler: NamedRouteHandler::Identify,
        },
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
                        // Enabling native operator EC diagnostics is separate
                        // from the browser/server lifecycle.
                        Ok(admin_ec_lookup_not_supported())
                    }
                    #[cfg(feature = "lab-store-check")]
                    NamedRouteHandler::LabStoreCheck => {
                        crate::lab_store_check::handle(
                            &services,
                            state.config_sha256.as_deref(),
                            req,
                            state.identity_graph(),
                        )
                        .await
                    }
                    NamedRouteHandler::AdminEidsLookup => {
                        handle_admin_eids_lookup(&state.partners, &req)
                    }
                    NamedRouteHandler::LegacyAdminDenied => Ok(legacy_admin_alias_denied()),
                    NamedRouteHandler::ConsentUpdate => {
                        dispatch_ec_route(&state, &services, req, EcRoute::ConsentUpdate).await
                    }
                    NamedRouteHandler::Identify => {
                        // Read-only identity lookup never enters generation or
                        // response finalization, including cookie ingestion.
                        if req.method() == Method::OPTIONS {
                            cors_preflight_identify(&state.settings, &req)
                        } else {
                            let context = build_ec_context(&state, &services, &req)?;
                            let graph = state.identity_graph().ok_or_else(|| {
                                Report::new(TrustedServerError::KvStore {
                                    store_name: "native EC".to_owned(),
                                    message: "native EC provider is not selected".to_owned(),
                                })
                            })?;
                            handle_identify(
                                &state.settings,
                                &graph,
                                &state.partners,
                                &req,
                                &context,
                            )
                            .await
                        }
                    }
                    NamedRouteHandler::Auction => {
                        dispatch_ec_route(&state, &services, req, EcRoute::Auction).await
                    }
                    NamedRouteHandler::PageBids => {
                        // SPA re-auction endpoint. `OPTIONS` is a CORS preflight
                        // for this side-effecting GET and is always denied so the
                        // GET handler's `X-TSJS-Page-Bids` gate stays trustworthy.
                        if req.method() == Method::OPTIONS {
                            Ok(page_bids_preflight_denied())
                        } else {
                            dispatch_ec_route(&state, &services, req, EcRoute::PageBids).await
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
            #[cfg(feature = "celld-kv")]
            kv: Some(StoreMetadata {
                default: crate::kv::STATE_STORE_ID,
                ids: &[crate::kv::STATE_STORE_ID],
            }),
            #[cfg(not(feature = "celld-kv"))]
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

    /// Builds a router with an explicit live conditional EC store.
    ///
    /// This seam uses the same request-local graph and lifecycle as production.
    /// It never reads provider selection from the environment.
    ///
    /// # Errors
    /// Returns an error when settings or runtime plans cannot initialize.
    ///
    /// # Panics
    /// Panics if a newly built application state unexpectedly has another owner.
    pub fn routes_with_settings_services_and_ec_store<S>(
        settings: Settings,
        services: RuntimeServices,
        store: S,
    ) -> Result<RouterService, Report<TrustedServerError>>
    where
        S: EcKvStore + Clone + Send + Sync + 'static,
    {
        let mut state = build_state_with_services(settings, Some(services))?;
        Arc::get_mut(&mut state)
            .expect("should uniquely own new app state")
            .set_ec_store(store);
        Ok(build_router(Some(state)))
    }
}

#[cfg(all(test, feature = "lab-store-check"))]
#[path = "lab_public_origin_tests.rs"]
mod lab_public_origin_tests;

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
