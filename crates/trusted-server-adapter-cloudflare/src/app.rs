use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Method, Request, Response, StatusCode, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{
    AuctionOrchestrator, AuctionProviderBuilder, build_orchestrator_with_providers,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
#[cfg(target_arch = "wasm32")]
use trusted_server_core::config_payload::settings_from_config_blob;
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::admin::{
    admin_ec_lookup_not_supported as core_admin_ec_lookup_not_supported,
    deny_admin_diagnostic_fallback, handle_admin_eids_lookup,
};
use trusted_server_core::ec::provider::{EdgeCookieProvider, build_reusable_provider};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistry, ProxyDispatchInput,
};
use trusted_server_core::platform::RuntimeServices;
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

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware, SanitizeRequestMiddleware};
use crate::platform::build_runtime_services;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
static CLOUDFLARE_CONFIG_JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(target_arch = "wasm32")]
pub fn set_cloudflare_config_json(value: String) {
    let _ = CLOUDFLARE_CONFIG_JSON.set(value);
}

/// Application state shared by everything that serves one request.
///
/// Not built once per Worker isolate, because
/// `edgezero_adapter_cloudflare::run_app` calls `build_app` inside the
/// per-request entry point, so this is rebuilt on every request.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    /// The Edge Cookie provider `[ec] provider` selects, resolved once here.
    ///
    /// This adapter runs a fresh instance per request, so application state and
    /// the request path used to resolve the same selection twice for every
    /// request, once to check it could be satisfied and once to use it.
    /// Resolving reads no request data, so the result is kept and handed to
    /// every request through
    /// [`RuntimeServices::resolved_ec_provider`](trusted_server_core::platform::RuntimeServices::resolved_ec_provider).
    /// `None` for a deployment that selects no provider, and for one whose
    /// provider must be resolved per request.
    resolved_ec_provider: Option<Arc<dyn EdgeCookieProvider>>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = load_startup_settings()?;
    build_state_with_settings(settings)
}

#[cfg(target_arch = "wasm32")]
fn load_startup_settings() -> Result<Settings, Report<TrustedServerError>> {
    settings_from_cloudflare_config_json()
}

#[cfg(not(target_arch = "wasm32"))]
fn load_startup_settings() -> Result<Settings, Report<TrustedServerError>> {
    Settings::from_toml(include_str!("../../../trusted-server.example.toml"))
}

#[cfg(target_arch = "wasm32")]
fn settings_from_cloudflare_config_json() -> Result<Settings, Report<TrustedServerError>> {
    let raw_config = CLOUDFLARE_CONFIG_JSON.get().ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "Cloudflare TRUSTED_SERVER_CONFIG is required".to_string(),
        })
        .attach("set TRUSTED_SERVER_CONFIG to JSON containing the app_config blob envelope")
    })?;
    let value: serde_json::Value = serde_json::from_str(raw_config).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: "invalid Cloudflare TRUSTED_SERVER_CONFIG JSON".to_string(),
        })
        .attach(format!("failed to parse TRUSTED_SERVER_CONFIG: {error}"))
    })?;
    let envelope = value
        .get("app_config")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "Cloudflare TRUSTED_SERVER_CONFIG missing app_config".to_string(),
            })
        })?;
    settings_from_config_blob(envelope)
}

/// Build the application state from explicit settings.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie provider cannot be built for
/// this adapter, or when the auction orchestrator or the integration registry
/// fail to initialize.
fn build_state_with_settings(
    settings: Settings,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    build_state_with_registrations(settings, &[], &[])
}

/// Build the application state from explicit settings, composing the built-in
/// integrations and auction providers with the externally supplied builders in
/// `integrations` and `auction_providers`.
///
/// A deployment that ships a vendor crate calls this to add that crate's
/// integration and auction provider builders without the adapter naming the
/// vendor.
///
/// # Errors
///
/// Returns an error when the selected Edge Cookie provider cannot be built for
/// this adapter, or when the auction orchestrator or the integration registry
/// fail to initialise, which includes two builders claiming the same
/// integration id or auction provider name.
pub fn build_state_with_registrations(
    settings: Settings,
    integrations: &[IntegrationBuilder],
    auction_providers: &[AuctionProviderBuilder],
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let orchestrator = build_orchestrator_with_providers(&settings, auction_providers)?;
    let registry = IntegrationRegistry::with_registrations(&settings, integrations)?;

    // Composition root: resolve the provider selection once, before any request
    // is served, so a selection this adapter can never supply fails here rather
    // than on the first request. Keeping what the resolution produced is what
    // stops the request path resolving the same settings again. The registry is
    // built first because a module can supply the vendor Edge Cookie provider
    // the selector names, and resolving without it would reject a selection
    // this deployment can in fact satisfy. This adapter supplies no host
    // signals, so that argument stays `None` until it does.
    let resolved_ec_provider = build_reusable_provider(&settings.ec, None, registry.ec_provider())?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        resolved_ec_provider,
    }))
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Build per-request [`RuntimeServices`], carrying the Edge Cookie provider the
/// composition root already resolved and applying the module-supplied geo
/// provider selected by `[geo] provider`.
///
/// No Edge Cookie provider is carried when the composition root found nothing
/// safe to keep, so the request path resolves the selection for itself.
///
/// For geo, unset and `"none"` both resolve nothing, so no client IP reaches a
/// host geo service. `"platform"` opts in to this adapter's own lookup, and any
/// other key names an integration module that declares a geo provider.
fn build_per_request_services(state: &AppState, ctx: &RequestContext) -> RuntimeServices {
    let mut services = build_runtime_services(ctx, &state.settings)
        .with_resolved_ec_provider(state.resolved_ec_provider.clone());
    if let Some(provider) = state.registry.geo_provider() {
        services = services.with_geo(provider);
    }
    if let Some(provider) = state.registry.ec_provider() {
        services = services.with_ec_provider(provider);
    }
    if let Some(provider) = state.registry.device_provider() {
        services = services.with_device_provider(provider);
    }
    services
}

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
/// Returns an error when the selected Edge Cookie provider cannot be built for
/// this request, or when the request's `Cookie` header is not valid UTF-8.
fn build_ec_context(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> Result<EcContext, Report<TrustedServerError>> {
    EcContext::read_from_request_resolving_geo(settings, req, services)
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
            let services = build_per_request_services(&s, &ctx);
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

fn admin_key_management_not_supported() -> Response {
    let body = edgezero_core::body::Body::from(
        "Admin key management is not supported on Cloudflare Workers.\n\
         Use the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n",
    );
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::NOT_IMPLEMENTED;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn admin_ec_lookup_not_supported() -> Response {
    core_admin_ec_lookup_not_supported()
}

/// Builds the local `404 Not Found` returned for legacy `/admin/keys/*`
/// aliases on the Cloudflare adapter.
///
/// These non-`/_ts` aliases are not matched by the `^/_ts/admin` basic-auth
/// handler, so they fail closed locally rather than fall through to the
/// publisher fallback — which would forward the caller's `Authorization` header
/// and key-management payload to the origin, leaking admin credentials.
fn legacy_admin_alias_denied() -> Response {
    let mut response = Response::new(edgezero_core::body::Body::from("Not found\n"));
    *response.status_mut() = edgezero_core::http::StatusCode::NOT_FOUND;
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
    /// Testing seam: cross-adapter parity tests use this to drive the router
    /// with known-good settings instead of the baked `get_settings()` result,
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

    /// Build the full application router from explicit settings, composing the
    /// built-in integrations and auction providers with the externally supplied
    /// builders in `integrations` and `auction_providers`.
    ///
    /// The route table is the one [`TrustedServerApp::routes_with_settings`]
    /// builds, so a composed deployment routes exactly as the plain one does.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or the integration
    /// registry fail to initialise, which includes two builders claiming the
    /// same integration id or auction provider name.
    pub fn routes_with_registrations(
        settings: Settings,
        integrations: &[IntegrationBuilder],
        auction_providers: &[AuctionProviderBuilder],
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_registrations(settings, integrations, auction_providers)?;
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
            let services = build_per_request_services(&state, &ctx);
            let mut req = ctx.into_request();
            if let Some(response) = deny_admin_diagnostic_fallback(&req) {
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
                let mut ec_context = EcContext::default();
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
                // Edge Cookie provider is unavailable). Answer with an error
                // rather than serving the page with no identity.
                let mut ec_context = match build_ec_context(&state.settings, &services, &req) {
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
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))
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
            // Canonical admin key routes. These match `Settings::ADMIN_ENDPOINTS`
            // and the production basic-auth handler regex (`^/_ts/admin`), so they
            // are auth-gated under a production-shaped config.
            //
            // The legacy non-`/_ts` aliases (`/admin/keys/*`) are registered
            // below to a local 404 for every publisher-fallback method: the
            // production handler regex `^/_ts/admin` does not match them, and
            // letting them fall through would forward the caller's
            // `Authorization` header and key-management payload to the origin,
            // leaking admin credentials.
            .post("/_ts/admin/keys/rotate", |_ctx: RequestContext| async {
                Ok::<Response, EdgeError>(admin_key_management_not_supported())
            })
            .post("/_ts/admin/keys/deactivate", |_ctx: RequestContext| async {
                Ok::<Response, EdgeError>(admin_key_management_not_supported())
            })
            // Admin EC lookup routes. Registered explicitly (like the key
            // routes above) so they never fall through to the publisher
            // fallback, and they match `Settings::ADMIN_ENDPOINTS` for auth
            // coverage. The EC identity graph is Fastly KV backed, so this
            // adapter has no store to read.
            .get("/_ts/admin/ec", |_ctx: RequestContext| async {
                Ok::<Response, EdgeError>(admin_ec_lookup_not_supported())
            })
            .get("/_ts/admin/ec/{id}", |_ctx: RequestContext| async {
                Ok::<Response, EdgeError>(admin_ec_lookup_not_supported())
            })
            // Admin EIDs echo: pure request inspection (no KV), so this
            // adapter serves the real handler.
            .get(
                "/_ts/admin/eids",
                make_handler(Arc::clone(&state), |s, _services, req| async move {
                    let partner_registry = PartnerRegistry::from_config(&s.settings.ec.partners)?;
                    handle_admin_eids_lookup(&partner_registry, &req)
                }),
            )
            .post(
                "/auction",
                make_handler(Arc::clone(&state), |s, services, req| async move {
                    // Build the geo-aware EC context so the auction consent gate
                    // sees the caller's jurisdiction — `EcContext::default()`
                    // fails it closed for consented users.
                    let ec_context = build_ec_context(&s.settings, &services, &req)?;
                    handle_auction(
                        &s.settings,
                        &s.orchestrator,
                        None,
                        None,
                        &ec_context,
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
            let ec_context = build_ec_context(&s.settings, &services, &req)?;
            let auction = AuctionDispatch {
                orchestrator: &s.orchestrator,
                slots: s.settings.creative_opportunity_slots(),
                registry: None,
            };
            handle_page_bids(&s.settings, &services, None, auction, &ec_context, req).await
        });
        let page_bids_preflight =
            make_handler(Arc::clone(&state), |_s, _services, _req| async move {
                Ok(page_bids_preflight_denied())
            });
        for path in [PAGE_BIDS_PATH, PAGE_BIDS_LEGACY_PATH] {
            router = router.route(path, Method::GET, page_bids.clone());
            router = router.route(path, Method::OPTIONS, page_bids_preflight.clone());
        }

        let legacy_admin_deny =
            make_handler(Arc::clone(&state), |_s, _services, _req| async move {
                Ok(legacy_admin_alias_denied())
            });
        for method in publisher_fallback_methods() {
            router = router.route(
                "/admin/keys/rotate",
                method.clone(),
                legacy_admin_deny.clone(),
            );
            router = router.route("/admin/keys/deactivate", method, legacy_admin_deny.clone());
        }

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

    /// Settings selecting a vendor Edge Cookie provider this adapter does not
    /// inject, with the `[ec.providers.<key>]` block configuration validation
    /// requires. `acme` is a fictional vendor key.
    const UNINJECTED_PROVIDER_TOML: &str = r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "admin"
        password = "admin-pass"

        [publisher]
        domain = "test-publisher.example.com"
        cookie_domain = ".test-publisher.example.com"
        origin_url = "https://origin.test-publisher.example.com"
        proxy_secret = "unit-test-proxy-secret"

        [ec]
        provider = "acme"

        [ec.providers.acme]
        endpoint = "https://ec.acme.example.com"

        # An Edge Cookie provider is configured, so single-jurisdiction
        # operation is acknowledged because no geo provider is selected.
        [geo]
        assume_single_jurisdiction = true
    "#;

    /// The per-request Edge Cookie read must return its error rather than a
    /// default context.
    ///
    /// This adapter used to log the failure and continue with
    /// `EcContext::default()`, so a deployment whose selected provider could not
    /// be built served every request with no identity. The call sites propagate
    /// the error to `http_error`, matching the Fastly adapter. The settings are
    /// parsed directly, bypassing the composition root's startup check, so the
    /// per-request behavior can be exercised with a selection the adapter
    /// cannot supply.
    #[test]
    fn build_ec_context_fails_when_the_selected_provider_is_unavailable() {
        let settings = Settings::from_toml(UNINJECTED_PROVIDER_TOML)
            .expect("should parse settings selecting an uninjected provider");
        let req = request_builder()
            .method("POST")
            .uri("https://test-publisher.example.com/auction")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        let ctx = RequestContext::new(req, PathParams::default());
        // No resolved provider is threaded here, so the request path resolves
        // the selection itself, which is what an embedder driving core
        // directly does and where the loud failure has to stay.
        let services = build_runtime_services(&ctx, &settings);
        let req = ctx.into_request();

        let error = build_ec_context(&settings, &services, &req)
            .expect_err("an unavailable Edge Cookie provider must fail the request");

        assert!(
            error.to_string().contains("acme"),
            "the error should name the selected provider, got: {error}"
        );
    }
}
