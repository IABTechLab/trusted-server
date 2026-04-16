//! Full `EdgeZero` application wiring for Trusted Server.
//!
//! Registers all routes from the legacy [`crate::route_request`] into a
//! [`RouterService`], attaches [`FinalizeResponseMiddleware`] (outermost) and
//! [`AuthMiddleware`] (inner), and builds the [`AppState`] once at startup.
//!
//! # Route inventory
//!
//! | Method | Path pattern | Handler |
//! |--------|-------------|---------|
//! | GET | `/.well-known/trusted-server.json` | [`handle_trusted_server_discovery`] |
//! | POST | `/verify-signature` | [`handle_verify_signature`] |
//! | POST | `/admin/keys/rotate` | [`handle_rotate_key`] |
//! | POST | `/admin/keys/deactivate` | [`handle_deactivate_key`] |
//! | POST | `/auction` | [`handle_auction`] |
//! | GET | `/first-party/proxy` | [`handle_first_party_proxy`] |
//! | GET | `/first-party/click` | [`handle_first_party_click`] |
//! | GET | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/sign` | [`handle_first_party_proxy_sign`] |
//! | POST | `/first-party/proxy-rebuild` | [`handle_first_party_proxy_rebuild`] |
//! | GET | `/{*rest}` | tsjs (if `/static/tsjs=` prefix), integration proxy, or publisher fallback |
//! | POST | `/{*rest}` | integration proxy or publisher fallback |

use std::sync::Arc;

use edgezero_adapter_fastly::FastlyRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{header, HeaderValue, Response};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{build_orchestrator, AuctionOrchestrator};
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::integrations::IntegrationRegistry;
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
use crate::platform::open_kv_store;
use crate::platform::{
    FastlyConsentKvStore, FastlyPlatformBackend, FastlyPlatformConfigStore, FastlyPlatformGeo,
    FastlyPlatformHttpClient, FastlyPlatformSecretStore, UnavailableKvStore,
};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once per Wasm instance and shared for its lifetime.
///
/// In Fastly Compute each request spawns a new Wasm instance, so this struct is
/// effectively per-request. It holds pre-parsed settings and all service handles.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    kv_store: Arc<dyn PlatformKvStore>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = get_settings()?;

    let orchestrator = build_orchestrator(&settings)?;

    let registry = IntegrationRegistry::new(&settings)?;

    let kv_store = match open_kv_store(&settings.synthetic.opid_store) {
        Ok(store) => store,
        Err(e) => {
            log::warn!(
                "KV store '{}' unavailable, synthetic ID routes will return errors: {e}",
                settings.synthetic.opid_store
            );
            Arc::new(UnavailableKvStore) as Arc<dyn PlatformKvStore>
        }
    };

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        kv_store,
    }))
}

// ---------------------------------------------------------------------------
// Per-request RuntimeServices
// ---------------------------------------------------------------------------

/// Construct per-request [`RuntimeServices`] from the `EdgeZero` request context.
///
/// Extracts the client IP address from the [`FastlyRequestContext`] extension
/// inserted by `edgezero_adapter_fastly::dispatch`. TLS metadata is not
/// available through the `EdgeZero` context so those fields are left empty.
fn build_per_request_services(state: &AppState, ctx: &RequestContext) -> RuntimeServices {
    let client_ip = FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip);

    RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore))
        .secret_store(Arc::new(FastlyPlatformSecretStore))
        .kv_store(Arc::clone(&state.kv_store))
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

/// Open the consent KV store named in `config`, returning `None` when not configured or unavailable.
pub(crate) fn open_consent_kv(
    config: &trusted_server_core::consent_config::ConsentConfig,
) -> Option<FastlyConsentKvStore> {
    config
        .consent_store
        .as_deref()
        .and_then(FastlyConsentKvStore::open)
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`],
/// mirroring [`crate::http_error_response`] exactly.
///
/// The near-identical function in `main.rs` is intentional: the legacy path
/// uses fastly HTTP types while this path uses `edgezero_core` types. The
/// duplication will be removed when `legacy_main` is deleted in PR 15.
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

/// Returns a [`RouterService`] that responds to every route with the startup error.
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

    RouterService::builder()
        .get("/", make(Arc::clone(&message)))
        .post("/", make(Arc::clone(&message)))
        .get("/{*rest}", make(Arc::clone(&message)))
        .post("/{*rest}", make(Arc::clone(&message)))
        .build()
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

        // Each handler below follows the same pattern: clone state, build
        // per-request services, consume the context into the request, call the
        // core handler, and convert errors with http_error. The pattern is kept
        // explicit rather than abstracted into a macro so each route can be
        // audited in isolation and handlers with differing signatures (sync vs
        // async, extra orchestrator argument) remain readable without special-casing.

        // /.well-known/trusted-server.json
        let s = Arc::clone(&state);
        let discovery_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_trusted_server_discovery(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /verify-signature
        let s = Arc::clone(&state);
        let verify_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_verify_signature(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /admin/keys/rotate
        let s = Arc::clone(&state);
        let rotate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_rotate_key(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /admin/keys/deactivate
        let s = Arc::clone(&state);
        let deactivate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_deactivate_key(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /auction
        let s = Arc::clone(&state);
        let auction_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                let consent_kv = open_consent_kv(&s.settings.consent);
                Ok(handle_auction(
                    &s.settings,
                    &s.orchestrator,
                    &services,
                    consent_kv
                        .as_ref()
                        .map(|kv| kv as &dyn trusted_server_core::consent::kv::ConsentKvOps),
                    req,
                )
                .await
                .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /first-party/proxy
        let s = Arc::clone(&state);
        let fp_proxy_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /first-party/click
        let s = Arc::clone(&state);
        let fp_click_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_click(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // GET /first-party/sign
        let s = Arc::clone(&state);
        let fp_sign_get_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy_sign(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // POST /first-party/sign
        let s = Arc::clone(&state);
        let fp_sign_post_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy_sign(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /first-party/proxy-rebuild
        let s = Arc::clone(&state);
        let fp_rebuild_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                Ok(
                    handle_first_party_proxy_rebuild(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // GET /{*rest} — tsjs (if /static/tsjs= prefix), integration proxy, or publisher fallback
        let s = Arc::clone(&state);
        let get_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                let path = req.uri().path().to_string();
                let method = req.method().clone();

                let result = if path.starts_with("/static/tsjs=") {
                    handle_tsjs_dynamic(&req, &s.registry)
                } else if s.registry.has_route(&method, &path) {
                    s.registry
                        .handle_proxy(&method, &path, &s.settings, &services, req)
                        .await
                        .unwrap_or_else(|| {
                            Err(Report::new(TrustedServerError::BadRequest {
                                message: format!("Unknown integration route: {path}"),
                            }))
                        })
                } else {
                    let consent_kv = open_consent_kv(&s.settings.consent);
                    handle_publisher_request(
                        &s.settings,
                        &s.registry,
                        &services,
                        consent_kv
                            .as_ref()
                            .map(|kv| kv as &dyn trusted_server_core::consent::kv::ConsentKvOps),
                        req,
                    )
                    .await
                };

                Ok(result.unwrap_or_else(|e| http_error(&e)))
            }
        };

        // POST /{*rest} — integration proxy or publisher origin fallback
        let s = Arc::clone(&state);
        let post_fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_per_request_services(&s, &ctx);
                let req = ctx.into_request();
                let path = req.uri().path().to_string();
                let method = req.method().clone();

                let result = if s.registry.has_route(&method, &path) {
                    s.registry
                        .handle_proxy(&method, &path, &s.settings, &services, req)
                        .await
                        .unwrap_or_else(|| {
                            Err(Report::new(TrustedServerError::BadRequest {
                                message: format!("Unknown integration route: {path}"),
                            }))
                        })
                } else {
                    let consent_kv = open_consent_kv(&s.settings.consent);
                    handle_publisher_request(
                        &s.settings,
                        &s.registry,
                        &services,
                        consent_kv
                            .as_ref()
                            .map(|kv| kv as &dyn trusted_server_core::consent::kv::ConsentKvOps),
                        req,
                    )
                    .await
                };

                Ok(result.unwrap_or_else(|e| http_error(&e)))
            }
        };

        RouterService::builder()
            .middleware(FinalizeResponseMiddleware::new(
                Arc::clone(&state.settings),
                Arc::new(FastlyPlatformGeo),
            ))
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))
            .get("/.well-known/trusted-server.json", discovery_handler)
            .post("/verify-signature", verify_handler)
            .post("/admin/keys/rotate", rotate_handler)
            .post("/admin/keys/deactivate", deactivate_handler)
            .post("/auction", auction_handler)
            .get("/first-party/proxy", fp_proxy_handler)
            .get("/first-party/click", fp_click_handler)
            .get("/first-party/sign", fp_sign_get_handler)
            .post("/first-party/sign", fp_sign_post_handler)
            .post("/first-party/proxy-rebuild", fp_rebuild_handler)
            // matchit's `/{*rest}` does not match the bare root `/` — register
            // explicit root routes so `/` reaches the publisher fallback too.
            .get("/", get_fallback.clone())
            .post("/", post_fallback.clone())
            .get("/{*rest}", get_fallback)
            .post("/{*rest}", post_fallback)
            .build()
    }
}
