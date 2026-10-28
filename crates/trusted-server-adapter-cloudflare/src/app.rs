use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use edgezero_core::app::{Hooks, StoreMetadata, StoresMetadata};
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::addressing::{RequestAddressing, addressed_router};
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::telemetry::warn_once_when_analytics_is_unsent;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan_with,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::closed_paths::closed_path_response;
#[cfg(target_arch = "wasm32")]
use trusted_server_core::config_payload::{
    DEFAULT_SECRET_STORE_ID, settings_from_config_blob_with,
};
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::device::{
    BuiltinDeviceModule, DeviceModule, DeviceSignals, build_device_module,
};
use trusted_server_core::ec::module::{EdgeCookieModule, build_reusable_module};
use trusted_server_core::ec::resolve::handle_ec_resolve;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::evidence::BorrowedRequestInfo;
use trusted_server_core::http_util::is_navigation_request;
use trusted_server_core::inspect::config::{CONFIG_PATHS, handle_config};
use trusted_server_core::inspect::data::{DATA_PAGE_PATH, handle_data};
use trusted_server_core::inspect::permissions::{PERMISSIONS_PATHS, handle_permissions};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistry, ProxyDispatchInput, RequestFilterEffects,
    RequestFilterRegistryInput, RequestFilterRegistryOutcome,
};
use trusted_server_core::module_context::{ModuleContext, ModuleRequest};
#[cfg(target_arch = "wasm32")]
use trusted_server_core::platform::StoreName;
use trusted_server_core::platform::{PlatformIdentity, RuntimeServices};
use trusted_server_core::proxy::{
    AssetProxyCachePolicy, handle_asset_proxy_request, handle_first_party_click,
    handle_first_party_proxy, handle_first_party_proxy_rebuild, handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AppContext, AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, PublisherResponse,
    buffer_publisher_response_async, handle_page_bids, handle_publisher_request,
    handle_tsjs_dynamic, page_bids_preflight_denied,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;

use crate::middleware::{FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::build_runtime_services;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state shared by everything that serves one request.
///
/// Not built once per Worker isolate, because the Worker's fetch entry point
/// builds the application for every request with `build_app`, so this is
/// rebuilt on every request.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    /// The Edge Cookie module `[ec] module` selects, resolved once here.
    ///
    /// This adapter runs a fresh instance per request and resolving reads no
    /// request data, so the selection is resolved when the state is built
    /// and handed to every request through
    /// [`RuntimeServices::resolved_ec_module`](trusted_server_core::platform::RuntimeServices::resolved_ec_module),
    /// rather than resolved again on the request path.
    /// `None` for a deployment that selects no module.
    ec_module: Option<Arc<dyn EdgeCookieModule>>,
    /// The permission signal modules `[permission-signal] modules` selects
    /// from the scheme crates this adapter links, in the order they run.
    /// Selected once here so a name no crate answers to fails startup rather
    /// than the first request, and handed to every request's services.
    permission_signal_modules:
        Arc<[Arc<dyn trusted_server_core::permission_signal::PermissionSignalModule>]>,
    /// The permission policy the domain's settings loaded, which every
    /// request is given.
    permission_policy: Arc<trusted_server_core::permissions::PermissionPolicy>,
    /// Services a caller supplied for every request, rather than services built
    /// from the request context. `None` in a deployment.
    services: Option<RuntimeServices>,
    /// The loading caches the `[cache.<name>]` tables select from the linked
    /// builders, taken when this state is built and handed to every request's
    /// services.
    loading_caches: Arc<trusted_server_core::platform::LoadingCaches>,
}

/// Builds the application a Worker serves a request with.
///
/// The settings are read from the config store with an awaited Workers KV
/// read, which `Hooks::build_app` cannot make, so the Worker's fetch entry
/// point calls this and serves the result with `EdgeZero`'s `dispatch_app`.
/// Settings that cannot be read or do not load give the startup error
/// router.
#[cfg(target_arch = "wasm32")]
pub async fn build_app(env: &worker::Env) -> edgezero_core::app::App {
    let router = match load_startup_settings(env)
        .await
        .and_then(build_state_with_settings)
    {
        Ok(state) => build_router(&state),
        Err(error) => {
            log::error!("failed to build application state: {:?}", error);
            startup_error_router(&error)
        }
    };
    edgezero_core::app::App::with_name(router, TrustedServerApp::name())
}

/// Reads the Worker's settings from its config store, the Workers KV
/// namespace that `ts config push --adapter cloudflare` writes.
///
/// The binding and the key are resolved as the push resolves them. The
/// binding is the config store's platform name, which is
/// `trusted_server_config` unless
/// `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` names another, and
/// the key is the store's logical ID unless the matching `__KEY` variable
/// names another.
///
/// # Errors
///
/// [`TrustedServerError::Configuration`] when the namespace cannot be opened
/// or read, holds nothing at the key, or the settings do not load.
#[cfg(target_arch = "wasm32")]
async fn load_startup_settings(env: &worker::Env) -> Result<Settings, Report<TrustedServerError>> {
    use edgezero_core::config_store::ConfigStore as _;

    let selectors = config_store_selectors(env);
    let store_name = trusted_server_core::settings_data::config_store_name(&selectors);
    let key = trusted_server_core::settings_data::config_key(&selectors);
    let unreadable = || TrustedServerError::Configuration {
        message: format!(
            "failed to read Trusted Server app config key `{key}` from config store `{store_name}`"
        ),
    };
    let store = edgezero_adapter_cloudflare::config_store::CloudflareConfigStore::from_env(
        env,
        store_name.as_ref(),
    )
    .map_err(|error| Report::new(unreadable()).attach(error.to_string()))?;
    let envelope = store
        .get(&key)
        .await
        .map_err(|error| Report::new(unreadable()).attach(error.to_string()))?
        .ok_or_else(|| {
            Report::new(unreadable()).attach("the namespace holds nothing at that key")
        })?;
    let secret_store = crate::platform::CloudflareSecretStoreAdapter { env: env.clone() };
    // The settings are validated as they load, against the same builders the
    // state is built with.
    settings_from_config_blob_with(
        &envelope,
        &secret_store,
        &StoreName::from(DEFAULT_SECRET_STORE_ID),
        trusted_server_core::platform::PolicyOrigin::Blob,
        &shipped_module_builders(),
    )
}

/// The config store selectors the Worker's variables set. They are read one
/// by one, because a Worker's environment cannot be listed.
#[cfg(target_arch = "wasm32")]
fn config_store_selectors(env: &worker::Env) -> edgezero_core::env_config::EnvConfig {
    let id = trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID.to_ascii_uppercase();
    let names = ["NAME", "KEY"].map(|setting| format!("EDGEZERO__STORES__CONFIG__{id}__{setting}"));
    edgezero_core::env_config::EnvConfig::from_vars(
        names
            .into_iter()
            .filter_map(|name| env.var(&name).ok().map(|value| (name, value.to_string()))),
    )
}

/// Build the application state from explicit settings.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie module cannot be built for
/// this adapter, or when the auction orchestrator or the integration registry
/// fail to initialize.
fn build_state_with_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_registrations(settings, &[])
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
pub fn build_state_with_registrations(
    settings: Settings,
    integrations: &[IntegrationBuilder],
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_registrations_and_services(settings, integrations, None)
}

/// Build the application state with the services every request will use,
/// rather than services built per request from the request context.
fn build_state_with_services(
    settings: Settings,
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_registrations_and_services(settings, &[], services)
}

/// The header Workers reads a shared cache policy from, which core writes
/// the policy in. Declaring it keeps an origin response with a policy in it
/// out of the stored copies.
pub const CDN_CACHE_CONTROL_HEADER: &str = "cloudflare-cdn-cache-control";

/// The header core writes a shared cache policy in on this adapter.
pub(crate) const EDGE_CACHE_HEADER: EdgeCacheHeader =
    EdgeCacheHeader::Platform(CDN_CACHE_CONTROL_HEADER);

fn build_state_with_registrations_and_services(
    settings: Settings,
    integrations: &[IntegrationBuilder],
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    trusted_server_core::platform::declare_platform_cache_headers(&[CDN_CACHE_CONTROL_HEADER]);
    // The modules this adapter links come first and the deployment's own
    // follow, which is the order their hooks run in.
    let integrations = module_builders_with(integrations);
    let integrations = integrations.as_slice();

    // The plan is compiled with the integrations this adapter was given, so an
    // `[ad-server]` or `[demand]` name one of their builders supplies resolves
    // here. Compiling without them would drop the implementation and report the
    // name as one no builder registers.
    let plan = Arc::new(compile_auction_plan_with(&settings, integrations)?);
    plan.validate_for_target(&crate::platform::auction_target())?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan))?;
    let registry = IntegrationRegistry::with_plan_and_registrations(&settings, plan, integrations)?
        .with_platform(PlatformIdentity::named("cloudflare"));

    // Composition root: resolve the module selection once, before any request
    // is served, so a selection this adapter can never supply fails here rather
    // than on the first request. Keeping what the resolution produced is what
    // stops the request path resolving the same settings again. The registry is
    // built first because a module can supply the vendor Edge Cookie module
    // the selector names, and resolving without it would reject a selection
    // this deployment can in fact satisfy. A caller supplying its own
    // `RuntimeServices` may have resolved one already, and that one comes first
    // because it is what the request path will see. This adapter supplies no
    // host signals, so that argument stays `None` until it does.
    let ec_module = build_reusable_module(
        &settings.ec,
        None,
        services
            .as_ref()
            .and_then(RuntimeServices::resolved_ec_module)
            .or_else(|| registry.ec_module()),
    )?;
    let permission_signal_modules =
        trusted_server_core::permission_signal::build_permission_signal_modules(
            &settings,
            &shipped_signal_modules(),
        )?;
    let loading_caches =
        trusted_server_core::integrations::loading_caches_from(&settings, integrations)?;
    // This adapter builds no telemetry sink, so a module `[analytics]` selects
    // sends nothing unless the caller's services carry a sink.
    warn_once_when_analytics_is_unsent(
        &settings,
        services.as_ref(),
        "Cloudflare",
        &UNSENT_ANALYTICS_LOGGED,
    );
    warn_once_when_the_identity_graph_is_unavailable(&settings, &IDENTITY_GRAPH_UNAVAILABLE_LOGGED);
    warn_once_when_an_asset_route_enables_the_image_optimizer(&settings);
    let permission_policy = settings.required_permission_policy()?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        ec_module,
        permission_signal_modules,
        permission_policy,
        services,
        loading_caches,
    }))
}

/// Set once startup has logged that this adapter sends no auction telemetry.
/// The state is built for every request, so the flag keeps the warning to
/// one for each instance.
static UNSENT_ANALYTICS_LOGGED: AtomicBool = AtomicBool::new(false);

/// Set once startup has logged that this adapter opens no identity graph. The
/// state can be built more than once, so the flag keeps the warning to one for
/// each instance.
static IDENTITY_GRAPH_UNAVAILABLE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Logs once, at warning level, that `[ec] ec_store` names an identity graph
/// this adapter does not open, so the Edge Cookie is still written but the
/// data the graph would keep with it, such as partner IDs, is not. `logged`
/// is set as the line is logged, and once set, nothing more is logged.
///
/// The identity graph needs strongly consistent reads and conditional writes,
/// which the key-value store this adapter can open does not offer.
fn warn_once_when_the_identity_graph_is_unavailable(settings: &Settings, logged: &AtomicBool) {
    let Some(store) = settings.ec.ec_store.as_deref() else {
        return;
    };
    if logged.swap(true, Ordering::Relaxed) {
        return;
    }
    log::warn!(
        "[ec] ec_store names `{store}`, but the Cloudflare adapter opens no identity \
         graph, so the Edge Cookie is still written but the data the graph would \
         keep with it, such as partner IDs, is not"
    );
}

/// Set once startup has logged that an asset route enables the Image
/// Optimizer, which this adapter does not translate. The state is built for
/// every request, so the flag keeps the warning to one for each instance.
static IMAGE_OPTIMIZER_UNAVAILABLE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Logs once, at warning level, the prefix of each asset route that enables
/// the Image Optimizer, whose metadata this adapter does not translate.
///
/// The HTTP client refuses a request carrying Image Optimizer metadata, so
/// such a route answers each request it would optimize with `502`. The route
/// is not refused at startup, because its SVG paths and debug requests are
/// still answered, as are the deployment's other routes and pages.
fn warn_once_when_an_asset_route_enables_the_image_optimizer(settings: &Settings) {
    let prefixes: Vec<String> = settings
        .proxy
        .asset_routes
        .iter()
        .filter(|route| route.image_optimizer.as_ref().is_some_and(|io| io.enabled))
        .map(|route| format!("`{}`", route.prefix))
        .collect();
    if prefixes.is_empty() || IMAGE_OPTIMIZER_UNAVAILABLE_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    log::warn!(
        "the Cloudflare adapter does not translate Image Optimizer metadata, so \
         each request these asset routes would optimize answers 502 Bad Gateway, \
         and their SVG paths and requests that set the profile set's debug \
         parameter are served unoptimized: {}",
        prefixes.join(", ")
    );
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
/// that adds a module of its own hands its builder to [`build_state_with_registrations`], which
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
        // Run by `[shared.51degrees]` rather than selected by a section.
        trusted_server_middleware_51degrees::builder(),
        // The analytics module `[analytics]` selects, which changes no page.
        trusted_server_analytics_tinybird::builder(),
        // Implementations `[demand]` and `[ad-server]` can name, which no
        // section selects.
        trusted_server_auction_protocol_openrtb::builder(),
        trusted_server_auction_prebid_server::builder(),
        trusted_server_auction_aps::builder(),
        trusted_server_ad_server_iabtl::builder(),
        // The origin signer a source's auth table can name, offered whatever
        // the settings say, which no section selects either.
        trusted_server_shared_aws::builder(),
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

impl AppState {
    /// Build per-request [`RuntimeServices`], carrying the Edge Cookie module
    /// the composition root already resolved and applying the module-supplied
    /// geo, Edge Cookie and device modules selected by `[geo]`, `[ec]` and
    /// `[device] module`, and the asset cache module `[asset-cache]` selects.
    /// The services also carry the permission policy the domain's settings
    /// loaded, whichever services the request is given.
    ///
    /// No Edge Cookie module is carried when the composition root found
    /// nothing safe to keep, so the request path resolves the selection for
    /// itself.
    ///
    /// For geo, unset and `"none"` both resolve nothing, so no client IP
    /// reaches a host geo service. `"platform"` opts in to this adapter's own
    /// lookup, and any other key names an integration module that declares a
    /// geo module. Identity and device are applied the same way when a module
    /// supplies them.
    fn services_for_request(&self, ctx: &RequestContext) -> RuntimeServices {
        let mut services = self
            .services
            .clone()
            .unwrap_or_else(|| {
                build_runtime_services(
                    ctx,
                    &self.settings,
                    &self.permission_signal_modules,
                    &self.permission_policy,
                )
                .with_loading_caches(Arc::clone(&self.loading_caches))
            })
            .with_resolved_ec_module(self.ec_module.clone())
            .with_permission_policy(Arc::clone(&self.permission_policy));
        if let Some(module) = self.registry.geo_module() {
            services = services.with_geo(module);
        }
        if let Some(module) = self.registry.ec_module() {
            services = services.with_ec_module(module);
        }
        if let Some(module) = self.registry.device_module() {
            services = services.with_device_module(module);
        }
        services = services.with_identifier_modules(self.registry.identifier_modules());
        if let Some(module) = self.registry.asset_cache_module() {
            services = services.with_asset_cache_module(module);
        }
        // The publisher this request resolved to names every key in a shared
        // store and every name the browser holds.
        let prefixes = self.registry.prefixes(
            &self.settings,
            trusted_server_core::module_context::ModuleRequest::of(
                ctx.request(),
                services.client_info(),
            ),
        );
        services.with_prefixes(prefixes)
    }
}

// ---------------------------------------------------------------------------
// Request cycle
// ---------------------------------------------------------------------------

/// Classifies the request with the device module `[device]` selects.
///
/// A module a registration supplies, which the request's services carry, is
/// shown the `User-Agent` and the cookies, which carry what a page gathers in
/// the browser. Otherwise [`build_device_module`] gives the built-in module,
/// which reads only the `User-Agent`. This adapter supplies no TLS or HTTP/2
/// signals, so `platform` classifies as the built-in module does. The module
/// is handed a module context carrying the request, that evidence, the
/// settings and the services.
async fn derive_device_signals(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> DeviceSignals {
    let registered = services.device_module();
    let mut headers = HeaderMap::new();
    for (name, shown) in [
        (header::USER_AGENT, true),
        (header::COOKIE, registered.is_some()),
    ] {
        if shown && let Some(value) = req.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    let client_ip = services
        .client_info()
        .client_ip
        .map(|ip| ip.to_string())
        .unwrap_or_default();
    let request_info = BorrowedRequestInfo::new(&client_ip, None).with_headers(&headers);
    let built;
    let module: &dyn DeviceModule = match registered.as_deref() {
        Some(module) => module,
        None => {
            built = build_device_module(settings, || {
                Box::new(BuiltinDeviceModule::new()) as Box<dyn DeviceModule>
            });
            built.as_ref()
        }
    };
    let context = ModuleContext::new(ModuleRequest::of(req, services.client_info()))
        .with_evidence(&request_info)
        .with_settings(settings)
        .with_services(services);
    module
        .detect(context.call(module.id(), module.required_permissions()))
        .await
}

/// Builds the geo-aware [`EcContext`] every route in the request cycle is
/// handed, carrying the request's device classification.
///
/// The geo lookup runs inside
/// [`EcContext::read_from_request_resolving_geo`], so every adapter reports the
/// same distinction: no location falls back to the top of the
/// `permissions.yaml` rules tree, while a failed lookup resolves every
/// permission at the requires-signal floor and is logged at error level.
/// Geo comes from the Workers `cf` object when deployed.
///
/// Mirrors the Fastly entry point, which keeps the report and answers with an
/// error response: when the Edge Cookie context cannot be read the request
/// fails rather than continuing with `EcContext::default()`, which would serve
/// every request with no identity. A malformed cookie value, a bad consent
/// string and a failed geo lookup do not reach this error path at all, so
/// failing here does not fail requests for ordinary parse problems.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie module cannot be built for
/// this request, or when the request's `Cookie` header cannot be read as
/// text, because it holds a byte outside visible ASCII, as an accented letter
/// in UTF-8 does.
async fn build_ec_context(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcContext, Report<TrustedServerError>> {
    let device_signals = derive_device_signals(settings, services, req).await;
    let mut context = EcContext::read_from_request_resolving_geo(settings, req, services).await?;
    context.set_device_signals(device_signals);
    Ok(context)
}

/// The request's Edge Cookie state, read before its route runs.
struct EcRequestState {
    ec_context: EcContext,
    /// Whether the request passed the bot gate, which creating an Edge Cookie
    /// on a page navigation sits behind.
    is_real_browser: bool,
}

/// The Edge Cookie state a route leaves on its response.
/// [`FinalizeResponseMiddleware`] takes it off the response and finalizes the
/// Edge Cookie once the response is finalized.
#[derive(Clone)]
pub(crate) struct EcFinalizeState {
    pub(crate) ec_context: EcContext,
    /// The request's services, which finalizing hands to the selected Edge
    /// Cookie module.
    pub(crate) services: RuntimeServices,
}

/// Reads the request's Edge Cookie state, with the bot gate's answer taken
/// from the device classification.
///
/// # Errors
///
/// The errors of [`build_ec_context`].
async fn build_ec_request_state(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcRequestState, Report<TrustedServerError>> {
    let ec_context = build_ec_context(settings, services, req).await?;
    let is_real_browser = ec_context
        .device_signals()
        .is_some_and(|signals| signals.looks_like_browser);
    if !is_real_browser {
        log::info!("Bot gate: blocking EC operations");
    }
    Ok(EcRequestState {
        ec_context,
        is_real_browser,
    })
}

/// Leaves the Edge Cookie state and the non-empty request-filter response
/// effects on a response of the request cycle, for
/// [`FinalizeResponseMiddleware`].
fn attach_dispatch_extensions(
    mut response: Response,
    ec: EcRequestState,
    services: &RuntimeServices,
    effects: RequestFilterEffects,
) -> Response {
    response.extensions_mut().insert(EcFinalizeState {
        ec_context: ec.ec_context,
        services: services.clone(),
    });
    attach_request_filter_effects(response, effects)
}

/// Result of the request-filter step that runs before a route.
enum PreRoute {
    /// A filter answered, or failed, and that answer stands in for the
    /// route's. Carries the response effects the filters left.
    ShortCircuit {
        response: Response,
        effects: RequestFilterEffects,
    },
    /// The route runs, and these response effects are applied afterwards.
    Continue { effects: RequestFilterEffects },
}

/// Runs the integration request filters before the route.
///
/// Request header changes are applied to `req`, so the route sees them, and
/// the response effects are returned for [`FinalizeResponseMiddleware`] to
/// apply once the response is finalized. A filter that answers, such as a
/// bot challenge, stands in for the route. Every filter reads the location
/// and the permissions resolved for the request's Edge Cookie context.
async fn run_pre_route_filters(
    state: &AppState,
    services: &RuntimeServices,
    req: &mut Request,
    ec_context: &EcContext,
) -> PreRoute {
    match state
        .registry
        .filter_request(RequestFilterRegistryInput {
            settings: &state.settings,
            services,
            req,
            geo_info: ec_context.geo_info(),
            permissions: Some(ec_context.permissions()),
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

/// Leaves non-empty request-filter response effects on `response`, for
/// [`FinalizeResponseMiddleware`] to apply after it finalizes the response.
fn attach_request_filter_effects(
    mut response: Response,
    effects: RequestFilterEffects,
) -> Response {
    if !effects.response_headers.is_empty() {
        response.extensions_mut().insert(effects);
    }
    response
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
            let services = s.services_for_request(&ctx);
            let mut req = ctx.into_request();
            if let Err(error) = s.registry.prepare_request(&s.settings, &mut req) {
                return Ok(http_error(&error));
            }
            Ok(f(s, services, req).await.unwrap_or_else(|e| http_error(&e)))
        })
    }
}

// ---------------------------------------------------------------------------
// Named routes
// ---------------------------------------------------------------------------

/// The routes that run in the request cycle, each registered by
/// [`build_router`] on the paths and methods it answers.
#[derive(Clone, Copy)]
enum NamedRouteHandler {
    TrustedServerDiscovery,
    VerifySignature,
    EcResolve,
    Auction,
    PageBids,
    FirstPartyProxy,
    FirstPartyClick,
    FirstPartySign,
    FirstPartyProxyRebuild,
}

fn named_route_handler(
    state: Arc<AppState>,
    handler: NamedRouteHandler,
) -> impl Fn(RequestContext) -> BoxedHandlerFuture + Clone + 'static {
    move |ctx: RequestContext| {
        let state = Arc::clone(&state);
        Box::pin(execute_named(state, ctx, handler)) as BoxedHandlerFuture
    }
}

/// Runs a named route's request through the request cycle.
///
/// The modules' preparers run first. The route is then handed the Edge
/// Cookie context and runs once the request filters let the request
/// continue.
async fn execute_named(
    state: Arc<AppState>,
    ctx: RequestContext,
    handler: NamedRouteHandler,
) -> Result<Response, EdgeError> {
    let services = state.services_for_request(&ctx);
    let mut req = ctx.into_request();
    if let Err(error) = state.registry.prepare_request(&state.settings, &mut req) {
        return Ok(http_error(&error));
    }

    let mut ec = match build_ec_request_state(&state.settings, &services, &req).await {
        Ok(ec) => ec,
        Err(report) => return Ok(http_error(&report)),
    };
    let effects = match run_pre_route_filters(&state, &services, &mut req, &ec.ec_context).await {
        PreRoute::ShortCircuit { response, effects } => {
            return Ok(attach_dispatch_extensions(response, ec, &services, effects));
        }
        PreRoute::Continue { effects } => effects,
    };

    let response = run_named_route(&state, &services, req, handler, &mut ec.ec_context)
        .await
        .unwrap_or_else(|error| http_error(&error));
    Ok(attach_dispatch_extensions(response, ec, &services, effects))
}

async fn run_named_route(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    handler: NamedRouteHandler,
    ec_context: &mut EcContext,
) -> Result<Response, Report<TrustedServerError>> {
    match handler {
        NamedRouteHandler::TrustedServerDiscovery => {
            handle_trusted_server_discovery(&state.settings, services, req)
        }
        NamedRouteHandler::VerifySignature => {
            handle_verify_signature(&state.settings, services, req)
        }
        NamedRouteHandler::EcResolve => {
            // This adapter opens no identity graph, so a created Edge Cookie
            // is set with no graph row.
            handle_ec_resolve(&state.settings, req, ec_context, None, services).await
        }
        NamedRouteHandler::Auction => {
            // The auction consent gate reads the caller's jurisdiction from
            // the geo-aware context, where `EcContext::default()` would fail it
            // closed for consented users.
            handle_auction(
                &state.settings,
                &state.orchestrator,
                None,
                None,
                ec_context,
                services,
                req,
            )
            .await
        }
        NamedRouteHandler::PageBids => {
            // The OPTIONS preflight is denied so the GET handler's
            // `X-TSJS-Page-Bids` gate stays trustworthy.
            if req.method() == Method::OPTIONS {
                return Ok(page_bids_preflight_denied());
            }
            let auction = AuctionDispatch {
                orchestrator: &state.orchestrator,
                slots: state.settings.creative_opportunity_slots(),
                registry: None,
            };
            handle_page_bids(&state.settings, services, None, auction, ec_context, req).await
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

// ---------------------------------------------------------------------------
// Fallback dispatcher (robots.txt / tsjs / integration proxy / publisher)
// ---------------------------------------------------------------------------

async fn execute_fallback(
    state: Arc<AppState>,
    ctx: RequestContext,
) -> Result<Response, EdgeError> {
    let services = state.services_for_request(&ctx);
    let mut req = ctx.into_request();
    if let Some(response) = closed_path_response(&req) {
        return Ok(response);
    }
    if let Err(error) = state.registry.prepare_request(&state.settings, &mut req) {
        return Ok(http_error(&error));
    }
    Ok(dispatch_fallback(&state, &services, req).await)
}

/// Dispatches the fallback routes: robots.txt, tsjs assets (`GET` only),
/// integration proxy routes, and the publisher origin.
///
/// The request arrives prepared, and is not to a closed path, because
/// [`execute_fallback`] answers a closed path and runs the preparers first.
async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    mut req: Request,
) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    // A crawler asking for robots.txt is answered once the modules have
    // prepared the request, before the Edge Cookie steps and the request
    // filters, so its Edge Cookie and consent are not read, no device
    // module is shown its cookies and no filter's answer stands in for the
    // file.
    if asks_for_robots_txt(&state.settings, &method, &path) {
        return answer_robots_txt(state, services, req, &method).await;
    }

    // Identity could not be established (for example the selected Edge
    // Cookie module is unavailable). Answer with an error rather than
    // serving the request with no identity.
    let mut ec = match build_ec_request_state(&state.settings, services, &req).await {
        Ok(ec) => ec,
        Err(report) => return http_error(&report),
    };
    let effects = match run_pre_route_filters(state, services, &mut req, &ec.ec_context).await {
        PreRoute::ShortCircuit { response, effects } => {
            return attach_dispatch_extensions(response, ec, services, effects);
        }
        PreRoute::Continue { effects } => effects,
    };

    // tsjs assets are served for `GET` only.
    let result = if method == Method::GET && path.starts_with("/static/tsjs=") {
        handle_tsjs_dynamic(&req, &state.registry, EDGE_CACHE_HEADER)
    } else if state.registry.has_route(&method, &path) {
        // A module route is handed what was resolved for its request, the
        // permissions among them, as the publisher path is.
        state
            .registry
            .handle_proxy(ProxyDispatchInput {
                method: &method,
                path: &path,
                settings: &state.settings,
                kv: None,
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
        // A `GET` or `HEAD` under an asset route goes to the route's own
        // source in place of the publisher's origin. Its response skips the
        // Edge Cookie steps, so it carries no Edge Cookie state.
        let matched_asset_route = matches!(method, Method::GET | Method::HEAD)
            .then(|| state.settings.asset_route_index_for_path(&path))
            .flatten();
        if let Some(index) = matched_asset_route {
            return dispatch_asset_fallback(state, services, req, index, effects).await;
        }
        // A real browser's page navigation is given an Edge Cookie when it has
        // none. Another request may lack consent signals such as `Sec-GPC`.
        if ec.is_real_browser
            && is_navigation_request(&req)
            && let Err(err) = ec
                .ec_context
                .generate_if_needed(&state.settings, None, services)
                .await
        {
            log::error!("EC generation failed for publisher proxy: {err:?}");
        }
        let auction = AuctionDispatch {
            orchestrator: &state.orchestrator,
            slots: state.settings.creative_opportunity_slots(),
            registry: None,
        };
        match handle_publisher_request(
            AppContext {
                settings: &state.settings,
                integration_registry: &state.registry,
            },
            services,
            None,
            &mut ec.ec_context,
            auction,
            req,
            EDGE_CACHE_HEADER,
        )
        .await
        {
            Ok(publisher_response) => {
                resolve_publisher_response(
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
        }
    };

    attach_dispatch_extensions(
        result.unwrap_or_else(|error| http_error(&error)),
        ec,
        services,
        effects,
    )
}

/// Whether an asset response carries a body.
///
/// `HEAD` responses and the bodiless statuses (204, 304) carry no body, so a
/// body stream is dropped for them. This adapter's HTTP client holds every
/// response whole and writes the length of the body it read as the
/// `Content-Length`, which for a `HEAD` is `0`.
fn asset_response_carries_body(method: &Method, status: StatusCode) -> bool {
    *method != Method::HEAD
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
}

/// Answers a request under the asset route at `index` from the route's own
/// source.
///
/// The route's source module locates the asset, its signer signs the request
/// where the source's `auth` table names one, and core answers from a copy it
/// holds before it asks the origin. The response carries its cache policy,
/// which [`FinalizeResponseMiddleware`] applies after the finalize headers,
/// and the request filters' effects, and no Edge Cookie state, because an
/// asset response skips the Edge Cookie steps.
async fn dispatch_asset_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    index: usize,
    effects: RequestFilterEffects,
) -> Response {
    let method = req.method().clone();
    let asset_route = &state.settings.proxy.asset_routes[index];
    let answer = match state.registry.route_source(&state.settings, index) {
        Ok(source) => {
            handle_asset_proxy_request(&state.settings, services, req, asset_route, source.as_ref())
                .await
        }
        Err(report) => Err(report),
    };
    let response = match answer {
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
    attach_request_filter_effects(response, effects)
}

/// Whether the request is a crawler's `GET` or `HEAD` of the robots.txt that
/// `[robots-txt]` makes.
fn asks_for_robots_txt(settings: &Settings, method: &Method, path: &str) -> bool {
    settings.robots_txt.is_some()
        && path == trusted_server_core::robots_txt::ROBOTS_TXT_PATH
        && matches!(*method, Method::GET | Method::HEAD)
}

/// Answers robots.txt through the publisher path with an empty Edge Cookie
/// context and no auction, so the crawler's Edge Cookie and consent
/// signals are not read.
async fn answer_robots_txt(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
    method: &Method,
) -> Response {
    let answer = handle_publisher_request(
        AppContext {
            settings: &state.settings,
            integration_registry: &state.registry,
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
        EDGE_CACHE_HEADER,
    )
    .await;
    let response = match answer {
        Ok(answer) => {
            resolve_publisher_response(
                answer,
                method,
                &state.settings,
                &state.registry,
                &state.orchestrator,
                services,
            )
            .await
        }
        Err(report) => Err(report),
    };
    response.unwrap_or_else(|report| http_error(&report))
}

// ---------------------------------------------------------------------------
// Publisher response helper
// ---------------------------------------------------------------------------

/// Collapse a [`PublisherResponse`] into a plain [`Response`].
///
/// Delegates to the shared [`buffer_publisher_response_async`], which collects
/// the dispatched server-side auction and enforces
/// `settings.publisher.max_buffered_body_bytes`, then removes any
/// `Transfer-Encoding` header since the buffered body is no longer chunked.
async fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: &Settings,
    registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
) -> Result<Response, Report<TrustedServerError>> {
    let mut response = buffer_publisher_response_async(
        publisher_response,
        method,
        settings,
        registry,
        orchestrator,
        services,
    )
    .await?;
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

/// The key-value store this adapter declares, which is also the Workers KV
/// binding `EdgeZero`'s dispatch opens for it on every request, because
/// `EdgeZero` binds a store by its id unless
/// `EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME` names another binding.
///
/// `EdgeZero` treats a declared store as required, so a Worker deployed
/// without the binding fails every request before any route runs.
const KV_STORE_ID: &str = "TRUSTED_SERVER_KV";

impl Hooks for TrustedServerApp {
    fn name() -> &'static str {
        "TrustedServer"
    }

    /// The fetch entry point installs this adapter's logger itself
    /// ([`crate::logging::init_logger`]) before it builds the application, so
    /// no `EdgeZero` entry point installs another.
    fn owns_logging() -> bool {
        true
    }

    /// The startup error router, because this hook cannot read the settings.
    ///
    /// A Worker reads its settings from the config store with an awaited
    /// read, so its fetch entry point builds the application with
    /// `build_app` and never calls this hook.
    fn routes() -> RouterService {
        startup_error_router(&Report::new(TrustedServerError::Configuration {
            message: "the Cloudflare adapter reads its settings from the config store \
                      when the Worker builds its application"
                .to_string(),
        }))
    }

    /// Declares the key-value store alone, so `EdgeZero`'s dispatch opens it
    /// for every request and the request's services keep copies in Workers KV
    /// between requests. `build_app` reads the settings from the config
    /// store's namespace itself, and secrets come from the Worker
    /// environment, so no config or secret store is declared.
    fn stores() -> StoresMetadata {
        StoresMetadata {
            config: None,
            kv: Some(StoreMetadata {
                default: KV_STORE_ID,
                ids: &[KV_STORE_ID],
            }),
            secrets: None,
        }
    }
}

impl TrustedServerApp {
    /// Build the full application router from explicit settings.
    ///
    /// Testing seam: cross-adapter parity tests use this to drive the router
    /// with settings they build, where a Worker reads its settings from its
    /// config store.
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

    /// Build the full application router from explicit settings, composing the
    /// built-in integrations with the externally supplied builders in
    /// `integrations`.
    ///
    /// The route table is the one [`TrustedServerApp::routes_with_settings`]
    /// builds, so a composed deployment routes exactly as the plain one does.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected Edge Cookie module cannot be built
    /// for this adapter, when the auction plan does not compile or cannot run
    /// on this adapter, or when the auction orchestrator or the integration
    /// registry fail to initialize, which includes two builders claiming the
    /// same integration id.
    pub fn routes_with_registrations(
        settings: Settings,
        integrations: &[IntegrationBuilder],
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_registrations(settings, integrations)?;
        Ok(build_router(&state))
    }

    /// Build the full application router as
    /// [`TrustedServerApp::routes_with_registrations`] does, asking
    /// `addressing` about every request before it is routed.
    ///
    /// This adapter reads one app-config blob, so the selector values the
    /// addressing names are ignored. A request it addresses continues at the
    /// path it gives, with its query kept, and carries the prefix it removed
    /// as a [`RemovedPathPrefix`](trusted_server_core::addressing::RemovedPathPrefix)
    /// extension. A request it addresses by host is routed as it is.
    ///
    /// # Errors
    ///
    /// Returns the errors [`TrustedServerApp::routes_with_registrations`]
    /// returns.
    pub fn routes_with_addressing(
        settings: Settings,
        integrations: &[IntegrationBuilder],
        addressing: Arc<dyn RequestAddressing>,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let router = Self::routes_with_registrations(settings, integrations)?;
        Ok(addressed_router(router, addressing))
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

fn build_router(state: &Arc<AppState>) -> RouterService {
    {
        let state = Arc::clone(state);

        // Shared fallback dispatch: routes to robots.txt, tsjs (GET only),
        // integration proxy, or publisher.
        let fallback = {
            let s = Arc::clone(&state);
            move |ctx: RequestContext| {
                let s = Arc::clone(&s);
                execute_fallback(s, ctx)
            }
        };
        let named = |handler: NamedRouteHandler| named_route_handler(Arc::clone(&state), handler);

        let mut router = RouterService::builder()
            // Outermost middleware: strips the configured trusted-client-IP
            // headers before anything else sees the request. Must stay first —
            // any middleware registered ahead of it would observe the
            // shared-secret authentication header.
            .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
            .get(
                "/.well-known/trusted-server.json",
                named(NamedRouteHandler::TrustedServerDiscovery),
            )
            .post(
                "/verify-signature",
                named(NamedRouteHandler::VerifySignature),
            )
            .post(
                "/_ts/api/v1/ec/resolve",
                named(NamedRouteHandler::EcResolve),
            )
            .post("/auction", named(NamedRouteHandler::Auction))
            .get(
                "/first-party/proxy",
                named(NamedRouteHandler::FirstPartyProxy),
            )
            .get(
                "/first-party/click",
                named(NamedRouteHandler::FirstPartyClick),
            )
            .get(
                "/first-party/sign",
                named(NamedRouteHandler::FirstPartySign),
            )
            .post(
                "/first-party/sign",
                named(NamedRouteHandler::FirstPartySign),
            )
            // GET serves the click guard's navigation fallback: the creative
            // iframe is an opaque origin (sandbox without `allow-same-origin`),
            // so its JSON POST is blocked by CORS and the guard navigates here
            // for a 302 instead.
            .get(
                "/first-party/proxy-rebuild",
                named(NamedRouteHandler::FirstPartyProxyRebuild),
            )
            .post(
                "/first-party/proxy-rebuild",
                named(NamedRouteHandler::FirstPartyProxyRebuild),
            );

        // SPA re-auction endpoint, registered on the canonical path and on the
        // deprecated `PAGE_BIDS_LEGACY_PATH` double-underscore alias. The alias
        // keeps tsjs bundles served before the `/_ts/page-bids` rename getting
        // ads on SPA navigations until they age out of browser caches.
        //
        // The OPTIONS preflight is denied on both so the GET handler's
        // `X-TSJS-Page-Bids` gate stays trustworthy — an alias that let the
        // preflight fall through to a permissive origin would reopen exactly
        // the cross-site hole the canonical path closes.
        let page_bids = named(NamedRouteHandler::PageBids);
        for path in [PAGE_BIDS_PATH, PAGE_BIDS_LEGACY_PATH] {
            router = router.route(path, Method::GET, page_bids.clone());
            router = router.route(path, Method::OPTIONS, page_bids.clone());
        }

        // What the deployment decided for the asking request, shown to anyone
        // as a page and as data. Read only, so nothing is written for the
        // reader.
        let permissions = make_handler(Arc::clone(&state), |s, services, req| async move {
            handle_permissions(&s.settings, &services, &req).await
        });
        for path in PERMISSIONS_PATHS {
            router = router.route(path, Method::GET, permissions.clone());
        }

        // The settings the deployment is running, masked, shown to anyone as
        // a page and as data.
        let config = make_handler(Arc::clone(&state), |s, _services, req| async move {
            Ok(handle_config(&s.settings, &req))
        });
        for path in CONFIG_PATHS {
            router = router.route(path, Method::GET, config.clone());
        }

        // What is held against the request's own Edge Cookie. This adapter
        // keeps no identity graph, so the page says nothing is held.
        let data = make_handler(Arc::clone(&state), |_s, services, req| async move {
            handle_data(None, None, services.public_prefix(), &req)
        });
        router = router.route(DATA_PAGE_PATH, Method::GET, data);

        for method in publisher_fallback_methods() {
            router = router.route("/", method.clone(), fallback.clone());
            router = router.route("/{*rest}", method, fallback.clone());
        }

        router.build()
    }
}

#[cfg(test)]
mod request_cycle_tests;

#[cfg(test)]
mod tests {
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;

    use super::*;

    /// Settings selecting a vendor Edge Cookie module this adapter does not
    /// inject, with the `[ec.acme]` block that module's settings live in.
    /// `acme` is a fictional vendor key.
    const UNINJECTED_MODULE_TOML: &str = r#"
        [source]
        module = "origin"

        [source.origin]
        url = "https://origin.test-publisher.example.com"

        [publisher]
        domain = "test-publisher.example.com"
        cookie_domain = ".test-publisher.example.com"
        proxy_secret = "unit-test-proxy-secret"

        [ec]
        module = "acme"

        [ec.acme]
        endpoint = "https://ec.acme.example.com"

        # An Edge Cookie module is configured, so single-jurisdiction
        # operation is acknowledged because no geo module is selected.
        [geo]
        assume_single_jurisdiction = true
    "#;

    /// The per-request Edge Cookie read must return its error rather than a
    /// default context.
    ///
    /// Continuing with `EcContext::default()` would serve every request with
    /// no identity when the selected module cannot be built. The call sites
    /// propagate the error to `http_error`, matching the Fastly adapter. The
    /// settings are parsed directly, bypassing the composition root's startup
    /// check, so the per-request behavior can be exercised with a selection
    /// the adapter cannot supply.
    #[tokio::test]
    async fn build_ec_context_fails_when_the_selected_module_is_unavailable() {
        let settings = Settings::from_toml(UNINJECTED_MODULE_TOML)
            .expect("should parse settings selecting an uninjected module");
        let req = request_builder()
            .method("POST")
            .uri("https://test-publisher.example.com/auction")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        let ctx = RequestContext::new(req, PathParams::default());
        // No resolved module is threaded here, so the request path resolves
        // the selection itself, which is what an embedder driving core
        // directly does and where the loud failure has to stay.
        let services = build_runtime_services(
            &ctx,
            &settings,
            &Arc::default(),
            &trusted_server_core::permissions::PermissionPolicy::sample(),
        );
        let req = ctx.into_request();

        let error = build_ec_context(&settings, &services, &req)
            .await
            .expect_err("an unavailable Edge Cookie module must fail the request");

        assert!(
            error.to_string().contains("acme"),
            "the error should name the selected module, got: {error}"
        );
    }

    fn aps_profile_settings() -> Settings {
        let mut settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "fictional-proxy-secret"

                [ec]
                passphrase = "fictional-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse startup test settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
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
        settings
    }

    /// Two ordinary `OpenRTB` demand sources, which is the fanout Cloudflare
    /// refuses because its HTTP client runs one request at a time.
    fn two_source_demand() -> trusted_server_core::provider_table::ProviderList {
        let names = ["provider_a", "provider_b"];
        trusted_server_core::provider_table::ProviderList::new(
            names.iter().map(|name| (*name).to_string()).collect(),
            names
                .iter()
                .map(|name| {
                    (
                        (*name).to_string(),
                        serde_json::Map::from_iter([
                            (
                                "implementation".to_string(),
                                serde_json::json!("auction-protocol.openrtb"),
                            ),
                            (
                                "endpoint".to_string(),
                                serde_json::json!(format!("https://{name}.example/openrtb")),
                            ),
                            ("routing".to_string(), serde_json::json!("all_eligible")),
                        ]),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn each_request_is_given_the_policy_its_domain_loaded() {
        let policy = trusted_server_core::test_support::policies::open();
        let state = build_state_with_settings(
            aps_profile_settings().with_permission_policy(Arc::clone(&policy)),
        )
        .expect("should build the state");
        let req = request_builder()
            .method("GET")
            .uri("https://publisher.example/article")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");

        let services = state.services_for_request(&RequestContext::new(req, PathParams::default()));

        assert!(
            Arc::ptr_eq(services.permission_policy(), &policy),
            "the request should decide under its domain's policy, not the sample"
        );
    }

    #[test]
    fn startup_registers_aps_renderer_route() {
        let state = build_state_with_settings(aps_profile_settings())
            .expect("Cloudflare startup should register APS renderer");
        assert!(
            state.registry.has_route(
                &edgezero_core::http::Method::GET,
                "/integrations/aps/renderer"
            ),
            "Cloudflare startup registry should expose the APS renderer"
        );
    }

    /// Every request's services carry the prefixes of the publisher its
    /// settings describe, which start every shared store key and every name
    /// the browser holds.
    #[test]
    fn a_requests_services_carry_the_publishers_prefixes() {
        let settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "www.publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "fictional-proxy-secret-32-bytes-ok"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse the settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        let state = build_state_with_settings(settings).expect("should build the state");
        let request = request_builder()
            .method("GET")
            .uri("https://www.publisher.example/")
            .body(edgezero_core::body::Body::empty())
            .expect("should build the request");
        let ctx = RequestContext::new(request, PathParams::default());

        let services = state.services_for_request(&ctx);

        assert_eq!(
            services.server_key_prefix().as_str(),
            "ts.publisher.example"
        );
        assert_eq!(services.public_prefix().as_str(), "ts");
    }

    #[test]
    fn disabled_startup_accepts_dormant_multi_provider_auction_plan() {
        let mut settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "fictional-proxy-secret"

                [ec]
                passphrase = "fictional-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse startup test settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        settings.auction.enabled = false;
        settings.demand = two_source_demand();

        build_state_with_settings(settings)
            .expect("disabled Cloudflare auction should accept dormant fanout");
    }

    #[test]
    fn startup_rejects_multi_provider_auction_plan() {
        let mut settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "fictional-proxy-secret"

                [ec]
                passphrase = "fictional-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse startup test settings")
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        settings.auction.enabled = true;
        settings.demand = two_source_demand();

        let error = match build_state_with_settings(settings) {
            Ok(_) => panic!("Cloudflare startup should reject multi-provider fanout"),
            Err(error) => error,
        };
        assert!(
            format!("{error:?}").contains("concurrent provider fanout"),
            "should identify unsupported fanout: {error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_hook_router_answers_every_route_with_the_startup_error() {
        // A Worker builds its application with `build_app`, which reads the
        // settings it awaits, so the synchronous hook has none to serve.
        let router = TrustedServerApp::routes();

        for method in publisher_fallback_methods() {
            for path in ["/", "/article"] {
                let req = edgezero_core::http::request_builder()
                    .method(method.clone())
                    .uri(format!("https://publisher.example{path}"))
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build request");
                let resp = router.oneshot(req).await.expect("should route the request");
                assert_eq!(
                    resp.status().as_u16(),
                    500,
                    "{method} {path} should answer the configuration error's 500"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_startup_router_answers_500_to_a_policy_load_error() {
        use trusted_server_core::test_support::policy_failure;

        let error = policy_failure::load_error();
        assert!(
            format!("{error:?}").contains(&format!("`{}`", policy_failure::POLICY_NAME)),
            "the startup error should name the policy: {error:?}"
        );

        let router = startup_error_router(&error);
        for method in publisher_fallback_methods() {
            for path in ["/", "/article"] {
                let req = edgezero_core::http::request_builder()
                    .method(method.clone())
                    .uri(path)
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build request");
                let resp = router.oneshot(req).await.expect("should route the request");
                assert_eq!(
                    resp.status().as_u16(),
                    500,
                    "{method} {path} should answer 500"
                );
            }
        }
    }

    /// `[analytics]` selecting a module still loads on this adapter, which
    /// sends no auction telemetry, and startup says so.
    #[test]
    fn startup_logs_that_this_adapter_sends_no_auction_telemetry() {
        let settings = Settings::from_toml(&format!(
            "{}\n{}",
            trusted_server_core::test_support::tests::crate_test_settings_str(),
            trusted_server_core::test_support::template::documented_block("[analytics]")
        ))
        .expect("should load settings selecting the documented analytics module")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());

        build_state_with_settings(settings).expect("should accept a selected analytics module");

        assert!(
            UNSENT_ANALYTICS_LOGGED.load(std::sync::atomic::Ordering::Relaxed),
            "startup should log that this adapter sends no auction telemetry"
        );
    }

    /// A response core makes private loses the policy core wrote in the
    /// Workers header and carries the private policy, and keeps that header
    /// where the origin sent it, which is a fetch middleware's to remove. The
    /// adapter declares its own platform's header and no other.
    #[test]
    fn a_private_response_follows_its_policy_and_keeps_the_origins_header() {
        let settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "www.publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "fictional-proxy-secret-32-bytes-ok"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse the settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
        build_state_with_settings(settings).expect("should build the state");
        assert_eq!(
            trusted_server_core::platform::platform_cache_headers(),
            vec![CDN_CACHE_CONTROL_HEADER],
            "should declare the Workers header and no other platform's"
        );
        let mut written = edgezero_core::http::response_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build the response");
        trusted_server_core::cache_policy::CachePolicy::public_immutable(
            std::time::Duration::from_secs(600),
        )
        .apply_to_response(&mut written, EDGE_CACHE_HEADER);
        assert!(written.headers().contains_key(CDN_CACHE_CONTROL_HEADER));
        trusted_server_core::response_privacy::enforce_private_no_store(&mut written);
        assert_eq!(
            written
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "should carry the private policy"
        );
        assert!(
            !written.headers().contains_key(CDN_CACHE_CONTROL_HEADER),
            "should remove the policy core wrote"
        );

        let mut sent = edgezero_core::http::response_builder()
            .header(CDN_CACHE_CONTROL_HEADER, "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build the response");
        trusted_server_core::response_privacy::enforce_private_no_store(&mut sent);
        assert_eq!(
            sent.headers()
                .get(CDN_CACHE_CONTROL_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("max-age=600"),
            "should keep the header the origin sent"
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
                "51degrees",
                "analytics.tinybird",
                "auction-protocol.openrtb",
                "auction.prebid-server",
                "auction.aps",
                "ad-server.iabtl",
                "shared.aws",
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
