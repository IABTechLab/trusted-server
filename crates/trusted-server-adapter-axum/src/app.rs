use std::sync::Arc;

use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, Response, StatusCode, header,
};
use edgezero_core::router::RouterService;
use error_stack::{Report, ResultExt as _};
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};
use trusted_server_core::cookies::extract_cookie_value;
use trusted_server_core::ec::admin::{
    deny_admin_diagnostic_fallback, handle_admin_ec_lookup, handle_admin_eids_lookup,
};
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::coordinator::ProviderCoordinator;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::page_load::NavigationPermit;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::{EcContext, EidSyncSource};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{
    IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome,
};
use trusted_server_core::proxy::{
    AssetProxyCachePolicy, handle_asset_proxy_request, handle_first_party_click,
    handle_first_party_proxy, handle_first_party_proxy_rebuild, handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, buffer_publisher_response_async,
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, page_bids_preflight_denied,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::{ProxyAssetRoute, Settings};
use trusted_server_core::settings_data::{
    default_config_key, default_config_store_name, get_settings_from_config_store,
};
use trusted_server_core::tester_cookie::{handle_clear_tester, handle_set_tester};

use trusted_server_core::platform::{ClientInfo, GeoInfo, RuntimeServices};
use trusted_server_core::trace::{TraceMetadata, TracePreDispatchHook};
use trusted_server_identity_providers::{
    build_after_response_coordinator, build_coordinator_with, partner_registry,
};

use crate::ec_kv::{LocalEcKvStore, LocalRateLimiter};
use crate::identity::{
    EcRequestState, build_ec_request_state, dispatch_page_lookups, finalize_navigation,
    prepare_after_response_lookups, spawn_post_response_work,
};
use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::{AxumPlatformConfigStore, AxumPlatformSecretStore, build_runtime_services};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    services: Option<RuntimeServices>,
    /// Partner registry, with the sources of enabled providers marked.
    partner_registry: Arc<PartnerRegistry>,
    /// Coordinator for providers collected before the response, if any.
    page_identity: Option<Arc<ProviderCoordinator>>,
    /// Coordinator for providers collected after the response, if any.
    after_response_identity: Option<Arc<ProviderCoordinator>>,
    /// Process-local EC identity store, present when `ec.ec_store` is set.
    ec_store: Option<LocalEcKvStore>,
    rate_limiter: LocalRateLimiter,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, the integration
/// registry, the partner registry or an enabled identity provider fail to
/// initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let store_name = default_config_store_name();
    let config_key = default_config_key();
    let settings = get_settings_from_config_store(
        &AxumPlatformConfigStore,
        &AxumPlatformSecretStore,
        &store_name,
        &config_key,
        &trusted_server_core::settings_data::default_secret_store_name(),
    )?;
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
    build_state_with_services(settings, None)
}

pub(crate) fn build_state_with_services(
    settings: Settings,
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(&settings)?);
    plan.validate_for_target(trusted_server_core::platform::AuctionTargetId::Axum)?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)?;
    let registry = IntegrationRegistry::with_plan(&settings, plan)?;

    // A provider that cannot run must stop startup: skipping it would quietly
    // hand its sources back to browser ingest.
    let partner_registry = Arc::new(partner_registry(&settings.ec)?);
    let page_identity = build_coordinator_with(&settings, Arc::clone(&partner_registry))
        .change_context(TrustedServerError::Configuration {
            message: "identity provider configuration is invalid".to_owned(),
        })?
        .map(Arc::new);
    let after_response_identity =
        build_after_response_coordinator(&settings, Arc::clone(&partner_registry))
            .change_context(TrustedServerError::Configuration {
                message: "identity provider configuration is invalid".to_owned(),
            })?
            .map(Arc::new);
    let ec_store = settings.ec.ec_store.as_deref().map(|store_name| {
        log::warn!(
            "Axum dev server: EC identity store `{store_name}` is process-local and is lost on exit"
        );
        LocalEcKvStore::new(store_name)
    });

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        services,
        partner_registry,
        page_identity,
        after_response_identity,
        ec_store,
        rate_limiter: LocalRateLimiter::default(),
    }))
}

impl AppState {
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn partner_registry(&self) -> &PartnerRegistry {
        &self.partner_registry
    }

    pub(crate) fn page_identity(&self) -> Option<&Arc<ProviderCoordinator>> {
        self.page_identity.as_ref()
    }

    pub(crate) fn after_response_identity(&self) -> Option<&Arc<ProviderCoordinator>> {
        self.after_response_identity.as_ref()
    }

    pub(crate) fn rate_limiter(&self) -> &LocalRateLimiter {
        &self.rate_limiter
    }

    /// Builds an identity graph over the shared process-local store.
    pub(crate) fn identity_graph(&self) -> Option<KvIdentityGraph> {
        self.ec_store.clone().map(KvIdentityGraph::new)
    }

    /// Partner registry for auction EID enrichment, `None` when empty.
    fn auction_partner_registry(&self) -> Option<&PartnerRegistry> {
        (!self.partner_registry.is_empty()).then_some(&*self.partner_registry)
    }

    fn services_for_request(&self, ctx: &RequestContext) -> RuntimeServices {
        self.services
            .clone()
            .unwrap_or_else(|| build_runtime_services(ctx))
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
// EC lifecycle helpers
// ---------------------------------------------------------------------------

/// Result of the pre-route integration request-filter step.
enum PreRoute {
    /// A filter responded or failed; return this response without running
    /// the route handler.
    ShortCircuit {
        response: Response,
        effects: RequestFilterEffects,
    },
    /// Continue to the route handler, carrying the accumulated effects.
    Continue { effects: RequestFilterEffects },
}

/// Runs the integration request filters, mirroring the Fastly adapter.
async fn run_pre_route_filters(
    state: &AppState,
    services: &RuntimeServices,
    req: &mut Request,
    geo_info: Option<&GeoInfo>,
) -> PreRoute {
    match state
        .registry
        .filter_request(RequestFilterRegistryInput {
            settings: &state.settings,
            services,
            req,
            geo_info,
        })
        .await
    {
        Ok(RequestFilterRegistryOutcome::Continue(effects)) => PreRoute::Continue { effects },
        Ok(RequestFilterRegistryOutcome::Respond { response, effects }) => PreRoute::ShortCircuit {
            response: *response,
            effects,
        },
        Err(report) => {
            log::error!("Failed to run integration request filters: {report:?}");
            PreRoute::ShortCircuit {
                response: http_error(&report),
                effects: RequestFilterEffects::default(),
            }
        }
    }
}

/// Stores non-empty request-filter effects on the response.
///
/// [`FinalizeResponseMiddleware`] applies them after the standard headers,
/// matching the Fastly entry point.
fn attach_request_filter_effects(response: &mut Response, effects: RequestFilterEffects) {
    if !effects.response_headers.is_empty() {
        response.extensions_mut().insert(effects);
    }
}

/// Runs EC finalization on a non-publisher response and spawns the
/// post-response work, as the Fastly entry point does for every response
/// that carries EC finalize state.
async fn finish_ec(
    state: &Arc<AppState>,
    services: &RuntimeServices,
    mut ec: EcRequestState,
    mut response: Response,
    effects: RequestFilterEffects,
) -> Response {
    finalize_navigation(state, services, &mut ec, None, &mut response).await;
    attach_request_filter_effects(&mut response, effects);
    drop(spawn_post_response_work(
        Arc::clone(state),
        services.clone(),
        &mut ec,
        None,
    ));
    response
}

// ---------------------------------------------------------------------------
// Fallback dispatcher (tsjs / integration proxy / asset / publisher)
// ---------------------------------------------------------------------------

async fn dispatch_fallback(
    state: &Arc<AppState>,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    if let Some(response) = deny_admin_diagnostic_fallback(&req) {
        return Ok(response);
    }

    trusted_server_core::integrations::gpt_diagnostics::prepare_request(&state.settings, &mut req)?;
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let mut ec = build_ec_request_state(state, services, &req);
    if let Some(report) = ec.setup_error.take() {
        let response = http_error(&report);
        return Ok(finish_ec(
            state,
            services,
            ec,
            response,
            RequestFilterEffects::default(),
        )
        .await);
    }

    let geo_info = ec.geo_info.clone();
    let effects = match run_pre_route_filters(state, services, &mut req, geo_info.as_ref()).await {
        PreRoute::ShortCircuit { response, effects } => {
            return Ok(finish_ec(state, services, ec, response, effects).await);
        }
        PreRoute::Continue { effects } => effects,
    };

    if method == Method::GET && path.starts_with("/static/tsjs=") {
        let response = handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SMaxageFallback)
            .unwrap_or_else(|report| http_error(&report));
        return Ok(finish_ec(state, services, ec, response, effects).await);
    }

    if state.registry.has_route(&method, &path) {
        let response = state
            .registry
            .handle_proxy(ProxyDispatchInput {
                method: &method,
                path: &path,
                settings: &state.settings,
                services,
                req,
            })
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            })
            .unwrap_or_else(|report| http_error(&report));
        return Ok(finish_ec(state, services, ec, response, effects).await);
    }

    let matched_asset_route = matches!(method, Method::GET | Method::HEAD)
        .then(|| state.settings.asset_route_for_path(&path))
        .flatten();
    if let Some(asset_route) = matched_asset_route {
        return Ok(dispatch_asset_fallback(state, services, req, asset_route, effects).await);
    }

    // Mint and look up only on document navigations by recognised browsers;
    // subresource requests may lack consent signals such as Sec-GPC.
    let is_navigation = is_navigation_request(&req);
    let mut has_permit = false;
    if is_navigation {
        ec.ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        // The page-load write permit uses the stricter Fetch Metadata test;
        // requests without it never write KV.
        if let Some(permit) = NavigationPermit::classify(&req) {
            ec.ec_context.set_navigation_permit(permit);
            has_permit = true;
        }
    }
    let is_publisher_navigation = ec.is_real_browser && is_navigation && has_permit;
    // A new EC is minted only when this request may also write its row.
    if is_publisher_navigation
        && let Err(err) = ec
            .ec_context
            .generate_if_needed(&state.settings, ec.kv_graph.as_ref())
    {
        log::warn!("EC generation failed for publisher proxy: {err:?}");
    }

    // Page-phase lookups start before the origin fetch so they overlap it.
    let page_lookups = if is_publisher_navigation {
        dispatch_page_lookups(state, services, &req, &mut ec).await
    } else {
        None
    };
    let after_response = if is_publisher_navigation {
        prepare_after_response_lookups(state, services, &req, &ec)
    } else {
        None
    };

    // Run the server-side auction with the configured creative-opportunity
    // slots; `handle_publisher_request` matches them against the request path.
    let auction = AuctionDispatch {
        integration_registry: &state.registry,
        orchestrator: &state.orchestrator,
        slots: state.settings.creative_opportunity_slots(),
        registry: Some(&*state.partner_registry),
    };
    let result = match handle_publisher_request(
        &state.settings,
        services,
        ec.kv_graph.as_ref(),
        &mut ec.ec_context,
        auction,
        req,
        EdgeCacheHeader::SMaxageFallback,
    )
    .await
    {
        Ok(publisher_response) => {
            // Origin start succeeded: authorize orphan recovery only for
            // real-browser document navigations.
            ec.ec_context
                .set_recovery_eligible(ec.is_real_browser && is_navigation);
            // Async finalize so the dispatched auction is collected and its
            // bids are injected before `</body>` (the sync buffer path would
            // drop them).
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
        Err(report) => Err(report),
    };

    let mut response = result.unwrap_or_else(|report| http_error(&report));
    finalize_navigation(state, services, &mut ec, page_lookups, &mut response).await;
    attach_request_filter_effects(&mut response, effects);
    drop(spawn_post_response_work(
        Arc::clone(state),
        services.clone(),
        &mut ec,
        after_response,
    ));
    Ok(response)
}

/// Returns `true` when an asset response should carry a body.
///
/// `HEAD` responses and bodiless statuses (204, 304) advertise the origin
/// representation length while carrying no body.
fn asset_response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
}

/// Proxies a matched asset route to its origin, mirroring the Fastly adapter.
///
/// EC finalization is skipped. The [`AssetProxyCachePolicy`] travels in the
/// response extensions so [`FinalizeResponseMiddleware`] reapplies it after
/// the standard headers.
async fn dispatch_asset_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    asset_route: &ProxyAssetRoute,
    effects: RequestFilterEffects,
) -> Response {
    log::info!("No explicit route matched; proxying via configured asset route");
    let method = req.method().clone();
    let mut response =
        match handle_asset_proxy_request(&state.settings, services, req, asset_route).await {
            Ok(asset_response) => {
                let cache_policy = asset_response.cache_policy();
                let (mut response, stream_body) = asset_response.into_response_and_body();
                if let Some(body) = stream_body
                    && asset_response_carries_body(&method, response.status())
                {
                    *response.body_mut() = body;
                }
                response.extensions_mut().insert(cache_policy);
                response
            }
            Err(report) => {
                let mut response = http_error(&report);
                response
                    .extensions_mut()
                    .insert(AssetProxyCachePolicy::NoStorePrivate);
                response
            }
        };
    attach_request_filter_effects(&mut response, effects);
    response
}

fn fallback_handler(
    state: Arc<AppState>,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(async move {
            let services = state.services_for_request(&ctx);
            let req = ctx.into_request();
            Ok(dispatch_fallback(&state, &services, req)
                .await
                .unwrap_or_else(|report| http_error(&report)))
        })
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
    AdminEcLookup,
    AdminEidsLookup,
    /// Legacy `/admin/keys/*` aliases — denied locally with 404 so they never
    /// reach the publisher fallback (which would leak admin credentials).
    LegacyAdminDenied,
    BatchSync,
    Identify,
    SetTester,
    ClearTester,
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

fn named_routes() -> [NamedRoute; 21] {
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
            handler: NamedRouteHandler::AdminEcLookup,
        },
        NamedRoute {
            path: "/_ts/admin/ec/{id}",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::AdminEcLookup,
        },
        // Admin EIDs echo: pure request inspection with no KV access.
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
            path: "/_ts/api/v1/batch-sync",
            primary_methods: &[Method::POST],
            handler: NamedRouteHandler::BatchSync,
        },
        NamedRoute {
            path: "/_ts/api/v1/identify",
            primary_methods: &[Method::GET, Method::OPTIONS],
            handler: NamedRouteHandler::Identify,
        },
        NamedRoute {
            path: "/_ts/set-tester",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::SetTester,
        },
        NamedRoute {
            path: "/_ts/clear-tester",
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::ClearTester,
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
        Box::pin(execute_named(state, ctx, handler))
    }
}

/// Dispatches a named route, mirroring the Fastly adapter's `execute_named`.
///
/// Batch sync, cache purge and the admin diagnostics skip the EC lifecycle.
/// Every other route builds the EC request state, runs the request filters
/// and the handler, then finalizes EC on the response.
async fn execute_named(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: NamedRouteHandler,
) -> Result<Response, EdgeError> {
    let services = state.services_for_request(&ctx);
    let mut req = ctx.into_request();

    match handler {
        NamedRouteHandler::BatchSync => return Ok(run_batch_sync(&state, req)),
        NamedRouteHandler::CachePurgeNotSupported => return Ok(cache_purge_not_supported()),
        NamedRouteHandler::AdminEcLookup => {
            // Not the bot-gated request-state graph: operators call this
            // authenticated diagnostic with curl.
            let graph = state.identity_graph();
            return Ok(
                handle_admin_ec_lookup(graph.as_ref(), &state.partner_registry, &req)
                    .unwrap_or_else(|report| http_error(&report)),
            );
        }
        NamedRouteHandler::AdminEidsLookup => {
            return Ok(handle_admin_eids_lookup(&state.partner_registry, &req)
                .unwrap_or_else(|report| http_error(&report)));
        }
        _ => {}
    }

    if let Err(report) = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
        &state.settings,
        &mut req,
    ) {
        return Ok(http_error(&report));
    }

    let mut ec = build_ec_request_state(&state, &services, &req);
    if let Some(report) = ec.setup_error.take() {
        let response = http_error(&report);
        return Ok(finish_ec(
            &state,
            &services,
            ec,
            response,
            RequestFilterEffects::default(),
        )
        .await);
    }

    let geo_info = ec.geo_info.clone();
    let effects = match run_pre_route_filters(&state, &services, &mut req, geo_info.as_ref()).await
    {
        PreRoute::ShortCircuit { response, effects } => {
            return Ok(finish_ec(&state, &services, ec, response, effects).await);
        }
        PreRoute::Continue { effects } => effects,
    };

    let response = run_named_route(&state, &services, req, handler, &mut ec)
        .await
        .unwrap_or_else(|report| http_error(&report));
    Ok(finish_ec(&state, &services, ec, response, effects).await)
}

async fn run_named_route(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    handler: NamedRouteHandler,
    ec: &mut EcRequestState,
) -> Result<Response, Report<TrustedServerError>> {
    match handler {
        NamedRouteHandler::TrustedServerDiscovery => {
            handle_trusted_server_discovery(&state.settings, services, req)
        }
        NamedRouteHandler::VerifySignature => {
            handle_verify_signature(&state.settings, services, req)
        }
        NamedRouteHandler::AdminNotSupported => Ok(admin_keys_not_supported()),
        NamedRouteHandler::BatchSync
        | NamedRouteHandler::CachePurgeNotSupported
        | NamedRouteHandler::AdminEcLookup
        | NamedRouteHandler::AdminEidsLookup => {
            unreachable!("should dispatch routes without an EC lifecycle before EC setup")
        }
        NamedRouteHandler::LegacyAdminDenied => Ok(legacy_admin_alias_denied()),
        NamedRouteHandler::Identify => {
            if req.method() == Method::OPTIONS {
                cors_preflight_identify(&state.settings, &req)
            } else {
                let graph = state.identity_graph().ok_or_else(|| {
                    Report::new(TrustedServerError::KvStore {
                        store_name: "ec_store".to_owned(),
                        message: "no EC identity store is configured".to_owned(),
                    })
                })?;
                handle_identify(
                    &state.settings,
                    &graph,
                    &state.partner_registry,
                    &req,
                    &ec.ec_context,
                )
            }
        }
        NamedRouteHandler::SetTester => handle_set_tester(&state.settings),
        NamedRouteHandler::ClearTester => handle_clear_tester(&state.settings),
        NamedRouteHandler::Auction => {
            ec.ec_context.set_eid_sync_source(EidSyncSource::Auction);
            handle_auction(
                &state.settings,
                &state.orchestrator,
                ec.kv_graph.as_ref(),
                state.auction_partner_registry(),
                &mut ec.ec_context,
                services,
                req,
            )
            .await
        }
        NamedRouteHandler::PageBids => {
            // SPA re-auction endpoint. `OPTIONS` is a CORS preflight for this
            // side-effecting GET and is always denied so the GET handler's
            // `X-TSJS-Page-Bids` gate stays trustworthy.
            if req.method() == Method::OPTIONS {
                return Ok(page_bids_preflight_denied());
            }
            let auction = AuctionDispatch {
                integration_registry: &state.registry,
                orchestrator: &state.orchestrator,
                slots: state.settings.creative_opportunity_slots(),
                registry: state.auction_partner_registry(),
            };
            handle_page_bids(
                &state.settings,
                services,
                ec.kv_graph.as_ref(),
                auction,
                &mut ec.ec_context,
                req,
            )
            .await
        }
        NamedRouteHandler::FirstPartyProxy => {
            handle_first_party_proxy(&state.settings, services, req).await
        }
        NamedRouteHandler::FirstPartyClick => {
            handle_first_party_click(&state.settings, services, req).await
        }
        NamedRouteHandler::FirstPartySign => {
            handle_first_party_proxy_sign(&state.settings, services, req).await
        }
        NamedRouteHandler::FirstPartyProxyRebuild => {
            handle_first_party_proxy_rebuild(&state.settings, services, req).await
        }
    }
}

/// Handles `POST /_ts/api/v1/batch-sync`, mirroring the Fastly adapter.
///
/// Batch sync authenticates with a bearer token, not EC cookies, so it
/// finalizes with a default EC context and no identity-graph write.
fn run_batch_sync(state: &AppState, req: Request) -> Response {
    let eids_cookie = extract_cookie_value(&req, COOKIE_TS_EIDS);
    let sharedid_cookie = extract_cookie_value(&req, COOKIE_SHAREDID);
    let result = state
        .identity_graph()
        .ok_or_else(|| {
            Report::new(TrustedServerError::KvStore {
                store_name: "ec_store".to_owned(),
                message: "no EC identity store is configured".to_owned(),
            })
        })
        .and_then(|graph| {
            handle_batch_sync(&graph, &state.partner_registry, &state.rate_limiter, req)
        });
    let mut response = result.unwrap_or_else(|report| http_error(&report));
    ec_finalize_response(
        &state.settings,
        &mut EcContext::default(),
        None,
        &state.partner_registry,
        eids_cookie.as_deref(),
        sharedid_cookie.as_deref(),
        &mut response,
    );
    response
}

fn cache_purge_not_supported() -> Response {
    // The Axum dev server has no template cache to purge. 501 rather than a
    // fallthrough 404, so a CMS webhook can tell "not supported here" from
    // "endpoint does not exist".
    not_implemented(
        "Template cache purge is not supported on the Axum dev server.\n\
         Use the Fastly adapter (via Viceroy or deployed) to purge.\n",
    )
}

fn admin_keys_not_supported() -> Response {
    // Config and secret-store writes are backed by read-only env vars on the
    // Axum dev server, so 501 is clearer than failing on the first write.
    not_implemented(
        "Admin key management is not supported on the Axum dev server.\n\
         Use the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n",
    )
}

fn not_implemented(message: &'static str) -> Response {
    let mut response = Response::new(edgezero_core::body::Body::from(message));
    *response.status_mut() = StatusCode::NOT_IMPLEMENTED;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
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

/// Returns a [`RouterService`] that responds to every route with the startup error.
fn startup_error_router(e: &Report<TrustedServerError>) -> RouterService {
    let message = Arc::new(format!("{}\n", e.current_context().user_message()));
    let status = e.current_context().status_code();

    let make_handler = |msg: Arc<String>| {
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
        router = router.route("/", method.clone(), make_handler(Arc::clone(&message)));
        router = router.route("/{*rest}", method, make_handler(Arc::clone(&message)));
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
        Ok(build_router(&state))
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
        Ok(build_router(&state))
    }
}

pub(crate) fn build_router(state: &Arc<AppState>) -> RouterService {
    let fallback = fallback_handler(Arc::clone(state));
    let trace_state = Arc::clone(state);

    let mut router = RouterService::builder()
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

    router.build()
}

fn trace_metadata(state: &AppState, request: &Request) -> TraceMetadata {
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
            client_ip: AxumRequestContext::get(request)
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

    use futures::executor::block_on;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedClientIpConfig;
    use trusted_server_core::trace::TraceTerminalResponse;

    #[test]
    fn unregistered_identity_provider_kind_fails_startup() {
        let mut settings = trace_settings(false);
        settings.ec.identity_providers =
            vec![trusted_server_core::test_support::fixtures::enabled_identity_provider()];

        let Err(error) = build_state_with_settings(settings) else {
            panic!("should refuse to start with an unregistered provider kind");
        };

        let message = format!("{error:?}");
        assert!(
            message.contains("example-provider") && message.contains("example-kind"),
            "should name the provider and its kind: {message}"
        );
    }

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
