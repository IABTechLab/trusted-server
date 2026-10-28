//! Full `EdgeZero` application wiring for Trusted Server.
//!
//! Registers all routes for Trusted Server into a
//! [`RouterService`]. On successful startup, attaches
//! [`FinalizeResponseMiddleware`]. When startup fails,
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
//! | POST | `/_ts/api/v1/batch-sync` | [`handle_batch_sync`] |
//! | GET | `/_ts/api/v1/identify` | [`handle_identify`] |
//! | GET | `/_ts/set-tester` | [`handle_set_tester`] |
//! | GET | `/_ts/clear-tester` | [`handle_clear_tester`] |
//! | GET | `/_ts/permissions` and `/_ts/permissions.json` | [`handle_permissions`] |
//! | GET | `/_ts/config` and `/_ts/config.json` | [`handle_config`] |
//! | GET | `/_ts/data` | [`handle_data`] |
//! | OPTIONS | `/_ts/api/v1/identify` | [`cors_preflight_identify`] |
//! | POST | `/_ts/api/v1/ec/resolve` | [`handle_ec_resolve`] |
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
//! > This is a known intentional restriction of the EdgeZero router; the entry-point
//! > `apply_finalize_headers` call in `lib.rs` still adds TS headers to those 405 responses.
//!
//! # EC identity lifecycle
//!
//! The `EdgeZero` path mirrors the EC identity lifecycle of the legacy
//! `route_request` (tracked in issue #495):
//!
//! - [`build_ec_request_state`] runs before every dispatched route (except
//!   batch-sync, which uses Bearer auth)
//!   and reproduces the legacy
//!   pre-routing prelude: device signals, bot gate, `sharedId` cookie
//!   capture, geo lookup, [`EcContext`] creation, and KV-graph gating.
//! - `handle_auction` and integration proxy dispatch receive the same
//!   [`EcContext`], [`KvIdentityGraph`], and [`PartnerRegistry`] inputs as
//!   legacy; the publisher fallback generates EC IDs for browser navigations.
//! - Handlers attach an [`EcFinalizeState`] to the response via extensions;
//!   `edgezero_main` pops it and runs `ec_finalize_response` plus the
//!   pull-sync hook on the converted fastly response before sending.
//!
//! ## Intentional deviations from legacy
//!
//! - **Publisher responses** keep Fastly origin bodies streaming through the
//!   `EdgeZero` response body when the body is processable or pass-through.
//!   Adapters without streaming-body support still use the bounded buffered
//!   finalizer.
//! - **Router-level 405s** (unregistered verbs) skip EC finalization along
//!   with the middleware chain; the entry point still adds TS headers.
//!
//! # Startup error handling
//!
//! When [`build_state`] fails, [`startup_error_router`] returns a minimal router
//! that responds to all routes with the startup error. This router does **not**
//! attach middleware. Startup-error responses may still receive entry-point
//! finalization (TS headers) when settings can be reloaded via
//! [`load_settings_from_config_store`]; if settings loading itself fails, they
//! are returned without TS headers.

use std::sync::Arc;

use crate::rate_limiter::{FastlyRateLimiter, RATE_COUNTER_NAME};
use edgezero_adapter_fastly::context::FastlyRequestContext;
use edgezero_core::app::{App, Hooks, StoreMetadata, StoresMetadata};
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, Response, StatusCode, header,
};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::AuctionTelemetrySink;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan_with,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::closed_paths::closed_path_response;
use trusted_server_core::config_payload::DEFAULT_SECRET_STORE_ID;
use trusted_server_core::constants::COOKIE_SHAREDID;
use trusted_server_core::ec::batch_sync::handle_batch_sync;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::identify::{cors_preflight_identify, handle_identify};
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::module::request_module;
use trusted_server_core::ec::module::{EdgeCookieModule, build_reusable_module};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::ec::resolve::handle_ec_resolve;
use trusted_server_core::ec::{EcContext, EidSyncSource};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::inspect::config::{CONFIG_JSON_PATH, CONFIG_PAGE_PATH, handle_config};
use trusted_server_core::inspect::data::{DATA_PAGE_PATH, handle_data};
use trusted_server_core::inspect::permissions::{
    PERMISSIONS_JSON_PATH, PERMISSIONS_PAGE_PATH, handle_permissions,
};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects,
    RequestFilterRegistryInput, RequestFilterRegistryOutcome,
};
use trusted_server_core::module_context::ModuleRequest;
use trusted_server_core::permissions::PermissionState;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, LoadingCaches, PlatformIdentity, PlatformKvStore, PolicyOrigin,
    RuntimeServices, StoreName, UNKNOWN, build_geo_module,
};
use trusted_server_core::proxy::{
    AssetProxyCachePolicy, handle_asset_proxy_request, handle_first_party_click,
    handle_first_party_proxy, handle_first_party_proxy_rebuild, handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AppContext, AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, handle_page_bids,
    handle_publisher_request, handle_tsjs_dynamic, page_bids_preflight_denied,
    publisher_response_into_streaming_response,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::{
    DEFAULT_CONFIG_STORE_ID, get_settings_from_config_store_with,
};
use trusted_server_core::tester_cookie::{handle_clear_tester, handle_set_tester};
use trusted_server_device_fastly::FastlyHostSignals;

use crate::middleware::FinalizeResponseMiddleware;
use crate::platform::{
    FastlyPlatformBackend, FastlyPlatformConfigStore, FastlyPlatformGeo, FastlyPlatformHttpClient,
    FastlyPlatformSecretStore, FastlyPolicyEntries, UnavailableKvStore,
};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// The stores a service reads and the key of its app-config blob.
///
/// [`Self::logical`] gives the logical stores and key. The entry point puts
/// the selector the config store holds in place of the key and resolves it
/// for the request before anything is built, with
/// `stores_with_stored_selector` and `address_request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeStoreConfig {
    pub(crate) config_store_name: StoreName,
    pub(crate) config_key: String,
    pub(crate) secret_store_name: StoreName,
}

impl RuntimeStoreConfig {
    /// Store bindings for the Fastly runtime.
    ///
    /// A deployment chooses each physical store, and `EdgeZero` links it to
    /// the service version under its logical ID, so the runtime opens every
    /// store by logical ID. The config key is the logical store ID until the
    /// entry point puts the store's selector in its place. A staging version
    /// is kept apart by the physical store it links.
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
    pub(crate) auction_telemetry_sink: Arc<dyn AuctionTelemetrySink>,
    /// The Edge Cookie module `[ec] module` selects, resolved once here.
    ///
    /// This adapter runs a fresh instance per request and resolving reads no
    /// request data, so the selection is resolved when the state is built
    /// and handed to every request through
    /// [`RuntimeServices::resolved_ec_module`](trusted_server_core::platform::RuntimeServices::resolved_ec_module),
    /// rather than resolved again on the request path.
    /// `None` for a deployment that selects no module.
    pub(crate) ec_module: Option<Arc<dyn EdgeCookieModule>>,
    /// The permission signal modules `[permission-signal] modules` selects
    /// from the scheme crates this adapter links, in the order they run.
    /// Selected once here so a name no crate answers to fails startup rather
    /// than the first request, and handed to every request's services.
    pub(crate) permission_signal_modules:
        Arc<[Arc<dyn trusted_server_core::permission_signal::PermissionSignalModule>]>,
    /// The loading caches the `[cache.<name>]` tables select from the linked
    /// builders, taken when this state is built and handed to every request's
    /// services.
    pub(crate) loading_caches: Arc<LoadingCaches>,
    /// The permission policy the domain's settings loaded, which every
    /// request is given.
    pub(crate) permission_policy: Arc<trusted_server_core::permissions::PermissionPolicy>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
pub(crate) fn build_state(
    stores: &RuntimeStoreConfig,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_from_settings(load_settings_from_config_store(stores)?)
}

pub(crate) fn load_settings_from_config_store(
    stores: &RuntimeStoreConfig,
) -> Result<Settings, Report<TrustedServerError>> {
    let entries = FastlyPolicyEntries::new(&FastlyPlatformConfigStore, &stores.config_store_name);
    let policy_origin = if blob_carries_its_policy(&stores.config_key) {
        PolicyOrigin::Blob
    } else {
        PolicyOrigin::Source(&entries)
    };
    // The settings are validated as they load, so the linked builders and the
    // ones a deployment registered are supplied here as well as to the state
    // build. Without them a `[demand]` or `[ad-server]` name one of them
    // supplies is refused before the state that knows them is built.
    get_settings_from_config_store_with(
        &FastlyPlatformConfigStore,
        &FastlyPlatformSecretStore,
        &stores.config_store_name,
        &stores.config_key,
        &stores.secret_store_name,
        policy_origin,
        &module_builders_with(registered_integrations()),
    )
}

/// Whether the blob under `config_key` carries its policy file, which a blob
/// at the logical store ID does, as `EdgeZero`'s push writes it. The keyed
/// push writes every other key, and its blobs name a shared policy entry
/// instead.
fn blob_carries_its_policy(config_key: &str) -> bool {
    config_key == DEFAULT_CONFIG_STORE_ID
}

/// Build the application state from explicit settings.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie module cannot be built for
/// this adapter, or when the auction orchestrator or the integration registry
/// fail to initialize.
pub(crate) fn build_state_from_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_registrations(settings, registered_integrations())
}

/// The integration builders a deployment offered through
/// [`crate::run_with`], set once before any request is served.
///
/// Held here rather than threaded through the build, because the state is
/// built inside the `EdgeZero` application hooks, which take no arguments.
/// [`IntegrationBuilder`] is `Copy` and holds only function pointers and
/// static references, so nothing here can change once it is set.
static REGISTERED_INTEGRATIONS: std::sync::OnceLock<Vec<IntegrationBuilder>> =
    std::sync::OnceLock::new();

/// Records the builders a deployment offers. The first call wins, and
/// [`crate::run_with`] is the only caller, so nothing registers after serving
/// has begun.
pub(crate) fn register_integrations(builders: Vec<IntegrationBuilder>) {
    let _ = REGISTERED_INTEGRATIONS.set(builders);
}

/// The builders a deployment registered, or none.
fn registered_integrations() -> &'static [IntegrationBuilder] {
    REGISTERED_INTEGRATIONS.get().map_or(&[], Vec::as_slice)
}

/// Build the application state from explicit settings, composing the modules
/// this adapter links with the externally supplied builders in
/// `integrations`.
///
/// A deployment that ships a vendor crate calls this to add that crate's
/// integration builder without the adapter naming the vendor. Auction
/// providers come from the compiled auction plan, as they do without any
/// external builders.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie module cannot be built for
/// this adapter, when the auction plan does not compile or cannot run on this
/// adapter, or when the auction orchestrator or the integration registry fail
/// to initialize, which includes two builders claiming the same integration
/// id.
pub(crate) fn build_state_with_registrations(
    settings: Settings,
    integrations: &[IntegrationBuilder],
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    warn_if_certificate_check_disabled(&settings);

    // The modules this adapter links come first and the deployment's own
    // follow, which is the order their hooks run in.
    let integrations = module_builders_with(integrations);
    let integrations = integrations.as_slice();

    // The plan is compiled with the integrations this adapter was given, so an
    // `[ad-server]` or `[demand]` name one of their builders supplies resolves
    // here. Compiling without them would drop the implementation and report the
    // name as one no builder registers.
    let plan = Arc::new(compile_auction_plan_with(&settings, integrations)?);
    plan.validate_for_target(trusted_server_core::platform::AuctionTargetId::Fastly)?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan))?;
    let registry = IntegrationRegistry::with_plan_and_registrations(&settings, plan, integrations)?
        .with_platform(fastly_platform_identity());

    // Composition root: resolve the module selection once, before any request
    // is served, so a selection this adapter can never supply fails here rather
    // than on the first request, and keep what the resolution produced so the
    // request path does not resolve the same settings again. The registry is
    // built first because a module can supply the vendor Edge Cookie module
    // the selector names, and resolving without it would reject a selection
    // this deployment can in fact satisfy.
    //
    // This adapter injects host signals on every request, so a startup instance
    // with no captured signals answers the only question the check asks,
    // which is whether the service exists at all. That same emptiness is why
    // `build_reusable_module` hands back nothing for a module built from
    // those signals, leaving it to be resolved per request against the
    // signals that request actually carried.
    let ec_module = build_reusable_module(
        &settings.ec,
        Some(Arc::new(FastlyHostSignals::default())),
        registry.ec_module(),
    )?;
    let permission_signal_modules =
        trusted_server_core::permission_signal::build_permission_signal_modules(
            &settings,
            &shipped_signal_modules(),
        )?;
    let permission_policy = settings.required_permission_policy()?;

    let auction_telemetry_sink = crate::tinybird::auction_sink_from_settings(&settings);
    let default_kv_store = Arc::new(UnavailableKvStore) as Arc<dyn PlatformKvStore>;
    let loading_caches =
        trusted_server_core::integrations::loading_caches_from(&settings, integrations)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        default_kv_store,
        auction_telemetry_sink,
        ec_module,
        permission_signal_modules,
        loading_caches,
        permission_policy,
    }))
}

/// What Fastly reports about the service answering, from the environment
/// variables Fastly gives every Compute service.
fn fastly_platform_identity() -> PlatformIdentity {
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| UNKNOWN.to_owned())
    };
    PlatformIdentity {
        name: "fastly".to_owned(),
        service_id: read("FASTLY_SERVICE_ID"),
        service_version: read("FASTLY_SERVICE_VERSION"),
        staging: std::env::var("FASTLY_IS_STAGING").is_ok_and(|value| value == "1"),
        pop: read("FASTLY_POP"),
    }
}

/// The permission signal modules this adapter links, in the order they run
/// when configuration names none. Global Privacy Control is first because it
/// is a browser setting with no interface of its own, and the three that
/// carry a choice someone made through an interface follow, so an answer
/// given at a prompt amends the header the visitor arrived with.
///
/// Core supplies no module of its own, so this is where a deployment's
/// schemes are decided. A scheme is added by linking its crate here, and a
/// scheme core has never heard of plugs in the same way.
fn shipped_signal_modules()
-> Vec<Arc<dyn trusted_server_core::permission_signal::PermissionSignalModule>> {
    vec![
        Arc::new(trusted_server_permission_signal_gpc::GpcModule::new()),
        Arc::new(trusted_server_permission_signal_gpp::GppSaleOptOutModule::new()),
        Arc::new(trusted_server_permission_signal_us_privacy::UsPrivacyModule::new()),
        Arc::new(trusted_server_permission_signal_tcf::TcfModule::new()),
        Arc::new(trusted_server_permission_signal_mtm::MtmModule::new()),
    ]
}

/// The module builders this adapter links, in the order their request hooks
/// run. Offering a module does not run it, because the registry builds a
/// module only when a section of the settings selects it.
///
/// A deployment that wants a different set links a different set, and one
/// that adds a module of its own hands its builder to [`crate::run_with`], which
/// runs it after these. None of these offers a loading cache, so a
/// deployment that wants copies kept hands one the same way.
#[must_use]
pub fn shipped_module_builders() -> Vec<IntegrationBuilder> {
    vec![
        // Prebid registers from the auction plan, and what it registers runs
        // ahead of the modules below.
        trusted_server_auction_prebid::builder(),
        trusted_server_testing_testlight::builder(),
        trusted_server_framework_nextjs::builder(),
        trusted_server_audience_permutive::builder(),
        trusted_server_identity_lockr::builder(),
        trusted_server_cmp_didomi::builder(),
        trusted_server_cmp_inmobi::builder(),
        trusted_server_cmp_sourcepoint::builder(),
        trusted_server_cmp_osano::builder(),
        trusted_server_tag_google_tag::builder(),
        trusted_server_tag_google_tag_manager::builder(),
        trusted_server_bot_protection_datadome::builder(),
        trusted_server_ad_tag_google::builder(),
        trusted_server_ad_tag_google::diagnostics::builder(),
        trusted_server_ads_txt_iabtl::builder(),
        trusted_server_headers_geo::builder(),
        // Implementations `[demand]` and `[ad-server]` can name, which no
        // section selects.
        trusted_server_auction_protocol_openrtb::builder(),
        trusted_server_auction_prebid_server::builder(),
        trusted_server_auction_aps::builder(),
        trusted_server_ad_server_iabtl::builder(),
    ]
}

/// The builders this adapter links followed by `extra`, a deployment's own,
/// which is the order their hooks run in.
#[must_use]
pub fn module_builders_with(extra: &[IntegrationBuilder]) -> Vec<IntegrationBuilder> {
    let mut all = shipped_module_builders();
    all.extend_from_slice(extra);
    all
}

fn warn_if_certificate_check_disabled(settings: &Settings) {
    if !settings.proxy.certificate_check {
        log::warn!(
            "INSECURE: proxy.certificate_check is disabled; HTTPS origin certificate verification is disabled"
        );
    }
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
///
/// Applies the module-supplied modules selected by `[geo]`, `[ec]`,
/// `[device] module` and `[asset-cache]` on top of the base services. Unset
/// and `none` resolve the disabled geo module, `platform` leaves the Fastly
/// lookup standing, and any other key names a module's module. Identity,
/// device and the asset cache are applied the same way when a module
/// supplies them.
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

    // The TLS JA4 and HTTP/2 signals arrive as trusted internal headers
    // injected by the entry point. They build the host-signal service a
    // host-signal module reads. Fastly always supplies the capability, so the
    // service is always set even when a request carried no signal.
    let tls_ja4 = ctx
        .request()
        .headers()
        .get("x-ts-tls-ja4")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let h2_fingerprint = ctx
        .request()
        .headers()
        .get("x-ts-h2-fingerprint")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let builder = RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(Arc::clone(&state.default_kv_store))
        .value_cache(Arc::new(crate::value_cache::FastlyValueCache))
        .loading_caches(Arc::clone(&state.loading_caches))
        // Constructed unconditionally, but only read when the assembly mode is a
        // shared-template one — which defaults to Inline, so this is inert until an
        // operator opts in.
        .template_cache(Arc::new(crate::template_cache::FastlyTemplateCache::new()))
        .template_assembler(Arc::new(crate::esi_assembly::FastlyTemplateAssembler))
        .backend(Arc::new(FastlyPlatformBackend))
        .http_client(Arc::new(FastlyPlatformHttpClient))
        .geo(build_geo_module(
            &state.settings,
            Arc::new(FastlyPlatformGeo),
        ))
        .auction_telemetry_sink(Arc::clone(&state.auction_telemetry_sink))
        .client_info(client_info)
        // The signal modules were selected once at startup from the scheme
        // crates this adapter links, so every request asks exactly the ones
        // configuration named, in that order.
        .permission_signal_modules(Arc::clone(&state.permission_signal_modules))
        // The policy the domain's settings loaded decides the request's
        // permissions.
        .permission_policy(Arc::clone(&state.permission_policy))
        .host_signals(Arc::new(FastlyHostSignals::new(tls_ja4, h2_fingerprint)));

    // Hand every request the module resolved at the composition root, so the
    // request path reuses that instance instead of resolving `[ec] module`
    // again. Nothing is set for a deployment that selects no module, or one
    // whose module is built from this request's own host signals, and both
    // are resolved on the request path instead.
    let services = match state.ec_module.clone() {
        Some(module) => builder.resolved_ec_module(module).build(),
        None => builder.build(),
    };

    // Unset and `"none"` both resolve nothing, so no client IP reaches a host
    // geo service. `"platform"` opts in to the Fastly lookup below, and any
    // other key names an integration module that declares a geo module.
    let mut services = services;
    if let Some(module) = state.registry.geo_module() {
        services = services.with_geo(module);
    }
    if let Some(module) = state.registry.ec_module() {
        services = services.with_ec_module(module);
    }
    if let Some(module) = state.registry.device_module() {
        services = services.with_device_module(module);
    }
    if let Some(module) = state.registry.asset_cache_module() {
        services = services.with_asset_cache_module(module);
    }
    let services = services.with_identifier_modules(state.registry.identifier_modules());
    // The publisher this request resolved to, its tenant included, names
    // every key in a shared store and every name the browser holds.
    let prefixes = state.registry.prefixes(
        &state.settings,
        ModuleRequest::of(ctx.request(), services.client_info()),
    );
    services.with_prefixes(prefixes)
}

/// Builds the services graph handed to a module the entry point calls outside
/// the router, which is the device module, before the per-request services
/// built by [`build_per_request_services`] exist.
///
/// The settings are ones the runtime loader loaded, so they carry the
/// permission policy their domain names, which every services graph holds.
///
/// # Panics
///
/// When the settings carry no permission policy, which only settings built
/// outside the runtime loader can lack.
pub(crate) fn build_entry_point_services(
    settings: &Settings,
    kv_store: Arc<dyn PlatformKvStore>,
    loading_caches: Arc<LoadingCaches>,
) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(kv_store)
        .value_cache(Arc::new(crate::value_cache::FastlyValueCache))
        .loading_caches(loading_caches)
        .backend(Arc::new(FastlyPlatformBackend))
        .http_client(Arc::new(FastlyPlatformHttpClient))
        .geo(build_geo_module(settings, Arc::new(FastlyPlatformGeo)))
        .client_info(ClientInfo::default())
        .permission_policy(
            settings
                .required_permission_policy()
                .expect("state is built only from loaded settings"),
        )
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

/// EC state threaded from route handlers to the `lib.rs` entry point via
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
    pub(crate) sharedid_cookie: Option<String>,
    pub(crate) is_real_browser: bool,
    /// Per-request services carried to the entry point so the pull-sync
    /// dispatcher can reuse the same platform HTTP client, and so finalization
    /// can hand them to the selected Edge Cookie module when it replaces an
    /// orphaned identifier.
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
async fn build_ec_request_state(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> EcRequestState {
    let device_signals = device_signals_for(req);
    let is_real_browser = device_signals.looks_like_browser;
    if !is_real_browser {
        log::info!(
            "Bot gate: blocking EC operations (ja4={:?}, platform={:?}, is_mobile={})",
            device_signals.ja4_class,
            device_signals.platform_class,
            device_signals.is_mobile,
        );
    }

    let sharedid_cookie = crate::extract_cookie_value(req, COOKIE_SHAREDID);

    let (ec_context, setup_error) =
        match EcContext::read_from_request_resolving_geo(settings, req, services).await {
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
    let geo_info = ec_context.geo_info().cloned();

    // Bot gate: suppress KV-backed EC writes for unrecognized clients, except
    // when the request carries an explicit withdrawal signal. The write path
    // stays open for withdrawal so tombstones remain authoritative even for
    // privacy-extension-heavy clients that do not look like known browsers. A
    // merely not-permitted (pre-consent or fail-closed) request writes nothing,
    // so it does not need the graph.
    let kv_graph = crate::maybe_identity_graph(settings);
    let finalize_kv_graph =
        if setup_error.is_none() && (is_real_browser || ec_context.storage_withdrawn()) {
            kv_graph.clone()
        } else {
            None
        };
    let kv_graph = if is_real_browser { kv_graph } else { None };

    EcRequestState {
        ec_context,
        kv_graph,
        finalize_kv_graph,
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
/// Mirrors the legacy `route_request` ordering: filters run before route
/// matching. Request header
/// mutations are applied to `req` so the routed handler observes them; response
/// effects are returned for the entry point to apply after EC finalization. A
/// filter that responds (e.g. a `DataDome` challenge) short-circuits routing.
///
/// `permissions` carries the state resolved when the EC context was built, so
/// every filter reads the same permissions as the rest of the request.
async fn run_pre_route_filters(
    state: &AppState,
    services: &RuntimeServices,
    req: &mut Request,
    geo_info: Option<&GeoInfo>,
    permissions: Option<&PermissionState>,
) -> PreRoute {
    match state
        .registry
        .filter_request(RequestFilterRegistryInput {
            settings: &state.settings,
            services,
            req,
            geo_info,
            permissions,
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
    // Read only, answered before the Edge Cookie lifecycle so nothing is written.
    if matches!(handler, NamedRouteHandler::Config) {
        return Ok(handle_config(&state.settings, &ctx.into_request()));
    }

    let services = build_per_request_services(&state, &ctx);
    let mut req = ctx.into_request();

    // S2S batch sync uses Bearer auth (not EC cookies), so it skips EC
    // context creation entirely — mirroring the dedicated early arm in the
    // legacy route_request. Batch-sync also skips request filters, matching
    // legacy, which returns before the filter step for this route.
    if matches!(handler, NamedRouteHandler::BatchSync) {
        return Ok(run_batch_sync(&state, &services, req));
    }

    // Read only, answered before the Edge Cookie lifecycle so nothing is written.
    if matches!(handler, NamedRouteHandler::Permissions) {
        let response = handle_permissions(&state.settings, &services, &req)
            .await
            .unwrap_or_else(|error| http_error(&error));
        return Ok(response);
    }

    // Read only, answered before the Edge Cookie lifecycle so nothing is written.
    if matches!(handler, NamedRouteHandler::Data) {
        let kv = crate::maybe_identity_graph(&state.settings);
        let response = request_module(&state.settings.ec, &services)
            .and_then(|module| {
                handle_data(
                    kv.as_ref(),
                    module.as_deref(),
                    services.public_prefix(),
                    &req,
                )
            })
            .unwrap_or_else(|error| http_error(&error));
        return Ok(response);
    }

    if let Err(report) = state.registry.prepare_request(&state.settings, &mut req) {
        return Ok(http_error(&report));
    }

    let mut ec = build_ec_request_state(&state.settings, &services, &req).await;
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

    let effects = match run_pre_route_filters(
        &state,
        &services,
        &mut req,
        ec.geo_info.as_ref(),
        Some(ec.ec_context.permissions()),
    )
    .await
    {
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
        NamedRouteHandler::SetTester => {
            handle_set_tester(&state.settings, services.public_prefix())
        }
        NamedRouteHandler::ClearTester => {
            handle_clear_tester(&state.settings, services.public_prefix())
        }
        NamedRouteHandler::Permissions | NamedRouteHandler::Config | NamedRouteHandler::Data => {
            unreachable!("the inspection endpoints are answered before EC setup")
        }
        NamedRouteHandler::EcResolve => {
            // The resolve endpoint persists the identity-graph row before
            // creating, so it takes the same bot-gated graph as generation: an
            // unrecognized client gets no graph, and therefore no new Edge Cookie.
            handle_ec_resolve(
                &state.settings,
                req,
                &ec.ec_context,
                ec.kv_graph.as_ref(),
                services,
            )
            .await
        }
        NamedRouteHandler::Auction => {
            ec.ec_context.set_eid_sync_source(EidSyncSource::Auction);
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
            let partner_registry = PartnerRegistry::from_config(&state.settings.ec.partners)?;
            let registry_ref = if partner_registry.is_empty() {
                None
            } else {
                Some(&partner_registry)
            };
            let auction = AuctionDispatch {
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
fn run_batch_sync(state: &AppState, services: &RuntimeServices, req: Request) -> Response {
    let device_signals = device_signals_for(&req);
    let is_real_browser = device_signals.looks_like_browser;
    let sharedid_cookie = crate::extract_cookie_value(&req, COOKIE_SHAREDID);

    let result = crate::require_identity_graph(&state.settings).and_then(|kv| {
        let partner_registry = PartnerRegistry::from_config(&state.settings.ec.partners)?;
        let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
        // A partner echoes back an identifier the deployment's own module
        // created, so validation and KV normalization are dispatched through
        // that module rather than the built-in HMAC grammar.
        let module = request_module(&state.settings.ec, services)?;
        handle_batch_sync(&kv, &partner_registry, &limiter, module.as_deref(), req)
    });

    let mut response = result.unwrap_or_else(|e| http_error(&e));
    // Legacy parity: batch-sync responses still pass through
    // ec_finalize_response with a default EC context and no finalize KV graph.
    response.extensions_mut().insert(EcFinalizeState {
        ec_context: EcContext::default(),
        use_finalize_kv: false,
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
    if let Some(response) = closed_path_response(&req) {
        return response;
    }

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if let Err(report) = state.registry.prepare_request(&state.settings, &mut req) {
        return http_error(&report);
    }

    // A crawler asking for robots.txt is answered once the modules have
    // prepared the request, before the Edge Cookie steps and the request
    // filters, so nothing is read from its cookies or written back, and no
    // filter's answer stands in for the file.
    if asks_for_robots_txt(&state.settings, &method, &path) {
        return answer_robots_txt(state, services, req, &method).await;
    }

    let mut ec = build_ec_request_state(&state.settings, services, &req).await;
    if let Some(report) = ec.setup_error.take() {
        let response = http_error(&report);
        return attach_dispatch_extensions(response, ec, RequestFilterEffects::default());
    }

    // Pre-route integration request filters (DataDome protection, etc.) run
    // before the route-type decision, matching legacy `route_request` ordering.
    let effects = match run_pre_route_filters(
        state,
        services,
        &mut req,
        ec.geo_info.as_ref(),
        Some(ec.ec_context.permissions()),
    )
    .await
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
            .then(|| state.settings.asset_route_index_for_path(&path))
            .flatten();
        if let Some(index) = matched_asset_route {
            return dispatch_asset_fallback(state, services, req, index, &effects).await;
        }

        // Generate an EC ID if needed — mirrors the legacy catch-all arm.
        // Only for document navigations by recognised browsers; subresource
        // requests may lack consent signals such as Sec-GPC.
        let is_navigation = is_navigation_request(&req);
        if is_navigation {
            ec.ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        }
        let is_publisher_navigation = ec.is_real_browser && is_navigation;
        if is_publisher_navigation
            && let Err(err) = ec
                .ec_context
                .generate_if_needed(&state.settings, ec.kv_graph.as_ref(), services)
                .await
        {
            log::error!("EC generation failed for publisher proxy: {err:?}");
        }

        // Run the server-side auction with the configured creative-
        // opportunity slots and collect dispatched bids from the lazy
        // publisher body stream. `handle_publisher_request` matches the
        // slots against the request path. The partner registry plus the
        // EC identity-graph KV (`ec.kv_graph`) enrich the bid request with
        // server-side EIDs, same as the legacy auction.
        let slots = state.settings.creative_opportunity_slots();
        match PartnerRegistry::from_config(&state.settings.ec.partners) {
            Ok(partner_registry) => {
                let auction = AuctionDispatch {
                    orchestrator: &state.orchestrator,
                    slots,
                    registry: Some(&partner_registry),
                };
                match handle_publisher_request(
                    AppContext {
                        settings: &state.settings,
                        integration_registry: state.registry.as_ref(),
                    },
                    services,
                    ec.kv_graph.as_ref(),
                    &mut ec.ec_context,
                    auction,
                    req,
                    EdgeCacheHeader::SurrogateControl,
                )
                .await
                {
                    Ok(pub_response) => {
                        // Origin start succeeded on the sole publisher-page
                        // path. Authorize orphan recovery only for real-browser
                        // document navigations so named routes, integration
                        // proxies, and filter short circuits cannot rotate an
                        // identity.
                        ec.ec_context.set_recovery_eligible(is_publisher_navigation);
                        publisher_response_into_streaming_response(
                            pub_response,
                            &method,
                            Arc::clone(&state.settings),
                            state.registry.as_ref(),
                            Arc::clone(&state.orchestrator),
                            services.clone(),
                        )
                        .await
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    };

    let response = result.unwrap_or_else(|e| http_error(&e));
    attach_dispatch_extensions(response, ec, effects)
}

/// Whether the request is a crawler's `GET` or `HEAD` of the robots.txt that
/// `[robots-txt]` makes.
fn asks_for_robots_txt(settings: &Settings, method: &Method, path: &str) -> bool {
    settings.robots_txt.is_some()
        && path == trusted_server_core::robots_txt::ROBOTS_TXT_PATH
        && matches!(*method, Method::GET | Method::HEAD)
}

/// Answers robots.txt through the publisher path with an empty Edge Cookie
/// context and no auction, so none of the crawler's cookies or consent
/// signals are read, and attaches no Edge Cookie state, so the finalize step
/// writes nothing back.
async fn answer_robots_txt(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    method: &Method,
) -> Response {
    let answer = handle_publisher_request(
        AppContext {
            settings: &state.settings,
            integration_registry: state.registry.as_ref(),
        },
        services,
        None,
        &mut EcContext::default(),
        AuctionDispatch {
            orchestrator: &state.orchestrator,
            slots: &[],
            registry: None,
        },
        req,
        EdgeCacheHeader::SurrogateControl,
    )
    .await;
    let response = match answer {
        Ok(answer) => {
            publisher_response_into_streaming_response(
                answer,
                method,
                Arc::clone(&state.settings),
                state.registry.as_ref(),
                Arc::clone(&state.orchestrator),
                services.clone(),
            )
            .await
        }
        Err(report) => Err(report),
    };
    response.unwrap_or_else(|report| http_error(&report))
}

/// Returns `true` when an asset response should carry a (streamed) body.
///
/// `HEAD` responses and bodiless statuses (204, 304) advertise the origin
/// representation length in their `Content-Length` header while carrying no
/// body. Attaching the origin stream would either contradict that header or
/// stream bytes a client does not expect, so those responses drop the stream and
/// keep the origin's `Content-Length` untouched.
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
/// Like legacy `route_request`, asset bodies are streamed straight to the client
/// with no cap: the origin stream is attached to the response and `edgezero_main`
/// commits the headers, then pipes the body chunk-by-chunk via
/// [`fastly::Response::stream_to_client`] and
/// [`stream_asset_body`](trusted_server_core::proxy::stream_asset_body). The
/// origin stream is left untouched here, so large images never materialize in
/// the Wasm heap.
async fn dispatch_asset_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    index: usize,
    effects: &RequestFilterEffects,
) -> Response {
    log::info!("No explicit route matched; proxying via configured asset route");

    let method = req.method().clone();
    let asset_route = &state.settings.proxy.asset_routes[index];
    let answer = match state.registry.route_source(&state.settings, index) {
        Ok(source) => {
            handle_asset_proxy_request(&state.settings, services, req, asset_route, source.as_ref())
                .await
        }
        Err(report) => Err(report),
    };

    match answer {
        Ok(asset_response) => {
            let cache_policy = asset_response.cache_policy();
            let (mut response, stream_body) = asset_response.into_response_and_body();

            // Attach the origin stream so `edgezero_main` streams it to the
            // client. HEAD and bodiless statuses (204, 304) advertise the origin
            // Content-Length but carry no body, so their stream is dropped to
            // preserve that header and avoid a length/body mismatch.
            if let Some(body) = stream_body
                && asset_response_carries_body(&method, response.status())
            {
                *response.body_mut() = body;
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

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`],
/// mirroring [`crate::http_error_response`] exactly.
///
/// The near-identical function in `lib.rs` is intentional: the legacy path
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
    BatchSync,
    Identify,
    SetTester,
    ClearTester,
    Permissions,
    Config,
    Data,
    EcResolve,
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
    // What the deployment decided for the asking request, shown to anyone.
    NamedRoute {
        path: PERMISSIONS_PAGE_PATH,
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::Permissions,
    },
    NamedRoute {
        path: PERMISSIONS_JSON_PATH,
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::Permissions,
    },
    // The settings the deployment is running, masked, shown to anyone.
    NamedRoute {
        path: CONFIG_PAGE_PATH,
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::Config,
    },
    NamedRoute {
        path: CONFIG_JSON_PATH,
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::Config,
    },
    // What is held against the request's own Edge Cookie, shown to a browser
    // opening the address as a page.
    NamedRoute {
        path: DATA_PAGE_PATH,
        primary_methods: &[Method::GET],
        handler: NamedRouteHandler::Data,
    },
    NamedRoute {
        path: "/_ts/api/v1/ec/resolve",
        primary_methods: &[Method::POST],
        handler: NamedRouteHandler::EcResolve,
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

impl TrustedServerApp {
    pub(crate) fn build_app_with_state(
        stores: &RuntimeStoreConfig,
    ) -> (App, Option<Arc<AppState>>) {
        let (router, state) = Self::router_with_state(stores);
        let mut app = App::with_name(router, Self::name());
        Self::configure(&mut app);
        (app, state)
    }

    fn router_with_state(stores: &RuntimeStoreConfig) -> (RouterService, Option<Arc<AppState>>) {
        let state = match build_state(stores) {
            Ok(state) => state,
            Err(ref e) => {
                log::error!("failed to build application state: {:?}", e);
                return (startup_error_router(e), None);
            }
        };

        (Self::routes_for_state(&state), Some(state))
    }

    fn routes_for_state(state: &Arc<AppState>) -> RouterService {
        let mut router = RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)));

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
        let stores = RuntimeStoreConfig::logical();
        Self::router_with_state(&stores).0
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
mod seam_probe_tests;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        AppState, DEFAULT_CONFIG_STORE_ID, EidSyncSource, IntegrationBuilder, NAMED_ROUTES,
        NamedRouteHandler, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, RuntimeStoreConfig,
        TrustedServerApp, blob_carries_its_policy, build_per_request_services,
        build_state_from_settings, module_builders_with, shipped_module_builders,
        startup_error_router,
    };
    use bytes::Bytes;
    use edgezero_core::app::Hooks as _;
    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::{
        HeaderValue, Method, Request, Response, StatusCode, header, request_builder,
    };
    use edgezero_core::key_value_store::NoopKvStore;
    use edgezero_core::params::PathParams;
    use edgezero_core::router::RouterService;
    use std::net::{IpAddr, Ipv4Addr};
    use trusted_server_core::auction::compile_auction_plan_with;

    use error_stack::Report;
    use futures::executor::block_on;
    use trusted_server_core::ec::device::DeviceSignals;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        HeaderMutation, IntegrationRegistry, IntegrationRequestFilter, RequestFilterDecision,
        RequestFilterEffects, RequestFilterInput,
    };
    use trusted_server_core::platform::{
        ClientInfo, PlatformBackend, PlatformBackendSpec, PlatformError, PlatformHttpClient,
        PlatformHttpRequest, PlatformKvStore, PlatformPendingRequest, PlatformResponse,
        PlatformSelectResult, RuntimeServices,
    };
    use trusted_server_core::settings::Settings;

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
        block_on(router.oneshot(request)).expect("should route request")
    }

    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [proxy]
            allowed_domains = ["*.example", "*.example.com"]

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [request_signing]
            enabled = false

            [auction]
            enabled = true
            modules = ["prebid"]

            [auction.prebid]
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [demand]
            modules = ["prebid"]

            [demand.prebid]
            implementation = "auction.prebid-server"
            endpoint = "https://test-prebid.com/openrtb2/auction"
            timeout_ms = 2000
            "#,
        )
        .expect("should parse test settings")
        // A loaded domain carries its policy, and these tests decide under the
        // repository's sample.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
    }

    fn test_router() -> RouterService {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        TrustedServerApp::routes_for_state(&state)
    }

    /// Every request's services carry the prefixes of the publisher it
    /// resolved to, which start every shared store key and every name the
    /// browser holds.
    #[test]
    fn per_request_services_carry_the_publishers_prefixes() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let context = RequestContext::new(
            empty_request(Method::GET, "/article"),
            PathParams::default(),
        );

        let services = build_per_request_services(&state, &context);

        assert_eq!(
            services.server_key_prefix().as_str(),
            "ts.test-publisher.com"
        );
        assert_eq!(services.public_prefix().as_str(), "ts");
    }

    #[test]
    fn only_the_blob_at_the_logical_store_id_carries_its_policy_file() {
        assert!(blob_carries_its_policy(DEFAULT_CONFIG_STORE_ID));
        assert!(
            !blob_carries_its_policy("a.publisher.example"),
            "a blob the keyed push wrote for a host names a shared entry"
        );
    }

    #[test]
    fn each_request_is_given_the_policy_its_domain_loaded() {
        let policy = trusted_server_core::test_support::policies::open();
        let state =
            build_state_from_settings(test_settings().with_permission_policy(Arc::clone(&policy)))
                .expect("should build test state");
        let context = RequestContext::new(
            empty_request(Method::GET, "/article"),
            PathParams::default(),
        );

        let services = build_per_request_services(&state, &context);

        assert!(
            Arc::ptr_eq(services.permission_policy(), &policy),
            "the request should decide under its domain's policy, not the sample"
        );
    }

    #[test]
    fn per_request_services_register_the_fastly_template_assembler() {
        let state = build_state_from_settings(test_settings()).expect("should build test state");
        let context = RequestContext::new(
            empty_request(Method::GET, "/article"),
            PathParams::default(),
        );

        let services = build_per_request_services(&state, &context);
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

    /// The sink startup builds from `[analytics]` is the one each request's
    /// services carry, so a selected module sends from the request path and a
    /// deployment that selects none sends nothing.
    #[test]
    fn per_request_services_carry_the_sink_analytics_selects() {
        let base = trusted_server_core::test_support::tests::crate_test_settings_str();
        let analytics =
            trusted_server_core::test_support::template::documented_block("[analytics]");
        let context = RequestContext::new(
            empty_request(Method::GET, "/article"),
            PathParams::default(),
        );

        for (document, sends) in [
            (base.clone(), false),
            (format!("{base}\n{analytics}"), true),
        ] {
            let settings = Settings::from_toml(&document)
                .expect("should load the settings")
                // A loaded domain carries its policy.
                .with_permission_policy(
                    trusted_server_core::permissions::PermissionPolicy::sample(),
                );
            let state = build_state_from_settings(settings).expect("should build state");

            let services = build_per_request_services(&state, &context);

            assert_eq!(
                services.auction_telemetry_sink().is_enabled(),
                sends,
                "a request's sink should send exactly when [analytics] selects a module"
            );
        }
    }

    /// Builds a router whose `AppState` uses a registry containing the given
    /// request filters (and no routes), so dispatch-level request-filter
    /// behavior can be exercised without a real integration.
    fn router_with_request_filters(
        filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    ) -> RouterService {
        let settings = test_settings();
        let plan = Arc::new(
            compile_auction_plan_with(&settings, &shipped_module_builders())
                .expect("should compile auction plan"),
        );
        let orchestrator = trusted_server_core::auction::build_orchestrator_with_plan(plan)
            .expect("should build orchestrator");
        let registry = IntegrationRegistry::from_request_filters(filters);
        let default_kv_store =
            Arc::new(crate::platform::UnavailableKvStore) as Arc<dyn super::PlatformKvStore>;
        // Resolved the same way the composition root resolves it, so this
        // router behaves like a served one.
        let ec_module = trusted_server_core::ec::module::build_reusable_module(
            &settings.ec,
            None,
            registry.ec_module(),
        )
        .expect("should resolve the Edge Cookie module selection");
        let state = Arc::new(super::AppState {
            auction_telemetry_sink: Arc::new(
                trusted_server_core::auction::NoopAuctionTelemetrySink,
            ),
            settings: Arc::new(settings),
            orchestrator: Arc::new(orchestrator),
            registry: Arc::new(registry),
            default_kv_store,
            ec_module,
            permission_signal_modules: Arc::default(),
            loading_caches: Arc::default(),
            permission_policy: trusted_server_core::permissions::PermissionPolicy::sample(),
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
    fn startup_registers_aps_renderer_route() {
        let mut settings = test_settings();
        settings.demand = trusted_server_core::provider_table::ProviderList::new(
            vec!["aps_main".to_string()],
            std::collections::BTreeMap::from([(
                "aps_main".to_string(),
                serde_json::Map::from_iter([
                    (
                        "implementation".to_string(),
                        serde_json::json!("auction.aps"),
                    ),
                    (
                        "endpoint".to_string(),
                        serde_json::json!("https://aps.example/e/pb/bid"),
                    ),
                    ("routing".to_string(), serde_json::json!("all_eligible")),
                    (
                        "account_id".to_string(),
                        serde_json::json!("example-account"),
                    ),
                ]),
            )]),
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
    fn startup_error_router_handles_head_and_options() {
        let report = Report::new(TrustedServerError::BadRequest {
            message: "startup failed".to_string(),
        });
        let router = startup_error_router(&report);

        let head_response = route(&router, empty_request(Method::HEAD, "/"));
        let options_response = route(&router, empty_request(Method::OPTIONS, "/any"));

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
    fn the_startup_router_answers_500_to_a_policy_load_error() {
        use trusted_server_core::test_support::policy_failure;

        let error = policy_failure::load_error();
        assert!(
            format!("{error:?}").contains(&format!("`{}`", policy_failure::POLICY_NAME)),
            "the startup error should name the policy: {error:?}"
        );

        let router = startup_error_router(&error);
        for method in super::publisher_fallback_methods() {
            for path in ["/", "/article"] {
                assert_eq!(
                    route(&router, empty_request(method.clone(), path)).status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{method} {path} should answer 500"
                );
            }
        }
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
    fn key_administration_paths_are_answered_here_and_never_proxied() {
        // A request to a key administration path carries an operator's
        // credentials and a key payload. Every publisher-fallback method must
        // be answered here with 404 and never proxied to the publisher origin,
        // which would be handed both. A publisher-fallback proxy without a
        // backend would surface as a 5xx, so a 404 proves the request was
        // answered here.
        let settings = Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse production-shaped settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        let state = build_state_from_settings(settings).expect("should build state");
        let router = TrustedServerApp::routes_for_state(&state);

        for path in [
            "/_ts/admin/keys/rotate",
            "/_ts/admin/keys/deactivate",
            "/_ts/admin/cache/purge",
            "/admin/keys/rotate",
            "/admin/keys/deactivate",
        ] {
            for method in super::publisher_fallback_methods() {
                let req = request_builder()
                    .method(method.clone())
                    .uri(format!("https://test-publisher.com{path}"))
                    .header(header::AUTHORIZATION, "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body(Body::from("{\"key_id\":\"leak-me\"}"))
                    .expect("should build authorized key administration request");

                let response = route(&router, req);

                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {path} with Authorization must be answered here (404), not proxied to publisher"
                );
            }
        }
    }

    #[test]
    fn an_authenticated_request_to_a_closed_path_is_answered_here() {
        let router = test_router();
        let ec_id = format!("{}.abc123", "a".repeat(64));
        for path in [
            "/_ts/admin/ec".to_owned(),
            format!("/_ts/admin/ec/{ec_id}"),
            "/_ts/admin/eids".to_owned(),
            "/_ts/admin/ec/".to_owned(),
            format!("/_ts/admin/ec/{ec_id}/extra"),
            "/_ts/admin/eids/".to_owned(),
            "/_ts/admin/eids/extra".to_owned(),
            "/_ts/admin/eids.json".to_owned(),
            "/_ts/admin/ec;foo".to_owned(),
            format!("/_ts/admin/ec%2F{ec_id}"),
            // A check for a literal slash would miss a percent-encoded separator,
            // so these are closed before the publisher fallback forwards
            // credentials upstream.
            "/_ts/admin%2Fec".to_owned(),
            "/_ts/admin%2fec".to_owned(),
            // The alias outside `/_ts`, with its descendants and encoded
            // separators.
            "/admin/keys".to_owned(),
            "/admin/keys/rotate/extra".to_owned(),
            "/admin/keys%2Frotate".to_owned(),
            "/admin%2fkeys/rotate".to_owned(),
            // Multi-encoded separators survive a single decode, so the reservation
            // decodes to a fixed point before the publisher fallback runs.
            "/admin%252Fkeys/rotate".to_owned(),
            "/_ts%252Fadmin/ec".to_owned(),
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

    #[test]
    fn dispatch_identify_get_routes_to_identity_handler() {
        // Parity guard: GET /_ts/api/v1/identify must reach the identify
        // handler chain. The test settings configure no ec.ec_store, so
        // require_identity_graph fails with a KvStore error (503) — proving
        // the request was NOT proxied to the publisher origin.
        let router = test_router();
        let response = route(&router, empty_request(Method::GET, "/_ts/api/v1/identify"));

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
        let response = route(
            &router,
            empty_request(Method::POST, "/_ts/api/v1/batch-sync"),
        );

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "POST batch-sync without ec_store should fail with the KvStore error, not reach the publisher"
        );
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
    fn dispatch_permissions_answers_as_data_and_as_a_page_and_writes_nothing() {
        let router = test_router();

        let data = route(&router, empty_request(Method::GET, "/_ts/permissions.json"));
        assert_eq!(data.status(), StatusCode::OK, "the data form should answer");
        assert_eq!(data.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(data.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            data.headers().get(header::SET_COOKIE).is_none(),
            "should write nothing for the reader"
        );
        assert!(
            data.extensions().get::<super::EcFinalizeState>().is_none(),
            "should not run the Edge Cookie lifecycle for a read"
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&data.into_body().into_bytes().unwrap_or_default())
                .expect("the data form should be JSON");
        for key in ["set", "awaiting", "signals", "tdls", "modules"] {
            assert!(
                payload.get(key).is_some(),
                "the resolution should carry `{key}`"
            );
        }

        let page = route(&router, empty_request(Method::GET, "/_ts/permissions"));
        assert_eq!(page.status(), StatusCode::OK, "the page should answer");
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn dispatch_config_answers_the_masked_settings_as_data_and_as_a_page() {
        let router = test_router();

        let data = route(&router, empty_request(Method::GET, "/_ts/config.json"));
        assert_eq!(data.status(), StatusCode::OK, "the data form should answer");
        assert_eq!(data.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(data.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            data.extensions().get::<super::EcFinalizeState>().is_none(),
            "should not run the Edge Cookie lifecycle for a read"
        );
        let body = data.into_body().into_bytes().unwrap_or_default();
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("the data form should be JSON");
        assert_eq!(
            payload["settings"]["publisher"]["domain"], "test-publisher.com",
            "should show an ordinary setting"
        );
        assert_eq!(
            payload["settings"]["publisher"]["proxy_secret"], "XXXX",
            "should mask a secret"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("unit-test-proxy-secret"),
            "should not carry the secret anywhere"
        );

        let page = route(&router, empty_request(Method::GET, "/_ts/config"));
        assert_eq!(page.status(), StatusCode::OK, "the page should answer");
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn dispatch_data_answers_a_browser_opening_the_page_and_nothing_else() {
        let router = test_router();

        let refused = route(&router, empty_request(Method::GET, "/_ts/data"));
        assert_eq!(
            refused.status(),
            StatusCode::FORBIDDEN,
            "should refuse a request that is not a browser opening the page"
        );
        assert!(
            refused
                .extensions()
                .get::<super::EcFinalizeState>()
                .is_none(),
            "should not run the Edge Cookie lifecycle for a refusal"
        );

        let mut opened = empty_request(Method::GET, "/_ts/data");
        opened
            .headers_mut()
            .insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        opened
            .headers_mut()
            .insert("sec-fetch-dest", HeaderValue::from_static("document"));
        let page = route(&router, opened);
        assert_eq!(page.status(), StatusCode::OK, "the page should answer");
        assert_eq!(
            page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store, private");
        assert!(
            page.headers().get(header::SET_COOKIE).is_none(),
            "should write nothing for the reader"
        );
        assert!(
            page.extensions().get::<super::EcFinalizeState>().is_none(),
            "should not run the Edge Cookie lifecycle for a read"
        );
        let body = page.into_body().into_bytes().unwrap_or_default();
        assert!(
            String::from_utf8_lossy(&body).contains("keeps no record"),
            "should say a deployment with no identity graph keeps no record"
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
    fn dispatch_ec_resolve_routes_to_resolve_handler() {
        // Parity guard: POST /_ts/api/v1/ec/resolve must reach the resolve
        // handler, not the publisher fallback or a router-level 405. The test
        // request carries no Origin, so the handler's own origin guard answers
        // 403, proving the request was handled here rather than proxied to
        // the publisher origin (which would error without a live backend).
        let router = test_router();
        let response =
            block_on(router.oneshot(empty_request(Method::POST, "/_ts/api/v1/ec/resolve")))
                .expect("should route request");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "POST ec/resolve should reach the resolve handler's origin guard, not the publisher"
        );
    }

    #[test]
    fn dispatch_fallback_attaches_ec_finalize_state() {
        // The publisher fallback must thread EC finalize state to the entry
        // point via response extensions — even on error responses — so that
        // edgezero_main can run ec_finalize_response and pull sync.
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

        let response = route(&router, req);

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
        let response = route(&router, empty_request(Method::GET, "/some-page"));

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
        // is maintained by the entry-point apply_finalize_headers call in lib.rs.
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
                .get(crate::middleware::HEADER_X_TS_FINALIZED)
                .is_none(),
            "router-level 405 bypasses FinalizeResponseMiddleware; lib.rs entry-point covers this"
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
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [request_signing]
            enabled = false

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            source.module = "origin"
            source.origin.url = "https://assets.example.com"
            "#,
        )
        .expect("should parse asset-route settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
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

    struct FixedBackend;

    impl PlatformBackend for FixedBackend {
        fn naming_policy(&self) -> trusted_server_core::platform::BackendNamingPolicy {
            trusted_server_core::platform::BackendNamingPolicy::Fastly
        }

        fn predict_name(
            &self,
            spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok(format!("{}-{}", spec.scheme, spec.host))
        }

        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.predict_name(spec)
        }
    }

    struct StreamingHttpClient;

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for StreamingHttpClient {
        fn supports_streaming_responses(&self) -> bool {
            true
        }

        async fn send(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            assert!(
                request.stream_response,
                "streaming Fastly routes should request an origin response stream"
            );
            let response = edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html")
                .body(Body::stream(futures::stream::iter(vec![
                    Bytes::from_static(b"streamed-asset"),
                ])))
                .map_err(|_| Report::new(PlatformError::HttpClient))?;
            Ok(PlatformResponse::new(response))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    fn streaming_runtime_services() -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(crate::platform::FastlyPlatformConfigStore))
            .secret_store(Arc::new(crate::platform::FastlyPlatformSecretStore))
            .kv_store(Arc::new(NoopKvStore) as Arc<dyn PlatformKvStore>)
            .backend(Arc::new(FixedBackend))
            .http_client(Arc::new(StreamingHttpClient))
            .geo(Arc::new(
                trusted_server_core::platform::PlatformGeoModule::new(
                    "platform",
                    Arc::new(crate::platform::FastlyPlatformGeo),
                ),
            ))
            .client_info(ClientInfo::default())
            .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
            .build()
    }

    #[test]
    fn dispatch_asset_fallback_streams_origin_body_without_buffering() {
        // Regression guard for the EdgeZero asset streaming cutover: a successful
        // asset proxy must hand `edgezero_main` a streaming body (`Body::Stream`)
        // rather than draining the origin into a buffered `Body::Once`. Injecting a
        // streaming HTTP client lets us assert the body type the entry point will
        // pipe straight to the client via `stream_to_client`.
        let settings = Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [request_signing]
            enabled = false

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            source.module = "origin"
            source.origin.url = "https://assets.example.com"
            "#,
        )
        .expect("should parse asset-route settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        let state = build_state_from_settings(settings).expect("should build state");

        let services = streaming_runtime_services();
        let req = empty_request(Method::GET, "/.images/logo.png");
        let index = state
            .settings
            .asset_route_index_for_path("/.images/logo.png")
            .expect("should match the configured asset route");
        let effects = RequestFilterEffects::default();

        let response = block_on(super::dispatch_asset_fallback(
            &state, &services, req, index, &effects,
        ));

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should preserve the streaming origin status"
        );
        assert!(
            matches!(response.body(), Body::Stream(_)),
            "EdgeZero asset dispatch must stream the origin body, not buffer it"
        );
    }

    #[test]
    fn dispatch_fallback_streams_publisher_body_without_buffering() {
        // Regression guard for the publisher streaming cutover (#849): a
        // successful publisher origin fetch must hand `edgezero_main` a lazy
        // streaming body (`Body::Stream`) so headers commit at origin first
        // byte, rather than draining the processed page into a buffered
        // `Body::Once`. Core tests cover the rewrite pipeline itself; this
        // guards the adapter wiring that could silently re-buffer.
        let settings = test_settings();
        let state = build_state_from_settings(settings).expect("should build state");
        let services = streaming_runtime_services();
        let req = empty_request(Method::GET, "/article");

        let response = block_on(super::dispatch_fallback(&state, &services, req));

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "publisher proxy should succeed against the streaming origin stub"
        );
        assert!(
            matches!(response.body(), Body::Stream(_)),
            "EdgeZero publisher dispatch must attach the lazy streaming body, not buffer it"
        );
        assert!(
            !response.headers().contains_key(header::CONTENT_LENGTH),
            "processed streaming publisher responses must not carry a stale Content-Length"
        );
    }

    /// `/robots.txt` is the publisher path's, streamed against an origin that
    /// answers each request in turn. A `GET` has the origin's file replaced
    /// by the refusal, a `HEAD` has the file made where the origin has none
    /// and gets no body, and a `POST` keeps the origin's `404`, because a
    /// file is made only for a `GET` or a `HEAD`. Finalizing gives each the
    /// refusal's `X-Robots-Tag`, the file's own answer included.
    #[test]
    fn robots_txt_is_served_through_the_publisher_path_with_its_tag() {
        use trusted_server_core::platform::test_support::{
            StubHttpClient, build_services_with_http_client,
        };

        let settings = Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [request_signing]
            enabled = false

            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
            "#,
        )
        .expect("should parse robots.txt test settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        let state = build_state_from_settings(settings).expect("should build state");
        let origin = Arc::new(StubHttpClient::new());
        origin.push_response_with_headers(
            200,
            b"User-agent: *\nAllow: /\n".to_vec(),
            vec![("content-type", "text/plain")],
        );
        for _ in 0..2 {
            origin.push_response_with_headers(
                404,
                b"<html><body>not found</body></html>".to_vec(),
                vec![("content-type", "text/html")],
            );
        }
        let services =
            build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>);
        let served = |method: Method| {
            let request = request_builder()
                .method(method)
                .uri("https://test-publisher.com/robots.txt")
                .header(header::HOST, "test-publisher.com")
                .body(Body::empty())
                .expect("should build request");
            let mut response = block_on(super::dispatch_fallback(&state, &services, request));
            crate::middleware::apply_finalize_headers(&state.settings, &mut response);
            response
        };
        let tag = |response: &Response| {
            response
                .headers()
                .get("x-robots-tag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let body = |response: Response| {
            let bytes = block_on(response.into_body().into_bytes_bounded(1024 * 1024))
                .expect("should read the body");
            String::from_utf8(bytes.to_vec()).expect("the body is text")
        };

        let get = served(Method::GET);
        assert_eq!(get.status(), StatusCode::OK);
        assert_eq!(tag(&get).as_deref(), Some("noindex, nofollow"));
        assert_eq!(body(get), "User-agent: *\nAllow: /ads.txt\nDisallow: /\n");

        let head = served(Method::HEAD);
        assert_eq!(
            head.status(),
            StatusCode::OK,
            "the file is made where the origin has none"
        );
        assert_eq!(tag(&head).as_deref(), Some("noindex, nofollow"));
        assert_eq!(body(head), "", "a HEAD gets no body");

        let post = served(Method::POST);
        assert_eq!(
            post.status(),
            StatusCode::NOT_FOUND,
            "a file is made only for a GET or a HEAD"
        );
        assert_eq!(tag(&post).as_deref(), Some("noindex, nofollow"));

        assert_eq!(
            origin.recorded_request_methods(),
            ["GET", "HEAD", "POST"],
            "each request reaches the origin"
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

    /// The modules this adapter links are offered in the order their hooks
    /// run. A module is named by what its section selects, and an
    /// implementation by what `[demand]` or `[ad-server]` names.
    #[test]
    fn the_linked_modules_are_offered_in_hook_order() {
        let names: Vec<&str> = shipped_module_builders()
            .iter()
            .map(|builder| {
                builder
                    .module_name()
                    .or_else(|| builder.demand().map(|demand| demand.id))
                    .or_else(|| builder.adserver().map(|adserver| adserver.id))
                    .unwrap_or_else(|| builder.id())
            })
            .collect();

        assert_eq!(
            names,
            [
                "auction.prebid",
                "testing.testlight",
                "framework.nextjs",
                "audience.permutive",
                "identity.lockr",
                "cmp.didomi",
                "cmp.inmobi",
                "cmp.sourcepoint",
                "cmp.osano",
                "tag.google-tag",
                "tag.google-tag-manager",
                "bot-protection.datadome",
                "ad-tag.google",
                "ad-tag.google.diagnostics",
                "ads-txt.iabtl",
                "headers.geo",
                "auction-protocol.openrtb",
                "auction.prebid-server",
                "auction.aps",
                "ad-server.iabtl",
            ],
            "should offer the linked modules in the order their hooks run"
        );
    }

    /// This adapter links no loading cache module, so every feature loads
    /// through `NoLoadingCache`, which keeps no copy, and a deployment that
    /// wants copies kept hands a loading cache module's builder to it.
    #[test]
    fn the_stock_build_links_no_loading_cache() {
        let offering: Vec<&str> = shipped_module_builders()
            .iter()
            .filter(|builder| builder.loading_cache().is_some())
            .map(IntegrationBuilder::id)
            .collect();

        assert!(
            offering.is_empty(),
            "a stock build should link no loading cache, and links {offering:?}"
        );
    }

    /// A deployment's builders follow the ones this adapter links.
    #[test]
    fn a_deployment_s_builders_follow_the_linked_ones() {
        let linked = shipped_module_builders().len();
        let extra = [IntegrationBuilder::implementations(
            "deployment_fixture",
            "deployment-crate",
        )];

        let all = module_builders_with(&extra);

        assert_eq!(
            all.len(),
            linked + 1,
            "should keep every linked builder and add the deployment's"
        );
        assert_eq!(
            all.last().map(IntegrationBuilder::source),
            Some("deployment-crate"),
            "should put the deployment's builder last"
        );
    }
}
