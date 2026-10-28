use core::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HandlerFuture, HeaderValue, Method, Request, Response, StatusCode, header,
};
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
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::module::ensure_module_available;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::inspect::config::{CONFIG_JSON_PATH, CONFIG_PAGE_PATH, handle_config};
use trusted_server_core::inspect::data::{DATA_PAGE_PATH, handle_data};
use trusted_server_core::inspect::permissions::{
    PERMISSIONS_JSON_PATH, PERMISSIONS_PAGE_PATH, handle_permissions,
};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistry, ProxyDispatchInput,
};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AppContext, AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH,
    buffer_publisher_response_async, handle_page_bids, handle_publisher_request,
    handle_tsjs_dynamic, page_bids_preflight_denied,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::{
    default_config_key, default_config_store_name, get_settings_from_config_store_with,
};

use trusted_server_core::platform::{PlatformIdentity, PolicyOrigin, RuntimeServices};

use crate::middleware::{FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::{
    AxumPlatformConfigStore, AxumPlatformSecretStore, AxumPolicyFolder, build_runtime_services,
};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
///
/// Built once here, unlike on the other three adapters, because `main`
/// calls [`TrustedServerApp::routes`] before serving, so the router holds
/// one state for the process's lifetime.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
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

/// Set once startup has logged that this adapter sends no auction telemetry.
static UNSENT_ANALYTICS_LOGGED: AtomicBool = AtomicBool::new(false);

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

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let store_name = default_config_store_name();
    let config_key = default_config_key();
    // The settings are validated as they load, against the same builders the
    // state is built with.
    let settings = get_settings_from_config_store_with(
        &AxumPlatformConfigStore,
        &AxumPlatformSecretStore,
        &store_name,
        &config_key,
        &trusted_server_core::settings_data::default_secret_store_name(),
        PolicyOrigin::Source(&AxumPolicyFolder::from_env()),
        &shipped_module_builders(),
    )?;
    build_state_with_settings(settings)
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

/// The header core writes a shared cache policy in on this adapter.
const EDGE_CACHE_HEADER: EdgeCacheHeader = EdgeCacheHeader::SMaxageFallback;

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

fn build_state_with_registrations_and_services(
    settings: Settings,
    integrations: &[IntegrationBuilder],
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
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
        .with_platform(PlatformIdentity::named("axum"));

    // Composition root: reject a module selection this adapter can never
    // supply, once, before any request is served. The registry is built first
    // because a module can supply the Edge Cookie module the selector names,
    // and `services_for_request` hands that module to every request, so the
    // check is given the module the request path sees. A caller supplying its
    // own `RuntimeServices` may have resolved one already, and that one comes
    // first because it is what `EcContext` will see.
    //
    // This adapter checks rather than keeps what the check resolved, unlike the
    // Fastly, Cloudflare and Spin adapters, because it is a long-lived process
    // whose application state is built once at start-up while theirs is rebuilt
    // for every request, so `EcContext` resolves the selection on every
    // request. It supplies no host signals, so the host-signals argument is
    // `None`.
    ensure_module_available(
        &settings.ec,
        None,
        services
            .as_ref()
            .and_then(RuntimeServices::resolved_ec_module)
            .or_else(|| registry.ec_module()),
    )?;
    // Services a caller supplies carry a geo lookup of their own for
    // `platform` to select, and this host's own resolves nothing.
    if services.is_none() {
        refuse_platform_geo(&settings)?;
    }
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
        "Axum",
        &UNSENT_ANALYTICS_LOGGED,
    );
    warn_when_certificate_check_is_off(&settings);
    let permission_policy = settings.required_permission_policy()?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        permission_signal_modules,
        permission_policy,
        services,
        loading_caches,
    }))
}

/// Warns that `proxy.certificate_check = false` is not honored here.
///
/// This adapter's HTTP client always verifies an HTTPS origin's certificate,
/// so a deployment that turned the check off for a self-signed origin would
/// otherwise see only failed requests. The state is built once for the
/// process, so this warns once, at startup.
fn warn_when_certificate_check_is_off(settings: &Settings) {
    if !settings.proxy.certificate_check {
        log::warn!(
            "proxy.certificate_check is false, but the Axum adapter does not honor it \
             and still verifies every HTTPS origin's certificate"
        );
    }
}

/// Refuses `[geo] module = "platform"`, because this host has no geo lookup.
///
/// The selection would locate no request and still count as a geo module,
/// so a deployment running an Edge Cookie module would never be asked to
/// acknowledge single-jurisdiction operation.
fn refuse_platform_geo(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    if settings.geo.module.as_deref() != Some("platform") {
        return Ok(());
    }
    Err(Report::new(TrustedServerError::Configuration {
        message: "`[geo] module = \"platform\"` selects the host's own geo lookup, and the \
                  Axum dev server has no geo lookup, so no request would be located. Select \
                  a geo module a linked crate offers, or leave `[geo] module` unset, which \
                  needs `[geo] assume_single_jurisdiction = true` when an Edge Cookie module \
                  is configured"
            .to_owned(),
    }))
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

impl AppState {
    /// Build per-request [`RuntimeServices`], applying the module-supplied geo,
    /// Edge Cookie and device modules selected by `[geo]`, `[ec]` and
    /// `[device] module` and the asset cache module `[asset-cache]` selects,
    /// and carrying the permission policy the domain's settings loaded
    /// whichever services the request is given.
    ///
    /// Unset and `"none"` both resolve nothing for geo, so no client IP reaches
    /// a host geo service. `"platform"` opts in to this adapter's own lookup,
    /// and any other key names an integration module that declares a geo
    /// module. Identity and device are applied the same way when a module
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
    let services = state.services_for_request(&ctx);
    let mut req = ctx.into_request();
    // The single preparation point for the adapter. Every route in the table
    // except `/health` is registered to `named_route_handler` or
    // `fallback_handler`, and both wrap this function, so each request has its
    // modules' preparers run exactly once and always before routing.
    if let Err(error) = state.registry.prepare_request(&state.settings, &mut req) {
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
/// The geo lookup runs inside
/// [`EcContext::read_from_request_resolving_geo`], so every adapter reports the
/// same distinction: no location falls back to the top of the
/// `permissions.yaml` rules tree, while a failed lookup resolves every
/// permission at the requires-signal floor and is logged at error level.
/// The platform geo is a no-op on the local Axum dev server, so a request there
/// resolves at that top node unless it carries a signal.
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
/// this request, or when the request's `Cookie` header is not valid UTF-8.
async fn build_ec_context(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcContext, Report<TrustedServerError>> {
    EcContext::read_from_request_resolving_geo(&state.settings, req, services).await
}

// ---------------------------------------------------------------------------
// Fallback dispatcher (tsjs / integration proxy / publisher)
// ---------------------------------------------------------------------------

/// Dispatches the fallback routes: tsjs assets, integration proxy routes, and
/// the publisher origin.
///
/// The request arrives already prepared, and is not to a closed path. Every
/// route that reaches here is registered to [`fallback_handler`], which
/// answers a closed path itself and runs any other request through
/// [`execute_handler`], and [`execute_handler`] calls
/// `registry.prepare_request` before it calls this function, so there is
/// nothing left to prepare here.
async fn dispatch_fallback(
    state: &AppState,
    services: &RuntimeServices,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if method == Method::GET && path.starts_with("/static/tsjs=") {
        return handle_tsjs_dynamic(&req, &state.registry, EDGE_CACHE_HEADER);
    }

    if state.registry.has_route(&method, &path) {
        // A module route is handed what was resolved for its request, the
        // permissions among them, as the publisher path is.
        let mut ec_context = build_ec_context(state, services, &req).await?;
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
    let mut ec_context = build_ec_context(state, services, &req).await?;
    let auction = AuctionDispatch {
        orchestrator: &state.orchestrator,
        slots: state.settings.creative_opportunity_slots(),
        registry: None,
    };
    let publisher_response = handle_publisher_request(
        AppContext {
            settings: &state.settings,
            integration_registry: &state.registry,
        },
        services,
        None,
        &mut ec_context,
        auction,
        req,
        EDGE_CACHE_HEADER,
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

fn fallback_handler(
    state: Arc<AppState>,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| -> HandlerFuture {
        // A closed path is answered before the modules' preparers run, as the
        // other adapters answer it, so no preparer sees or answers it.
        if let Some(response) = closed_path_response(ctx.request()) {
            return Box::pin(async move { Ok::<_, EdgeError>(response) });
        }
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
    Permissions,
    Config,
    Data,
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

fn named_routes() -> [NamedRoute; 14] {
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
        // What is held against the request's own Edge Cookie. This adapter
        // keeps no identity graph, so the page says nothing is held.
        NamedRoute {
            path: DATA_PAGE_PATH,
            primary_methods: &[Method::GET],
            handler: NamedRouteHandler::Data,
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
                        handle_verify_signature(&state.settings, &services, req)
                    }
                    NamedRouteHandler::Permissions => {
                        handle_permissions(&state.settings, &services, &req).await
                    }
                    NamedRouteHandler::Config => Ok(handle_config(&state.settings, &req)),
                    NamedRouteHandler::Data => {
                        handle_data(None, None, services.public_prefix(), &req)
                    }
                    NamedRouteHandler::Auction => {
                        // Build the geo-aware EC context so the auction consent
                        // gate sees the caller's jurisdiction — `EcContext::default()`
                        // fails it closed for consented users.
                        let mut ec_context = build_ec_context(&state, &services, &req).await?;
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
                            let mut ec_context = build_ec_context(&state, &services, &req).await?;
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
    /// settings they build, where the process loads its settings from the
    /// config store when it starts.
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

    /// Build the full router as
    /// [`TrustedServerApp::routes_with_registrations`] does, with every
    /// request handed a clone of `services`, as
    /// [`TrustedServerApp::routes_with_settings_and_services`] hands them.
    ///
    /// # Errors
    ///
    /// Returns the errors [`TrustedServerApp::routes_with_registrations`]
    /// returns.
    pub fn routes_with_registrations_and_services(
        settings: Settings,
        integrations: &[IntegrationBuilder],
        services: RuntimeServices,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state =
            build_state_with_registrations_and_services(settings, integrations, Some(services))?;
        Ok(build_router(&state))
    }
}

fn build_router(state: &Arc<AppState>) -> RouterService {
    let fallback = fallback_handler(Arc::clone(state));

    let mut router = RouterService::builder()
        // Outermost middleware: strips the configured trusted-client-IP
        // headers before anything else sees the request. Must stay first —
        // any middleware registered ahead of it would observe the
        // shared-secret authentication header.
        .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))
        .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)));

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

#[cfg(test)]
mod tests {
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;
    // These fixtures supply no integration builders, so they compile the plan
    // without them rather than through the module-aware compiler the adapter
    // itself uses.
    use trusted_server_core::auction::compile_auction_plan;

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

    /// What `run` returns, and each line it logs on this thread at warning
    /// level or above, as the level and then the message.
    fn warnings_logged_by<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
        use std::cell::RefCell;
        use std::sync::Once;

        thread_local! {
            static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
        }

        struct ThreadLogger;

        impl log::Log for ThreadLogger {
            fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
                CAPTURED.with(|captured| captured.borrow().is_some())
            }

            fn log(&self, record: &log::Record<'_>) {
                CAPTURED.with(|captured| {
                    if let Some(lines) = captured.borrow_mut().as_mut() {
                        lines.push(format!("{} {}", record.level(), record.args()));
                    }
                });
            }

            fn flush(&self) {}
        }

        static LOGGER: ThreadLogger = ThreadLogger;
        static INSTALL: Once = Once::new();

        INSTALL.call_once(|| {
            log::set_logger(&LOGGER).expect("should be the only logger these tests install");
            log::set_max_level(log::LevelFilter::Warn);
        });
        CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
        let output = run();
        let lines = CAPTURED.with(|captured| captured.borrow_mut().take().unwrap_or_default());
        (output, lines)
    }

    /// This adapter's HTTP client always verifies an origin's certificate, so
    /// startup with `proxy.certificate_check = false` warns once that the
    /// setting is not honored, and startup with the check on warns nothing.
    #[test]
    fn startup_warns_once_that_turning_the_certificate_check_off_is_not_honored() {
        let warnings_naming_the_check = |certificate_check: bool| {
            let settings = Settings::from_toml(&format!(
                r#"
                    [source]
                    module = "origin"

                    [source.origin]
                    url = "https://origin.publisher.example"

                    [publisher]
                    domain = "www.publisher.example"
                    cookie_domain = ".publisher.example"
                    proxy_secret = "fictional-proxy-secret-32-bytes-ok"

                    [proxy]
                    certificate_check = {certificate_check}

                    [geo]
                    assume_single_jurisdiction = true
                "#
            ))
            .expect("should parse the settings")
            // A loaded domain carries its policy.
            .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());
            let (state, lines) = warnings_logged_by(|| build_state_with_settings(settings));
            state.expect("should build the state");
            lines
                .into_iter()
                .filter(|line| line.contains("proxy.certificate_check"))
                .collect::<Vec<_>>()
        };

        let off = warnings_naming_the_check(false);
        assert_eq!(off.len(), 1, "should warn once: {off:?}");
        assert!(
            off[0].starts_with("WARN ") && off[0].contains("still verifies"),
            "should say certificates are still verified: {off:?}"
        );
        assert!(
            warnings_naming_the_check(true).is_empty(),
            "should not warn while the check is on"
        );
    }

    /// Builds application state directly, bypassing the composition root's
    /// startup check, so the per-request behavior can be exercised with a
    /// selection the adapter cannot supply.
    fn state_with_uninjected_module() -> AppState {
        let settings = Settings::from_toml(UNINJECTED_MODULE_TOML)
            .expect("should parse settings selecting an uninjected module");
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));
        let orchestrator =
            build_orchestrator_with_plan(Arc::clone(&plan)).expect("should build orchestrator");
        let registry =
            IntegrationRegistry::with_plan(&settings, plan).expect("should build registry");
        AppState {
            settings: Arc::new(settings),
            orchestrator: Arc::new(orchestrator),
            registry: Arc::new(registry),
            // These tests exercise the Edge Cookie module path, and a
            // request with no signal module resolves at the place baseline.
            permission_signal_modules: Arc::default(),
            permission_policy: trusted_server_core::permissions::PermissionPolicy::sample(),
            // This test drives the per-request path, which builds its services
            // from the request context.
            services: None,
            loading_caches: Arc::default(),
        }
    }

    /// The per-request Edge Cookie read must return its error rather than a
    /// default context.
    ///
    /// Continuing with `EcContext::default()` would serve every request with
    /// no identity when the selected module cannot be built. The call sites
    /// propagate the error to `http_error`, matching the Fastly adapter.
    #[tokio::test]
    async fn build_ec_context_fails_when_the_selected_module_is_unavailable() {
        let state = state_with_uninjected_module();
        let req = request_builder()
            .method("POST")
            .uri("https://test-publisher.example.com/auction")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        let ctx = RequestContext::new(req, PathParams::default());
        let services = build_runtime_services(
            &ctx,
            &state.settings,
            &state.permission_signal_modules,
            &state.permission_policy,
        );
        let req = ctx.into_request();

        let error = build_ec_context(&state, &services, &req)
            .await
            .expect_err("an unavailable Edge Cookie module must fail the request");

        assert!(
            error.to_string().contains("acme"),
            "the error should name the selected module, got: {error}"
        );
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

    /// A response core makes private carries the private policy, with no
    /// `s-maxage`, and keeps a platform's header the origin sent, which is a
    /// fetch middleware's to remove. The adapter declares no platform's header.
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
        assert!(
            trusted_server_core::platform::platform_cache_headers().is_empty(),
            "should declare no platform's header"
        );
        let mut response = edgezero_core::http::response_builder()
            .header("example-platform-cache-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build the response");
        trusted_server_core::cache_policy::CachePolicy::public_immutable(
            std::time::Duration::from_secs(600),
        )
        .apply_to_response(&mut response, EDGE_CACHE_HEADER);

        trusted_server_core::response_privacy::enforce_private_no_store(&mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "should carry the private policy"
        );
        assert_eq!(
            response
                .headers()
                .get("example-platform-cache-control")
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

    mod startup_policy {
        use std::path::Path;

        use edgezero_core::blob_envelope::BlobEnvelope;
        use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;
        use trusted_server_core::permissions::{SAMPLE_POLICY_TEXT, policy_sha256};
        use trusted_server_core::settings::PermissionsConfig;

        use super::*;
        use crate::platform::{PERMISSIONS_DIR_ENV_VAR, config_env_var};

        const PROXY_SECRET_VAR: &str =
            "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET";

        /// Settings whose secrets are the key names the tests' variables
        /// resolve.
        fn startup_settings() -> Settings {
            Settings::from_toml(
                r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.publisher.example"

                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                proxy_secret = "publisher_proxy_secret"
                "#,
            )
            .expect("should parse the startup settings")
        }

        /// The sample with a comment naming `test`, so its digest is one no
        /// earlier load in the process holds and the folder is read.
        fn file_only_this_test_pins(test: &str) -> String {
            format!("{SAMPLE_POLICY_TEXT}# pinned only by {test}\n")
        }

        /// The blob the Axum push writes for settings naming `standard`,
        /// pinned to `policy_text` and carrying no text of its own.
        fn blob_naming_standard(policy_text: &str) -> String {
            let mut settings = startup_settings();
            settings.permissions = PermissionsConfig {
                policy: Some("standard".to_owned()),
                sha256: Some(policy_sha256(policy_text)),
                text: None,
            };
            let data = serde_json::to_value(&settings).expect("should serialize settings");
            serde_json::to_string(&BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned()))
                .expect("should serialize the envelope")
        }

        /// Builds the state as the process starts, with `blob` as its settings
        /// and `folder` as its policy folder.
        fn start(
            blob: &str,
            folder: Option<&Path>,
        ) -> Result<Arc<AppState>, Report<TrustedServerError>> {
            let config_var = config_env_var(DEFAULT_CONFIG_STORE_ID, DEFAULT_CONFIG_STORE_ID);
            temp_env::with_vars(
                [
                    (config_var.as_str(), Some(blob)),
                    (
                        PROXY_SECRET_VAR,
                        Some("a-proxy-secret-for-the-startup-tests-32-bytes"),
                    ),
                    (PERMISSIONS_DIR_ENV_VAR, folder.and_then(Path::to_str)),
                    (
                        "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME",
                        None,
                    ),
                    ("EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY", None),
                    (
                        "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME",
                        None,
                    ),
                ],
                build_state,
            )
        }

        #[test]
        fn each_request_is_given_the_policy_its_domain_loaded() {
            let policy = trusted_server_core::test_support::policies::open();
            let state = build_state_with_settings(
                startup_settings().with_permission_policy(Arc::clone(&policy)),
            )
            .expect("should build the state");
            let request = request_builder()
                .method("GET")
                .uri("https://publisher.example/article")
                .body(edgezero_core::body::Body::empty())
                .expect("should build the request");

            let services =
                state.services_for_request(&RequestContext::new(request, PathParams::default()));

            assert!(
                Arc::ptr_eq(services.permission_policy(), &policy),
                "the request should decide under its domain's policy, not the sample"
            );
        }

        #[test]
        fn startup_reads_the_named_policy_file_from_the_policy_folder() {
            let pushed = file_only_this_test_pins("the folder read test");
            let folder = tempfile::tempdir().expect("should create a policy folder");
            std::fs::write(folder.path().join("standard.yaml"), &pushed)
                .expect("should write the policy file");

            let state = start(&blob_naming_standard(&pushed), Some(folder.path()))
                .expect("should load the policy file the settings name");

            let policy = state
                .settings
                .permission_policy()
                .expect("should keep the policy with the settings");
            assert_eq!(policy.name(), "standard");
            assert_eq!(policy.sha256(), policy_sha256(&pushed));
        }

        #[test]
        fn startup_without_the_pushed_policy_file_answers_500_on_every_route() {
            let pushed = file_only_this_test_pins("the startup refusal test");
            let blob = blob_naming_standard(&pushed);
            let empty = tempfile::tempdir().expect("should create an empty folder");
            let edited = tempfile::tempdir().expect("should create a policy folder");
            std::fs::write(
                edited.path().join("standard.yaml"),
                format!("{pushed}# edited after the push\n"),
            )
            .expect("should write an edited policy file");

            for (folder, reason) in [
                (None, PERMISSIONS_DIR_ENV_VAR),
                (Some(empty.path()), "standard.yaml"),
                (Some(edited.path()), "is not the file that was pushed"),
            ] {
                let Err(error) = start(&blob, folder) else {
                    panic!("the domain should not load from {folder:?}");
                };
                let message = format!("{error:?}");
                assert!(
                    message.contains("`standard`") && message.contains(reason),
                    "the logged reason should name the policy and say `{reason}`: {message}"
                );

                let router = startup_error_router(&error);
                for method in publisher_fallback_methods() {
                    for path in ["/", "/article"] {
                        let request = request_builder()
                            .method(method.clone())
                            .uri(path)
                            .body(edgezero_core::body::Body::empty())
                            .expect("should build the request");
                        let response = futures::executor::block_on(router.oneshot(request))
                            .expect("should route the request");
                        assert_eq!(
                            response.status(),
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "{method} {path} should answer 500"
                        );
                    }
                }
            }
        }
    }
}
