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
//!
//! # Route inventory
//!
//! | Method | Path pattern | Handler |
//! |--------|-------------|---------|
//! | GET | `/.well-known/trusted-server.json` | [`handle_trusted_server_discovery`] |
//! | POST | `/verify-signature` | [`handle_verify_signature`] |
//! | POST | `/_ts/admin/keys/rotate` | [`handle_rotate_key`] |
//! | POST | `/_ts/admin/keys/deactivate` | [`handle_deactivate_key`] |
//! | POST | `/admin/keys/rotate` (legacy alias) | [`handle_rotate_key`] |
//! | POST | `/admin/keys/deactivate` (legacy alias) | [`handle_deactivate_key`] |
//! | POST | `/_ts/api/v1/batch-sync` | [`handle_batch_sync`] |
//! | GET | `/_ts/api/v1/identify` | [`handle_identify`] |
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

use edgezero_adapter_fastly::FastlyRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{header, HandlerFuture, HeaderValue, Method, Request, Response};
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
use trusted_server_core::ec::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
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
/// be opened, returns `Err` so the caller can respond with 503 (fail-closed). This
/// matches the legacy `route_request` behavior where a misconfigured consent store
/// makes consent-dependent routes unavailable rather than proceeding without consent.
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
/// Extracts the client IP address from the [`FastlyRequestContext`] extension
/// inserted by `edgezero_adapter_fastly::dispatch`. TLS metadata is not
/// available through the `EdgeZero` context; scheme detection relies on the
/// trusted `fastly-ssl` header injected by `edgezero_main` after sanitization.
fn build_per_request_services(state: &AppState, ctx: &RequestContext) -> RuntimeServices {
    let client_ip = FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip);

    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(Arc::clone(&state.default_kv_store))
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
    pub(crate) finalize_kv_graph: Option<KvIdentityGraph>,
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
            finalize_kv_graph: self.finalize_kv_graph,
            eids_cookie: self.eids_cookie,
            sharedid_cookie: self.sharedid_cookie,
            is_real_browser: self.is_real_browser,
            services: self.services,
        }
    }
}

/// Derives device signals from the request's `User-Agent` header.
///
/// TLS and H2 fingerprints are unavailable in the `EdgeZero` request context
/// (see module docs), so classification is UA-only on this path — matching
/// the signals the legacy path effectively had after request conversion.
fn derive_request_device_signals(req: &Request) -> DeviceSignals {
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    DeviceSignals::derive(user_agent, None, None)
}

/// Builds the per-request EC state, mirroring the pre-routing prelude of the
/// legacy `route_request` step by step.
fn build_ec_request_state(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> EcRequestState {
    let device_signals = derive_request_device_signals(req);
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
        setup_error,
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn execute_named(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: NamedRouteHandler,
) -> Result<Response, EdgeError> {
    let services = build_per_request_services(&state, &ctx);
    let req = ctx.into_request();

    // S2S batch sync uses Bearer auth (not EC cookies), so it skips EC
    // context creation entirely — mirroring the dedicated early arm in the
    // legacy route_request.
    if matches!(handler, NamedRouteHandler::BatchSync) {
        return Ok(run_batch_sync(&state, &services, req));
    }

    let mut ec = build_ec_request_state(&state.settings, &services, &req);
    let mut response = match ec.setup_error.take() {
        Some(report) => http_error(&report),
        None => run_named_route(&state, &services, req, handler, &mut ec)
            .await
            .unwrap_or_else(|e| http_error(&e)),
    };
    response.extensions_mut().insert(ec.into_finalize_state());
    Ok(response)
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
    let device_signals = derive_request_device_signals(&req);
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
        finalize_kv_graph: None,
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

async fn dispatch_fallback(state: &AppState, services: &RuntimeServices, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let mut ec = build_ec_request_state(&state.settings, services, &req);
    if let Some(report) = ec.setup_error.take() {
        let mut response = http_error(&report);
        response.extensions_mut().insert(ec.into_finalize_state());
        return response;
    }

    let result = if uses_dynamic_tsjs_fallback(&method, &path) {
        handle_tsjs_dynamic(&req, &state.registry)
    } else if state.registry.has_route(&method, &path) {
        // Integration-proxy responses are not bounded by publisher.max_buffered_body_bytes.
        // Only the handle_publisher_request branch below routes through
        // resolve_publisher_response_buffered. Integration responses are small in practice
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
                handle_publisher_request(&state.settings, &state.registry, &publisher_services, req)
                    .await
                    .and_then(|pub_response| {
                        crate::resolve_publisher_response_buffered(
                            pub_response,
                            &state.settings,
                            &state.registry,
                        )
                    })
            }
            Err(e) => Err(e),
        }
    };

    let mut response = result.unwrap_or_else(|e| http_error(&e));
    response.extensions_mut().insert(ec.into_finalize_state());
    response
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
// Route registration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum NamedRouteHandler {
    TrustedServerDiscovery,
    VerifySignature,
    RotateKey,
    DeactivateKey,
    BatchSync,
    Identify,
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
    // Legacy aliases without the `/_ts` prefix, kept for parity with
    // route_request in main.rs. Auth coverage comes from settings.handlers
    // (enforced by AuthMiddleware), same as on the legacy path.
    NamedRoute {
        path: "/admin/keys/rotate",
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::RotateKey,
    },
    NamedRoute {
        path: "/admin/keys/deactivate",
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::DeactivateKey,
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
    use super::{build_state_from_settings, startup_error_router, AppState, TrustedServerApp};

    use std::sync::Arc;

    use edgezero_core::body::Body;
    use edgezero_core::http::{header, request_builder, Method, StatusCode};
    use edgezero_core::router::RouterService;
    use error_stack::Report;
    use futures::executor::block_on;
    use serde_json::json;
    use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
    use trusted_server_core::error::TrustedServerError;
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

    fn test_router() -> RouterService {
        let settings = Settings::from_toml(
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
        .expect("should parse test settings");
        let state = build_state_from_settings(settings).expect("should build test state");
        TrustedServerApp::routes_for_state(&state)
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
    fn dispatch_admin_alias_routes_are_registered_and_auth_gated() {
        // Parity guard for the legacy non-`/_ts` admin aliases: both alias
        // paths must be registered (no router-level 405) and protected by the
        // `^/admin` handler in the test settings, mirroring how legacy
        // route_request applies enforce_basic_auth before its route match.
        let router = test_router();

        for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
            let req = empty_request(Method::POST, path);

            let response = block_on(router.oneshot(req));

            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "POST {path} without credentials should be rejected by AuthMiddleware"
            );
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
            block_on(router.oneshot(empty_request(Method::POST, "/admin/keys/rotate")));
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
}
