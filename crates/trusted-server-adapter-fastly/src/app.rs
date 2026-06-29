//! Full `EdgeZero` application wiring for Trusted Server.
//!
//! Registers all routes from the legacy [`crate::route_request`] into a
//! [`RouterService`]. On successful startup, attaches [`FinalizeResponseMiddleware`]
//! (outermost) and [`AuthMiddleware`] (inner). When startup fails,
//! [`startup_error_router`] returns a bare router without middleware.
//! Builds the [`AppState`] once per Wasm instance.
//!
//! `EdgeZero`'s current Fastly request context exposes client IP but not TLS
//! protocol or cipher metadata. `edgezero_main` injects a trusted `fastly-ssl`
//! header after stripping client-spoofable headers, so [`detect_request_scheme`]
//! in `http_util` can still derive the correct scheme for HTTPS traffic.
//! It also captures the full [`ClientInfo`] (TLS, JA4, H2 fingerprint, server
//! metadata) into the request extensions, which [`build_per_request_services`]
//! reads back so integration bot protection sees the authoritative signals.
//!
//! # Route inventory
//!
//! | Method | Path pattern | Handler |
//! |--------|-------------|---------|
//! | GET | `/.well-known/trusted-server.json` | [`handle_trusted_server_discovery`] |
//! | POST | `/verify-signature` | [`handle_verify_signature`] |
//! | POST | `/_ts/admin/keys/rotate` | [`handle_rotate_key`] |
//! | POST | `/_ts/admin/keys/deactivate` | [`handle_deactivate_key`] |
//! | POST | `/_ts/api/v1/batch-sync` | [`handle_batch_sync`] |
//! | GET | `/_ts/api/v1/identify` | [`handle_identify`] |
//! | GET | `/_ts/set-tester` | [`handle_set_tester`] |
//! | GET | `/_ts/clear-tester` | [`handle_clear_tester`] |
//! | OPTIONS | `/_ts/api/v1/identify` | [`cors_preflight_identify`] |
//! | POST | `/auction` | [`handle_auction`] |
//! | GET | `/first-party/proxy` | [`handle_first_party_proxy`] |
//! | GET | `/first-party/click` | [`handle_first_party_click`] |
//! | GET | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/proxy-rebuild` | [`handle_first_party_proxy_rebuild`] |
//! | GET | `/` and `/{*rest}` | tsjs (if `/static/tsjs=` prefix), integration proxy, or publisher fallback |
//! | POST, HEAD, OPTIONS, PUT, PATCH, DELETE | `/` and `/{*rest}` | integration proxy or publisher fallback |
//! | POST, HEAD, OPTIONS, PUT, PATCH, DELETE | named paths above | publisher fallback (legacy parity for non-primary methods) |
//!
//! > **Note:** Methods not in the list above (e.g. `TRACE`, `CONNECT`, WebDAV verbs) return a
//! > router-level 405. Legacy routing proxied *every* method through to the publisher origin.
//! > This is a known intentional restriction of the EdgeZero router; the entry-point
//! > `apply_finalize_headers` call in `main.rs` still adds TS headers to those 405 responses.
//!
//! # EC identity lifecycle
//!
//! The `EdgeZero` path mirrors the EC identity lifecycle of the legacy
//! `route_request` (tracked in issue #495):
//!
//! - [`build_ec_request_state`] runs before every dispatched route (except
//!   batch-sync, which uses Bearer auth) and reproduces the legacy
//!   pre-routing prelude: device signals, bot gate, `ts-eids`/`sharedid`
//!   cookie capture, geo lookup, [`EcContext`] creation, and KV-graph gating.
//! - `handle_auction` and integration proxy dispatch receive the same
//!   [`EcContext`], [`KvIdentityGraph`], and [`PartnerRegistry`] inputs as
//!   legacy; the publisher fallback generates EC IDs for browser navigations.
//! - Handlers attach an [`EcFinalizeState`] to the response via extensions;
//!   `edgezero_main` pops it and runs `ec_finalize_response` plus the
//!   pull-sync hook on the converted fastly response before sending.
//!
//! ## Intentional deviations from legacy
//!
//! - **401 auth challenges**: [`AuthMiddleware`] short-circuits before the
//!   handler runs, so no EC state is built and `ec_finalize_response` does not
//!   run on these responses. Legacy ran EC finalization on its own auth
//!   challenges. Like the 401 geo-skip, this is privacy-conservative: no EC
//!   cookies are issued to unauthenticated callers.
//! - **Streaming publisher responses** are buffered (bounded by
//!   `publisher.max_buffered_body_bytes`) instead of streamed to the client.
//! - **Router-level 405s** (unregistered verbs) skip EC finalization along
//!   with the middleware chain; the entry point still adds TS headers.
//!
//! # Startup error handling
//!
//! When [`build_state`] fails, [`startup_error_router`] returns a minimal router
//! that responds to all routes with the startup error. This router does **not**
//! attach middleware. Startup-error responses may still receive entry-point
//! finalization (geo and TS headers) when settings can be reloaded via
//! [`trusted_server_core::settings_data::get_settings`]; if settings loading itself
//! fails, they are returned without geo or TS headers.

use std::sync::Arc;

use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use edgezero_adapter_fastly::FastlyRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    header, HandlerFuture, HeaderValue, Method, Request, Response, StatusCode,
};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{
    IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome,
};
use trusted_server_core::platform::{ClientInfo, GeoInfo, PlatformKvStore, RuntimeServices};
use trusted_server_core::proxy::{
    handle_asset_proxy_request, handle_first_party_click, handle_first_party_proxy,
    handle_first_party_proxy_rebuild, handle_first_party_proxy_sign, stream_asset_body,
    AssetProxyCachePolicy,
};
use trusted_server_core::publisher::{
    buffer_publisher_response, handle_publisher_request, handle_tsjs_dynamic, BoundedWriter,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::{ProxyAssetRoute, Settings};
use trusted_server_core::settings_data::get_settings;
use trusted_server_core::tester_cookie::{handle_clear_tester, handle_set_tester};

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware};
use crate::platform::{
    open_kv_store, FastlyPlatformBackend, FastlyPlatformConfigStore, FastlyPlatformGeo,
    FastlyPlatformHttpClient, FastlyPlatformSecretStore, UnavailableKvStore,
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
    pub(crate) default_kv_store: Arc<dyn PlatformKvStore>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
pub(crate) fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_from_settings(get_settings()?)
}

pub(crate) fn build_state_from_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let orchestrator = build_orchestrator(&settings)?;
    let registry = IntegrationRegistry::new(&settings)?;

    let default_kv_store = Arc::new(UnavailableKvStore) as Arc<dyn PlatformKvStore>;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        default_kv_store,
    }))
}

/// Resolves per-request consent KV store services for routes that read consent data.
///
/// When `settings.consent.consent_store` is configured and the named KV store cannot
/// be opened, returns `Err` so the caller can respond with 503 (fail-closed). This is
/// intentional hardening over the legacy `route_request` path, which builds
/// `runtime_services` with `UnavailableKvStore` and never opens the named consent
/// store, so it never fails closed — the `EdgeZero` path instead makes consent-dependent
/// routes unavailable rather than proceeding without consent.
///
/// # Errors
///
/// Returns an error when the configured consent store cannot be opened.
pub(crate) fn runtime_services_for_consent_route(
    settings: &Settings,
    runtime_services: &RuntimeServices,
) -> Result<RuntimeServices, Report<TrustedServerError>> {
    let Some(store_name) = settings.consent.consent_store.as_deref() else {
        return Ok(runtime_services.clone());
    };

    open_kv_store(store_name)
        .map(|store| runtime_services.clone().with_kv_store(store))
        .map_err(|e| {
            Report::new(TrustedServerError::KvStore {
                store_name: store_name.to_string(),
                message: e.to_string(),
            })
        })
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Construct per-request [`RuntimeServices`] from the `EdgeZero` request context.
///
/// Prefers the full [`ClientInfo`] captured by `edgezero_main` from the original
/// `FastlyRequest` (TLS protocol/cipher, JA4, H2 fingerprint, and server
/// hostname/region) and stored in the request extensions — the metadata
/// integration bot protection (e.g. `DataDome`) serializes, which the
/// reconstructed `EdgeZero` request cannot expose. Falls back to the client IP
/// from the dispatch-inserted [`FastlyRequestContext`] when the extension is
/// absent (e.g. tests that dispatch without the entry point). Scheme detection
/// continues to rely on the trusted `fastly-ssl` header injected by
/// `edgezero_main` after sanitization.
fn build_per_request_services(state: &AppState, ctx: &RequestContext) -> RuntimeServices {
    let client_info = ctx
        .request()
        .extensions()
        .get::<ClientInfo>()
        .cloned()
        .unwrap_or_else(|| ClientInfo {
            client_ip: FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip),
            ..ClientInfo::default()
        });

    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(Arc::clone(&state.default_kv_store))
        .backend(Arc::new(FastlyPlatformBackend))
        .http_client(Arc::new(FastlyPlatformHttpClient))
        .geo(Arc::new(FastlyPlatformGeo))
        .client_info(client_info)
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

// ---------------------------------------------------------------------------
// EC request state
// ---------------------------------------------------------------------------

/// EC state threaded from route handlers to the `main.rs` entry point via
/// response extensions.
///
/// `edgezero_main` pops this from the response after dispatch and runs
/// [`trusted_server_core::ec::finalize::ec_finalize_response`] plus the
/// pull-sync hook on the converted fastly response — the same EC response
/// lifecycle the legacy path drives through `RouteResult`.
#[derive(Clone)]
pub(crate) struct EcFinalizeState {
    pub(crate) ec_context: EcContext,
    /// Whether EC finalization may write to the KV identity graph.
    /// `KvIdentityGraph` wraps a non-`Sync` `dyn EcKvStore` and cannot ride
    /// in response extensions, so `edgezero_main` rebuilds the graph from
    /// settings when this is set.
    pub(crate) use_finalize_kv: bool,
    pub(crate) eids_cookie: Option<String>,
    pub(crate) sharedid_cookie: Option<String>,
    pub(crate) is_real_browser: bool,
    /// Per-request services carried to the entry point so the pull-sync
    /// dispatcher can reuse the same platform HTTP client.
    pub(crate) services: RuntimeServices,
}

/// Per-request EC identity state built before dispatch, mirroring the
/// pre-routing prelude of the legacy `route_request` (device signals, bot
/// gate, cookie capture, consent/geo-aware [`EcContext`], and KV graph
/// gating).
struct EcRequestState {
    ec_context: EcContext,
    kv_graph: Option<KvIdentityGraph>,
    finalize_kv_graph: Option<KvIdentityGraph>,
    eids_cookie: Option<String>,
    sharedid_cookie: Option<String>,
    is_real_browser: bool,
    services: RuntimeServices,
    /// Geo lookup result reused by the pre-route request-filter step so it does
    /// not repeat the lookup the legacy path also shares between EC setup and
    /// filtering.
    geo_info: Option<GeoInfo>,
    /// Error from [`EcContext`] creation. When set, handlers return this as
    /// the response without running the route handler (legacy parity: the
    /// legacy path short-circuits with an error response and a default
    /// context).
    setup_error: Option<Report<TrustedServerError>>,
}

impl EcRequestState {
    fn into_finalize_state(self) -> EcFinalizeState {
        EcFinalizeState {
            ec_context: self.ec_context,
            use_finalize_kv: self.finalize_kv_graph.is_some(),
            eids_cookie: self.eids_cookie,
            sharedid_cookie: self.sharedid_cookie,
            is_real_browser: self.is_real_browser,
            services: self.services,
        }
    }
}

/// Derives device signals from the request's `User-Agent` header.
///
/// Used as the fallback when no entry-point-captured [`DeviceSignals`] are
/// present in the request extensions (see [`device_signals_for`]). TLS and H2
/// fingerprints cannot be reconstructed from the `EdgeZero` request, so this
/// fallback is UA-only — matching the signals the legacy path effectively had
/// after request conversion.
fn derive_request_device_signals(req: &Request) -> DeviceSignals {
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    DeviceSignals::derive(user_agent, None, None)
}

/// Returns the device signals for an `EdgeZero` request.
///
/// `edgezero_main` derives [`DeviceSignals`] from the original `FastlyRequest`
/// — the only place Fastly's TLS JA4 and HTTP/2 fingerprint accessors return
/// real values — and stores them in the request extensions. Reading them back
/// here preserves the bot gate's browser classification, which the `EdgeZero`
/// request cannot expose on its own. Falls back to UA-only
/// [`derive_request_device_signals`] when the extension is absent (e.g. in
/// tests that dispatch without the entry point).
fn device_signals_for(req: &Request) -> DeviceSignals {
    req.extensions()
        .get::<DeviceSignals>()
        .cloned()
        .unwrap_or_else(|| derive_request_device_signals(req))
}

/// Builds the per-request EC state, mirroring the pre-routing prelude of the
/// legacy `route_request` step by step.
fn build_ec_request_state(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> EcRequestState {
    let device_signals = device_signals_for(req);
    let is_real_browser = device_signals.looks_like_browser();
    if !is_real_browser {
        log::info!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    let eids_cookie = crate::extract_cookie_value(req, COOKIE_TS_EIDS);
    let sharedid_cookie = crate::extract_cookie_value(req, COOKIE_SHAREDID);

    let geo_info = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed during EC setup: {e}");
            None
        });

    let (ec_context, setup_error) =
        match EcContext::read_from_request_with_geo(settings, req, services, geo_info.as_ref()) {
            Ok(mut context) => {
                context.set_device_signals(device_signals);
                (context, None)
            }
            Err(report) => (EcContext::default(), Some(report)),
        };

    // Bot gate: suppress KV-backed EC writes for unrecognized clients, except
    // consent withdrawals. Revocations keep the write path so tombstones stay
    // authoritative even for privacy-extension-heavy clients.
    let kv_graph = crate::maybe_identity_graph(settings);
    let finalize_kv_graph = if setup_error.is_none()
        && (is_real_browser || ec_consent_withdrawn(ec_context.consent()))
    {
        kv_graph.clone()
    } else {
        None
    };
    let kv_graph = if is_real_browser { kv_graph } else { None };

    EcRequestState {
        ec_context,
        kv_graph,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
        services: services.clone(),
        geo_info,
        setup_error,
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Result of the pre-route integration request-filter step.
enum PreRoute {
    /// A filter elected to respond (or errored); return this response without
    /// running the route handler. Carries the accumulated response effects.
    ShortCircuit {
        response: Response,
        effects: RequestFilterEffects,
    },
    /// Continue to the route handler; apply these response effects afterward.
    Continue { effects: RequestFilterEffects },
}

/// Runs the integration request-filter pipeline before route dispatch.
///
/// Mirrors the legacy `route_request` ordering: filters run after auth
/// (`AuthMiddleware` on this path) and before route matching. Request header
/// mutations are applied to `req` so the routed handler observes them; response
/// effects are returned for the entry point to apply after EC finalization. A
/// filter that responds (e.g. a `DataDome` challenge) short-circuits routing.
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

/// Attaches the EC finalize state and any non-empty request-filter response
/// effects to a dispatched response via extensions, so `edgezero_main` can run
/// EC finalization and apply the filter effects after it (legacy ordering).
fn attach_dispatch_extensions(
    mut response: Response,
    ec: EcRequestState,
    effects: RequestFilterEffects,
) -> Response {
    response.extensions_mut().insert(ec.into_finalize_state());
    if !effects.response_headers.is_empty() {
        response.extensions_mut().insert(effects);
    }
    response
}

async fn execute_named(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: NamedRouteHandler,
) -> Result<Response, EdgeError> {
    let services = build_per_request_services(&state, &ctx);
    let mut req = ctx.into_request();

    // S2S batch sync uses Bearer auth (not EC cookies), so it skips EC
    // context creation entirely — mirroring the dedicated early arm in the
    // legacy route_request. Batch-sync also skips request filters, matching
    // legacy, which returns before the filter step for this route.
    if matches!(handler, NamedRouteHandler::BatchSync) {
        return Ok(run_batch_sync(&state, &services, req));
    }

    let mut ec = build_ec_request_state(&state.settings, &services, &req);
    // EcContext creation errors short-circuit before filters, mirroring legacy:
    // the legacy path returns its error response before running filter_request.
    if let Some(report) = ec.setup_error.take() {
        let response = http_error(&report);
        return Ok(attach_dispatch_extensions(
            response,
            ec,
            RequestFilterEffects::default(),
        ));
    }

    let effects =
        match run_pre_route_filters(&state, &services, &mut req, ec.geo_info.as_ref()).await {
            PreRoute::ShortCircuit { response, effects } => {
                return Ok(attach_dispatch_extensions(response, ec, effects));
            }
            PreRoute::Continue { effects } => effects,
        };

    let response = run_named_route(&state, &services, req, handler, &mut ec)
        .await
        .unwrap_or_else(|e| http_error(&e));
    Ok(attach_dispatch_extensions(response, ec, effects))
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
        NamedRouteHandler::RotateKey => handle_rotate_key(&state.settings, services, req),
        NamedRouteHandler::DeactivateKey => handle_deactivate_key(&state.settings, services, req),
        NamedRouteHandler::LegacyAdminDenied => Ok(legacy_admin_alias_denied()),
        NamedRouteHandler::BatchSync => {
            // Dispatched by execute_named before EC state is built.
            unreachable!("batch-sync should be handled by run_batch_sync")
        }
        NamedRouteHandler::Identify => {
            if req.method() == Method::OPTIONS {
                cors_preflight_identify(&state.settings, &req)
            } else {
                let kv = crate::require_identity_graph(&state.settings)?;
                let partner_registry = PartnerRegistry::from_config(&state.settings.ec.partners)?;
                handle_identify(
                    &state.settings,
                    &kv,
                    &partner_registry,
                    &req,
                    &ec.ec_context,
                )
            }
        }
        NamedRouteHandler::SetTester => handle_set_tester(&state.settings),
        NamedRouteHandler::ClearTester => handle_clear_tester(&state.settings),
        NamedRouteHandler::Auction => {
            // The auction reads consent data, so the consent KV store must be
            // available — fail closed with 503 when it is configured but
            // cannot be opened, matching legacy behavior.
            let consent_services = runtime_services_for_consent_route(&state.settings, services)?;
            let partner_registry = PartnerRegistry::from_config(&state.settings.ec.partners)?;
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(&partner_registry)
            };
            handle_auction(
                &state.settings,
                &state.orchestrator,
                ec.kv_graph.as_ref(),
                registry_ref,
                &ec.ec_context,
                &consent_services,
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

/// Handles `POST /_ts/api/v1/batch-sync`, mirroring the legacy arm: identity
/// graph + partner registry + rate limiter, with a default EC context for
/// response finalization.
fn run_batch_sync(state: &AppState, services: &RuntimeServices, req: Request) -> Response {
    let device_signals = device_signals_for(&req);
    let is_real_browser = device_signals.looks_like_browser();
    let eids_cookie = crate::extract_cookie_value(&req, COOKIE_TS_EIDS);
    let sharedid_cookie = crate::extract_cookie_value(&req, COOKIE_SHAREDID);

    let result = crate::require_identity_graph(&state.settings).and_then(|kv| {
        let partner_registry = PartnerRegistry::from_config(&state.settings.ec.partners)?;
        let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
        handle_batch_sync(&kv, &partner_registry, &limiter, req)
    });

    let mut response = result.unwrap_or_else(|e| http_error(&e));
    // Legacy parity: batch-sync responses still pass through
    // ec_finalize_response with a default EC context and no finalize KV graph.
    response.extensions_mut().insert(EcFinalizeState {
        ec_context: EcContext::default(),
        use_finalize_kv: false,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
        services: services.clone(),
    });
    response
}

async fn execute_fallback(
    state: Arc<AppState>,
    ctx: RequestContext,
) -> Result<Response, EdgeError> {
    let services = build_per_request_services(&state, &ctx);
    let req = ctx.into_request();
    Ok(dispatch_fallback(&state, &services, req).await)
}

async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    mut req: Request,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let mut ec = build_ec_request_state(&state.settings, services, &req);
    if let Some(report) = ec.setup_error.take() {
        let response = http_error(&report);
        return attach_dispatch_extensions(response, ec, RequestFilterEffects::default());
    }

    // Pre-route integration request filters (DataDome protection, etc.) run
    // before the route-type decision, matching legacy `route_request` ordering.
    let effects = match run_pre_route_filters(state, services, &mut req, ec.geo_info.as_ref()).await
    {
        PreRoute::ShortCircuit { response, effects } => {
            return attach_dispatch_extensions(response, ec, effects);
        }
        PreRoute::Continue { effects } => effects,
    };

    let result = if uses_dynamic_tsjs_fallback(&method, &path) {
        handle_tsjs_dynamic(&req, &state.registry)
    } else if state.registry.has_route(&method, &path) {
        // Integration-proxy responses are not bounded by publisher.max_buffered_body_bytes.
        // Only the handle_publisher_request branch below routes through
        // buffer_publisher_response. Integration responses are small in practice
        // and the EdgeZero flag is off by default; extend the cap here if that changes.
        state
            .registry
            .handle_proxy(ProxyDispatchInput {
                method: &method,
                path: &path,
                settings: &state.settings,
                kv: ec.kv_graph.as_ref(),
                ec_context: &mut ec.ec_context,
                services,
                req,
            })
            .await
            .unwrap_or_else(|| {
                Err(Report::new(TrustedServerError::BadRequest {
                    message: format!("Unknown integration route: {path}"),
                }))
            })
    } else {
        // Asset-route fallback (GET/HEAD), mirroring the legacy catch-all arm:
        // matched asset paths proxy to the configured asset origin instead of the
        // publisher origin. Must be checked after tsjs/integration routes and
        // before the publisher fallback. Asset responses skip EC finalization
        // (no EcFinalizeState attached), matching legacy's `should_finalize_ec = false`.
        let matched_asset_route = matches!(method, Method::GET | Method::HEAD)
            .then(|| state.settings.asset_route_for_path(&path))
            .flatten();
        if let Some(asset_route) = matched_asset_route {
            return dispatch_asset_fallback(state, services, req, asset_route, &effects).await;
        }

        // Generate an EC ID if needed — mirrors the legacy catch-all arm.
        // Only for document navigations by recognised browsers; subresource
        // requests may lack consent signals such as Sec-GPC.
        if ec.is_real_browser && is_navigation_request(&req) {
            if let Err(err) = ec
                .ec_context
                .generate_if_needed(&state.settings, ec.kv_graph.as_ref())
            {
                log::warn!("EC generation failed for publisher proxy: {err:?}");
            }
        }

        // Publisher pages read consent data, so the consent KV store must be
        // available — fail closed with 503 when it is configured but cannot
        // be opened, matching legacy behavior.
        match runtime_services_for_consent_route(&state.settings, services) {
            Ok(publisher_services) => {
                // Server-side auction is not yet wired into the EdgeZero buffered
                // finalize path (`buffer_publisher_response` runs the
                // synchronous pipeline, which does not collect dispatched SSP
                // bids). Pass no slots so `handle_publisher_request` dispatches no
                // auction and no bid requests are wasted. The legacy path runs the
                // full server-side auction; wiring it here is deferred to the
                // EdgeZero cutover.
                let auction = trusted_server_core::publisher::AuctionDispatch {
                    orchestrator: &state.orchestrator,
                    slots: &[],
                    registry: None,
                };
                handle_publisher_request(
                    &state.settings,
                    &publisher_services,
                    ec.kv_graph.as_ref(),
                    &mut ec.ec_context,
                    auction,
                    req,
                )
                .await
                .and_then(|pub_response| {
                    buffer_publisher_response(
                        pub_response,
                        &method,
                        &state.settings,
                        &state.registry,
                    )
                })
            }
            Err(e) => Err(e),
        }
    };

    let response = result.unwrap_or_else(|e| http_error(&e));
    attach_dispatch_extensions(response, ec, effects)
}

/// Returns `true` when an asset response should carry a buffered body and a
/// recomputed `Content-Length`.
///
/// `HEAD` responses and bodiless statuses (204, 304) advertise the origin
/// representation length in their `Content-Length` header while carrying no
/// body. Rewriting that header to the buffered byte count (0) would corrupt the
/// metadata, so those responses keep the origin's `Content-Length` untouched.
fn asset_response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
}

/// Handles the asset-route fallback on the `EdgeZero` path, mirroring the legacy
/// `route_request` asset branch.
///
/// Proxies the request to the configured asset origin and threads the
/// [`AssetProxyCachePolicy`] out via response extensions so `edgezero_main`
/// can reapply protected cache directives after finalization. EC finalization
/// is intentionally skipped: no [`EcFinalizeState`] is attached, matching the
/// legacy `should_finalize_ec = false` behavior for asset responses.
///
/// Unlike legacy `route_request`, which streams asset bodies straight to the
/// client with no cap, the `EdgeZero` path buffers them: `edgezero_main`
/// converts the whole response before sending, so there is no streaming seam
/// yet. The buffer is bounded by `publisher.max_buffered_body_bytes` as an
/// interim Wasm-heap OOM guard. Reusing the publisher cap and restoring
/// uncapped streaming are both resolved by the streaming cutover (issue #495);
/// whether assets get a dedicated cap is deferred to that work.
async fn dispatch_asset_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    asset_route: &ProxyAssetRoute,
    effects: &RequestFilterEffects,
) -> Response {
    log::info!("No explicit route matched; proxying via configured asset route");

    let method = req.method().clone();

    match handle_asset_proxy_request(&state.settings, services, req, asset_route).await {
        Ok(asset_response) => {
            let cache_policy = asset_response.cache_policy();
            let (mut response, stream_body) = asset_response.into_response_and_body();

            if let Some(body) = stream_body {
                match buffer_asset_body(body, state.settings.publisher.max_buffered_body_bytes)
                    .await
                {
                    Ok(bytes) => {
                        // Preserve the origin's Content-Length for HEAD and
                        // bodiless statuses; only body-bearing responses get a
                        // recomputed length and the buffered body attached.
                        if asset_response_carries_body(&method, response.status()) {
                            response.headers_mut().insert(
                                header::CONTENT_LENGTH,
                                HeaderValue::from(bytes.len() as u64),
                            );
                            *response.body_mut() = edgezero_core::body::Body::from(bytes);
                        }
                    }
                    Err(report) => {
                        let mut response = http_error(&report);
                        response
                            .extensions_mut()
                            .insert(AssetProxyCachePolicy::NoStorePrivate);
                        attach_request_filter_effects(&mut response, effects);
                        return response;
                    }
                }
            }

            response.extensions_mut().insert(cache_policy);
            attach_request_filter_effects(&mut response, effects);
            response
        }
        Err(report) => {
            let mut response = http_error(&report);
            response
                .extensions_mut()
                .insert(AssetProxyCachePolicy::NoStorePrivate);
            attach_request_filter_effects(&mut response, effects);
            response
        }
    }
}

/// Attaches non-empty request-filter response effects to an asset response.
///
/// Asset responses skip EC finalization but still carry filter effects so the
/// entry point applies them after finalization, matching the legacy asset
/// streaming path which applies `request_filter_effects` to every asset
/// response.
fn attach_request_filter_effects(response: &mut Response, effects: &RequestFilterEffects) {
    if !effects.response_headers.is_empty() {
        response.extensions_mut().insert(effects.clone());
    }
}

/// Buffers a streaming asset body into memory, bounded by `max_bytes`
/// (the interim `publisher.max_buffered_body_bytes` OOM guard; see
/// [`dispatch_asset_fallback`]).
///
/// # Errors
///
/// Returns an error if the body exceeds the configured cap or the underlying
/// stream yields an error.
async fn buffer_asset_body(
    body: edgezero_core::body::Body,
    max_bytes: usize,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    let mut output = BoundedWriter::new(max_bytes);
    stream_asset_body(body, &mut output).await?;
    Ok(output.into_inner())
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

/// Builds the local `404 Not Found` returned for legacy `/admin/keys/*`
/// aliases on the `EdgeZero` path.
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
// Route registration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum NamedRouteHandler {
    TrustedServerDiscovery,
    VerifySignature,
    RotateKey,
    DeactivateKey,
    /// Legacy `/admin/keys/*` aliases — denied locally with 404 so they never
    /// reach the publisher fallback (which would leak admin credentials).
    LegacyAdminDenied,
    BatchSync,
    Identify,
    SetTester,
    ClearTester,
    Auction,
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

const NAMED_ROUTES: &[NamedRoute] = &[
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
    NamedRoute {
        path: "/_ts/admin/keys/rotate",
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::RotateKey,
    },
    NamedRoute {
        path: "/_ts/admin/keys/deactivate",
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::DeactivateKey,
    },
    // The legacy non-`/_ts` aliases (`/admin/keys/*`) are denied locally with a
    // 404 instead of executing key operations: the production basic-auth handler
    // regex `^/_ts/admin` does not match them, and letting them fall through to
    // publisher fallback for any fallback method would forward the caller's
    // `Authorization` header and key-management payload to the origin, leaking
    // admin credentials.
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
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::FirstPartyProxyRebuild,
    },
];

fn named_route_handler(
    state: Arc<AppState>,
    handler: NamedRouteHandler,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(execute_named(state, ctx, handler))
    }
}

fn fallback_route_handler(
    state: Arc<AppState>,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(execute_fallback(state, ctx))
    }
}

// ---------------------------------------------------------------------------
// TrustedServerApp
// ---------------------------------------------------------------------------

/// `EdgeZero` [`Hooks`] implementation for the Trusted Server application.
pub struct TrustedServerApp;

impl TrustedServerApp {
    fn routes_for_state(state: &Arc<AppState>) -> RouterService {
        let mut router = RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(
                Arc::clone(&state.settings),
                Arc::new(FastlyPlatformGeo),
            ))
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)));

        let fallback_handler = fallback_route_handler(Arc::clone(state));

        // matchit prefers exact path+method over a wildcard catch-all. Each
        // named route is registered from this single table, then every
        // non-primary publisher fallback method is registered from the same
        // row. Adding a named route now requires editing only this table.
        for route in NAMED_ROUTES {
            for method in route.primary_methods {
                router = router.route(
                    route.path,
                    method.clone(),
                    named_route_handler(Arc::clone(state), route.handler),
                );
            }

            for method in publisher_fallback_methods() {
                if !route.primary_methods.contains(&method) {
                    router = router.route(route.path, method, fallback_handler.clone());
                }
            }
        }

        // matchit's `/{*rest}` does not match the bare root `/` — register
        // explicit root routes so `/` reaches the publisher fallback too.
        for method in publisher_fallback_methods() {
            router = router.route("/", method.clone(), fallback_handler.clone());
            router = router.route("/{*rest}", method, fallback_handler.clone());
        }

        router.build()
    }
}

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

        Self::routes_for_state(&state)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        build_state_from_settings, startup_error_router, AppState, NamedRouteHandler,
        TrustedServerApp, NAMED_ROUTES,
    };

    use edgezero_core::body::Body;
    use edgezero_core::http::{header, request_builder, Method, StatusCode};
    use edgezero_core::router::RouterService;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use error_stack::Report;
    use futures::executor::block_on;
    use serde_json::json;
    use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
    use trusted_server_core::ec::device::DeviceSignals;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        HeaderMutation, IntegrationRegistry, IntegrationRequestFilter, RequestFilterDecision,
        RequestFilterEffects, RequestFilterInput,
    };
    use trusted_server_core::platform::ClientInfo;
    use trusted_server_core::settings::Settings;

    fn settings_with_missing_consent_store() -> Settings {
        Settings::from_toml(
            r#"
                [[handlers]]
                path = "^/(_ts/)?admin"
                username = "admin"
                password = "admin-pass"

                [publisher]
                domain = "test-publisher.com"
                cookie_domain = ".test-publisher.com"
                origin_url = "https://origin.test-publisher.com"
                proxy_secret = "unit-test-proxy-secret"

                [ec]
                passphrase = "test-passphrase-at-least-32-bytes!!"

                [request_signing]
                enabled = false
                config_store_id = "test-config-store-id"
                secret_store_id = "test-secret-store-id"

                [consent]
                consent_store = "missing-consent-store"

                [integrations.prebid]
                enabled = true
                server_url = "https://test-prebid.com/openrtb2/auction"

                [integrations.datadome]
                enabled = true

                [auction]
                enabled = true
                providers = ["prebid"]
                timeout_ms = 2000
            "#,
        )
        .expect("should parse EdgeZero app test settings")
    }

    fn app_state_for_settings(settings: Settings) -> Arc<AppState> {
        build_state_from_settings(settings).expect("should build app state from settings")
    }

    fn empty_request(method: Method, path: &str) -> edgezero_core::http::Request {
        // Production requests arrive with absolute URIs from the fastly
        // adapter — mirror that here so URI-derived logic behaves the same.
        let uri = format!("https://test-publisher.com{path}");
        request_builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("should build request")
    }

    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [[handlers]]
            path = "^/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [integrations.prebid]
            enabled = true
            server_url = "https://test-prebid.com/openrtb2/auction"

            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000
            "#,
        )
        .expect("should parse test settings")
    }

    fn test_router() -> RouterService {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        TrustedServerApp::routes_for_state(&state)
    }

    /// Builds a router whose `AppState` uses a registry containing the given
    /// request filters (and no routes), so dispatch-level request-filter
    /// behavior can be exercised without a real integration.
    fn router_with_request_filters(
        filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    ) -> RouterService {
        let settings = test_settings();
        let orchestrator = trusted_server_core::auction::build_orchestrator(&settings)
            .expect("should build orchestrator");
        let registry = IntegrationRegistry::from_request_filters(filters);
        let default_kv_store =
            Arc::new(crate::platform::UnavailableKvStore) as Arc<dyn super::PlatformKvStore>;
        let state = Arc::new(super::AppState {
            settings: Arc::new(settings),
            orchestrator: Arc::new(orchestrator),
            registry: Arc::new(registry),
            default_kv_store,
        });
        TrustedServerApp::routes_for_state(&state)
    }

    /// Continues routing while mutating the request and emitting a response
    /// header effect — mirrors `DataDome`'s allow path.
    struct RecordingRequestFilter;

    #[async_trait::async_trait(?Send)]
    impl IntegrationRequestFilter for RecordingRequestFilter {
        fn integration_id(&self) -> &'static str {
            "recording"
        }

        async fn filter_request(
            &self,
            _input: RequestFilterInput<'_>,
        ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
            Ok(RequestFilterDecision::Continue(RequestFilterEffects {
                request_headers: vec![HeaderMutation::set("x-filter-ran", "1")],
                response_headers: vec![HeaderMutation::set("x-filter-effect", "applied")],
            }))
        }
    }

    /// Short-circuits routing with a 403 — mirrors a `DataDome` challenge/block.
    struct ChallengeRequestFilter;

    #[async_trait::async_trait(?Send)]
    impl IntegrationRequestFilter for ChallengeRequestFilter {
        fn integration_id(&self) -> &'static str {
            "challenge"
        }

        async fn filter_request(
            &self,
            _input: RequestFilterInput<'_>,
        ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
            let mut response = edgezero_core::http::Response::new(Body::from("blocked"));
            *response.status_mut() = StatusCode::FORBIDDEN;
            Ok(RequestFilterDecision::Respond {
                response: Box::new(response),
                effects: RequestFilterEffects {
                    request_headers: Vec::new(),
                    response_headers: vec![HeaderMutation::set("x-challenge", "1")],
                },
            })
        }
    }

    /// Records the [`ClientInfo`] a request filter observes via its
    /// [`RequestFilterInput`], so a test can assert the entry-point-captured
    /// bot-protection metadata reaches integration filters like `DataDome`.
    struct ClientInfoCapturingFilter(Arc<Mutex<Option<ClientInfo>>>);

    #[async_trait::async_trait(?Send)]
    impl IntegrationRequestFilter for ClientInfoCapturingFilter {
        fn integration_id(&self) -> &'static str {
            "client-info-capture"
        }

        async fn filter_request(
            &self,
            input: RequestFilterInput<'_>,
        ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
            *self.0.lock().expect("should lock captured client info") =
                Some(input.services.client_info().clone());
            Ok(RequestFilterDecision::Continue(RequestFilterEffects {
                request_headers: Vec::new(),
                response_headers: Vec::new(),
            }))
        }
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

    #[test]
    fn asset_response_carries_body_preserves_bodiless_content_length() {
        // GET/200 buffers a body and recomputes Content-Length.
        assert!(
            super::asset_response_carries_body(&Method::GET, StatusCode::OK),
            "a GET 200 asset response should carry a buffered body"
        );
        // HEAD advertises the origin representation length with no body — the
        // recomputed (zero) length must not overwrite it.
        assert!(
            !super::asset_response_carries_body(&Method::HEAD, StatusCode::OK),
            "HEAD asset responses must preserve the origin Content-Length"
        );
        // Bodiless statuses keep their origin metadata regardless of method.
        assert!(
            !super::asset_response_carries_body(&Method::GET, StatusCode::NO_CONTENT),
            "204 responses must preserve the origin Content-Length"
        );
        assert!(
            !super::asset_response_carries_body(&Method::GET, StatusCode::NOT_MODIFIED),
            "304 responses must preserve the origin Content-Length"
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
        // Test settings protect `^/(_ts/)?admin` with basic-auth. Sending the
        // request without an Authorization header causes AuthMiddleware to
        // short-circuit with a 401, which then bubbles through
        // FinalizeResponseMiddleware for header injection.
        //
        // This is safe to run without Viceroy: enforce_basic_auth is pure Rust
        // (reads settings + request headers only) and FastlyPlatformGeo.lookup(None)
        // short-circuits without calling any Fastly ABI.
        let router = test_router();
        let req = empty_request(Method::POST, "/_ts/admin/keys/rotate");

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
    fn legacy_admin_aliases_route_to_local_deny_not_key_handlers() {
        // Security guard for the legacy non-`/_ts` admin aliases. They must be
        // registered to the local `LegacyAdminDenied` 404 handler — not the
        // rotate/deactivate key handlers, and not left unrouted. Leaving them
        // unrouted would fall through to the publisher fallback, which forwards
        // the request (including the `Authorization` header and key-management
        // payload) to the origin, leaking admin credentials. Mapping them to the
        // key handlers would expose key operations, since the production
        // basic-auth regex `^/_ts/admin` does not match `/admin/keys/*`.
        let handler_for = |path: &str| {
            NAMED_ROUTES
                .iter()
                .find(|route| route.path == path)
                .map(|route| route.handler)
        };
        let methods_for = |path: &str| {
            NAMED_ROUTES
                .iter()
                .find(|route| route.path == path)
                .map(|route| route.primary_methods)
                .unwrap_or(&[])
        };

        assert!(
            matches!(
                handler_for("/_ts/admin/keys/rotate"),
                Some(NamedRouteHandler::RotateKey)
            ),
            "canonical /_ts/admin/keys/rotate must map to the rotate handler"
        );
        assert!(
            matches!(
                handler_for("/_ts/admin/keys/deactivate"),
                Some(NamedRouteHandler::DeactivateKey)
            ),
            "canonical /_ts/admin/keys/deactivate must map to the deactivate handler"
        );
        assert!(
            matches!(
                handler_for("/admin/keys/rotate"),
                Some(NamedRouteHandler::LegacyAdminDenied)
            ),
            "legacy /admin/keys/rotate must map to the local deny handler, not the key handler"
        );
        assert!(
            matches!(
                handler_for("/admin/keys/deactivate"),
                Some(NamedRouteHandler::LegacyAdminDenied)
            ),
            "legacy /admin/keys/deactivate must map to the local deny handler, not the key handler"
        );

        for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
            for method in super::publisher_fallback_methods() {
                assert!(
                    methods_for(path).contains(&method),
                    "legacy {method} {path} must route to the local deny handler, not publisher fallback"
                );
            }
        }
    }

    #[test]
    fn legacy_admin_aliases_denied_locally_not_proxied_to_publisher() {
        // Regression for the credential-leak finding: with a production-shaped
        // config (only `^/_ts/admin` is auth-gated, so `/admin/keys/*` is NOT
        // matched by any handler), any publisher-fallback method to a legacy
        // alias carrying an `Authorization` header must be denied locally with
        // 404 — never proxied to the publisher origin (which would leak the
        // admin credentials and the key-management body). A publisher-fallback
        // proxy without a backend would surface as a 5xx, so a 404 proves the
        // deny route ran instead.
        let settings = Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse production-shaped settings");
        let state = build_state_from_settings(settings).expect("should build state");
        let router = TrustedServerApp::routes_for_state(&state);

        for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
            for method in super::publisher_fallback_methods() {
                let req = request_builder()
                    .method(method.clone())
                    .uri(format!("https://test-publisher.com{path}"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body(Body::from("{\"key_id\":\"leak-me\"}"))
                    .expect("should build authorized legacy-alias request");

                let response = block_on(router.oneshot(req));

                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {path} with Authorization must be denied locally (404), not proxied to publisher"
                );
            }
        }
    }

    #[test]
    fn dispatch_identify_options_routes_to_cors_preflight() {
        // Parity guard: OPTIONS /_ts/api/v1/identify must reach
        // cors_preflight_identify (200 for a request without an Origin
        // header), not the publisher fallback, which would fail with a
        // gateway error without a live backend.
        let router = test_router();
        let response =
            block_on(router.oneshot(empty_request(Method::OPTIONS, "/_ts/api/v1/identify")));

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "OPTIONS identify should be answered by the CORS preflight handler"
        );
    }

    #[test]
    fn dispatch_identify_get_routes_to_identity_handler() {
        // Parity guard: GET /_ts/api/v1/identify must reach the identify
        // handler chain. The test settings configure no ec.ec_store, so
        // require_identity_graph fails with a KvStore error (503) — proving
        // the request was NOT proxied to the publisher origin.
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(Method::GET, "/_ts/api/v1/identify")));

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "GET identify without ec_store should fail with the KvStore error, not a publisher proxy error"
        );
    }

    #[test]
    fn dispatch_batch_sync_routes_to_batch_sync_handler() {
        // Parity guard: POST /_ts/api/v1/batch-sync must reach the batch-sync
        // handler chain instead of forwarding the request (body and
        // Authorization header included) to the publisher origin. With no
        // ec.ec_store configured, require_identity_graph fails with a KvStore
        // error (503).
        let router = test_router();
        let response =
            block_on(router.oneshot(empty_request(Method::POST, "/_ts/api/v1/batch-sync")));

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "POST batch-sync without ec_store should fail with the KvStore error, not reach the publisher"
        );
    }

    #[test]
    fn dispatch_set_tester_is_disabled_by_default() {
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(Method::GET, "/_ts/set-tester")));

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "disabled tester-cookie route should return 404"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "disabled tester-cookie route should not set a cookie"
        );
    }

    #[test]
    fn dispatch_set_tester_sets_cookie_on_configured_domain() {
        let mut settings = test_settings();
        settings.tester_cookie.enabled = true;
        let state = app_state_for_settings(settings);
        let router = TrustedServerApp::routes_for_state(&state);
        let response = block_on(router.oneshot(empty_request(Method::GET, "/_ts/set-tester")));

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "enabled tester-cookie route should return no content"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set tester cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie, "ts-tester=true; Domain=.test-publisher.com; Path=/; Secure; SameSite=Lax",
            "tester cookie should use publisher.cookie_domain"
        );
    }

    #[test]
    fn dispatch_clear_tester_is_disabled_by_default() {
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(Method::GET, "/_ts/clear-tester")));

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "disabled clear tester-cookie route should return 404"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "disabled clear tester-cookie route should not set a cookie"
        );
    }

    #[test]
    fn dispatch_clear_tester_clears_cookie_on_configured_domain() {
        let mut settings = test_settings();
        settings.tester_cookie.enabled = true;
        let state = app_state_for_settings(settings);
        let router = TrustedServerApp::routes_for_state(&state);
        let response = block_on(router.oneshot(empty_request(Method::GET, "/_ts/clear-tester")));

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "enabled clear tester-cookie route should return no content"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should clear tester cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie,
            "ts-tester=; Domain=.test-publisher.com; Path=/; Secure; SameSite=Lax; Max-Age=0",
            "tester cookie clear should use publisher.cookie_domain"
        );
    }

    #[test]
    fn dispatch_fallback_attaches_ec_finalize_state() {
        // The publisher fallback must thread EC finalize state to the entry
        // point via response extensions — even on error responses — so that
        // edgezero_main can run ec_finalize_response and pull sync.
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(Method::GET, "/some-page")));

        assert!(
            response.extensions().get::<super::EcFinalizeState>().is_some(),
            "publisher fallback responses should carry EcFinalizeState for entry-point EC finalization"
        );
    }

    #[test]
    fn browser_device_signals_from_extension_reach_ec_finalize_state() {
        // Regression guard for the EdgeZero JA4/H2 signal loss: `edgezero_main`
        // derives DeviceSignals from the original FastlyRequest (the only place
        // get_tls_ja4/get_client_h2_fingerprint return real values) and stores
        // them in the request extensions. A browser-shaped signal must survive
        // dispatch so the EC bot gate classifies the request as a real browser
        // and keeps the KV-backed generation/finalize path active.
        let router = test_router();
        let mut req = empty_request(Method::GET, "/some-page");
        req.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));

        let response = block_on(router.oneshot(req));

        let finalize = response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("fallback response should carry EcFinalizeState");
        assert!(
            finalize.is_real_browser,
            "browser-shaped JA4/H2 signals from the request extension must mark the request as a real browser"
        );
    }

    #[test]
    fn missing_device_signals_extension_classifies_as_non_browser() {
        // Without the entry-point-captured signals, the reconstructed request
        // cannot expose JA4/H2, so the bot gate falls back to non-browser. This
        // documents the regression the extension threading fixes: the same
        // request that looks like a browser above is treated as a bot here.
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(Method::GET, "/some-page")));

        let finalize = response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("fallback response should carry EcFinalizeState");
        assert!(
            !finalize.is_real_browser,
            "a request without captured device signals should not be classified as a real browser"
        );
    }

    #[test]
    fn entry_point_client_info_reaches_request_filters() {
        // Regression guard for the EdgeZero bot-protection metadata loss:
        // `edgezero_main` captures the full ClientInfo (TLS protocol/cipher, JA4,
        // H2 fingerprint, server hostname/region) from the original FastlyRequest
        // and stores it in the request extensions. It must survive dispatch so
        // integration request filters (e.g. DataDome) serialize the same signals
        // the legacy path provides, not an empty payload.
        let captured = Arc::new(Mutex::new(None));
        let filter = Arc::new(ClientInfoCapturingFilter(Arc::clone(&captured)))
            as Arc<dyn IntegrationRequestFilter>;
        let router = router_with_request_filters(vec![filter]);

        let mut req = empty_request(Method::GET, "/some-page");
        req.extensions_mut().insert(ClientInfo {
            client_ip: Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))),
            tls_protocol: Some("TLSv1.3".to_string()),
            tls_cipher: Some("TLS_AES_128_GCM_SHA256".to_string()),
            tls_ja4: Some("t13d1516h2_8daaf6152771_b186095e22b6".to_string()),
            h2_fingerprint: Some("1:65536;2:0;4:6291456;6:262144".to_string()),
            server_hostname: Some("edge-test.example.net".to_string()),
            server_region: Some("US-East".to_string()),
        });

        let _ = block_on(router.oneshot(req));

        let observed = captured
            .lock()
            .expect("should lock captured client info")
            .clone()
            .expect("request filter should have observed the entry-point ClientInfo");
        assert_eq!(
            observed.tls_protocol.as_deref(),
            Some("TLSv1.3"),
            "filter should see the captured TLS protocol"
        );
        assert_eq!(
            observed.tls_cipher.as_deref(),
            Some("TLS_AES_128_GCM_SHA256"),
            "filter should see the captured TLS cipher"
        );
        assert_eq!(
            observed.tls_ja4.as_deref(),
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            "filter should see the captured JA4 fingerprint"
        );
        assert_eq!(
            observed.h2_fingerprint.as_deref(),
            Some("1:65536;2:0;4:6291456;6:262144"),
            "filter should see the captured H2 fingerprint"
        );
        assert_eq!(
            observed.server_hostname.as_deref(),
            Some("edge-test.example.net"),
            "filter should see the captured server hostname"
        );
        assert_eq!(
            observed.server_region.as_deref(),
            Some("US-East"),
            "filter should see the captured server region"
        );
    }

    #[test]
    fn dispatch_named_route_attaches_ec_finalize_state() {
        // Named routes must also thread EC finalize state, mirroring how the
        // legacy path finalizes every response with the pre-routing EcContext.
        let router = test_router();
        let response = block_on(router.oneshot(empty_request(
            Method::GET,
            "/.well-known/trusted-server.json",
        )));

        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_some(),
            "named-route responses should carry EcFinalizeState for entry-point EC finalization"
        );
    }

    #[test]
    fn dispatch_head_on_named_get_route_falls_through_to_publisher_fallback() {
        // Regression guard: HEAD /first-party/proxy must reach the publisher
        // fallback, not return a router-level 405. Legacy route_request proxies
        // every (method, path) combination not matched by a specific arm through
        // to the publisher origin.
        //
        // Without a live backend the publisher proxy errors (502/503), but the
        // important invariant is that the status is NOT 405.
        let router = test_router();
        let req = empty_request(Method::HEAD, "/first-party/proxy");

        let response = block_on(router.oneshot(req));

        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "HEAD on a named GET path should reach the publisher fallback, not return 405"
        );
    }

    #[test]
    fn dispatch_auction_with_missing_consent_store_returns_503() {
        let state = app_state_for_settings(settings_with_missing_consent_store());
        let router = TrustedServerApp::routes_for_state(&state);
        let body = json!({ "adUnits": [] }).to_string();
        let req = request_builder()
            .method(Method::POST)
            .uri("/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("should build auction request");

        let response = block_on(router.oneshot(req));

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "auction route should fail closed when configured consent store cannot be opened"
        );
    }

    #[test]
    fn dispatch_unregistered_method_returns_405_at_router_level() {
        // Documents the known router-level behavior for verbs outside the
        // publisher_fallback_methods() list (e.g. TRACE, CONNECT): the RouterService
        // returns 405 before the middleware chain runs, so FinalizeResponseMiddleware
        // does not inject TS headers at this layer.
        //
        // The full-system guarantee (TS headers on ALL responses including these 405s)
        // is maintained by the entry-point apply_finalize_headers call in main.rs.
        let router = test_router();
        let req = empty_request(
            Method::from_bytes(b"TRACE").expect("should parse TRACE"),
            "/",
        );

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

    #[test]
    fn edgezero_missing_consent_store_breaks_only_consent_routes() {
        let state = app_state_for_settings(settings_with_missing_consent_store());
        let router = TrustedServerApp::routes_for_state(&state);

        let admin_response =
            block_on(router.oneshot(empty_request(Method::POST, "/_ts/admin/keys/rotate")));
        assert_eq!(
            admin_response.status(),
            StatusCode::UNAUTHORIZED,
            "admin auth behavior should not depend on consent KV availability"
        );

        let auction_request = request_builder()
            .method(Method::POST)
            .uri("/auction")
            .body(Body::from(r#"{"adUnits":[]}"#))
            .expect("should build auction request");
        let auction_response = block_on(router.oneshot(auction_request));
        assert_eq!(
            auction_response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "auction should fail closed when configured consent KV cannot be opened"
        );

        let publisher_response =
            block_on(router.oneshot(empty_request(Method::GET, "/articles/example")));
        assert_eq!(
            publisher_response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "publisher fallback should fail closed when configured consent KV cannot be opened"
        );

        // Integration routes must NOT require the consent KV — runtime_services_for_consent_route
        // is wired only into the publisher and auction branches of dispatch_fallback, not into
        // the integration proxy branch. A missing consent store must not 503 integration routes.
        let integration_response =
            block_on(router.oneshot(empty_request(Method::GET, "/integrations/datadome/tags.js")));
        assert_ne!(
            integration_response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "integration routes should be unaffected by a missing consent KV store"
        );
    }

    #[test]
    fn dispatch_fallback_asset_route_skips_ec_finalization() {
        // Parity guard for the configured asset-route fallback: a GET matching a
        // proxy.asset_routes prefix must dispatch through the asset proxy (not the
        // publisher fallback) and skip EC finalization. Without a live asset
        // backend the proxy errors, but the response must carry the asset cache
        // policy and must NOT carry an EcFinalizeState — proving the asset branch
        // ran instead of the publisher fallback (which always attaches one).
        let settings = Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://assets.example.com"
            "#,
        )
        .expect("should parse asset-route settings");
        let state = build_state_from_settings(settings).expect("should build state");
        let router = TrustedServerApp::routes_for_state(&state);

        let response = block_on(router.oneshot(empty_request(Method::GET, "/.image/banner.png")));

        assert!(
            response
                .extensions()
                .get::<trusted_server_core::proxy::AssetProxyCachePolicy>()
                .is_some(),
            "asset-route responses should carry the asset cache policy"
        );
        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_none(),
            "asset-route responses must skip EC finalization (no EcFinalizeState)"
        );
    }

    #[test]
    fn dispatch_runs_request_filter_and_threads_response_effects() {
        // Regression guard for the EdgeZero request-filter bypass: the publisher
        // fallback must run the integration request-filter pipeline (DataDome
        // protection registers here) and thread the filter's response effects out
        // via extensions so the entry point applies them after EC finalization.
        // Without a live backend the publisher proxy errors, but the response must
        // still carry the RequestFilterEffects the filter emitted — proving the
        // filter ran on the dispatch path.
        let router = router_with_request_filters(vec![Arc::new(RecordingRequestFilter)]);
        let response = block_on(router.oneshot(empty_request(Method::GET, "/some-page")));

        let effects = response
            .extensions()
            .get::<RequestFilterEffects>()
            .expect("dispatched response should carry request-filter effects");
        assert!(
            effects
                .response_headers
                .iter()
                .any(|m| m.name == "x-filter-effect"),
            "the filter's response-header effect must be threaded out for the entry point to apply"
        );
    }

    #[test]
    fn dispatch_short_circuits_when_request_filter_responds() {
        // Regression guard: a request filter that responds (a DataDome
        // challenge/block) must short-circuit routing before the publisher
        // fallback, return its own response, still carry EcFinalizeState (legacy
        // parity: Respond keeps EC finalization), and thread its response effects.
        let router = router_with_request_filters(vec![Arc::new(ChallengeRequestFilter)]);
        let response = block_on(router.oneshot(empty_request(Method::GET, "/some-page")));

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a filter Respond must short-circuit routing with its own response"
        );
        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_some(),
            "a short-circuit filter response should still carry EcFinalizeState (legacy parity)"
        );
        let effects = response
            .extensions()
            .get::<RequestFilterEffects>()
            .expect("short-circuit response should carry request-filter effects");
        assert!(
            effects
                .response_headers
                .iter()
                .any(|m| m.name == "x-challenge"),
            "the filter's response-header effect must be threaded out"
        );
    }
}
