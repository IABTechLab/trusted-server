use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Method, Request, Response, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
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
use trusted_server_core::ec::module::{EdgeCookieModule, build_reusable_module};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::inspect::config::{CONFIG_PATHS, handle_config};
use trusted_server_core::inspect::data::{DATA_PAGE_PATH, handle_data};
use trusted_server_core::inspect::permissions::{PERMISSIONS_PATHS, handle_permissions};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistry, ProxyDispatchInput,
};
use trusted_server_core::platform::RuntimeServices;
#[cfg(target_arch = "wasm32")]
use trusted_server_core::platform::StoreName;
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
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
        &trusted_server_modules::builders(),
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
/// a stock build ships with the externally supplied builders in
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
    // The modules a stock build ships come first and the deployment's own
    // follow, which is the order their hooks run in.
    let integrations = trusted_server_modules::builders_with(integrations);
    let integrations = integrations.as_slice();

    // The plan is compiled with the integrations this adapter was given, so an
    // `[ad-server]` or `[demand]` name one of their builders supplies resolves
    // here. Compiling without them would drop the implementation and report the
    // name as one no builder registers.
    let plan = Arc::new(compile_auction_plan_with(&settings, integrations)?);
    plan.validate_for_target(trusted_server_core::platform::AuctionTargetId::Cloudflare)?;
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan))?;
    let registry = IntegrationRegistry::with_plan_and_registrations(&settings, plan, integrations)?;

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
    let permission_policy = settings.required_permission_policy()?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        ec_module,
        permission_signal_modules,
        permission_policy,
        services,
    }))
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

impl AppState {
    /// Build per-request [`RuntimeServices`], carrying the Edge Cookie module
    /// the composition root already resolved and applying the module-supplied
    /// geo, Edge Cookie and device modules selected by `[geo]`, `[ec]` and
    /// `[device] module`. The services also carry the permission policy the
    /// domain's settings loaded, whichever services the request is given.
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
        services
    }
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Builds the geo-aware [`EcContext`] for consent-gated endpoints (`/auction`,
/// `/_ts/page-bids`, and the publisher fallback).
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
/// this request, or when the request's `Cookie` header is not valid UTF-8.
async fn build_ec_context(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcContext, Report<TrustedServerError>> {
    EcContext::read_from_request_resolving_geo(settings, req, services).await
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

impl Hooks for TrustedServerApp {
    fn name() -> &'static str {
        "TrustedServer"
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

        // Shared fallback dispatch: routes to tsjs (GET only), integration proxy, or publisher.
        async fn dispatch(
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
            let path = req.uri().path().to_owned();
            let method = req.method().clone();
            // tsjs assets are served for GET only, matching the Axum/Fastly adapters.
            let allow_tsjs = method == Method::GET;

            let result = if allow_tsjs && path.starts_with("/static/tsjs=") {
                handle_tsjs_dynamic(
                    &req,
                    &state.registry,
                    EdgeCacheHeader::CloudflareCdnCacheControl,
                )
            } else if state.registry.has_route(&method, &path) {
                // A module route is handed what was resolved for its request,
                // the permissions among them, as the publisher path is.
                let mut ec_context = match build_ec_context(&state.settings, &services, &req).await
                {
                    Ok(context) => context,
                    Err(report) => return Ok(http_error(&report)),
                };
                state
                    .registry
                    .handle_proxy(ProxyDispatchInput {
                        method: &method,
                        path: &path,
                        settings: &state.settings,
                        kv: None,
                        ec_context: &mut ec_context,
                        services: &services,
                        req,
                    })
                    .await
                    .unwrap_or_else(|| {
                        Err(Report::new(TrustedServerError::BadRequest {
                            message: format!("Unknown integration route: {path}"),
                        }))
                    })
            } else {
                // Identity could not be established (for example the selected
                // Edge Cookie module is unavailable). Answer with an error
                // rather than serving the page with no identity.
                let mut ec_context = match build_ec_context(&state.settings, &services, &req).await
                {
                    Ok(context) => context,
                    Err(report) => return Ok(http_error(&report)),
                };
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
                    &services,
                    None,
                    &mut ec_context,
                    auction,
                    req,
                    EdgeCacheHeader::CloudflareCdnCacheControl,
                )
                .await
                {
                    Ok(pr) => {
                        resolve_publisher_response(
                            pr,
                            &method,
                            &state.settings,
                            &state.registry,
                            &state.orchestrator,
                            &services,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                }
            };

            Ok(result.unwrap_or_else(|e| http_error(&e)))
        }

        let fallback = {
            let s = Arc::clone(&state);
            move |ctx: RequestContext| {
                let s = Arc::clone(&s);
                dispatch(s, ctx)
            }
        };

        let mut router = RouterService::builder()
            // Outermost middleware: strips the configured trusted-client-IP
            // headers before anything else sees the request. Must stay first —
            // any middleware registered ahead of it would observe the
            // shared-secret authentication header.
            .middleware(SanitizeRequestMiddleware::new(Arc::clone(&state.settings)))
            .middleware(FinalizeResponseMiddleware::new(Arc::clone(&state.settings)))
            .get(
                "/.well-known/trusted-server.json",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_trusted_server_discovery(&s.settings, &services, req)
                }),
            )
            .post(
                "/verify-signature",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_verify_signature(&s.settings, &services, req)
                }),
            )
            .post(
                "/auction",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    // Build the geo-aware EC context so the auction consent gate
                    // sees the caller's jurisdiction — `EcContext::default()`
                    // fails it closed for consented users.
                    let mut ec_context = build_ec_context(&s.settings, &services, &req).await?;
                    handle_auction(
                        &s.settings,
                        &s.orchestrator,
                        None,
                        None,
                        &mut ec_context,
                        &services,
                        req,
                    )
                    .await
                }),
            )
            .get(
                "/first-party/proxy",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy(&s.settings, &services, req).await
                }),
            )
            .get(
                "/first-party/click",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_click(&s.settings, &services, req).await
                }),
            )
            .get(
                "/first-party/sign",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_sign(&s.settings, &services, req).await
                }),
            )
            .post(
                "/first-party/sign",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_sign(&s.settings, &services, req).await
                }),
            )
            // GET serves the click guard's navigation fallback: the creative
            // iframe is an opaque origin (sandbox without `allow-same-origin`),
            // so its JSON POST is blocked by CORS and the guard navigates here
            // for a 302 instead.
            .get(
                "/first-party/proxy-rebuild",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_rebuild(&s.settings, &services, req).await
                }),
            )
            .post(
                "/first-party/proxy-rebuild",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    handle_first_party_proxy_rebuild(&s.settings, &services, req).await
                }),
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
        let page_bids = make_handler(Arc::clone(&state), |s, services, req| async move {
            let mut ec_context = build_ec_context(&s.settings, &services, &req).await?;
            let auction = AuctionDispatch {
                orchestrator: &s.orchestrator,
                slots: s.settings.creative_opportunity_slots(),
                registry: None,
            };
            handle_page_bids(&s.settings, &services, None, auction, &mut ec_context, req).await
        });
        let page_bids_preflight =
            make_handler(Arc::clone(&state), |_s, _services, _req| async move {
                Ok(page_bids_preflight_denied())
            });
        for path in [PAGE_BIDS_PATH, PAGE_BIDS_LEGACY_PATH] {
            router = router.route(path, Method::GET, page_bids.clone());
            router = router.route(path, Method::OPTIONS, page_bids_preflight.clone());
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
        let data = make_handler(Arc::clone(&state), |_s, _services, req| async move {
            handle_data(None, None, &req)
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
mod tests {
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;

    use super::*;

    /// Settings selecting a vendor Edge Cookie module this adapter does not
    /// inject, with the `[ec.acme]` block that module's settings live in.
    /// `acme` is a fictional vendor key.
    const UNINJECTED_MODULE_TOML: &str = r#"
        [publisher]
        domain = "test-publisher.example.com"
        cookie_domain = ".test-publisher.example.com"
        origin_url = "https://origin.test-publisher.example.com"
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
                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                origin_url = "https://origin.publisher.example"
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

    #[test]
    fn disabled_startup_accepts_dormant_multi_provider_auction_plan() {
        let mut settings = Settings::from_toml(
            r#"
                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                origin_url = "https://origin.publisher.example"
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
                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                origin_url = "https://origin.publisher.example"
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
}
