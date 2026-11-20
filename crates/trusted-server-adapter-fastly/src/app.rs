//! Full `EdgeZero` application wiring for Trusted Server.
//!
//! Registers all routes for Trusted Server into a
//! [`RouterService`], with [`FinalizeResponseMiddleware`] (outermost) and
//! [`AuthMiddleware`] (inner). [`assemble_app`] builds the [`AppState`] and
//! the [`App`] once per sandbox through [`App::from_router`].
//!
//! `EdgeZero`'s current Fastly request context exposes client IP but not TLS
//! protocol or cipher metadata. The entry point injects a trusted `fastly-ssl`
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
//! | GET | `/_ts/admin/ec`, `/_ts/admin/ec/{id}`, `/_ts/admin/eids` | [`IdentityApp::mount`] |
//! | POST | `/_ts/api/v1/batch-sync` | [`handle_batch_sync`] |
//! | GET | `/_ts/api/v1/identify` | [`handle_identify`] |
//! | GET | `/_ts/set-tester` | [`handle_set_tester`] |
//! | GET | `/_ts/clear-tester` | [`handle_clear_tester`] |
//! | OPTIONS | `/_ts/api/v1/identify` | [`answer_identify_locally`] (CORS preflight) |
//! | other | `/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync` | `405` with `Allow` |
//! | POST | `/auction` | [`handle_auction`] |
//! | GET | `/first-party/proxy` | [`handle_first_party_proxy`] |
//! | GET | `/first-party/click` | [`handle_first_party_click`] |
//! | GET | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | GET | `/first-party/proxy-rebuild` | [`handle_first_party_proxy_rebuild`] |
//! | POST | `/first-party/proxy-rebuild` | [`handle_first_party_proxy_rebuild`] |
//! | GET | `/` and `/{*rest}` | tsjs (if `/static/tsjs=` prefix), integration proxy, or publisher fallback |
//! | POST, HEAD, OPTIONS, PUT, PATCH, DELETE | `/` and `/{*rest}` | integration proxy or publisher fallback |
//! | POST, HEAD, OPTIONS, PUT, PATCH, DELETE | named paths above | publisher fallback (legacy parity for non-primary methods) |
//!
//! > **Note:** Methods not in the list above (e.g. `TRACE`, `CONNECT`, WebDAV verbs) return a
//! > router-level 405. Legacy routing proxied *every* method through to the publisher origin.
//! > This is a known intentional restriction of the EdgeZero router; the entry point's
//! > `finalize` step still adds TS headers to those 405 responses.
//!
//! # EC identity lifecycle
//!
//! The `EdgeZero` path mirrors the EC identity lifecycle of the legacy
//! `route_request` (tracked in issue #495):
//!
//! - [`build_ec_request_state`] runs before every dispatched route (except
//!   batch-sync, which uses Bearer auth, and the read-only admin diagnostics)
//!   and reproduces the legacy
//!   pre-routing prelude: device signals, bot gate, `ts-eids`/`sharedid`
//!   cookie capture, geo lookup, [`EcContext`] creation, and KV-graph gating.
//! - `handle_auction` and integration proxy dispatch receive the same
//!   [`EcContext`], [`KvIdentityGraph`], and [`PartnerRegistry`] inputs as
//!   legacy; the publisher fallback generates EC IDs for browser navigations.
//! - Handlers attach an [`EcFinalizeState`] to the response via extensions;
//!   the entry point's `finalize` step pops it and runs `ec_finalize_response`,
//!   and post-delivery work runs pull sync once the response is delivered.
//!
//! ## Intentional deviations from legacy
//!
//! - **401 auth challenges**: [`AuthMiddleware`] short-circuits before the
//!   handler runs, so no EC state is built and `ec_finalize_response` does not
//!   run on these responses. Legacy ran EC finalization on its own auth
//!   challenges. Like the 401 geo-skip, this is privacy-conservative: no EC
//!   cookies are issued to unauthenticated callers.
//! - **Publisher responses** keep Fastly origin bodies streaming through the
//!   `EdgeZero` response body when the body is processable or pass-through.
//!   Adapters without streaming-body support still use the bounded buffered
//!   finalizer.
//! - **Router-level 405s** (unregistered verbs) skip EC finalization along
//!   with the middleware chain; the entry point still adds TS headers.
//!
//! # Startup error handling
//!
//! [`assemble_app`] and [`Hooks::routes`] return a settings or state failure
//! as the [`EdgeError`] from [`startup_error`], and [`App::from_router`]
//! returns a `configure` failure. The entry point answers either with
//! `EdgeZero`'s assembly-error response, which carries no TS headers, and
//! retains nothing, so the next request retries.

use std::cell::RefCell;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use crate::ec_kv::FastlyEcKvStore;
use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use edgezero_adapter_fastly::FASTLY_PLATFORM;
use edgezero_core::app::{App, Hooks, StoreMetadata, StoresMetadata};
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, RequestParts, Response, StatusCode, header,
};
use edgezero_core::manifest::ResolvedLoggingConfig;
use edgezero_core::outbound::ResponseDecoding;
use edgezero_core::router::RouterService;
use error_stack::{Report, ResultExt as _};
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::plan::validate_fastly_backend_bound;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::config_payload::DEFAULT_SECRET_STORE_ID;
use trusted_server_core::constants::{COOKIE_TS_EIDS, ENV_FASTLY_SERVICE_VERSION};
use trusted_server_core::ec::admin::deny_admin_diagnostic_fallback;
use trusted_server_core::ec::app::{IdentityApp, IdentityPlatform, IdentityProviders};
use trusted_server_core::ec::batch_sync::{answer_batch_sync_locally, handle_batch_sync};
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::coordinator::transport::{InFlightLookups, OutboundLookupTransport};
use trusted_server_core::ec::coordinator::{
    IdentityCoordinator as _, LookupContext, PendingLookups, ProviderCoordinator,
};
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::eid_types::UnixSeconds;
use trusted_server_core::ec::identify::{answer_identify_locally, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::kv_backend::EcKvStore;
use trusted_server_core::ec::page_load::NavigationPermit;
use trusted_server_core::ec::provider::{ClientSignals, RequestCookies};
use trusted_server_core::ec::sharedid::SharedIdRequest;
use trusted_server_core::ec::{EcContext, EcKvSnapshot, EidSyncSource};
use trusted_server_core::edge_app::install_edge_policies;
use trusted_server_core::error::{TrustedServerError, http_error, startup_error};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::integrations::{
    IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome,
};
use trusted_server_core::outbound::request_outbound_client;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformKvStore, RuntimeServices, StoreName,
};
use trusted_server_core::proxy::{
    AssetProxyCachePolicy, handle_asset_proxy_request, handle_first_party_click,
    handle_first_party_proxy, handle_first_party_proxy_rebuild, handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, PublisherOriginOptions,
    handle_page_bids, handle_publisher_request, handle_tsjs_dynamic, page_bids_preflight_denied,
    publisher_owner_error, publisher_response_into_streaming_response,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::{ProxyAssetRoute, Settings};
use trusted_server_core::settings_data::{DEFAULT_CONFIG_STORE_ID, get_settings_from_config_store};
use trusted_server_core::tester_cookie::{handle_clear_tester, handle_set_tester};
use trusted_server_core::trace::{TraceMetadata, TracePreDispatchHook};
use trusted_server_identity_providers::{
    build_after_response_coordinator, build_coordinator_with, partner_registry,
};

use crate::middleware::{
    AuthMiddleware, FinalizeResponseMiddleware, version_synthesized_headers,
    warn_unsynthesizable_operator_response_headers,
};
use crate::platform::{
    FastlyPlatformConfigStore, FastlyPlatformSecretStore, RequestGeo, UnavailableKvStore,
};
use crate::sandbox::RetainedApp;
use crate::tinybird::{QueueingTelemetrySink, TelemetryQueue};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeStoreConfig {
    pub(crate) config_store_name: StoreName,
    pub(crate) config_key: String,
    pub(crate) secret_store_name: StoreName,
}

impl RuntimeStoreConfig {
    /// Store bindings for the Fastly runtime.
    ///
    /// Store selection is a deploy-time input; no store selector reaches the
    /// Fastly runtime. `EdgeZero` links each selected physical store to the
    /// service version under its logical ID, so the runtime opens stores by
    /// logical ID and reads the config entry under that same ID for every
    /// publication target. Staging isolation comes from linking a different
    /// physical store, never from a different key.
    pub(crate) fn logical() -> Self {
        Self {
            config_store_name: StoreName::from(DEFAULT_CONFIG_STORE_ID),
            config_key: DEFAULT_CONFIG_STORE_ID.to_owned(),
            secret_store_name: StoreName::from(DEFAULT_SECRET_STORE_ID),
        }
    }
}

/// Application state built once per Wasm instance and shared for its lifetime.
///
/// By default Fastly serves one request per instance. With the opt-in reusable
/// sandbox feature and complete runtime bounds, a successful application build
/// retains this state across requests until the sandbox retires. Settings and
/// shared services live here; request-specific state must remain request-local.
pub(crate) struct AppState {
    pub(crate) settings: Arc<Settings>,
    pub(crate) orchestrator: Arc<AuctionOrchestrator>,
    pub(crate) registry: Arc<IntegrationRegistry>,
    pub(crate) default_kv_store: Arc<dyn PlatformKvStore>,
    /// Identity state (partner registry, identity-provider coordinators, EC
    /// record store, rate limiter) built once and shared by every identity
    /// route and EC finalization.
    pub(crate) identity: Arc<IdentityApp>,
}

pub(crate) fn load_settings_from_config_store(
    stores: &RuntimeStoreConfig,
) -> Result<Settings, Report<TrustedServerError>> {
    get_settings_from_config_store(
        &FastlyPlatformConfigStore::read_only(),
        &FastlyPlatformSecretStore::read_only(),
        &stores.config_store_name,
        &stores.config_key,
        &stores.secret_store_name,
    )
}

pub(crate) fn build_state_from_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(&settings)?);
    validate_fastly_backend_bound(&plan)?;
    // Each request's synthesized set leaves these entries out at debug, so
    // they warn here, once per sandbox.
    warn_unsynthesizable_operator_response_headers(&settings);
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)?;
    let registry = IntegrationRegistry::with_plan(&settings, plan)?;

    let default_kv_store = Arc::new(UnavailableKvStore) as Arc<dyn PlatformKvStore>;
    let settings = Arc::new(settings);
    let identity = build_identity_app(Arc::clone(&settings))?;

    Ok(Arc::new(AppState {
        settings,
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        default_kv_store,
        identity,
    }))
}

/// Builds the core identity app with Fastly's EC record store, Edge Rate
/// Limiting counter and identity providers.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the partner
/// configuration cannot build a partner registry, or when an identity
/// provider cannot run.
pub(crate) fn build_identity_app(
    settings: Arc<Settings>,
) -> Result<Arc<IdentityApp>, Report<TrustedServerError>> {
    // A provider that cannot run must stop startup: skipping it would quietly
    // hand its sources back to browser ingest.
    let partners = Arc::new(partner_registry(&settings.ec)?);
    let page = build_coordinator_with(&settings, Arc::clone(&partners))
        .change_context(TrustedServerError::Configuration {
            message: "identity provider configuration is invalid".to_owned(),
        })?
        .map(Arc::new);
    let after_response = build_after_response_coordinator(&settings, Arc::clone(&partners))
        .change_context(TrustedServerError::Configuration {
            message: "identity provider configuration is invalid".to_owned(),
        })?
        .map(Arc::new);
    identity_app_with_providers(
        settings,
        IdentityProviders {
            partners,
            page,
            after_response,
        },
    )
}

/// Builds the core identity app with Fastly's EC record store and Edge Rate
/// Limiting counter around already built identity `providers`.
///
/// # Errors
///
/// Returns the errors of [`IdentityApp::build`].
fn identity_app_with_providers(
    settings: Arc<Settings>,
    providers: IdentityProviders,
) -> Result<Arc<IdentityApp>, Report<TrustedServerError>> {
    let record_store = settings.ec.ec_store.as_deref().map(|store_name| {
        Arc::new(FastlyEcKvStore::new(store_name)) as Arc<dyn EcKvStore + Send + Sync>
    });
    IdentityApp::build(
        settings,
        IdentityPlatform {
            record_store,
            rate_limiter: Some(Arc::new(FastlyRateLimiter::new(RATE_COUNTER_NAME))),
            providers: Some(providers),
        },
    )
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Construct per-request [`RuntimeServices`] from the `EdgeZero` request context.
///
/// Uses the [`ClientInfo`] the entry point captured from the original
/// `FastlyRequest` (TLS protocol/cipher, JA4, H2 fingerprint, and server
/// hostname/region) and stored in the request extensions: the metadata
/// integration bot protection (e.g. `DataDome`) serializes, which the
/// reconstructed `EdgeZero` request cannot expose. Scheme detection continues
/// to rely on the trusted `fastly-ssl` header injected by the entry point after
/// sanitization.
///
/// The outbound client is the one `EdgeZero`'s Fastly request conversion
/// installs in the request context, with the proxy marker turned off. The
/// config and secret stores make their management API writes through it. The
/// clock is the context's monotonic clock. Geo answers from the request's
/// [`RequestGeo`], the lookup the entry point ran once.
///
/// With Tinybird enabled, the auction telemetry sink queues NDJSON on the
/// request's [`TelemetryQueue`], which the entry point inserts and sends after
/// delivery. Otherwise the sink is the no-op default.
///
/// # Errors
///
/// Returns an internal [`EdgeError`] when the request carries no outbound
/// client, no [`RequestGeo`], no [`ClientInfo`], or no telemetry queue while
/// Tinybird is enabled.
fn build_per_request_services(
    state: &AppState,
    ctx: &RequestContext,
) -> Result<RuntimeServices, EdgeError> {
    // No cooperative timer is installed: Fastly host calls block. EOF consumers
    // must bound their own transport calls using the remaining finish budget.
    let outbound_client = request_outbound_client(ctx)?;
    let request_geo = ctx
        .extensions()
        .get::<RequestGeo>()
        .cloned()
        .ok_or_else(|| EdgeError::internal(std::io::Error::other("request has no geo")))?;
    let client_info = ctx
        .extensions()
        .get::<ClientInfo>()
        .cloned()
        .ok_or_else(|| EdgeError::internal(std::io::Error::other("request has no client info")))?;

    let mut services = RuntimeServices::builder();
    if state.settings.tinybird.enabled {
        let queue = ctx
            .extensions()
            .get::<TelemetryQueue>()
            .cloned()
            .ok_or_else(|| {
                EdgeError::internal(std::io::Error::other("request has no telemetry queue"))
            })?;
        services = services.auction_telemetry_sink(Arc::new(QueueingTelemetrySink::new(
            &state.settings.tinybird,
            queue,
        )));
    }

    Ok(services
        .config_store(Arc::new(FastlyPlatformConfigStore::with_outbound_client(
            outbound_client.clone(),
        )))
        .secret_store(Arc::new(FastlyPlatformSecretStore::with_outbound_client(
            outbound_client.clone(),
        )))
        .kv_store(Arc::clone(&state.default_kv_store))
        // Constructed unconditionally, but only read when the assembly mode is a
        // shared-template one — which defaults to Inline, so this is inert until an
        // operator opts in.
        .template_cache(Arc::new(crate::template_cache::FastlyTemplateCache::new()))
        .template_assembler(Arc::new(crate::esi_assembly::FastlyTemplateAssembler))
        .outbound_client(outbound_client)
        .clock(ctx.monotonic_clock())
        .geo(Arc::new(request_geo))
        .client_info(client_info)
        .build())
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
/// The entry point's `finalize` step pops this from the routed response and
/// runs [`trusted_server_core::ec::finalize::ec_finalize_response`], and
/// post-delivery work runs pull sync from it, the same EC response lifecycle
/// the legacy path drives through `RouteResult`.
#[derive(Clone)]
pub(crate) struct EcFinalizeState {
    pub(crate) ec_context: EcContext,
    /// Whether EC finalization may write to the KV identity graph, which the
    /// entry point takes from the identity app built once per sandbox.
    pub(crate) use_finalize_kv: bool,
    pub(crate) eids_cookie: Option<String>,
    pub(crate) sharedid: SharedIdRequest,
}

/// Identity lookups dispatched for this navigation, collected before finalize.
pub(crate) struct IdentityLookups {
    pub(crate) coordinator: Arc<ProviderCoordinator>,
    pub(crate) transport: OutboundLookupTransport,
    pub(crate) pending: PendingLookups<InFlightLookups>,
}

/// What a navigation's after-response identity lookups need once the response
/// has been sent.
///
/// The router owns the request, so the request signals the providers read are
/// copied here. The lookups are dispatched only after the response, so they
/// never delay the page.
pub(crate) struct AfterResponseLookups {
    pub(crate) coordinator: Arc<ProviderCoordinator>,
    pub(crate) ec_id: String,
    pub(crate) client_ip: Option<IpAddr>,
    pub(crate) user_agent: Option<String>,
    pub(crate) cookie_header: String,
}

/// Identity work a navigation hands to the entry point.
///
/// An in-flight `EdgeZero` batch is not `Send`, so it cannot ride in response
/// extensions with [`EcFinalizeState`], and `EdgeZero` offers no other
/// request-scoped slot that both the handler and `finalize` reach. The work
/// travels through one request-local slot instead: the entry point clears it
/// before sending the request and `finalize` takes it. The batch stays bound to
/// the serving callback's outbound scope, which outlives `finalize`.
#[derive(Default)]
pub(crate) struct NavigationLookups {
    /// Page-phase lookups, collected before EC finalization.
    pub(crate) page: Option<IdentityLookups>,
    /// After-response lookups, dispatched once the response has been sent.
    pub(crate) after_response: Option<AfterResponseLookups>,
}

thread_local! {
    static NAVIGATION_LOOKUPS: RefCell<NavigationLookups> = RefCell::new(NavigationLookups::default());
}

/// Takes the identity work stored for the current request.
///
/// The entry point calls this before routing (discarding anything a prior
/// request left behind) and after the router returns.
pub(crate) fn take_navigation_lookups() -> NavigationLookups {
    NAVIGATION_LOOKUPS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Request signals lent to identity providers.
struct ProviderRequestSignals<'a> {
    client_ip: Option<IpAddr>,
    user_agent: Option<&'a str>,
    cookie_header: String,
}

impl<'a> ProviderRequestSignals<'a> {
    fn read(req: &'a Request, services: &RuntimeServices) -> Self {
        Self {
            // The trusted raw address: `EcContext::client_ip` is the
            // normalized EC-hash input, which drops IPv6 separators.
            client_ip: services.client_info().client_ip,
            user_agent: req
                .headers()
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            cookie_header: joined_cookie_header(req),
        }
    }
}

/// Joins every `Cookie` header on the request into one `; `-separated value.
///
/// HTTP/2 and HTTP/3 clients may split cookies across several headers.
fn joined_cookie_header(req: &Request) -> String {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Captures the request signals for after-response identity lookups on a
/// permitted publisher navigation.
///
/// Returns `None` when no after-response provider (including a pull-sync
/// partner) is enabled, the client failed the browser check, the request has
/// no EC, or the identity graph is unavailable.
fn prepare_after_response_lookups(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
    ec: &EcRequestState,
) -> Option<AfterResponseLookups> {
    let coordinator = Arc::clone(state.identity.after_response_identity()?);
    // Bot gate: pull sync and other after-response lookups disclose the EC ID
    // to partners, so they run only for recognized browsers.
    if !ec.is_real_browser {
        return None;
    }
    ec.kv_graph.as_ref()?;
    let ec_id = ec.ec_context.ec_value()?.to_owned();
    let signals = ProviderRequestSignals::read(req, services);
    Some(AfterResponseLookups {
        coordinator,
        ec_id,
        client_ip: signals.client_ip,
        user_agent: signals.user_agent.map(str::to_owned),
        cookie_header: signals.cookie_header,
    })
}

/// Starts identity-provider lookups for a permitted publisher navigation.
///
/// Returns `None` when no provider is enabled, the request has no EC, or the
/// identity graph is unavailable. For an EC minted on this request only
/// providers that do not send the EC ID are dispatched, against the row this
/// navigation will create.
async fn dispatch_identity_lookups(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
    ec: &mut EcRequestState,
) -> Option<IdentityLookups> {
    let coordinator = Arc::clone(state.identity.page_identity()?);
    let graph = ec.kv_graph.as_ref()?;
    let ec_id = ec.ec_context.ec_value()?.to_owned();
    if matches!(ec.ec_context.kv_snapshot(), EcKvSnapshot::NotRead) {
        let snapshot = graph.load_snapshot(&ec_id).await;
        ec.ec_context.set_kv_snapshot(snapshot);
    }
    // The request's own client: the batch it starts is bound to the serving
    // callback, so it stays in flight until `finalize` collects it.
    let transport = OutboundLookupTransport::for_services(services);
    let signals = ProviderRequestSignals::read(req, services);
    let now = trusted_server_core::ec::current_timestamp();
    let pending = coordinator
        .dispatch(
            &transport,
            &LookupContext {
                ec_id: &ec_id,
                snapshot: ec.ec_context.kv_snapshot(),
                minted_entry: ec.ec_context.pending_entry(),
                consent: ec.ec_context.consent(),
                client: ClientSignals {
                    ip: signals.client_ip,
                    user_agent: signals.user_agent,
                },
                publisher_domain: &state.settings.publisher.domain,
                cookies: RequestCookies::new(&signals.cookie_header),
                rate_limiter: state.identity.rate_limiter(),
                now: UnixSeconds::new(now),
            },
        )
        .await;
    Some(IdentityLookups {
        coordinator,
        transport,
        pending,
    })
}

/// Records on the EC context the sources whose identity providers' own
/// cookies on this request record an opt-out.
///
/// Runs on every route before any identity egress, so the publisher auction,
/// page-bids, `/auction` and identify withhold those sources on the request
/// the opt-out cookie arrives on. Only cookies are read: no lookup is made.
fn apply_cookie_opt_outs(state: &AppState, req: &Request, ec: &mut EcRequestState) {
    let coordinators = [
        state.identity.page_identity(),
        state.identity.after_response_identity(),
    ];
    if coordinators.iter().all(Option::is_none) {
        return;
    }
    let cookie_header = joined_cookie_header(req);
    let cookies = RequestCookies::new(&cookie_header)
        .at(UnixSeconds::new(
            trusted_server_core::ec::current_timestamp(),
        ))
        .where_gdpr_applies(ec.ec_context.consent().gdpr_applies);
    let sources: Vec<String> = coordinators
        .into_iter()
        .flatten()
        .flat_map(|coordinator| coordinator.cookie_opted_out_sources(cookies))
        .map(|source| source.as_str().to_owned())
        .collect();
    if !sources.is_empty() {
        log::debug!("Identity egress: withholding sources opted out in cookies: {sources:?}");
    }
    ec.ec_context.set_cookie_opted_out_sources(sources);
}

/// Per-request EC identity state built before dispatch, mirroring the
/// pre-routing prelude of the legacy `route_request` (device signals, bot
/// gate, cookie capture, consent/geo-aware [`EcContext`], and KV graph
/// gating).
struct EcRequestState {
    ec_context: EcContext,
    kv_graph: Option<KvIdentityGraph>,
    use_finalize_kv: bool,
    eids_cookie: Option<String>,
    sharedid: SharedIdRequest,
    is_real_browser: bool,
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
            use_finalize_kv: self.use_finalize_kv,
            eids_cookie: self.eids_cookie,
            sharedid: self.sharedid,
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
/// The entry point derives [`DeviceSignals`] from the original `FastlyRequest`
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
    identity: &IdentityApp,
    services: &RuntimeServices,
    req: &Request,
) -> EcRequestState {
    let settings = identity.settings().as_ref();
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
    let sharedid = SharedIdRequest::from_request(identity.sharedid_storage().cookie_name(), req);

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
                // Orphan-recovery eligibility is intentionally left false here.
                // Authorizing it during generic pre-routing would let named
                // routes and request-filter short circuits (e.g. a DataDome
                // challenge) reach EC finalization and rotate an identity off a
                // non-publisher response. It is granted only inside the
                // publisher fallback, after filters pass and the origin start
                // succeeds — see `dispatch_fallback`.
                (context, None)
            }
            Err(report) => (EcContext::default(), Some(report)),
        };

    // Bot gate: suppress KV-backed EC writes for unrecognized clients, except
    // consent withdrawals. Revocations keep the write path so tombstones stay
    // authoritative even for privacy-extension-heavy clients.
    let identity_graph = identity.identity_graph();
    let use_finalize_kv = identity_graph.is_some()
        && setup_error.is_none()
        && (is_real_browser || ec_consent_withdrawn(ec_context.consent()));
    let kv_graph = if is_real_browser {
        identity_graph
    } else {
        None
    };

    EcRequestState {
        ec_context,
        kv_graph,
        use_finalize_kv,
        eids_cookie,
        sharedid,
        is_real_browser,
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
        Err(report) => PreRoute::ShortCircuit {
            response: http_error(&report),
            effects: RequestFilterEffects::default(),
        },
    }
}

/// Attaches the EC finalize state and any non-empty request-filter response
/// effects to a dispatched response via extensions, so the entry point's
/// `finalize` step can run EC finalization and apply the filter effects after it
/// (legacy ordering).
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
    let services = build_per_request_services(&state, &ctx)?;
    let mut req = ctx.into_request()?;

    // S2S batch sync uses Bearer auth (not EC cookies), so it skips EC
    // context creation entirely — mirroring the dedicated early arm in the
    // legacy route_request. Batch-sync also skips request filters, matching
    // legacy, which returns before the filter step for this route.
    if matches!(handler, NamedRouteHandler::BatchSync) {
        return Ok(run_batch_sync(&state, req).await);
    }

    // An operator cache purge is not a reader request: running the EC lifecycle would
    // attach finalization state and could ingest the operator's cookies into KV.
    if matches!(handler, NamedRouteHandler::AdminCachePurge) {
        let principal = trusted_server_core::auth::authenticated_username(&req);
        let response = trusted_server_core::cache_purge::handle_cache_purge(
            &services,
            req,
            principal.as_deref(),
        )
        .await
        .unwrap_or_else(|error| http_error(&error));
        return Ok(response);
    }

    if let Err(report) = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
        &state.settings,
        &mut req,
    ) {
        return Ok(http_error(&report));
    }

    let mut ec = build_ec_request_state(&state.identity, &services, &req);
    apply_cookie_opt_outs(&state, &req, &mut ec);
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
            handle_verify_signature(&state.settings, services, req).await
        }
        NamedRouteHandler::RotateKey => handle_rotate_key(&state.settings, services, req).await,
        NamedRouteHandler::DeactivateKey => {
            handle_deactivate_key(&state.settings, services, req).await
        }
        NamedRouteHandler::AdminCachePurge => {
            unreachable!("cache purge should be handled before EC setup")
        }
        NamedRouteHandler::LegacyAdminDenied => Ok(legacy_admin_alias_denied()),
        NamedRouteHandler::BatchSync => {
            // Dispatched by execute_named before EC state is built.
            unreachable!("batch-sync should be handled by run_batch_sync")
        }
        NamedRouteHandler::Identify => {
            let identity_graph = state
                .identity
                .identity_graph()
                .filter(|_| req.method() == Method::GET);
            let Some(kv) = identity_graph else {
                return answer_identify_locally(&state.settings, &req);
            };
            handle_identify(
                &state.settings,
                &kv,
                state.identity.partners(),
                &req,
                &ec.ec_context,
            )
            .await
        }
        NamedRouteHandler::SetTester => handle_set_tester(&state.settings),
        NamedRouteHandler::ClearTester => handle_clear_tester(&state.settings),
        NamedRouteHandler::Auction => {
            ec.ec_context.set_eid_sync_source(EidSyncSource::Auction);
            let partner_registry = state.identity.partners().as_ref();
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(partner_registry)
            };
            handle_auction(
                &state.settings,
                &state.orchestrator,
                ec.kv_graph.as_ref(),
                registry_ref,
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
            let partner_registry = state.identity.partners().as_ref();
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(partner_registry)
            };
            let auction = AuctionDispatch {
                integration_registry: &state.registry,
                orchestrator: &state.orchestrator,
                slots: state.settings.creative_opportunity_slots(),
                registry: registry_ref,
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

/// Handles `POST /_ts/api/v1/batch-sync`, mirroring the legacy arm: identity
/// graph + partner registry + rate limiter, with a default EC context for
/// response finalization.
async fn run_batch_sync(state: &AppState, req: Request) -> Response {
    let eids_cookie = crate::extract_cookie_value(&req, COOKIE_TS_EIDS);
    let sharedid =
        SharedIdRequest::from_request(state.identity.sharedid_storage().cookie_name(), &req);

    let identity = &state.identity;
    let identity_graph = identity
        .identity_graph()
        .filter(|_| req.method() == Method::POST);
    let mut response = match identity_graph {
        None => answer_batch_sync_locally(&req),
        Some(kv) => handle_batch_sync(&kv, identity.partners(), identity.rate_limiter(), req)
            .await
            .unwrap_or_else(|e| http_error(&e)),
    };
    // Legacy parity: batch-sync responses still pass through
    // ec_finalize_response with a default EC context and no finalize KV graph.
    response.extensions_mut().insert(EcFinalizeState {
        ec_context: EcContext::default(),
        use_finalize_kv: false,
        eids_cookie,
        sharedid,
    });
    response
}

async fn execute_fallback(
    state: Arc<AppState>,
    ctx: RequestContext,
) -> Result<Response, EdgeError> {
    let services = build_per_request_services(&state, &ctx)?;
    let req = ctx.into_request()?;
    Ok(dispatch_fallback(&state, &services, req).await)
}

async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    mut req: Request,
) -> Response {
    if let Some(response) = deny_admin_diagnostic_fallback(&req) {
        return response;
    }

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if let Err(report) = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
        &state.settings,
        &mut req,
    ) {
        return http_error(&report);
    }

    let mut ec = build_ec_request_state(&state.identity, services, &req);
    apply_cookie_opt_outs(state, &req, &mut ec);
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
        handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SurrogateControl)
    } else if state.registry.has_route(&method, &path) {
        // Integration-proxy responses are not bounded by
        // publisher.max_buffered_body_bytes. Publisher fallback below uses the
        // publisher-specific streaming finalizer instead.
        state
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
        let is_navigation = is_navigation_request(&req);
        let mut has_permit = false;
        if is_navigation {
            ec.ec_context.set_eid_sync_source(EidSyncSource::Navigation);
            // The page-load write permit uses the stricter Fetch Metadata test
            // (spec §3.1); requests without it never write KV.
            if let Some(permit) = NavigationPermit::classify(&req) {
                ec.ec_context.set_navigation_permit(permit);
                has_permit = true;
            }
        }
        let is_publisher_navigation = ec.is_real_browser && is_navigation;
        // A new EC is minted only when this request may also write its row,
        // so a client without Fetch Metadata never receives a throwaway EC.
        if is_publisher_navigation
            && has_permit
            && let Err(err) = ec
                .ec_context
                .generate_if_needed(&state.settings, ec.kv_graph.as_ref())
                .await
        {
            log::warn!("EC generation failed for publisher proxy: {err:?}");
        }

        // Identity lookups start before the origin fetch so they overlap it;
        // the entry point collects them before EC finalization.
        if is_publisher_navigation
            && has_permit
            && let Some(lookups) = dispatch_identity_lookups(state, services, &req, &mut ec).await
        {
            NAVIGATION_LOOKUPS.with(|slot| slot.borrow_mut().page = Some(lookups));
        }
        // After-response providers only capture request signals here; they
        // are built and dispatched once the response has been sent.
        if is_publisher_navigation
            && has_permit
            && let Some(lookups) = prepare_after_response_lookups(state, services, &req, &ec)
        {
            NAVIGATION_LOOKUPS.with(|slot| slot.borrow_mut().after_response = Some(lookups));
        }

        // Run the server-side auction with the configured creative-
        // opportunity slots and collect dispatched bids from the lazy
        // publisher body stream. `handle_publisher_request` matches the
        // slots against the request path. The partner registry plus the
        // EC identity-graph KV (`ec.kv_graph`) enrich the bid request with
        // server-side EIDs, same as the legacy auction.
        let slots = state.settings.creative_opportunity_slots();
        let auction = AuctionDispatch {
            integration_registry: &state.registry,
            orchestrator: &state.orchestrator,
            slots,
            registry: Some(state.identity.partners().as_ref()),
        };
        match handle_publisher_request(
            &state.settings,
            services,
            ec.kv_graph.as_ref(),
            &mut ec.ec_context,
            auction,
            req,
            PublisherOriginOptions {
                edge_header: EdgeCacheHeader::SurrogateControl,
                input_cap: None,
                response_decoding: ResponseDecoding::Original,
            },
        )
        .await
        {
            Ok((pub_response, owner)) => {
                // Origin start succeeded on the sole publisher-page
                // path. Authorize orphan recovery only for real-browser
                // document navigations so named routes, integration
                // proxies, and filter short circuits cannot rotate an
                // identity.
                ec.ec_context.set_recovery_eligible(is_publisher_navigation);
                // The owner keeps the auction batch and the origin body
                // alive until the client body ends: `EdgeZero`'s
                // delivery takes it from the body and holds it to the end.
                publisher_response_into_streaming_response(
                    pub_response,
                    &method,
                    Arc::clone(&state.settings),
                    state.registry.as_ref(),
                    Arc::clone(&state.orchestrator),
                    services.clone(),
                )
                .await
                .and_then(|response| owner.attach(response).map_err(publisher_owner_error))
            }
            Err(e) => Err(e),
        }
    };

    let response = result.unwrap_or_else(|e| http_error(&e));
    attach_dispatch_extensions(response, ec, effects)
}

/// Handles the asset-route fallback on the `EdgeZero` path, mirroring the legacy
/// `route_request` asset branch.
///
/// Proxies the request to the configured asset origin and threads the
/// [`AssetProxyCachePolicy`] out via response extensions so the entry point's
/// `finalize` step can reapply protected cache directives after finalization.
/// EC finalization is intentionally skipped: no [`EcFinalizeState`] is
/// attached, matching the legacy `should_finalize_ec = false` behavior for
/// asset responses.
///
/// Like legacy `route_request`, asset bodies are streamed straight to the client
/// with no cap: the response keeps the origin stream, and `EdgeZero`'s response
/// delivery commits the headers and pumps the body chunk by chunk, so large
/// images never materialize in the Wasm heap. A `HEAD`, 204, or 304 carries an
/// empty body. A `HEAD` or 304 keeps the origin's `Content-Length`; a 204 has
/// none.
async fn dispatch_asset_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    asset_route: &ProxyAssetRoute,
    effects: &RequestFilterEffects,
) -> Response {
    log::info!("No explicit route matched; proxying via configured asset route");

    match handle_asset_proxy_request(&state.settings, services, req, asset_route).await {
        Ok(asset_response) => {
            let cache_policy = asset_response.cache_policy();
            let mut response = asset_response.into_response();
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
// Route registration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum NamedRouteHandler {
    TrustedServerDiscovery,
    VerifySignature,
    RotateKey,
    DeactivateKey,
    AdminCachePurge,
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

/// Every method an admin route must claim to keep non-primary methods from falling
/// through to the publisher with the `Authorization` header still attached.
///
/// Named for the legacy `/admin/*` aliases it was introduced for, and reused by every
/// route with the same requirement here and in the Axum and Spin adapters.
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
    // Every method is claimed, not just POST. A method this route did not claim would
    // fall through to the publisher, and `enforce_basic_auth` leaves the `Authorization`
    // header in place, so a GET would ship the shared admin credential to the origin.
    // The handler answers the non-POST methods with 405 itself.
    NamedRoute {
        path: "/_ts/admin/cache/purge",
        primary_methods: LEGACY_ADMIN_DENY_METHODS,
        handler: NamedRouteHandler::AdminCachePurge,
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
    // Partner APIs claim every method so no request on these paths, nor the
    // partner's Bearer token, reaches the publisher. The handlers answer
    // methods other than the primary ones with 405.
    NamedRoute {
        path: "/_ts/api/v1/batch-sync",
        primary_methods: LEGACY_ADMIN_DENY_METHODS,
        handler: NamedRouteHandler::BatchSync,
    },
    NamedRoute {
        path: "/_ts/api/v1/identify",
        primary_methods: LEGACY_ADMIN_DENY_METHODS,
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
    // Deprecated double-underscore alias. tsjs bundles served before the
    // `/_ts/page-bids` rename keep requesting this path from already-loaded
    // pages and browser caches; dropping it would strand SPA navigations
    // without ads until those bundles age out. See `PAGE_BIDS_LEGACY_PATH`;
    // removal is tracked by IABTechLab/trusted-server#970.
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
        // GET serves the click guard's navigation fallback: the creative iframe
        // is an opaque origin (sandbox without `allow-same-origin`), so its JSON
        // POST is blocked by CORS and the guard navigates here for a 302 instead.
        primary_methods: &[Method::GET, Method::POST],
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

/// Assembles the application from loaded `settings`.
///
/// Builds the [`AppState`] and its routes, then [`App::from_router`], which
/// applies the name, [`Hooks::configure`], and the platform check exactly once.
/// The entry point retains the result in its sandbox only on success.
/// `log_context` ends the settings or state failure log, so it matches the
/// request that triggered assembly.
///
/// # Errors
///
/// Returns the [`EdgeError`] from [`startup_error`] when settings or state
/// fail, or the `configure` error, either of which the entry point answers
/// with `EdgeZero`'s assembly-error response.
pub(crate) fn assemble_app(
    settings: Result<Settings, Report<TrustedServerError>>,
    log_context: &dyn fmt::Display,
) -> Result<RetainedApp, EdgeError> {
    let state = startup_state(settings, log_context)?;
    let app = App::from_router::<TrustedServerApp>(
        TrustedServerApp::routes_for_state(&state),
        FASTLY_PLATFORM,
    )?;
    Ok(RetainedApp { app, state })
}

/// Builds the [`AppState`] from loaded `settings`.
///
/// # Errors
///
/// Returns the [`EdgeError`] from [`startup_error`] after logging the full
/// report, followed by `log_context`.
fn startup_state(
    settings: Result<Settings, Report<TrustedServerError>>,
    log_context: &dyn fmt::Display,
) -> Result<Arc<AppState>, EdgeError> {
    settings
        .and_then(build_state_from_settings)
        .map_err(|report| {
            log::error!("failed to build application state: {report:?}{log_context}");
            startup_error(&report)
        })
}

impl TrustedServerApp {
    fn routes_for_state(state: &Arc<AppState>) -> RouterService {
        let mut router = RouterService::builder()
            .pre_dispatch_hook_arc(Arc::new(TracePreDispatchHook::new(
                Arc::clone(&state.settings),
                Arc::new(trace_metadata),
            )))
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)));

        let fallback_handler = fallback_route_handler(Arc::clone(state));

        // The core identity app registers `GET` on the admin EC and EIDs
        // lookups. Their other methods reach the catch-all fallback, which
        // denies admin diagnostics locally.
        router = state.identity.mount(router);

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

fn trace_metadata(request: &RequestParts) -> TraceMetadata {
    let client_info = request
        .extensions
        .get::<ClientInfo>()
        .cloned()
        .unwrap_or_default();
    let geo = request
        .extensions
        .get::<RequestGeo>()
        .and_then(RequestGeo::geo)
        .cloned();
    TraceMetadata { client_info, geo }
}

impl Hooks for TrustedServerApp {
    /// Installs the edge policies with the version headers as the synthesized set.
    ///
    /// The set is [`version_synthesized_headers`] for `FASTLY_SERVICE_VERSION`,
    /// read as the finalize middleware reads it. The entry point replaces the
    /// set for each request with the finalize headers that do not depend on
    /// the response.
    fn configure(app: &mut App) -> Result<(), EdgeError> {
        install_edge_policies(
            app,
            version_synthesized_headers(
                trusted_server_core::runtime_env!(ENV_FASTLY_SERVICE_VERSION)
                    .ok()
                    .as_deref(),
            ),
        )
    }

    /// Returns `[adapters.<adapter>.logging]` from the repository's `edgezero.toml`.
    ///
    /// `EdgeZero`'s `run_custom` and `serve_custom` call this before every
    /// callback to install the managed Fastly logger. `EdgeZero`'s
    /// `logging_config!` resolves and validates the table at compile time, the
    /// same way the `app!` macro does, so no request parses the manifest and it
    /// stays the single source of the Fastly logging settings.
    fn logging_for(adapter: &str) -> ResolvedLoggingConfig {
        edgezero_core::logging_config!("../../edgezero.toml", adapter)
    }

    fn name() -> &'static str {
        "TrustedServer"
    }

    fn routes() -> Result<RouterService, EdgeError> {
        let state = startup_state(
            load_settings_from_config_store(&RuntimeStoreConfig::logical()),
            &"",
        )?;
        Ok(Self::routes_for_state(&state))
    }

    fn stores() -> StoresMetadata {
        StoresMetadata {
            config: Some(StoreMetadata {
                default: DEFAULT_CONFIG_STORE_ID,
                ids: &[DEFAULT_CONFIG_STORE_ID],
            }),
            kv: Some(StoreMetadata {
                default: "trusted_server_kv",
                ids: &["trusted_server_kv"],
            }),
            secrets: Some(StoreMetadata {
                default: DEFAULT_SECRET_STORE_ID,
                ids: &[DEFAULT_SECRET_STORE_ID],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        App, AppState, AuctionDispatch, EcContext, EdgeCacheHeader, EidSyncSource, FASTLY_PLATFORM,
        HandlerFuture, NAMED_ROUTES, NamedRouteHandler, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH,
        PublisherOriginOptions, RuntimeStoreConfig, TrustedServerApp, assemble_app,
        build_orchestrator_with_plan, build_per_request_services, build_state_from_settings,
        compile_auction_plan, handle_publisher_request, load_settings_from_config_store,
        publisher_owner_error, publisher_response_into_streaming_response,
    };
    use base64::Engine as _;
    use bytes::Bytes;
    use edgezero_adapter_fastly::FastlyLogging;
    use edgezero_core::app::Hooks as _;
    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::{
        HeaderValue, Method, Request, Response, StatusCode, header, request_builder,
    };
    use edgezero_core::key_value_store::NoopKvStore;
    use edgezero_core::outbound::ResponseDecoding;
    use edgezero_core::params::PathParams;
    use edgezero_core::router::RouterService;
    use std::net::{IpAddr, Ipv4Addr};

    use error_stack::Report;
    use futures::StreamExt as _;
    use futures::executor::block_on;
    use std::borrow::Cow;
    use std::num::NonZeroUsize;
    use trusted_server_core::auction::AuctionEventBatch;
    use trusted_server_core::constants::{ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE};
    use trusted_server_core::ec::app::IdentityProviders;
    use trusted_server_core::ec::coordinator::{ProviderCoordinator, RegisteredProvider};
    use trusted_server_core::ec::device::DeviceSignals;
    use trusted_server_core::ec::eid_types::SourceDomain;
    use trusted_server_core::ec::provider::{
        CookieRead, EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome,
        ProviderCapabilities, ProviderEid, ProviderError, ProviderExpiry, ProviderRequest,
        ProviderResponse, ProviderUid, RequestCookies, SourceCapability, SourceInput,
    };
    use trusted_server_core::ec::registry::PartnerRegistry;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        HeaderMutation, IntegrationRegistry, IntegrationRequestFilter, RequestFilterDecision,
        RequestFilterEffects, RequestFilterInput,
    };
    use trusted_server_core::platform::{
        ClientInfo, GeoInfo, HttpClient, PlatformKvStore, PlatformTemplateCache,
        PlatformTemplateCacheReservation, RuntimeServices, TemplateCacheError, TemplateCacheKey,
        TemplateCacheLookup, TemplateCacheMiss, TemplateCacheReservation, TemplateEntry,
        TemplateMetadata,
    };
    use trusted_server_core::settings::{EcPartner, IdentityProviderConfig, Settings};
    use trusted_server_core::test_support::logging::LogCapture;
    use trusted_server_core::test_support::scripted_client::{
        RecordedRequest, ScriptedHttpClient, ScriptedReply, TestClock,
    };
    use trusted_server_core::test_support::{insert_outbound_client, nextjs_auction};

    use crate::platform::RequestGeo;
    use crate::tinybird::TelemetryQueue;

    #[test]
    fn hooks_store_metadata_matches_edgezero_manifest() {
        let manifest: toml::Value = toml::from_str(include_str!("../../../edgezero.toml"))
            .expect("should parse edgezero manifest");
        let manifest_stores = manifest
            .get("stores")
            .and_then(toml::Value::as_table)
            .expect("manifest should declare stores");
        let metadata = TrustedServerApp::stores();

        for (kind, runtime_store) in [
            (
                "config",
                metadata.config.expect("should declare config stores"),
            ),
            ("kv", metadata.kv.expect("should declare KV stores")),
            (
                "secrets",
                metadata.secrets.expect("should declare secret stores"),
            ),
        ] {
            let manifest_store = manifest_stores
                .get(kind)
                .and_then(toml::Value::as_table)
                .unwrap_or_else(|| panic!("manifest should declare {kind} stores"));
            let manifest_default = manifest_store
                .get("default")
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("manifest {kind} stores should declare a default"));
            let manifest_ids = manifest_store
                .get("ids")
                .and_then(toml::Value::as_array)
                .unwrap_or_else(|| panic!("manifest {kind} stores should declare ids"))
                .iter()
                .map(toml::Value::as_str)
                .collect::<Option<Vec<_>>>()
                .unwrap_or_else(|| panic!("manifest {kind} store ids should be strings"));

            assert_eq!(runtime_store.default, manifest_default);
            assert_eq!(runtime_store.ids, manifest_ids);
        }
    }

    #[test]
    fn logging_for_reads_the_fastly_logging_table() {
        let logging = FastlyLogging::from(TrustedServerApp::logging_for("fastly"));

        assert_eq!(
            logging.endpoint.as_deref(),
            Some("tslog"),
            "should log to the manifest's tslog endpoint"
        );
        assert_eq!(
            logging.level,
            log::LevelFilter::Info,
            "should log at the manifest's info level"
        );
        assert!(
            logging.echo_stdout,
            "should echo records to stdout as the manifest says"
        );
    }

    #[test]
    fn runtime_store_config_opens_logical_store_ids_and_key() {
        let stores = RuntimeStoreConfig::logical();

        assert_eq!(stores.config_store_name.as_ref(), "trusted_server_config");
        assert_eq!(stores.config_key, "trusted_server_config");
        assert_eq!(stores.secret_store_name.as_ref(), "trusted_server_secrets");
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

    fn route(router: &RouterService, request: edgezero_core::http::Request) -> Response {
        // `oneshot` skips `EdgeZero`'s request conversion, which installs the
        // adapter's default client, so install one that fails every request.
        route_through(router, request, &ScriptedHttpClient::forbidding())
    }

    /// Routes `request` with `origin` as its outbound client.
    fn route_through(
        router: &RouterService,
        mut request: edgezero_core::http::Request,
        origin: &ScriptedHttpClient,
    ) -> Response {
        // `oneshot` also skips the entry point's `prepare`, so add the client
        // info and the geo `prepare` would hold for the request's client IP,
        // without host geo data.
        insert_outbound_client(&mut request, HttpClient::with_client(origin.clone()));
        let client_ip = request
            .extensions_mut()
            .get_or_insert_default::<ClientInfo>()
            .client_ip;
        request
            .extensions_mut()
            .insert(RequestGeo::new(client_ip, None));
        block_on(router.oneshot(request)).expect("should route request")
    }

    /// Routes `request` with `client`, and the geo lookup `prepare` runs for
    /// the request's client IP.
    ///
    /// Viceroy geolocates loopback to a US privacy state, which tests that
    /// need consent to mint an EC rely on.
    fn route_with_host_geo(
        router: &RouterService,
        mut request: edgezero_core::http::Request,
        client: &ScriptedHttpClient,
    ) -> Response {
        insert_outbound_client(&mut request, HttpClient::with_client(client.clone()));
        let client_ip = request
            .extensions_mut()
            .get_or_insert_default::<ClientInfo>()
            .client_ip;
        request
            .extensions_mut()
            .insert(RequestGeo::lookup_once(client_ip));
        block_on(router.oneshot(request)).expect("should route request")
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

            [proxy]
            allowed_domains = ["*.example", "*.example.com"]

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [integrations.prebid]
            enabled = true
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [auction]
            enabled = true
            [auction.providers.prebid]
            protocol = "openrtb-2.6"
            profile = "prebid-server"
            endpoint = "https://test-prebid.com/openrtb2/auction"
            timeout_ms = 2000
            "#,
        )
        .expect("should parse test settings")
    }

    fn test_router() -> RouterService {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        TrustedServerApp::routes_for_state(&state)
    }

    /// Wraps `request` in a context that carries `client`, as `EdgeZero`'s Fastly
    /// request conversion installs its default client, and an empty
    /// [`RequestGeo`] and the request's [`ClientInfo`] (empty unless the
    /// caller set one), as the entry point inserts them.
    fn context_with_client(
        mut request: edgezero_core::http::Request,
        client: HttpClient,
    ) -> RequestContext {
        request.extensions_mut().insert(client);
        request.extensions_mut().insert(RequestGeo::default());
        request
            .extensions_mut()
            .get_or_insert_default::<ClientInfo>();
        RequestContext::new(request, PathParams::default())
    }

    #[test]
    fn per_request_services_carry_the_context_client_with_the_proxy_marker_off() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let client = HttpClient::with_client(ScriptedHttpClient::forbidding());
        assert!(
            client.proxy_marker_enabled(),
            "should start from a client whose marker is on"
        );
        let context = context_with_client(empty_request(Method::GET, "/article"), client);

        let services = build_per_request_services(&state, &context)
            .expect("should build per-request services");

        assert!(
            !services.outbound_client().proxy_marker_enabled(),
            "should turn the proxy marker off on the context client"
        );
    }

    #[test]
    fn per_request_services_reject_a_request_without_an_outbound_client() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let context = RequestContext::new(
            empty_request(Method::GET, "/article"),
            PathParams::default(),
        );

        let error = build_per_request_services(&state, &context)
            .expect_err("should reject a request with no outbound client");

        assert_eq!(
            error.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should answer a missing client with an internal error"
        );
        assert_eq!(
            error.to_string(),
            "internal error: request has no outbound client",
            "should name the missing client"
        );
    }

    #[test]
    fn per_request_services_reject_a_request_without_geo() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let mut request = empty_request(Method::GET, "/article");
        request
            .extensions_mut()
            .insert(ScriptedHttpClient::forbidding().http_client());
        let context = RequestContext::new(request, PathParams::default());

        let error = build_per_request_services(&state, &context)
            .expect_err("should reject a request with no geo");

        assert_eq!(
            error.to_string(),
            "internal error: request has no geo",
            "should name the missing geo"
        );
    }

    #[test]
    fn per_request_services_reject_a_request_without_client_info() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let mut request = empty_request(Method::GET, "/article");
        request
            .extensions_mut()
            .insert(ScriptedHttpClient::forbidding().http_client());
        request.extensions_mut().insert(RequestGeo::default());
        let context = RequestContext::new(request, PathParams::default());

        let error = build_per_request_services(&state, &context)
            .expect_err("should reject a request with no client info");

        assert_eq!(
            error.to_string(),
            "internal error: request has no client info",
            "should name the missing client info"
        );
    }

    #[test]
    fn per_request_services_answer_geo_from_the_entry_point_lookup() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let reader_ip = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7));
        let held = GeoInfo {
            city: "Springfield".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 39.78,
            longitude: -89.65,
            metro_code: 648,
            region: Some("IL".to_owned()),
            asn: None,
        };
        let mut request = empty_request(Method::GET, "/article");
        request.extensions_mut().insert(ClientInfo {
            client_ip: Some(reader_ip),
            ..ClientInfo::default()
        });
        let mut context =
            context_with_client(request, ScriptedHttpClient::forbidding().http_client());
        context
            .extensions_mut()
            .insert(RequestGeo::new(Some(reader_ip), Some(held)));

        let services = build_per_request_services(&state, &context)
            .expect("should build per-request services");

        assert_eq!(
            services
                .geo()
                .lookup(services.client_info().client_ip)
                .expect("should answer geo")
                .map(|geo| geo.city),
            Some("Springfield".to_owned()),
            "should answer the client IP from the entry point's lookup"
        );
    }

    #[test]
    fn per_request_services_queue_telemetry_when_tinybird_is_enabled() {
        let mut settings = test_settings();
        settings.tinybird = crate::tinybird::tests::enabled_config();
        let state = build_state_from_settings(settings).expect("should build test state");
        let queue = TelemetryQueue::default();
        let mut request = empty_request(Method::GET, "/article");
        request.extensions_mut().insert(queue.clone());
        let client = ScriptedHttpClient::forbidding();
        let context = context_with_client(request, client.http_client());

        let services = build_per_request_services(&state, &context)
            .expect("should build per-request services");
        block_on(services.auction_telemetry_sink().emit_auction_events(
            &services,
            AuctionEventBatch::new(vec![crate::tinybird::tests::test_row()]),
        ))
        .expect("should queue the batch");

        client.assert_no_requests();

        assert!(
            services.auction_telemetry_sink().is_enabled(),
            "should enable the telemetry sink"
        );
        assert_eq!(
            queue.take().len(),
            1,
            "should queue the batch on the request's queue instead of sending it"
        );
    }

    #[test]
    fn per_request_services_reject_tinybird_without_a_telemetry_queue() {
        let mut settings = test_settings();
        settings.tinybird = crate::tinybird::tests::enabled_config();
        let state = build_state_from_settings(settings).expect("should build test state");
        let context = context_with_client(
            empty_request(Method::GET, "/article"),
            ScriptedHttpClient::forbidding().http_client(),
        );

        let error = build_per_request_services(&state, &context)
            .expect_err("should reject a request with no telemetry queue");

        assert_eq!(
            error.to_string(),
            "internal error: request has no telemetry queue",
            "should name the missing queue"
        );
    }

    #[test]
    fn per_request_services_send_no_telemetry_when_tinybird_is_disabled() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let context = context_with_client(
            empty_request(Method::GET, "/article"),
            ScriptedHttpClient::forbidding().http_client(),
        );

        let services = build_per_request_services(&state, &context)
            .expect("should build per-request services without a queue");

        assert!(
            !services.auction_telemetry_sink().is_enabled(),
            "should keep the no-op sink"
        );
    }

    #[test]
    fn per_request_services_register_the_fastly_template_assembler() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let context = context_with_client(
            empty_request(Method::GET, "/article"),
            ScriptedHttpClient::forbidding().http_client(),
        );

        let services = build_per_request_services(&state, &context)
            .expect("should build per-request services");
        let template = format!(
            "<html><body>article{}</body></html>",
            trusted_server_core::publisher::AD_ASSEMBLY_SEAM
        );
        let fragment = b"<script>reader state</script>";
        let assembled = services
            .template_assembler()
            .assemble(template.as_bytes(), fragment)
            .expect("Fastly services should provide ESI assembly");

        assert_eq!(
            assembled,
            template
                .replace(
                    trusted_server_core::publisher::AD_ASSEMBLY_SEAM,
                    std::str::from_utf8(fragment).expect("fragment should be UTF-8")
                )
                .into_bytes()
        );
    }

    /// Builds a router whose `AppState` uses a registry containing the given
    /// request filters (and no routes), so dispatch-level request-filter
    /// behavior can be exercised without a real integration.
    fn router_with_request_filters(
        filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    ) -> RouterService {
        let settings = test_settings();
        let plan = Arc::new(
            trusted_server_core::auction::compile_auction_plan(&settings)
                .expect("should compile auction plan"),
        );
        let orchestrator =
            trusted_server_core::auction::build_orchestrator_with_plan(plan, &settings)
                .expect("should build orchestrator");
        let registry = IntegrationRegistry::from_request_filters(filters);
        let default_kv_store =
            Arc::new(crate::platform::UnavailableKvStore) as Arc<dyn super::PlatformKvStore>;
        let settings = Arc::new(settings);
        let identity =
            super::build_identity_app(Arc::clone(&settings)).expect("should build identity app");
        let state = Arc::new(super::AppState {
            settings,
            orchestrator: Arc::new(orchestrator),
            registry: Arc::new(registry),
            default_kv_store,
            identity,
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

    struct TraceCountingKv(Arc<AtomicUsize>);

    impl TraceCountingKv {
        fn unavailable(&self) -> edgezero_core::key_value_store::KvError {
            self.0.fetch_add(1, Ordering::SeqCst);
            edgezero_core::key_value_store::KvError::Unavailable
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformKvStore for TraceCountingKv {
        async fn get_bytes(
            &self,
            _key: &str,
        ) -> Result<Option<Bytes>, edgezero_core::key_value_store::KvError> {
            Err(self.unavailable())
        }
        async fn put_bytes(
            &self,
            _key: &str,
            _value: Bytes,
        ) -> Result<(), edgezero_core::key_value_store::KvError> {
            Err(self.unavailable())
        }
        async fn put_bytes_with_ttl(
            &self,
            _key: &str,
            _value: Bytes,
            _ttl: Duration,
        ) -> Result<(), edgezero_core::key_value_store::KvError> {
            Err(self.unavailable())
        }
        async fn delete(&self, _key: &str) -> Result<(), edgezero_core::key_value_store::KvError> {
            Err(self.unavailable())
        }
        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<edgezero_core::key_value_store::KvPage, edgezero_core::key_value_store::KvError>
        {
            Err(self.unavailable())
        }
    }

    #[test]
    fn trace_dispatch_bypasses_fastly_identity_filters_and_telemetry() {
        let mut settings = test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &serde_json::json!({"enabled":true,"trace_page_enabled":true}),
            )
            .expect("should configure trace");
        settings.tinybird = crate::tinybird::tests::enabled_config();
        let mut state = build_state_from_settings(settings).expect("should build trace state");
        let kv = Arc::new(AtomicUsize::new(0));
        let telemetry = TelemetryQueue::default();
        let filter = Arc::new(Mutex::new(None));
        let mutable = Arc::get_mut(&mut state)
            .expect("should retain unique test state before router construction");
        mutable.default_kv_store = Arc::new(TraceCountingKv(Arc::clone(&kv)));
        mutable.registry = Arc::new(IntegrationRegistry::from_request_filters(vec![Arc::new(
            ClientInfoCapturingFilter(Arc::clone(&filter)),
        )]));
        let router = TrustedServerApp::routes_for_state(&state);
        for (method, path, status) in [
            (Method::GET, "/_ts/trace/state", StatusCode::OK),
            (Method::GET, "/_ts/trace", StatusCode::OK),
            (Method::GET, "/_ts/trace/assets/v1.js", StatusCode::OK),
            (Method::POST, "/_ts/trace/enable", StatusCode::FORBIDDEN),
            (Method::PATCH, "/_ts/trace", StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let mut request = empty_request(method, path);
            request.extensions_mut().insert(telemetry.clone());
            let response = route(&router, request);
            assert_eq!(
                response.status(),
                status,
                "should serve trace through the terminal dispatcher"
            );
            assert!(
                response
                    .extensions()
                    .get::<RequestFilterEffects>()
                    .is_none(),
                "should not assemble ordinary filter effects"
            );
            assert!(
                response
                    .extensions()
                    .get::<super::EcFinalizeState>()
                    .is_none(),
                "should not enter identity finalization"
            );
        }
        assert_eq!(
            kv.load(Ordering::SeqCst),
            0,
            "should never touch identity KV for trace"
        );
        assert!(
            telemetry.take().is_empty(),
            "should never queue auction telemetry for trace"
        );
        assert!(
            filter
                .lock()
                .expect("should inspect filter counter")
                .is_none(),
            "should never run integration request filters for trace"
        );
    }

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

    /// [`test_settings`] with `count` standard providers in place of its own,
    /// each on its own port, so each is a distinct Fastly backend tuple.
    fn settings_with_provider_tuples(count: u16) -> Settings {
        let mut settings = test_settings();
        settings.auction.providers = (0..count)
            .map(|index| {
                (
                    format!("provider-{index}")
                        .parse()
                        .expect("should parse provider ID"),
                    trusted_server_core::auction::ProviderConfig {
                        allow_insecure_http: false,
                        protocol: "openrtb-2.6".to_string(),
                        profile: "standard".to_string(),
                        endpoint: format!("https://bid.example:{}/openrtb", 9000 + index),
                        timeout_ms: Some(1000),
                        routing: trusted_server_core::auction::RoutingMode::AllEligible,
                        notifications: trusted_server_core::auction::NotificationConfig::default(),
                        profile_config: serde_json::json!({}),
                    },
                )
            })
            .collect();
        settings
    }

    #[test]
    fn state_builder_enforces_the_fastly_backend_bound() {
        build_state_from_settings(settings_with_provider_tuples(160))
            .expect("should start with 160 distinct provider backend tuples");

        let Err(error) = build_state_from_settings(settings_with_provider_tuples(161)) else {
            panic!("should refuse to start with 161 distinct provider backend tuples");
        };
        assert!(
            error.to_string().contains("161") && error.to_string().contains("160"),
            "should name the tuple count and the bound: {error}"
        );
    }

    #[test]
    fn state_builder_warns_once_for_an_unsynthesizable_operator_response_header() {
        let mut settings = test_settings();
        settings.response_headers =
            HashMap::from([("content-type".to_owned(), "text/plain".to_owned())]);
        let capture = LogCapture::start();

        build_state_from_settings(settings).expect("should build state with the operator header");

        let records = capture.take_all();
        drop(capture);
        let naming_content_type: Vec<(log::Level, &str)> = records
            .iter()
            .filter(|record| record.message.contains("`content-type`"))
            .map(|record| (record.level, record.message.as_str()))
            .collect();
        assert!(
            matches!(naming_content_type.as_slice(), [(log::Level::Warn, _)]),
            "should warn once, naming `content-type`, as it loads the settings, got {naming_content_type:?}"
        );
    }

    #[test]
    fn startup_registers_aps_renderer_route() {
        let mut settings = test_settings();
        settings.auction.providers.clear();
        settings.auction.providers.insert(
            "aps-main".parse().expect("should parse APS provider ID"),
            trusted_server_core::auction::ProviderConfig {
                allow_insecure_http: false,
                protocol: "openrtb-2.6".to_string(),
                profile: "aps".to_string(),
                endpoint: "https://aps.example/e/pb/bid".to_string(),
                timeout_ms: None,
                routing: trusted_server_core::auction::RoutingMode::AllEligible,
                notifications: trusted_server_core::auction::NotificationConfig::default(),
                profile_config: serde_json::json!({"account_id":"example-account"}),
            },
        );

        let state = build_state_from_settings(settings)
            .expect("Fastly startup should register APS renderer");
        assert!(
            state.registry.has_route(
                &edgezero_core::http::Method::GET,
                "/integrations/aps/renderer"
            ),
            "Fastly startup registry should expose the APS renderer"
        );
    }

    #[test]
    fn hooks_routes_report_a_missing_config_blob_as_the_startup_error() {
        // The test runtime links an empty `trusted_server_config` store, so the
        // app config blob is missing.
        let Err(error) = TrustedServerApp::routes() else {
            panic!("should fail route building without an app config blob");
        };

        assert_eq!(
            error.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should keep today's configuration-failure status"
        );
    }

    /// Completes `envelope` and returns the response the client would receive.
    fn delivered_response(envelope: edgezero_core::ResponseEgressEnvelope) -> Response {
        let Ok((prepared, _policy, mut attempt, clock)) = envelope.begin(
            edgezero_core::response_egress_framing::ResponseEgressFramingMode::HostAutomatic,
        ) else {
            panic!("should begin response egress");
        };
        assert!(attempt.begin_writing(), "should start writing");
        assert!(attempt.complete(clock.now()), "should complete the body");
        prepared.into_response()
    }

    #[test]
    fn missing_config_blob_gets_500_then_recovers_in_same_sandbox() {
        let mut sandbox = crate::sandbox::Sandbox::default();

        // Request 1: the linked store has no app config blob.
        let error = sandbox
            .initialize(|| {
                assemble_app(
                    load_settings_from_config_store(&RuntimeStoreConfig::logical()),
                    &"",
                )
            })
            .expect_err("should fail assembly without an app config blob");
        assert!(
            sandbox.state().is_none(),
            "should retain nothing after a failed assembly"
        );
        let first = crate::sandbox::SandboxCounters::capture(&sandbox, 1, "request-1");
        let clock = edgezero_core::time::MonotonicClock::default();
        let response = delivered_response(App::assembly_error_egress(
            error,
            Method::GET,
            clock.now(),
            clock,
        ));
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should answer with today's configuration-failure status"
        );
        assert!(
            response
                .headers()
                .get(trusted_server_core::constants::HEADER_X_TS_VERSION)
                .is_none(),
            "should carry no configured headers on an assembly error"
        );
        let body = block_on(response.into_body().into_bytes_bounded(4096))
            .expect("should read the assembly error body");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("should answer with EdgeZero's JSON error");
        assert!(
            json["error"].is_object(),
            "should render EdgeZero's error body: {json}"
        );

        // Request 2: the blob is restored.
        sandbox
            .initialize(|| assemble_app(Ok(test_settings()), &""))
            .expect("should assemble once the blob is restored");
        let second = crate::sandbox::SandboxCounters::capture(&sandbox, 2, "request-2");

        assert!(
            sandbox.state().is_some(),
            "should retain the recovered application"
        );
        assert_eq!(
            second.builds,
            first.builds + 1,
            "should count one more build in the same sandbox"
        );
    }

    #[test]
    fn assembly_runs_configure_once_with_the_edge_policies() {
        let retained = assemble_app(Ok(test_settings()), &"").expect("should assemble the app");
        let headers = edgezero_core::http::HeaderMap::new();
        let request_start = edgezero_core::time::MonotonicInstant::now();
        let egress_started_at = request_start
            .checked_add(Duration::from_secs(5))
            .expect("should represent an egress start after the request start");
        let head = edgezero_core::response_egress::ResponseEgressHead::new(
            StatusCode::OK,
            edgezero_core::http::Version::HTTP_11,
            &headers,
            None,
            request_start,
            None,
        );

        let policy = (retained.app.response_egress_policy())(&head, egress_started_at);

        assert_eq!(
            retained.app.platform(),
            FASTLY_PLATFORM,
            "should assemble under Fastly's platform metadata"
        );
        assert_eq!(
            retained.app.name(),
            TrustedServerApp::name(),
            "should name the app after its hooks"
        );
        assert_eq!(
            policy.write_deadline.instant(),
            egress_started_at
                .checked_add(trusted_server_core::edge_app::RESPONSE_WRITE_DEADLINE)
                .expect("should represent the expected deadline"),
            "should install configure's write deadline instead of EdgeZero's default"
        );
    }

    /// Sets `FASTLY_SERVICE_VERSION` for one test and restores it on drop.
    struct ServiceVersionGuard(Option<String>);

    impl ServiceVersionGuard {
        fn set(value: &str) -> Self {
            let previous = std::env::var(ENV_FASTLY_SERVICE_VERSION).ok();
            // SAFETY: the wasm32-wasip1 test binary runs its tests one at a
            // time on one thread, so nothing reads the environment meanwhile.
            unsafe { std::env::set_var(ENV_FASTLY_SERVICE_VERSION, value) };
            Self(previous)
        }
    }

    impl Drop for ServiceVersionGuard {
        fn drop(&mut self) {
            // SAFETY: as in `set`, the test binary is single-threaded.
            unsafe {
                match &self.0 {
                    Some(previous) => std::env::set_var(ENV_FASTLY_SERVICE_VERSION, previous),
                    None => std::env::remove_var(ENV_FASTLY_SERVICE_VERSION),
                }
            }
        }
    }

    #[test]
    fn configure_failure_fails_assembly_with_500_and_retries_next_request() {
        let mut sandbox = crate::sandbox::Sandbox::default();

        // A service version past the synthesized set's 4096-byte bound is the
        // smallest configure failure a deployment can reach.
        let guard = ServiceVersionGuard::set(&"9".repeat(4097));
        let error = sandbox
            .initialize(|| assemble_app(Ok(test_settings()), &""))
            .expect_err("should fail assembly when configure rejects the synthesized set");
        drop(guard);

        assert_eq!(
            error.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should answer a configure failure with a 500"
        );
        assert!(
            sandbox.state().is_none(),
            "should retain nothing after configure fails"
        );
        sandbox
            .initialize(|| assemble_app(Ok(test_settings()), &""))
            .expect("should assemble on the next request once configure succeeds");
        assert_eq!(
            sandbox.initialization_attempts(),
            2,
            "should retry assembly on the next request"
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
        // (reads settings + request headers only) and the finalize middleware
        // reads the geo-less RequestGeo that `route` inserts, so nothing calls a
        // Fastly ABI.
        let router = test_router();
        let req = empty_request(Method::POST, "/_ts/admin/keys/rotate");

        let response = route(&router, req);

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
    fn proxy_rebuild_registers_get_and_post() {
        let route = NAMED_ROUTES
            .iter()
            .find(|route| route.path == "/first-party/proxy-rebuild")
            .expect("should register proxy rebuild route");
        assert!(matches!(
            route.handler,
            NamedRouteHandler::FirstPartyProxyRebuild
        ));
        assert!(route.primary_methods.contains(&Method::GET));
        assert!(route.primary_methods.contains(&Method::POST));
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
    fn cache_purge_claims_every_method_that_could_reach_the_publisher() {
        // The guard this route exists behind. `enforce_basic_auth` authenticates on the raw
        // path and leaves the `Authorization` header attached, so any method this route does
        // not claim falls through to the publisher fallback carrying the shared admin
        // credential to the origin. Asserted against the fallback list itself rather than a
        // copy of it, so a method added there cannot quietly open a hole here.
        let route = NAMED_ROUTES
            .iter()
            .find(|route| route.path == "/_ts/admin/cache/purge")
            .expect("cache purge must be a named route");

        for method in super::publisher_fallback_methods() {
            assert!(
                route.primary_methods.contains(&method),
                "{method} /_ts/admin/cache/purge must be claimed, or it reaches the publisher \
                 with the admin credential attached"
            );
        }
        assert!(matches!(route.handler, NamedRouteHandler::AdminCachePurge));
    }

    #[test]
    fn cache_purge_has_no_legacy_unauthenticated_alias() {
        // The production basic-auth regex is `^/_ts/admin`. An `/admin/...` spelling would
        // not match it, so it must not exist at all.
        assert!(
            !NAMED_ROUTES
                .iter()
                .any(|route| route.path == "/admin/cache/purge"),
            "an /admin-prefixed alias would sit outside the basic-auth regex"
        );
    }

    #[test]
    fn admin_ec_lookup_routes_are_registered() {
        let router = test_router();

        let routes = router.routes();

        for path in ["/_ts/admin/ec", "/_ts/admin/ec/{id}", "/_ts/admin/eids"] {
            assert!(
                routes
                    .iter()
                    .any(|route| route.path() == path && *route.method() == Method::GET),
                "should register GET {path} through the core identity app"
            );
            assert!(
                !NAMED_ROUTES.iter().any(|route| route.path == path),
                "{path} should be mounted by the core identity app, not the adapter table"
            );
        }
    }

    #[test]
    fn page_bids_serves_canonical_path_and_deprecated_alias() {
        // The SPA re-auction endpoint lives at the canonical single-underscore
        // `/_ts/page-bids`, matching every other internal route. The deprecated
        // `/__ts/page-bids` alias must stay registered to the same handler with
        // the same methods until pre-rename tsjs bundles age out of browser
        // caches — dropping it would leave those clients without ads on SPA
        // navigations.
        //
        // The paths are literals, not `PAGE_BIDS_PATH` / `PAGE_BIDS_LEGACY_PATH`.
        // Looking a route up by the same const it was registered with is
        // tautological: it keeps passing if the const's value changes, which is
        // exactly the break that would silently desync the server from the tsjs
        // client's hardcoded fetch path. Pin the consts to their literals too so
        // a rename has to be deliberate.
        assert_eq!(
            PAGE_BIDS_PATH, "/_ts/page-bids",
            "canonical page-bids path must match the path tsjs fetches"
        );
        assert_eq!(
            PAGE_BIDS_LEGACY_PATH, "/__ts/page-bids",
            "legacy alias must match the path pre-rename tsjs bundles fetch"
        );

        for path in ["/_ts/page-bids", "/__ts/page-bids"] {
            let route = NAMED_ROUTES
                .iter()
                .find(|route| route.path == path)
                .unwrap_or_else(|| panic!("{path} should be registered"));

            assert!(
                matches!(route.handler, NamedRouteHandler::PageBids),
                "{path} must map to the page-bids handler"
            );
            assert_eq!(
                route.primary_methods,
                &[Method::GET, Method::OPTIONS],
                "{path} must handle GET and OPTIONS directly, not fall through to the publisher"
            );
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

                let response = route(&router, req);

                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {path} with Authorization must be denied locally (404), not proxied to publisher"
                );
            }
        }
    }

    #[test]
    fn authenticated_admin_diagnostic_fallback_is_denied_locally() {
        let router = test_router();
        let ec_id = format!("{}.abc123", "a".repeat(64));
        let valid_paths = [
            "/_ts/admin/ec".to_owned(),
            format!("/_ts/admin/ec/{ec_id}"),
            "/_ts/admin/eids".to_owned(),
        ];

        for path in valid_paths {
            for method in [
                Method::POST,
                Method::HEAD,
                Method::OPTIONS,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ] {
                let request = request_builder()
                    .method(method.clone())
                    .uri(format!("https://test-publisher.com{path}"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body(Body::from("sensitive-admin-body"))
                    .expect("should build authenticated admin request");
                let response = route(&router, request);

                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(
                    response
                        .headers()
                        .get(header::ALLOW)
                        .and_then(|v| v.to_str().ok()),
                    Some("GET")
                );
                assert_eq!(
                    response
                        .headers()
                        .get(header::CACHE_CONTROL)
                        .and_then(|v| v.to_str().ok()),
                    Some("no-store")
                );
            }
        }

        for path in [
            "/_ts/admin/ec/".to_owned(),
            format!("/_ts/admin/ec/{ec_id}/extra"),
            "/_ts/admin/eids/".to_owned(),
            "/_ts/admin/eids/extra".to_owned(),
            "/_ts/admin/eids.json".to_owned(),
            "/_ts/admin/ec;foo".to_owned(),
            format!("/_ts/admin/ec%2F{ec_id}"),
            // Percent-encoded separators match the `^/_ts/admin` basic-auth
            // handler but not a literal-slash namespace check, so they must be
            // reserved before publisher fallback forwards credentials upstream.
            "/_ts/admin%2Fec".to_owned(),
            "/_ts/admin%2fec".to_owned(),
            // Retired non-`/_ts` alias namespace: only the two exact paths are
            // routed to a local deny, so descendants and encoded separators must
            // be reserved at the shared fallback boundary.
            "/admin/keys".to_owned(),
            "/admin/keys/rotate/extra".to_owned(),
            "/admin/keys%2Frotate".to_owned(),
            "/admin%2fkeys/rotate".to_owned(),
        ] {
            for method in [Method::GET, Method::POST] {
                let request = request_builder()
                    .method(method)
                    .uri(format!("https://test-publisher.com{path}"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body(Body::from("sensitive-admin-body"))
                    .expect("should build malformed admin request");
                let response = route(&router, request);

                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert_eq!(
                    response
                        .headers()
                        .get(header::CACHE_CONTROL)
                        .and_then(|v| v.to_str().ok()),
                    Some("no-store")
                );
            }
        }
    }

    #[test]
    fn authenticated_forwarder_preserves_configured_identify_cors_policy() {
        let mut settings = test_settings();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings.publisher.cookie_domain = ".publisher.example.com".to_owned();
        settings.trusted_forwarder = Some(trusted_server_core::settings::TrustedForwarderConfig {
            auth_header: "x-ts-forwarder-auth".to_owned(),
            shared_secret: trusted_server_core::redacted::Redacted::new(
                "fictional-forwarder-secret-0123456789".to_owned(),
            ),
        });
        let state = build_state_from_settings(settings).expect("should build forwarder state");
        let router = TrustedServerApp::routes_for_state(&state);
        for (origin, allowed) in [
            ("https://publisher.example.com", true),
            ("https://other.publisher.example.com:9443", true),
            ("https://foreign.example.com", false),
            ("http://publisher.example.com", false),
        ] {
            let request = request_builder()
                .method(Method::OPTIONS)
                .uri("http://internal.example.com/_ts/api/v1/identify")
                .header(header::HOST, "internal.example.com")
                .header(header::ORIGIN, origin)
                .header("x-forwarded-host", "shop.publisher.example.com:8443")
                .header("x-forwarded-proto", "https")
                .header(
                    "x-ts-forwarder-auth",
                    "fictional-forwarder-secret-0123456789",
                )
                .body(Body::empty())
                .expect("should build forwarded identify preflight");
            let response = route(&router, request);
            assert_eq!(
                response.status(),
                if allowed {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                },
                "should retain configured identify origin policy for {origin}"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .and_then(|value| value.to_str().ok()),
                allowed.then_some(origin),
                "should echo only an allowed browser Origin"
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
        let response = route(
            &router,
            empty_request(Method::OPTIONS, "/_ts/api/v1/identify"),
        );

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "OPTIONS identify should be answered by the CORS preflight handler"
        );
    }

    fn partner_api_request(method: Method, path: &str) -> edgezero_core::http::Request {
        request_builder()
            .method(method)
            .uri(format!("https://publisher.example.com{path}"))
            .header("host", "publisher.example.com")
            .header("authorization", "Bearer fictional-partner-token-0123456789")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"mappings":[]}"#))
            .expect("should build partner API request")
    }

    fn partner_api_statuses(
        router: &RouterService,
        origin: &ScriptedHttpClient,
    ) -> [StatusCode; 2] {
        [
            route_through(
                router,
                partner_api_request(Method::GET, "/_ts/api/v1/identify"),
                origin,
            )
            .status(),
            route_through(
                router,
                partner_api_request(Method::POST, "/_ts/api/v1/batch-sync"),
                origin,
            )
            .status(),
        ]
    }

    #[test]
    fn scripted_origin_receives_publisher_fallback_requests() {
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        origin.respond_to(
            Method::GET,
            &format!("{}/article", test_settings().publisher.origin_url),
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "text/plain")
                .body("served by origin"),
        );
        let router = test_router();

        let response = route_through(&router, empty_request(Method::GET, "/article"), &origin);

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should proxy the publisher page to the origin"
        );
        assert_eq!(
            origin
                .requests()
                .iter()
                .map(RecordedRequest::method_and_path)
                .collect::<Vec<_>>(),
            vec!["GET /article".to_owned()],
            "should record the proxied publisher request"
        );
    }

    #[test]
    fn identify_and_batch_sync_answer_501_locally_without_record_store() {
        let origin = ScriptedHttpClient::forbidding();
        let router = test_router();

        let statuses = partner_api_statuses(&router, &origin);

        assert_eq!(
            statuses,
            [StatusCode::NOT_IMPLEMENTED; 2],
            "identify and batch sync should answer 501 without ec.ec_store"
        );
        origin.assert_no_requests();
    }

    #[test]
    fn identify_and_batch_sync_answer_other_methods_405_locally() {
        let cases = [
            (Method::HEAD, "/_ts/api/v1/identify"),
            (Method::POST, "/_ts/api/v1/identify"),
            (Method::PUT, "/_ts/api/v1/identify"),
            (Method::PATCH, "/_ts/api/v1/identify"),
            (Method::DELETE, "/_ts/api/v1/identify"),
            (Method::GET, "/_ts/api/v1/batch-sync"),
            (Method::HEAD, "/_ts/api/v1/batch-sync"),
            (Method::OPTIONS, "/_ts/api/v1/batch-sync"),
            (Method::PUT, "/_ts/api/v1/batch-sync"),
            (Method::PATCH, "/_ts/api/v1/batch-sync"),
            (Method::DELETE, "/_ts/api/v1/batch-sync"),
        ];
        for ec_store in [None, Some("ec_identity_store".to_owned())] {
            let origin = ScriptedHttpClient::forbidding();
            let mut settings = test_settings();
            settings.ec.ec_store = ec_store.clone();
            let router = TrustedServerApp::routes_for_state(&app_state_for_settings(settings));

            let statuses: Vec<StatusCode> = cases
                .iter()
                .map(|(method, path)| {
                    route_through(&router, partner_api_request(method.clone(), path), &origin)
                        .status()
                })
                .collect();

            assert_eq!(
                statuses,
                vec![StatusCode::METHOD_NOT_ALLOWED; cases.len()],
                "should answer every other partner API method with 405 (ec_store {ec_store:?}): {cases:?}"
            );
            origin.assert_no_requests();
        }
    }

    #[test]
    fn identify_and_batch_sync_run_locally_with_record_store() {
        let origin = ScriptedHttpClient::forbidding();
        let mut settings = test_settings();
        settings.ec.ec_store = Some("ec_identity_store".to_owned());
        let router = TrustedServerApp::routes_for_state(&app_state_for_settings(settings));

        let statuses = partner_api_statuses(&router, &origin);

        assert_eq!(
            statuses,
            [StatusCode::UNAUTHORIZED; 2],
            "identify and batch sync should run and reject the unknown partner token"
        );
        origin.assert_no_requests();
    }

    #[test]
    fn dispatch_set_tester_is_disabled_by_default() {
        let router = test_router();
        let response = route(&router, empty_request(Method::GET, "/_ts/set-tester"));

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
        let response = route(&router, empty_request(Method::GET, "/_ts/set-tester"));

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
            set_cookie,
            "ts-tester=true; Domain=.test-publisher.com; Path=/; Secure; SameSite=Lax; Max-Age=2592000",
            "tester cookie should use publisher.cookie_domain and persist across browser restarts"
        );
    }

    #[test]
    fn dispatch_clear_tester_is_disabled_by_default() {
        let router = test_router();
        let response = route(&router, empty_request(Method::GET, "/_ts/clear-tester"));

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
        let response = route(&router, empty_request(Method::GET, "/_ts/clear-tester"));

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
        // its `finalize` step can run ec_finalize_response and post-delivery
        // work can run pull sync.
        let router = test_router();
        let response = route(&router, empty_request(Method::GET, "/some-page"));

        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_some(),
            "publisher fallback responses should carry EcFinalizeState for entry-point EC finalization"
        );
    }

    fn browser_request(method: Method, path: &str, fetch_destination: &str) -> Request {
        let mut request = empty_request(method, path);
        request.headers_mut().insert(
            "sec-fetch-dest",
            HeaderValue::from_bytes(fetch_destination.as_bytes())
                .expect("should parse fetch destination"),
        );
        request.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));
        request
    }

    fn eid_sync_source_of(response: &Response) -> Option<EidSyncSource> {
        response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("response should carry EC finalization state")
            .ec_context
            .eid_sync_source()
    }

    #[test]
    fn dispatch_limits_returning_user_eid_sync_to_eligible_routes() {
        let router = test_router();

        let navigation = route(
            &router,
            browser_request(Method::GET, "/article", "document"),
        );
        assert_eq!(
            eid_sync_source_of(&navigation),
            Some(EidSyncSource::Navigation)
        );

        let mut navigation_without_browser_signals =
            browser_request(Method::GET, "/another-article", "document");
        navigation_without_browser_signals
            .extensions_mut()
            .remove::<DeviceSignals>();
        let navigation_without_browser_signals = route(&router, navigation_without_browser_signals);
        assert_eq!(
            eid_sync_source_of(&navigation_without_browser_signals),
            Some(EidSyncSource::Navigation),
            "route classification should not depend on EC generation's browser gate"
        );

        let auction = route(&router, browser_request(Method::POST, "/auction", "empty"));
        assert_eq!(eid_sync_source_of(&auction), Some(EidSyncSource::Auction));

        let mut page_bids_request = browser_request(Method::GET, "/_ts/page-bids", "empty");
        page_bids_request
            .headers_mut()
            .insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        let page_bids = route(&router, page_bids_request);
        assert_eq!(
            eid_sync_source_of(&page_bids),
            Some(EidSyncSource::PageBids),
            "an admitted SPA page-bids request should persist returning-user EID cookies"
        );

        let mut denied_page_bids_request = browser_request(Method::GET, "/_ts/page-bids", "empty");
        denied_page_bids_request
            .headers_mut()
            .insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        let denied_page_bids = route(&router, denied_page_bids_request);
        assert_eq!(
            eid_sync_source_of(&denied_page_bids),
            None,
            "a denied cross-site page-bids request must not persist EID cookies"
        );

        for request in [
            browser_request(Method::GET, "/static/tsjs=prebid", "script"),
            browser_request(Method::GET, "/analytics.gif", "image"),
            browser_request(Method::GET, "/integrations/prebid/bundle.js", "script"),
        ] {
            let response = route(&router, request);
            assert_eq!(
                eid_sync_source_of(&response),
                None,
                "static, analytics, and integration requests must not persist EID cookies"
            );
        }
    }

    fn navigation_request(path: &str) -> Request {
        let mut request = browser_request(Method::GET, path, "document");
        request
            .headers_mut()
            .insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        request
    }

    #[test]
    fn provider_cookies_join_every_cookie_header() {
        // Arrange: HTTP/2 clients may split cookies across headers.
        let mut request = navigation_request("/article");
        let headers = request.headers_mut();
        headers.append(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
        headers.append(header::COOKIE, HeaderValue::from_static("vendor_id=c-1"));

        // Act
        let joined = super::joined_cookie_header(&request);

        // Assert
        assert_eq!(
            joined, "a=1; b=2; vendor_id=c-1",
            "should join every Cookie header"
        );
        assert_eq!(
            RequestCookies::new(&joined).get("vendor_id"),
            Some("c-1"),
            "should let providers read a cookie from a later header"
        );
    }

    fn take_finalize_state(response: &mut Response) -> super::EcFinalizeState {
        response
            .extensions_mut()
            .remove::<super::EcFinalizeState>()
            .expect("should attach finalize state")
    }

    #[test]
    fn publisher_navigation_receives_navigation_permit() {
        let router = test_router();

        let mut response = route(&router, navigation_request("/article"));

        let mut state = take_finalize_state(&mut response);
        assert!(
            state.ec_context.take_navigation_permit_for_test().is_some(),
            "should issue a permit"
        );
    }

    fn consented_navigation(mut request: Request) -> Request {
        // Viceroy geolocates loopback to a US privacy state; a US Privacy
        // string without a sale opt-out lets the navigation mint an EC.
        request.extensions_mut().insert(ClientInfo {
            client_ip: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            ..ClientInfo::default()
        });
        request
            .headers_mut()
            .insert(header::COOKIE, HeaderValue::from_static("us_privacy=1YNN"));
        request
    }

    #[test]
    fn enabled_provider_with_unregistered_kind_fails_startup() {
        let mut settings = test_settings();
        settings.ec.partners.push(
            serde_json::from_value(serde_json::json!({
                "name": "Example IDs",
                "source_domain": "ids.example.com"
            }))
            .expect("should parse partner"),
        );
        settings.ec.identity_providers.push(
            toml::from_str(
                "id = \"example-provider\"\nkind = \"example-kind\"\nendpoint = \"https://ids.example.com/v1\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse provider"),
        );

        let Err(error) = build_state_from_settings(settings) else {
            panic!("should refuse to start with a provider this build cannot run");
        };

        let message = format!("{error:?}");
        assert!(
            message.contains("example-provider") && message.contains("example-kind"),
            "should name the provider and its unregistered kind: {message}"
        );
    }

    #[test]
    fn navigation_without_fetch_metadata_mints_no_ec() {
        let router = test_router();
        let mut request = consented_navigation(empty_request(Method::GET, "/article"));
        request
            .headers_mut()
            .insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        request.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));

        let mut response = route_with_host_geo(&router, request, &ScriptedHttpClient::forbidding());

        let state = take_finalize_state(&mut response);
        assert!(
            state.ec_context.ec_value().is_none(),
            "should not mint an EC that no row can back"
        );
    }

    #[test]
    fn navigation_with_fetch_metadata_mints_an_ec() {
        let router = test_router();

        let mut response = route_with_host_geo(
            &router,
            consented_navigation(navigation_request("/article")),
            &ScriptedHttpClient::forbidding(),
        );

        let state = take_finalize_state(&mut response);
        assert!(
            state.ec_context.ec_value().is_some(),
            "should mint an EC on a page-load navigation"
        );
    }

    #[test]
    fn publisher_navigation_without_providers_dispatches_nothing() {
        let router = test_router();

        let mut response = route(&router, navigation_request("/article"));

        let _state = take_finalize_state(&mut response);
        assert!(
            super::take_navigation_lookups().page.is_none(),
            "should dispatch no identity lookups without enabled providers"
        );
    }

    /// Looks up `ids.example.com` and returns the response body as its UID.
    /// The flag is whether its request carries the EC ID.
    struct EchoProvider(bool, ProviderCapabilities);

    impl IdentityProvider for EchoProvider {
        fn capabilities(&self) -> &ProviderCapabilities {
            &self.1
        }

        fn sends_ec_id(&self) -> bool {
            self.0
        }

        fn build_request(
            &self,
            _lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            Ok(ProviderRequest {
                method: Method::GET,
                url: LOOKUP_URL.parse().expect("should parse the lookup URL"),
                headers: edgezero_core::http::HeaderMap::new(),
                body: Bytes::new(),
                context: None,
            })
        }

        fn parse_response<'body>(
            &self,
            response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            let id = std::str::from_utf8(response.body)
                .map_err(|_| Report::new(ProviderError::InvalidResponse))?;
            Ok(LookupOutcome::Found(vec![ProviderEid {
                source: Cow::Borrowed("ids.example.com"),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
                uids: vec![ProviderUid {
                    id: Cow::Borrowed(id),
                    atype: None,
                    ext: None,
                    expiry: ProviderExpiry::Unspecified,
                    refresh: None,
                }],
            }]))
        }

        fn read_request_cookies<'cookie>(
            &self,
            cookies: RequestCookies<'cookie>,
        ) -> CookieRead<'cookie> {
            if cookies.get("vendor_id") == Some("optout") {
                CookieRead::OptedOut
            } else {
                CookieRead::Absent
            }
        }
    }

    #[test]
    fn provider_cookie_opt_outs_reach_the_ec_context_of_every_route() {
        for after_response in [false, true] {
            for (cookie, opted_out) in [("vendor_id=optout", true), ("vendor_id=c-1", false)] {
                // Arrange
                let fixture = identity_fixture(after_response);
                let mut request = browser_request(Method::POST, "/auction", "empty");
                request
                    .headers_mut()
                    .append(header::COOKIE, HeaderValue::from_static(cookie));
                let mut ec = super::EcRequestState {
                    ec_context: EcContext::default(),
                    kv_graph: None,
                    use_finalize_kv: false,
                    eids_cookie: None,
                    sharedid: super::SharedIdRequest::default(),
                    is_real_browser: true,
                    geo_info: None,
                    setup_error: None,
                };

                // Act
                super::apply_cookie_opt_outs(&fixture.state, &request, &mut ec);

                // Assert
                assert_eq!(
                    ec.ec_context.source_opted_out_in_cookies("ids.example.com"),
                    opted_out,
                    "should withhold the source only when the provider's cookies opt out \
                     (after_response={after_response}, cookie={cookie})"
                );
            }
        }
    }

    /// The echo provider's lookup endpoint.
    const LOOKUP_URL: &str = "https://ids.example.com/lookup";

    struct AfterResponseFixture {
        state: Arc<AppState>,
        clock: TestClock,
        /// Answers the echo provider's lookups with `p-1`; it is both the
        /// request's client and the post-delivery client.
        client: ScriptedHttpClient,
        graph: trusted_server_core::ec::kv::KvIdentityGraph,
    }

    impl AfterResponseFixture {
        /// How many lookups reached the provider.
        fn sends(&self) -> usize {
            self.client
                .requests()
                .iter()
                .filter(|request| request.uri.host() == Some("ids.example.com"))
                .count()
        }
    }

    /// An app state whose after-response phase runs the echo provider.
    fn after_response_fixture() -> AfterResponseFixture {
        identity_fixture(true)
    }

    /// An app state that runs the echo provider in the page or the
    /// after-response phase.
    fn identity_fixture(after_response: bool) -> AfterResponseFixture {
        identity_fixture_with(after_response, true)
    }

    fn identity_fixture_with(after_response: bool, sends_ec_id: bool) -> AfterResponseFixture {
        let provider_config: IdentityProviderConfig = toml::from_str(
            "id = \"example-provider\"\nkind = \"echo\"\nendpoint = \"https://ids.example.com/lookup\"\nallowed_domains = [\"ids.example.com\"]\nsources = [\"ids.example.com\"]",
        )
        .expect("should parse provider config");
        let partner: EcPartner = serde_json::from_value(serde_json::json!({
            "name": "ids", "source_domain": "ids.example.com", "bidstream_enabled": true
        }))
        .expect("should build partner");
        let mut settings = test_settings();
        settings.ec.ec_store = Some("ec_identity_store".to_owned());
        settings.ec.partners = vec![partner];
        // This build registers no kind, so the app state is built without the
        // provider and the test supplies its registry and coordinator.
        let partners = {
            let mut with_provider = settings.ec.clone();
            with_provider.identity_providers = vec![provider_config.clone()];
            Arc::new(
                PartnerRegistry::from_ec_with_kinds(&with_provider, |_| true)
                    .expect("should build registry"),
            )
        };
        let provider: Box<dyn IdentityProvider> = Box::new(EchoProvider(
            sends_ec_id,
            ProviderCapabilities {
                sources: Box::new([SourceCapability {
                    source: SourceDomain::try_from("ids.example.com").expect("should parse source"),
                    input: SourceInput::Anonymous,
                }]),
                shape: EidShape {
                    max_eids: NonZeroUsize::new(4).expect("should be non-zero"),
                    max_uids_per_eid: NonZeroUsize::new(4).expect("should be non-zero"),
                    eid_level_fields: EidLevelFields::Preserved,
                },
            },
        ));
        let coordinator = ProviderCoordinator::new(
            vec![RegisteredProvider::new(provider_config, provider)],
            Arc::clone(&partners),
            Duration::from_millis(200),
        )
        .expect("should build coordinator");
        let coordinator = Some(Arc::new(coordinator));
        let (page_identity, after_response_identity) = if after_response {
            (None, coordinator)
        } else {
            (coordinator, None)
        };
        let app_state =
            Arc::into_inner(app_state_for_settings(settings)).expect("should own the app state");
        let identity = super::identity_app_with_providers(
            Arc::clone(&app_state.settings),
            IdentityProviders {
                partners,
                page: page_identity,
                after_response: after_response_identity,
            },
        )
        .expect("should build the identity app");
        let graph = crate::require_identity_graph(&identity).expect("should open identity graph");
        let state = Arc::new(AppState {
            identity,
            ..app_state
        });
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        // One answer for each phase that may look up.
        for _ in 0..2 {
            client.respond_to(
                Method::GET,
                LOOKUP_URL,
                ScriptedReply::status(StatusCode::OK).body("p-1"),
            );
        }
        AfterResponseFixture {
            state,
            clock,
            client,
            graph,
        }
    }

    fn seed_live_row(graph: &trusted_server_core::ec::kv::KvIdentityGraph, ec_id: &str) {
        let mut entry = trusted_server_core::ec::kv_types::KvEntry::tombstone(
            trusted_server_core::ec::current_timestamp(),
        );
        entry.consent.ok = true;
        block_on(graph.create(ec_id, &entry)).expect("should seed the row");
    }

    /// Runs the after-response phase on the fixture's post-delivery client.
    fn run_after_response(
        fixture: &AfterResponseFixture,
        ec_state: &mut super::EcFinalizeState,
        lookups: Option<super::AfterResponseLookups>,
    ) {
        fixture
            .clock
            .drive(crate::post_delivery::run_after_response_lookups(
                ec_state,
                lookups,
                &fixture.state.identity,
                &fixture.state.settings,
                &fixture.client.http_client(),
            ));
    }

    /// Routes one navigation, finalizes it and returns its after-response work.
    fn finalized_navigation(
        fixture: &AfterResponseFixture,
        ec_id: Option<&str>,
        client_ip: IpAddr,
    ) -> (super::EcFinalizeState, Option<super::AfterResponseLookups>) {
        let (ec_state, _, lookups) = finalized_navigation_with_page(fixture, ec_id, client_ip);
        (ec_state, lookups)
    }

    /// Like [`finalized_navigation`], and also reports whether page-phase
    /// lookups were dispatched.
    fn finalized_navigation_with_page(
        fixture: &AfterResponseFixture,
        ec_id: Option<&str>,
        client_ip: IpAddr,
    ) -> (
        super::EcFinalizeState,
        bool,
        Option<super::AfterResponseLookups>,
    ) {
        let router = TrustedServerApp::routes_for_state(&fixture.state);
        let mut request = navigation_request("/article");
        request.extensions_mut().insert(ClientInfo {
            client_ip: Some(client_ip),
            ..ClientInfo::default()
        });
        // Viceroy geolocates loopback to a US privacy state; a US Privacy
        // string without a sale opt-out grants consent.
        let cookie = ec_id.map_or_else(
            || "us_privacy=1YNN".to_owned(),
            |ec_id| format!("us_privacy=1YNN; ts-ec={ec_id}"),
        );
        request.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_str(&cookie).expect("should build cookie header"),
        );
        super::take_navigation_lookups();
        // Page-phase lookups leave on the request's client while it routes.
        let mut response = route_with_host_geo(&router, request, &fixture.client);
        let navigation = super::take_navigation_lookups();
        let lookups = navigation.after_response;
        let page = navigation.page;
        let page_dispatched = page.is_some();
        let mut ec_state = take_finalize_state(&mut response);
        fixture.clock.drive(crate::apply_edgezero_ec_finalize(
            &fixture.state,
            &mut ec_state,
            page,
            &mut response,
        ));
        (ec_state, page_dispatched, lookups)
    }

    #[test]
    fn page_phase_lookups_reach_the_write_for_a_returning_ec() {
        // Arrange
        let fixture = identity_fixture(false);
        let ec_id = format!("{}.pg0001", "c".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);

        // Act
        let (_, dispatched, _) =
            finalized_navigation_with_page(&fixture, Some(&ec_id), IpAddr::V4(Ipv4Addr::LOCALHOST));

        // Assert
        assert!(dispatched, "should dispatch page-phase lookups");
        assert_eq!(
            owned_uid(&fixture, &ec_id).as_deref(),
            Some("p-1"),
            "should write the provider UID in the page-load write"
        );
    }

    #[test]
    fn page_phase_lookups_skip_a_newly_minted_ec() {
        // Arrange
        let fixture = identity_fixture(false);

        // Act
        let (ec_state, _, _) =
            finalized_navigation_with_page(&fixture, None, IpAddr::V4(Ipv4Addr::LOCALHOST));

        // Assert
        assert!(
            ec_state.ec_context.ec_value().is_some(),
            "should mint an EC"
        );
        let ec_id = ec_state.ec_context.ec_value().expect("should mint an EC");
        assert_eq!(
            owned_uid(&fixture, ec_id),
            None,
            "should not send a newly minted EC to a provider that sends the EC ID"
        );
    }

    #[test]
    fn page_phase_lookups_write_a_minted_ec_row_for_a_provider_without_the_ec_id() {
        // Arrange
        let fixture = identity_fixture_with(false, false);

        // Act
        let (ec_state, dispatched, _) =
            finalized_navigation_with_page(&fixture, None, IpAddr::V4(Ipv4Addr::LOCALHOST));

        // Assert
        let ec_id = ec_state
            .ec_context
            .ec_value()
            .expect("should mint an EC")
            .to_owned();
        assert!(dispatched, "should dispatch page-phase lookups");
        assert_eq!(
            owned_uid(&fixture, &ec_id).as_deref(),
            Some("p-1"),
            "should write the provider UID in the write that creates the row"
        );
    }

    fn owned_uid(fixture: &AfterResponseFixture, ec_id: &str) -> Option<String> {
        let snapshot = block_on(fixture.graph.load_snapshot(ec_id));
        snapshot
            .entry_for(ec_id)
            .and_then(|entry| entry.ids.get("ids.example.com"))
            .map(|record| record.uid.clone())
    }

    #[test]
    fn after_response_lookups_capture_the_raw_ipv6_address() {
        // Arrange
        let fixture = after_response_fixture();
        let ec_id = format!("{}.ar0006", "7".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);
        let ipv6 = IpAddr::V6(std::net::Ipv6Addr::new(
            0x2001, 0xdb8, 0xabcd, 1, 0, 0, 0, 0x1234,
        ));

        // Act
        let (_, lookups) = finalized_navigation(&fixture, Some(&ec_id), ipv6);

        // Assert
        assert_eq!(
            lookups.and_then(|lookups| lookups.client_ip),
            Some(ipv6),
            "should pass the raw IPv6 address to providers"
        );
    }

    #[test]
    fn after_response_lookups_write_to_a_live_row() {
        // Arrange
        let fixture = after_response_fixture();
        let ec_id = format!("{}.ar0001", "1".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);

        // Act
        let (mut ec_state, lookups) =
            finalized_navigation(&fixture, Some(&ec_id), IpAddr::V4(Ipv4Addr::LOCALHOST));
        run_after_response(&fixture, &mut ec_state, lookups);

        // Assert
        assert_eq!(fixture.sends(), 1, "should send one lookup");
        assert_eq!(
            owned_uid(&fixture, &ec_id).as_deref(),
            Some("p-1"),
            "should write the provider UID to the row"
        );
    }

    #[test]
    fn after_response_lookups_skip_a_newly_minted_ec() {
        // Arrange
        let fixture = after_response_fixture();

        // Act
        let (mut ec_state, lookups) =
            finalized_navigation(&fixture, None, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let ec_id = ec_state
            .ec_context
            .ec_value()
            .expect("should mint an EC")
            .to_owned();
        run_after_response(&fixture, &mut ec_state, lookups);

        // Assert
        assert_eq!(fixture.sends(), 0, "should send nothing");
        assert_eq!(
            owned_uid(&fixture, &ec_id),
            None,
            "should not write provider UIDs"
        );
    }

    #[test]
    fn after_response_lookups_skip_when_the_ec_changed_since_capture() {
        // Arrange
        let fixture = after_response_fixture();
        let ec_id = format!("{}.ar0002", "2".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);
        let (mut ec_state, lookups) =
            finalized_navigation(&fixture, Some(&ec_id), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut lookups = lookups.expect("should capture after-response work");
        lookups.ec_id = format!("{}.ar0003", "3".repeat(64));
        // A live row for the other EC, so only the EC check can stop the send.
        seed_live_row(&fixture.graph, &lookups.ec_id);

        // Act
        run_after_response(&fixture, &mut ec_state, Some(lookups));

        // Assert
        assert_eq!(fixture.sends(), 0, "should send nothing");
    }

    #[test]
    fn after_response_lookups_skip_without_a_permit() {
        // Arrange
        let fixture = after_response_fixture();
        let ec_id = format!("{}.ar0004", "4".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);
        let (mut ec_state, lookups) =
            finalized_navigation(&fixture, Some(&ec_id), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(
            ec_state.ec_context.take_after_response_permit().is_some(),
            "should have issued a permit"
        );

        // Act
        run_after_response(&fixture, &mut ec_state, lookups);

        // Assert
        assert_eq!(fixture.sends(), 0, "should send nothing");
    }

    #[test]
    fn after_response_lookups_skip_a_row_withdrawn_before_the_phase() {
        // Arrange
        let fixture = after_response_fixture();
        let ec_id = format!("{}.ar0005", "b".repeat(64));
        seed_live_row(&fixture.graph, &ec_id);
        let (mut ec_state, lookups) =
            finalized_navigation(&fixture, Some(&ec_id), IpAddr::V4(Ipv4Addr::LOCALHOST));
        block_on(fixture.graph.delete(&ec_id)).expect("should delete the row");
        block_on(fixture.graph.create(
            &ec_id,
            &trusted_server_core::ec::kv_types::KvEntry::tombstone(
                trusted_server_core::ec::current_timestamp(),
            ),
        ))
        .expect("should write the tombstone");

        // Act
        run_after_response(&fixture, &mut ec_state, lookups);

        // Assert
        assert_eq!(fixture.sends(), 0, "should send nothing");
        assert_eq!(
            owned_uid(&fixture, &ec_id),
            None,
            "should keep the tombstone"
        );
    }

    #[test]
    fn publisher_request_without_fetch_metadata_navigation_gets_no_permit() {
        let router = test_router();
        let mut prefetch = navigation_request("/article");
        prefetch
            .headers_mut()
            .insert("sec-purpose", HeaderValue::from_static("prefetch"));

        for request in [
            browser_request(Method::GET, "/article", "document"),
            prefetch,
        ] {
            let mut response = route(&router, request);

            let mut state = take_finalize_state(&mut response);
            assert!(
                state.ec_context.take_navigation_permit_for_test().is_none(),
                "should not issue a permit without a top-level navigation"
            );
        }
    }

    #[test]
    fn integration_proxy_never_generates_ec() {
        let router = test_router();

        let mut response = route(
            &router,
            navigation_request("/integrations/prebid/bundle.js"),
        );

        let mut state = take_finalize_state(&mut response);
        assert!(
            !state.ec_context.ec_generated(),
            "should not mint an EC on an integration proxy"
        );
        assert!(
            state.ec_context.take_navigation_permit_for_test().is_none(),
            "should not issue a permit to an integration proxy"
        );
    }

    #[test]
    fn browser_device_signals_from_extension_reach_ec_finalize_state() {
        // Regression guard for the EdgeZero JA4/H2 signal loss: the entry point
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

        let response = route(&router, req);

        let finalize = response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("fallback response should carry EcFinalizeState");
        assert!(
            finalize
                .ec_context
                .device_signals()
                .is_some_and(DeviceSignals::looks_like_browser),
            "browser-shaped JA4/H2 signals from the request extension must mark the request as a real browser"
        );
    }

    #[test]
    fn ec_request_state_reads_the_configured_sharedid_cookie() {
        let mut settings = test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "bundle": { "modules": { "bidder": ["exampleBidderBidAdapter"] } },
                    "managed_user_ids": [
                        { "name": "sharedId", "storage": { "type": "cookie", "name": "_pubcid" } }
                    ],
                }),
            )
            .expect("should configure Prebid");
        let state = build_state_from_settings(settings).expect("should build app state");
        let req = request_builder()
            .method(Method::GET)
            .uri("https://www.example.com/article")
            .header(header::COOKIE, "sharedId=legacy-id; _pubcid=configured-id")
            .body(Body::empty())
            .expect("should build request");
        let context = context_with_client(
            empty_request(Method::GET, "/article"),
            ScriptedHttpClient::forbidding().http_client(),
        );
        let services =
            build_per_request_services(&state, &context).expect("should build request services");

        let ec = super::build_ec_request_state(&state.identity, &services, &req);

        assert_eq!(
            ec.sharedid.readable_value(),
            Some("configured-id"),
            "should read the cookie named by Prebid's storage.name, not `sharedId`"
        );
    }

    #[test]
    fn after_response_lookups_run_only_for_real_browsers() {
        let mut settings = test_settings();
        settings.ec.ec_store = Some("ec_identity_store".to_owned());
        settings.ec.partners = vec![
            serde_json::from_value(serde_json::json!({
                "name": "Pull partner",
                "source_domain": "partner.example.com",
                "pull_sync_enabled": true,
                "pull_sync_url": "https://sync.example.com/pull",
                "pull_sync_allowed_domains": ["sync.example.com"],
                "ts_pull_token": "example-pull-token-32-bytes-minimum",
            }))
            .expect("should build pull-sync partner"),
        ];
        let state = build_state_from_settings(settings).expect("should build app state");
        let browser_request = || {
            let mut req = request_builder()
                .method(Method::GET)
                .uri("https://www.example.com/article")
                .header(header::COOKIE, format!("ts-ec={}.test01", "a".repeat(64)))
                .body(Body::empty())
                .expect("should build request");
            req.extensions_mut().insert(DeviceSignals::derive(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
                Some("t13d1516h2_8daaf6152771_b186095e22b6"),
                Some("1:65536;2:0;4:6291456;6:262144"),
            ));
            req
        };
        let context = context_with_client(
            browser_request(),
            ScriptedHttpClient::forbidding().http_client(),
        );
        let services =
            build_per_request_services(&state, &context).expect("should build request services");
        let req = browser_request();
        let mut ec = super::build_ec_request_state(&state.identity, &services, &req);
        assert!(
            ec.is_real_browser,
            "should classify the request as a browser"
        );

        let browser = super::prepare_after_response_lookups(&state, &services, &req, &ec);
        ec.is_real_browser = false;
        let bot = super::prepare_after_response_lookups(&state, &services, &req, &ec);

        assert!(
            browser.is_some(),
            "should prepare pull sync for a real browser"
        );
        assert!(
            bot.is_none(),
            "should never prepare pull sync for a client that fails the browser check"
        );
    }

    #[test]
    fn missing_device_signals_extension_classifies_as_non_browser() {
        // Without the entry-point-captured signals, the reconstructed request
        // cannot expose JA4/H2, so the bot gate falls back to non-browser. This
        // documents the regression the extension threading fixes: the same
        // request that looks like a browser above is treated as a bot here.
        let router = test_router();
        let response = route(&router, empty_request(Method::GET, "/some-page"));

        let finalize = response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("fallback response should carry EcFinalizeState");
        assert!(
            !finalize
                .ec_context
                .device_signals()
                .is_some_and(DeviceSignals::looks_like_browser),
            "a request without captured device signals should not be classified as a real browser"
        );
    }

    #[test]
    fn entry_point_client_info_reaches_request_filters() {
        // Regression guard for the EdgeZero bot-protection metadata loss:
        // the entry point captures the full ClientInfo (TLS protocol/cipher, JA4,
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

        let response = route(&router, req);

        let observed = captured
            .lock()
            .expect("should lock captured client info")
            .clone()
            .expect("request filter should have observed the entry-point ClientInfo");
        assert_eq!(
            observed.client_ip,
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))),
            "request-scoped services should preserve the resolved client IP used by EC"
        );
        let finalize = response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("fallback response should carry EC finalization state");
        assert_eq!(
            finalize.ec_context.client_ip(),
            Some("203.0.113.7"),
            "EC should capture the resolved client IP from request-scoped services"
        );
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
        let response = route(
            &router,
            empty_request(Method::GET, "/.well-known/trusted-server.json"),
        );

        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_some(),
            "named-route responses should carry EcFinalizeState for entry-point EC finalization"
        );
    }

    #[test]
    fn admin_eids_diagnostic_skips_ec_finalization() {
        let router = test_router();
        let ec_id = format!("{}.abc123", "a".repeat(64));
        let eids = serde_json::json!([{
            "source": "example.com",
            "uids": [{ "id": "example-uid", "atype": 1 }]
        }]);
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(eids.to_string());
        let mut request = request_builder()
            .method(Method::GET)
            .uri("https://test-publisher.com/_ts/admin/eids")
            .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
            .header(
                header::COOKIE,
                format!("ts-ec={ec_id}; ts-eids={eids_cookie}; sharedId=example-shared-id"),
            )
            .body(Body::empty())
            .expect("should build authenticated EIDs diagnostic request");
        request.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));

        let response = route(&router, request);

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_none(),
            "admin EIDs diagnostics should not attach EC finalization state"
        );
    }

    #[test]
    fn admin_ec_diagnostic_skips_ec_finalization() {
        let router = test_router();
        let ec_id = format!("{}.abc123", "a".repeat(64));
        let eids = serde_json::json!([{
            "source": "example.com",
            "uids": [{ "id": "example-uid", "atype": 1 }]
        }]);
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(eids.to_string());
        let mut request = request_builder()
            .method(Method::GET)
            .uri(format!("https://test-publisher.com/_ts/admin/ec/{ec_id}"))
            .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
            .header(
                header::COOKIE,
                format!("ts-ec={ec_id}; ts-eids={eids_cookie}; sharedId=example-shared-id"),
            )
            .body(Body::empty())
            .expect("should build authenticated EC diagnostic request");
        request.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));

        let response = route(&router, request);

        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "configured admin EC handler should run and report the unavailable test KV graph"
        );
        assert!(
            response
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_none(),
            "admin EC diagnostics should not attach EC finalization state"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "admin EC diagnostics should not mutate the EC cookie"
        );
    }

    #[test]
    fn admin_ec_route_without_credentials_returns_401() {
        let router = test_router();

        let response = route(&router, empty_request(Method::GET, "/_ts/admin/ec"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            response.headers().contains_key(header::WWW_AUTHENTICATE),
            "admin EC 401 should include the Basic authentication challenge"
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

        let response = route(&router, req);

        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "HEAD on a named GET path should reach the publisher fallback, not return 405"
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

        let response = route(&router, req);

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

        let response = route(&router, empty_request(Method::GET, "/.image/banner.png"));

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

    /// Streaming runtime services whose `EdgeZero` outbound requests go to `upstream`.
    fn runtime_services_with_upstream(
        upstream: &ScriptedHttpClient,
        clock: &TestClock,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(
                crate::platform::FastlyPlatformConfigStore::with_outbound_client(
                    upstream.http_client(),
                ),
            ))
            .secret_store(Arc::new(
                crate::platform::FastlyPlatformSecretStore::with_outbound_client(
                    upstream.http_client(),
                ),
            ))
            .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
            .outbound_client(upstream.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(RequestGeo::default()))
            .client_info(ClientInfo::default())
            .build()
    }

    #[derive(Default)]
    struct DispatchTemplateCache {
        entries: Arc<Mutex<HashMap<String, TemplateEntry>>>,
    }

    struct DispatchTemplateReservation {
        entries: Arc<Mutex<HashMap<String, TemplateEntry>>>,
        key: TemplateCacheKey,
    }

    impl PlatformTemplateCacheReservation for DispatchTemplateReservation {
        fn insert(
            self: Box<Self>,
            metadata: &TemplateMetadata,
            body: Vec<u8>,
            _max_age: Duration,
        ) -> Result<(), TemplateCacheError> {
            self.entries.lock().expect("should lock entries").insert(
                self.key.to_cache_key(),
                TemplateEntry {
                    metadata: metadata.clone(),
                    body,
                },
            );
            Ok(())
        }

        fn cancel(self: Box<Self>) -> Result<(), TemplateCacheError> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformTemplateCache for DispatchTemplateCache {
        async fn lookup_or_reserve(
            &self,
            key: &TemplateCacheKey,
        ) -> Result<TemplateCacheLookup, TemplateCacheError> {
            if let Some(entry) = self
                .entries
                .lock()
                .expect("should lock entries")
                .get(&key.to_cache_key())
                .cloned()
            {
                return Ok(TemplateCacheLookup::Hit(entry));
            }
            Ok(TemplateCacheLookup::Reserved(
                TemplateCacheReservation::new(Box::new(DispatchTemplateReservation {
                    entries: Arc::clone(&self.entries),
                    key: key.clone(),
                })),
            ))
        }

        async fn get(&self, key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss> {
            self.entries
                .lock()
                .expect("should lock entries")
                .get(&key.to_cache_key())
                .cloned()
                .ok_or(TemplateCacheMiss::NotFound)
        }

        async fn put(
            &self,
            key: &TemplateCacheKey,
            metadata: &TemplateMetadata,
            body: Vec<u8>,
            _max_age: Duration,
        ) -> Result<(), TemplateCacheError> {
            self.entries.lock().expect("should lock entries").insert(
                key.to_cache_key(),
                TemplateEntry {
                    metadata: metadata.clone(),
                    body,
                },
            );
            Ok(())
        }

        async fn purge_url(&self, key: &TemplateCacheKey) -> Result<(), TemplateCacheError> {
            self.entries
                .lock()
                .expect("should lock entries")
                .remove(&key.to_cache_key());
            Ok(())
        }

        /// A no-op beyond succeeding: this double stores by cache key, so it cannot
        /// resolve a surrogate key to entries the way the platform does.
        async fn purge_url_surrogate_key(&self, _key: &str) -> Result<(), TemplateCacheError> {
            Ok(())
        }

        async fn purge_all(&self) -> Result<(), TemplateCacheError> {
            self.entries.lock().expect("should lock entries").clear();
            Ok(())
        }
    }

    #[test]
    fn dispatch_edge_authenticated_esi_request_stores_then_hits_template() {
        let settings = Arc::new(
            Settings::from_toml(
                r#"
                    [[handlers]]
                    path = "^/secure"
                    username = "user"
                    password = "pass"

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

                    [auction]
                    enabled = true
                    providers = {}

                    [creative_opportunities]
                    gam_network_id = "99999"
                    assembly_mode = "esi"

                    [[creative_opportunities.slot]]
                    id = "test-slot"
                    page_patterns = ["/secure/article"]
                    formats = [{ width = 728, height = 90 }]
                "#,
            )
            .expect("should parse dispatch cache settings"),
        );
        let cache = Arc::new(DispatchTemplateCache::default());
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        // Two replies: a warm dispatch that wrongly fetched the origin would take the second.
        for _ in 0..2 {
            origin.respond_to(
                Method::GET,
                &format!("{}/secure/article", settings.publisher.origin_url),
                ScriptedReply::status(StatusCode::OK)
                    .header("content-type", "text/html; charset=utf-8")
                    .header("cache-control", "public, max-age=300")
                    .body("<html><head></head><body>origin</body></html>"),
            );
        }
        let services = RuntimeServices::builder()
            .config_store(Arc::new(
                crate::platform::FastlyPlatformConfigStore::with_outbound_client(
                    origin.http_client(),
                ),
            ))
            .secret_store(Arc::new(
                crate::platform::FastlyPlatformSecretStore::with_outbound_client(
                    origin.http_client(),
                ),
            ))
            .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
            .template_cache(Arc::clone(&cache) as Arc<dyn PlatformTemplateCache>)
            .template_assembler(Arc::new(crate::esi_assembly::FastlyTemplateAssembler))
            .outbound_client(origin.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(RequestGeo::default()))
            .client_info(ClientInfo::default())
            .build();
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));
        let registry = Arc::new(
            IntegrationRegistry::with_plan(&settings, Arc::clone(&plan))
                .expect("should build integration registry"),
        );
        let orchestrator = Arc::new(
            build_orchestrator_with_plan(plan, &settings)
                .expect("should build auction orchestrator"),
        );

        let handler = {
            let settings = Arc::clone(&settings);
            let services = services.clone();
            let registry = Arc::clone(&registry);
            let orchestrator = Arc::clone(&orchestrator);
            move |ctx: RequestContext| {
                let settings = Arc::clone(&settings);
                let services = services.clone();
                let registry = Arc::clone(&registry);
                let orchestrator = Arc::clone(&orchestrator);
                Box::pin(async move {
                    let request = ctx.into_request()?;
                    let method = request.method().clone();
                    let mut ec_context =
                        match EcContext::read_from_request(&settings, &request, &services) {
                            Ok(context) => context,
                            Err(report) => return Ok(super::http_error(&report)),
                        };
                    let response = match handle_publisher_request(
                        &settings,
                        &services,
                        None,
                        &mut ec_context,
                        AuctionDispatch {
                            integration_registry: &registry,
                            orchestrator: &orchestrator,
                            slots: settings.creative_opportunity_slots(),
                            registry: None,
                        },
                        request,
                        PublisherOriginOptions {
                            edge_header: EdgeCacheHeader::SurrogateControl,
                            input_cap: None,
                            response_decoding: ResponseDecoding::Original,
                        },
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(report) => return Ok(super::http_error(&report)),
                    };
                    let (response, owner) = response;
                    match publisher_response_into_streaming_response(
                        response,
                        &method,
                        Arc::clone(&settings),
                        &registry,
                        orchestrator,
                        services,
                    )
                    .await
                    .and_then(|response| owner.attach(response).map_err(publisher_owner_error))
                    {
                        Ok(response) => Ok(response),
                        Err(report) => Ok(super::http_error(&report)),
                    }
                }) as HandlerFuture
            }
        };
        let router = RouterService::builder()
            .middleware(crate::middleware::AuthMiddleware::new(Arc::clone(
                &settings,
            )))
            .route("/secure/article", Method::GET, handler)
            .build();
        let authorized_request = || {
            request_builder()
                .method(Method::GET)
                .uri("https://test-publisher.com/secure/article")
                .header(header::HOST, "test-publisher.com")
                .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-mode", "navigate")
                .body(Body::empty())
                .expect("should build authorized navigation")
        };

        let cold = route(&router, authorized_request());
        assert_eq!(
            cold.headers()
                .get("x-ts-template-cache")
                .and_then(|value| value.to_str().ok()),
            Some("miss-stored")
        );
        block_on(cold.into_body().into_bytes_bounded(1024 * 1024))
            .expect("should drain cold response");

        let warm = route(&router, authorized_request());
        assert_eq!(
            warm.headers()
                .get("x-ts-template-cache")
                .and_then(|value| value.to_str().ok()),
            Some("hit")
        );
        block_on(warm.into_body().into_bytes_bounded(1024 * 1024))
            .expect("should drain warm response");
        assert_eq!(
            origin.requests().len(),
            1,
            "the warm dispatch must not fetch the publisher origin"
        );
    }

    #[test]
    fn dispatch_asset_fallback_streams_origin_body_without_buffering() {
        // Regression guard for the EdgeZero asset streaming cutover: a successful
        // asset proxy must hand the entry point a streaming body (`Body::Stream`)
        // rather than draining the origin into a buffered `Body::Once`. A scripted
        // upstream with a chunked body lets us assert the body type the entry point
        // hands to `EdgeZero`'s delivery to pump straight to the client.
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
            prefix = "/.images/"
            origin_url = "https://assets.example.com"
            "#,
        )
        .expect("should parse asset-route settings");
        let state = build_state_from_settings(settings).expect("should build state");

        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        upstream.respond_to(
            Method::GET,
            "https://assets.example.com/.images/logo.png",
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "image/png")
                .chunks(vec![Bytes::from_static(b"streamed-asset")]),
        );
        let services = runtime_services_with_upstream(&upstream, &clock);
        let req = empty_request(Method::GET, "/.images/logo.png");
        let asset_route = state
            .settings
            .asset_route_for_path("/.images/logo.png")
            .expect("should match the configured asset route");
        let effects = RequestFilterEffects::default();

        let response = block_on(super::dispatch_asset_fallback(
            &state,
            &services,
            req,
            asset_route,
            &effects,
        ));

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should preserve the streaming origin status"
        );
        assert!(
            response.body().is_stream(),
            "EdgeZero asset dispatch must stream the origin body, not buffer it"
        );
    }

    #[test]
    fn dispatch_fallback_nextjs_gtm_response_is_complete() {
        for gzip in [false, true] {
            let state = build_state_from_settings(nextjs_auction::script_composition_settings())
                .expect("should build mixed state");
            let origin = nextjs_auction::NextJsAuctionOrigin::with_html_response(
                &nextjs_auction::script_composition_html(),
                gzip,
                true,
            );
            let services = nextjs_auction::services(&origin);
            let request = request_builder()
                .method(Method::GET)
                .uri(format!(
                    "https://{}/article",
                    nextjs_auction::PUBLISHER_HOST
                ))
                .header("host", nextjs_auction::PUBLISHER_HOST)
                .header("accept", "text/html")
                .body(Body::empty())
                .expect("should build navigation");
            let response = block_on(super::dispatch_fallback(&state, &services, request));
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "should return a successful composed publisher response"
            );
            assert!(
                !response.headers().contains_key(header::CONTENT_LENGTH),
                "should drop the stale content length after streaming rewrites"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .map(|value| value.to_str().expect("should have ASCII coding")),
                gzip.then_some("gzip"),
                "should retain the requested response compression"
            );
            let Ok(mut stream) = response.into_body().into_stream() else {
                panic!("should retain lazy Fastly body");
            };
            let mut bytes = Vec::new();
            block_on(async {
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk.expect("should process every stream item"));
                }
            });
            nextjs_auction::assert_script_composition_response(&bytes, gzip);
        }
    }

    #[test]
    fn dispatch_fallback_streams_publisher_body_without_buffering() {
        // Regression guard for the publisher streaming cutover (#849): a
        // successful publisher origin fetch must hand the entry point a lazy
        // streaming body (`Body::Stream`) so headers commit at origin first
        // byte, rather than draining the processed page into a buffered
        // `Body::Once`. Core tests cover the rewrite pipeline itself; this
        // guards the adapter wiring that could silently re-buffer.
        let settings = test_settings();
        let state = build_state_from_settings(settings).expect("should build state");
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        origin.respond_to(
            Method::GET,
            &format!("{}/article", state.settings.publisher.origin_url),
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "text/html")
                .chunks(vec![
                    Bytes::from_static(b"<html><body>"),
                    Bytes::from_static(b"streamed</body></html>"),
                ]),
        );
        let services = runtime_services_with_upstream(&origin, &clock);
        let req = empty_request(Method::GET, "/article");

        let response = block_on(super::dispatch_fallback(&state, &services, req));

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "publisher proxy should succeed against the streaming origin stub"
        );
        assert!(
            response.body().is_stream(),
            "EdgeZero publisher dispatch must attach the lazy streaming body, not buffer it"
        );
        assert!(
            !response.headers().contains_key(header::CONTENT_LENGTH),
            "processed streaming publisher responses must not carry a stale Content-Length"
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
        let response = route(&router, empty_request(Method::GET, "/some-page"));

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
        let response = route(&router, empty_request(Method::GET, "/some-page"));

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

    fn recovery_eligible_of(response: &Response) -> bool {
        response
            .extensions()
            .get::<super::EcFinalizeState>()
            .expect("response should carry EcFinalizeState")
            .ec_context
            .recovery_eligible()
    }

    fn browser_navigation_request(path: &str) -> edgezero_core::http::Request {
        let uri = format!("https://test-publisher.com{path}");
        let mut req = request_builder()
            .method(Method::GET)
            .uri(uri)
            .header("sec-fetch-dest", "document")
            .body(Body::empty())
            .expect("should build request");
        req.extensions_mut().insert(DeviceSignals::derive(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            Some("t13d1516h2_8daaf6152771_b186095e22b6"),
            Some("1:65536;2:0;4:6291456;6:262144"),
        ));
        req
    }

    #[test]
    fn named_route_response_is_not_recovery_eligible() {
        // Orphan recovery must never be authorized on a named route: it is not a
        // publisher-page navigation, so a missing KV row must not rotate the
        // identity there.
        let router = test_router();
        let response = route(
            &router,
            empty_request(Method::GET, "/.well-known/trusted-server.json"),
        );

        assert!(
            !recovery_eligible_of(&response),
            "named-route responses must not authorize orphan recovery"
        );
    }

    #[test]
    fn filter_short_circuit_response_is_not_eligible_for_eid_persistence() {
        // A request-filter short circuit (e.g. a DataDome challenge/block) must
        // not authorize orphan recovery or EID persistence. No publisher page
        // or auction was served, so the challenged request must not write EIDs.
        // Explicit consent withdrawal remains independently eligible.
        let router = router_with_request_filters(vec![Arc::new(ChallengeRequestFilter)]);
        let navigation = route(&router, browser_navigation_request("/some-page"));

        assert_eq!(
            navigation.status(),
            StatusCode::FORBIDDEN,
            "the challenge filter should short-circuit navigation routing"
        );
        assert!(
            !recovery_eligible_of(&navigation),
            "a short-circuit filter response must not authorize orphan recovery"
        );
        assert_eq!(
            eid_sync_source_of(&navigation),
            None,
            "a challenged navigation must not authorize EID persistence"
        );

        let auction = route(&router, browser_request(Method::POST, "/auction", "empty"));
        assert_eq!(
            auction.status(),
            StatusCode::FORBIDDEN,
            "the challenge filter should short-circuit auction routing"
        );
        assert_eq!(
            eid_sync_source_of(&auction),
            None,
            "a challenged auction must not authorize EID persistence"
        );
    }

    #[test]
    fn publisher_navigation_origin_start_failure_is_not_recovery_eligible() {
        // Recovery is authorized only after a successful origin start. With no
        // live backend the publisher origin fails, so even a real-browser
        // document navigation must leave recovery unauthorized.
        let router = test_router();
        let response = route(&router, browser_navigation_request("/some-page"));

        assert!(
            !recovery_eligible_of(&response),
            "an origin-start failure must not authorize orphan recovery"
        );
    }
}
#[cfg(test)]
mod trace_dispatch_tests {
    use super::*;
    use futures::executor::block_on;
    use trusted_server_core::trace::TraceTerminalResponse;

    fn router(enabled: bool) -> RouterService {
        let settings = Settings::from_toml(&format!(
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
        .expect("should parse trace settings");
        TrustedServerApp::routes_for_state(
            &build_state_from_settings(settings).expect("should build Fastly state"),
        )
    }

    #[test]
    fn trace_dispatch_reserves_all_methods_before_ordinary_lifecycle() {
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
}
