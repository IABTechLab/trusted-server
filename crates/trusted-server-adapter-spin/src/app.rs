use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_adapter_spin::config_store::SpinConfigStore;
use edgezero_adapter_spin::context::SpinRequestContext;
use edgezero_core::app::{App, Hooks};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{
    HeaderValue, Method, Request, RequestParts, Response, StatusCode, header,
};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::outbound::ResponseDecoding;
use edgezero_core::router::{RouterBuilder, RouterService};
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{
    AuctionOrchestrator, build_orchestrator_with_plan, compile_auction_plan,
};
use trusted_server_core::cache_policy::EdgeCacheHeader;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::config_payload::settings_from_config_blob;
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::admin::{
    admin_ec_lookup_not_supported as core_admin_ec_lookup_not_supported,
    deny_admin_diagnostic_fallback, handle_admin_eids_lookup,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::edge_app::install_edge_policies;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::{RuntimeRequestOrigin, sanitize_forwarded_headers};
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::platform::{ClientInfo, HttpClient, RuntimeServices};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::platform::{PlatformConfigStore, StoreName};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    AuctionDispatch, PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH, PublisherOriginOptions,
    PublisherResponse, buffer_publisher_response_async, handle_page_bids, handle_publisher_request,
    handle_tsjs_dynamic, page_bids_preflight_denied, publisher_owner_error,
};
use trusted_server_core::request_signing::{
    handle_trusted_server_discovery, handle_verify_signature,
};
use trusted_server_core::settings::Settings;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::settings_data::{default_config_key, default_secret_store_name};
use trusted_server_core::trace::{TraceMetadata, TracePreDispatchHook};

use crate::middleware::{
    AuthMiddleware, FinalizeResponseMiddleware, NormalizeMiddleware, SanitizeRequestMiddleware,
};
use crate::platform::build_runtime_services;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use crate::platform::{ConfigStoreHandleAdapter, SpinSecretStoreAdapter};

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Spin auto-provides this key-value store label without runtime configuration.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
const SPIN_DEFAULT_CONFIG_STORE: &str = "default";

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
    services: Option<RuntimeServices>,
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

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
fn load_startup_settings() -> Result<Settings, Report<TrustedServerError>> {
    let config_store_name = StoreName::from(SPIN_DEFAULT_CONFIG_STORE);
    let config_key = default_config_key();
    // `for_label` defers open failures to the first read, which the
    // read error branch below reports.
    let config_store = SpinConfigStore::for_label(config_store_name.as_ref().to_owned());
    let config_handle = ConfigStoreHandle::new(Arc::new(config_store));
    let config_adapter = ConfigStoreHandleAdapter(config_handle);
    let raw_envelope = config_adapter
        .get(&config_store_name, &config_key)
        .map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: "failed to read Spin Trusted Server app-config blob".to_string(),
            })
            .attach(error.to_string())
        })?;
    let secret_store = SpinSecretStoreAdapter;
    settings_from_config_blob(&raw_envelope, &secret_store, &default_secret_store_name())
}

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
fn load_startup_settings() -> Result<Settings, Report<TrustedServerError>> {
    Err(Report::new(TrustedServerError::Configuration {
        message: "Spin startup settings require the production config store".to_string(),
    })
    .attach("use TrustedServerApp::routes_with_settings for host tests"))
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

fn build_state_with_services(
    mut settings: Settings,
    services: Option<RuntimeServices>,
) -> Result<Arc<AppState>, Report<TrustedServerError>> {
    // Spin cannot send a host other than the origin URI authority.
    settings
        .publisher
        .drop_origin_host_header_override_for_spin();
    settings
        .ec
        .resolve_storage(trusted_server_core::ec::storage::EcRuntime::Spin)?;
    let plan = Arc::new(compile_auction_plan(&settings)?);
    let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)?;
    let registry = IntegrationRegistry::with_plan(&settings, plan)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
        services,
    }))
}

impl AppState {
    fn services_for_request(&self, ctx: &RequestContext) -> Result<RuntimeServices, EdgeError> {
        self.services
            .clone()
            .map_or_else(|| build_runtime_services(ctx), Ok)
    }
}

// ---------------------------------------------------------------------------
// Publisher response helper
// ---------------------------------------------------------------------------

/// Largest publisher origin body the Spin adapter collects before egress.
///
/// It keeps the 8 MiB limit the adapter's own client applied before the
/// publisher origin moved to `EdgeZero`'s client.
const PUBLISHER_ORIGIN_INPUT_BYTES: u64 = 8 * 1024 * 1024;

/// Returns the origin input cap: the smaller of the publisher buffer setting
/// and [`PUBLISHER_ORIGIN_INPUT_BYTES`].
fn publisher_input_cap(settings: &Settings) -> u64 {
    u64::try_from(settings.publisher.max_buffered_body_bytes)
        .unwrap_or(u64::MAX)
        .min(PUBLISHER_ORIGIN_INPUT_BYTES)
}

/// Collapse a [`PublisherResponse`] into a plain [`Response`].
///
/// Delegates to the shared [`buffer_publisher_response_async`], which collects
/// the dispatched server-side auction, collects raw origin bodies under
/// `input_cap`, and enforces `settings.publisher.max_buffered_body_bytes` on
/// processed output, so a large origin response fails safely instead of
/// exhausting the Wasm heap.
async fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: &Settings,
    registry: &IntegrationRegistry,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    input_cap: u64,
) -> Result<Response, Report<TrustedServerError>> {
    buffer_publisher_response_async(
        publisher_response,
        method,
        settings,
        registry,
        orchestrator,
        services,
        Some(input_cap),
    )
    .await
}

// ---------------------------------------------------------------------------
// Publisher fallback method table
// ---------------------------------------------------------------------------

// Methods routed through the publisher/integration fallback, matching the
// Fastly and Axum adapters so legacy origin behaviour (HEAD, OPTIONS/CORS
// preflight, PUT/PATCH/DELETE) is preserved instead of returning 405.
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

// Named routes paired with the methods they handle directly. Every other
// publisher-fallback method on these paths is routed to the fallback so it
// reaches the publisher origin rather than a router-level 405.
//
// The EC API routes that the Fastly entry point registers — POST
// `/_ts/api/v1/batch-sync`, GET/OPTIONS `/_ts/api/v1/identify` — are
// intentionally absent here, matching the Axum and Cloudflare adapters: those
// handlers require a platform KV `ec_store` (and, for batch-sync, a partner
// registry and rate limiter) that the portability adapters do not yet wire.
// On Spin these paths fall through to the publisher/integration fallback,
// identical to the other non-Fastly adapters.
const LEGACY_ADMIN_DENY_METHODS: &[Method] = &[
    Method::GET,
    Method::POST,
    Method::HEAD,
    Method::OPTIONS,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
];

fn named_fallback_paths() -> [(&'static str, &'static [Method]); 17] {
    [
        ("/.well-known/trusted-server.json", &[Method::GET]),
        ("/verify-signature", &[Method::POST]),
        ("/_ts/admin/keys/rotate", &[Method::POST]),
        ("/_ts/admin/keys/deactivate", &[Method::POST]),
        ("/_ts/admin/ec", &[Method::GET]),
        ("/_ts/admin/ec/{id}", &[Method::GET]),
        ("/_ts/admin/eids", &[Method::GET]),
        ("/_ts/admin/cache/purge", LEGACY_ADMIN_DENY_METHODS),
        ("/admin/keys/rotate", LEGACY_ADMIN_DENY_METHODS),
        ("/admin/keys/deactivate", LEGACY_ADMIN_DENY_METHODS),
        ("/auction", &[Method::POST]),
        (PAGE_BIDS_PATH, &[Method::GET, Method::OPTIONS]),
        (PAGE_BIDS_LEGACY_PATH, &[Method::GET, Method::OPTIONS]),
        ("/first-party/proxy", &[Method::GET]),
        ("/first-party/click", &[Method::GET]),
        ("/first-party/sign", &[Method::GET, Method::POST]),
        ("/first-party/proxy-rebuild", &[Method::GET, Method::POST]),
    ]
}

// ---------------------------------------------------------------------------
// Spin host extraction
// ---------------------------------------------------------------------------

// Extracts (scheme, "host[:port]") from a full URL string
// (e.g. "https://www.example.com:3000/path" → ("https", "www.example.com:3000")).
// Used to reconstruct the trusted Host and scheme from Spin's spin-full-url
// synthetic header. Returns None when the URL has no scheme or host.
fn scheme_host_from_spin_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    // Strip optional `userinfo@` so a `scheme://user@host/…` URL yields the bare
    // host. Not reachable from Spin's runtime today (it never emits userinfo), so
    // this is cosmetic robustness rather than a security fix.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if scheme.is_empty() || host.is_empty() {
        None
    } else {
        Some((scheme.to_ascii_lowercase(), host.to_string()))
    }
}

// Parses an IP from a `spin-client-addr` value: `ip:port`, `[ip6]:port`, or a
// bare IP with no port. Mirrors the parsing Spin's runtime applies; kept local
// because edgezero's equivalent helper is crate-private.
fn parse_client_addr(raw: &str) -> Option<IpAddr> {
    if let Ok(sock) = raw.parse::<SocketAddr>() {
        return Some(sock.ip());
    }
    raw.parse::<IpAddr>().ok()
}

// Strips client-spoofable forwarded headers, reconstructs the trusted Host and
// scheme from Spin's `spin-full-url` synthetic header, and rebuilds the core
// request URI into an absolute form.
//
// A client can spoof `Forwarded`/`X-Forwarded-*` to hijack the host and scheme
// that publisher HTML rewriting, integration URL rewriting, and request-signing
// context consume. Stripping them first (mirroring the Fastly/Axum edge
// sanitization) means the typed origin used by `RequestInfo` originates from
// the trusted runtime URL rather than the client.
//
// Spin builds the core request URI from `IncomingRequest::path_with_query()`, so
// it is path-only (e.g. "/first-party/proxy?..."). The shared first-party
// proxy/click/sign handlers parse `req.uri().to_string()` with `url::Url::parse`,
// which rejects a relative path. Rebuilding an absolute URI from the trusted
// scheme+host lets those handlers validate the signed target instead of failing
// with "Invalid URL".
pub(crate) fn normalize_spin_request(req: &mut RequestParts) {
    sanitize_forwarded_headers(&mut req.headers);

    // Strip any client-supplied `x-forwarded-for`. Spin exposes no trusted
    // upstream forwarded-for chain — the only trusted client IP comes from the
    // synthetic `spin-client-addr` captured below. Shared proxy code forwards an
    // inbound `x-forwarded-for` to origins and Prebid copies it into its outbound
    // headers, so leaving it would let a Spin client spoof the IP seen by
    // downstream services while `device.ip` uses the trusted runtime address.
    req.headers.remove("x-forwarded-for");

    // Re-derive the trusted client IP from the *last* `spin-client-addr` header
    // and overwrite `SpinRequestContext`, which `build_runtime_services` reads
    // for `ClientInfo::client_ip`. edgezero populates that context from the
    // *first* `spin-client-addr` match, but Spin's WASI HTTP bridge appends its
    // synthetic header *after* the original client headers — so a client can send
    // its own `spin-client-addr` ahead of Spin's and forge the IP that EC ID
    // hashing, `/auction` `device.ip`, and integration `X-Forwarded-For` consume.
    // Selecting the last occurrence pins the value to the trusted runtime one.
    let trusted_client_addr = req
        .headers
        .get_all("spin-client-addr")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_addr);
    let trusted_full_url = req
        .headers
        .get_all("spin-full-url")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    req.extensions.insert(SpinRequestContext {
        client_addr: trusted_client_addr,
        full_url: trusted_full_url.clone(),
    });

    // Spin's WASI HTTP bridge copies synthetic `spin-client-addr` and
    // `spin-full-url` headers onto the core request. They are runtime metadata,
    // not client- or application-supplied headers. The trusted `spin-client-addr`
    // values are captured into `SpinRequestContext` above; remove every copy here
    // so the shared publisher/integration handlers neither forward them to
    // publisher origins nor mistake them for client-controlled input.
    req.headers.remove("spin-client-addr");

    // Spin's WASI HTTP bridge appends its synthetic `spin-full-url` *after* the
    // original client headers, so when duplicate names are present the trusted
    // runtime value is the last one. `HeaderMap::remove` returns the *first*
    // value, which a client could control by sending its own `spin-full-url`
    // ahead of Spin's synthetic one — letting it choose the host/scheme consumed
    // by publisher HTML rewriting, integration URL rewriting, and request
    // signing. Select the last occurrence as the trusted authority, then strip
    // every copy so none reach publisher origins or the shared handlers.
    req.headers.remove("spin-full-url");
    let Some((scheme, host)) = trusted_full_url
        .as_deref()
        .and_then(scheme_host_from_spin_url)
    else {
        return;
    };
    let Ok(origin) = InboundOrigin::parse(&scheme, &host, OriginSource::RuntimeUri) else {
        return;
    };
    req.extensions.insert(RuntimeRequestOrigin::new(origin));

    // Always set Host from the trusted spin-full-url rather than preserving any
    // incoming value. Spin's WASI HTTP bridge does not normally surface the
    // incoming Host header (without it extract_request_host() returns "" and
    // classify_response_route falls back to BufferedUnmodified, skipping the HTML
    // processor), but when a Host *is* present it is client-controllable. Keeping
    // it while rebuilding req.uri() from the spin-full-url host below would let the
    // shared RequestInfo path (publisher HTML rewriting, integration URL rewriting,
    // signing context) read one host while handlers parsing req.uri() see another.
    // Overriding from the single trusted authority keeps both consistent.
    if let Ok(hval) = HeaderValue::from_str(&host) {
        req.headers.insert(header::HOST, hval);
    }

    // Promote the path-only URI to an absolute one so the shared first-party
    // proxy/click/sign handlers can parse `req.uri()` as a full URL.
    let path_and_query = req
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_string());
    if let Ok(uri) = format!("{scheme}://{host}{path_and_query}").parse() {
        req.uri = uri;
    }
}

// ---------------------------------------------------------------------------
// Health probe
// ---------------------------------------------------------------------------

/// Builds the `GET /health` liveness response (`200 ok`, `text/plain`).
///
/// Mirrors the Fastly entry point and Axum adapter so deployments reusing
/// Trusted Server health probes see identical behaviour on Spin. Served only
/// once application state is built, so a startup failure answers 503 here too.
/// Spin's platform-provided `/.well-known/spin/health` is left untouched.
fn health_response() -> Response {
    let mut resp = Response::new(edgezero_core::body::Body::from("ok"));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    resp
}

/// Builds the geo-aware [`EcContext`] for consent-gated endpoints (`/auction`,
/// `/_ts/page-bids`, and the publisher fallback).
///
/// Mirrors the Fastly entry point: `EcContext::default()` leaves jurisdiction
/// Unknown, which fails the auction consent gate closed even for consented
/// users. Spin's platform geo is a no-op, so jurisdiction stays Unknown unless
/// the request carries TCF consent. A malformed consent string is logged and
/// falls back to the default (fail-closed) context rather than being silently
/// swallowed.
fn build_ec_context(settings: &Settings, services: &RuntimeServices, req: &Request) -> EcContext {
    let geo_info = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    EcContext::read_from_request_with_geo(settings, req, services, geo_info.as_ref())
        .unwrap_or_else(|e| {
            log::warn!("EC context read failed: {e:?}");
            EcContext::default()
        })
}

fn cache_purge_not_supported() -> Response {
    let body = edgezero_core::body::Body::from(
        "Template cache purge is not supported on Spin.\n\
         Use the Fastly adapter (via Viceroy or deployed) to purge.\n",
    );
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::NOT_IMPLEMENTED;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn admin_key_management_not_supported() -> Response {
    let body = edgezero_core::body::Body::from(
        "Admin key management is not supported on Fermyon Spin.\n\
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
/// aliases on the Spin adapter.
///
/// These non-`/_ts` aliases are not matched by the `^/_ts/admin` basic-auth
/// handler, so they fail closed locally rather than fall through to the
/// publisher fallback, which would forward the caller's `Authorization` header
/// and key-management payload to the origin.
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
// TrustedServerApp
// ---------------------------------------------------------------------------

/// `EdgeZero` [`Hooks`] implementation for the Trusted Server application.
pub struct TrustedServerApp;

impl Hooks for TrustedServerApp {
    fn configure(app: &mut App) -> Result<(), EdgeError> {
        install_edge_policies(app)
    }

    fn name() -> &'static str {
        "TrustedServer"
    }

    fn routes() -> Result<RouterService, EdgeError> {
        // Every startup failure answers 503 with a generic message, so the
        // logged report never reaches an anonymous caller.
        let state = build_state().map_err(|report| {
            log::error!("{report:?}");
            EdgeError::service_unavailable("Service Unavailable")
        })?;

        Ok(router_builder(&state).build())
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
        Ok(router_builder(&state).build())
    }

    /// Build the full application router from explicit settings, with `client`
    /// in router state.
    ///
    /// Testing seam: `EdgeZero` hands a client in router state to every
    /// request in place of the adapter's own, so an app served from this
    /// router on `EdgeZero`'s Axum test server starts no real transport.
    /// Production installs no client in router state.
    ///
    /// # Errors
    ///
    /// Returns an error when the auction orchestrator or the integration
    /// registry fail to initialise.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let client = HttpClient::with_client(ScriptedHttpClient::forbidding());
    /// let router = TrustedServerApp::routes_with_settings_and_outbound_client(settings, client)?;
    /// ```
    pub fn routes_with_settings_and_outbound_client(
        settings: Settings,
        client: HttpClient,
    ) -> Result<RouterService, Report<TrustedServerError>> {
        let state = build_state_with_settings(settings)?;
        Ok(router_builder(&state).with_state(client).build())
    }

    /// Build the full router with explicit settings and runtime services.
    ///
    /// Each request receives a clone of the supplied services, allowing callers
    /// to exercise production routes with deterministic platform dependencies.
    /// The supplied client metadata applies to every request to this router.
    /// The services' outbound client also goes into router state, so an app
    /// served from this router starts no real `EdgeZero` transport.
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
        let client = services.outbound_client().clone();
        let state = build_state_with_services(settings, Some(services))?;
        Ok(router_builder(&state).with_state(client).build())
    }
}

/// Registers every route, middleware, and hook for `state` on a router builder.
fn router_builder(state: &Arc<AppState>) -> RouterBuilder {
    {
        let state = Arc::clone(state);

        // /.well-known/trusted-server.json
        let s = Arc::clone(&state);
        let discovery_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_trusted_server_discovery(&s.settings, &services, req)
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // /verify-signature
        let s = Arc::clone(&state);
        let verify_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_verify_signature(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        let admin_not_supported_handler = |_ctx: RequestContext| async {
            Ok::<Response, EdgeError>(admin_key_management_not_supported())
        };

        let cache_purge_unsupported_handler =
            |_ctx: RequestContext| async { Ok::<Response, EdgeError>(cache_purge_not_supported()) };

        let admin_ec_not_supported_handler = |_ctx: RequestContext| async {
            Ok::<Response, EdgeError>(admin_ec_lookup_not_supported())
        };

        // Admin EIDs echo: pure request inspection (no KV), so this adapter
        // serves the real handler.
        let s = Arc::clone(&state);
        let admin_eids_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let req = ctx.into_request()?;
                let result = PartnerRegistry::from_config(&s.settings.ec.partners)
                    .and_then(|registry| handle_admin_eids_lookup(&registry, &req));
                Ok::<Response, EdgeError>(result.unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /auction
        let s = Arc::clone(&state);
        let auction_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                // Request normalization (forwarded-header stripping, trusted
                // Host/scheme/client-IP derivation) is applied centrally by
                // `NormalizeMiddleware` before this handler runs, so the signed
                // OpenRTB metadata that auction signing derives from
                // `RequestInfo::from_request` uses the trusted runtime authority.
                let mut req = ctx.into_request()?;
                if let Err(error) =
                    trusted_server_core::integrations::gpt_diagnostics::prepare_request(
                        &s.settings,
                        &mut req,
                    )
                {
                    return Ok(http_error(&error));
                }
                // Build the geo-aware EC context so the auction consent gate sees
                // the caller's jurisdiction — `EcContext::default()` fails it
                // closed for consented users.
                let mut ec_context = build_ec_context(&s.settings, &services, &req);
                Ok::<Response, EdgeError>(
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
                    .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // GET /_ts/page-bids — SPA re-auction endpoint.
        let s = Arc::clone(&state);
        let page_bids_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let mut req = ctx.into_request()?;
                if let Err(error) =
                    trusted_server_core::integrations::gpt_diagnostics::prepare_request(
                        &s.settings,
                        &mut req,
                    )
                {
                    return Ok(http_error(&error));
                }
                let mut ec_context = build_ec_context(&s.settings, &services, &req);
                let auction = AuctionDispatch {
                    integration_registry: &s.registry,
                    orchestrator: &s.orchestrator,
                    slots: s.settings.creative_opportunity_slots(),
                    registry: None,
                };
                Ok::<Response, EdgeError>(
                    handle_page_bids(&s.settings, &services, None, auction, &mut ec_context, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // OPTIONS /_ts/page-bids — deny the CORS preflight for this
        // side-effecting GET so the `X-TSJS-Page-Bids` gate stays trustworthy.
        let page_bids_options_handler = |_ctx: RequestContext| async {
            Ok::<Response, EdgeError>(page_bids_preflight_denied())
        };

        // GET /first-party/proxy
        let s = Arc::clone(&state);
        let fp_proxy_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_first_party_proxy(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // /first-party/click
        let s = Arc::clone(&state);
        let fp_click_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_first_party_click(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // GET + POST /first-party/sign — identical handler, cloned for both bindings
        let s = Arc::clone(&state);
        let fp_sign_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_first_party_proxy_sign(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };
        let fp_sign_post_handler = fp_sign_handler.clone();

        // GET + POST /first-party/proxy-rebuild — GET serves the click guard's
        // navigation fallback: the creative iframe is an opaque origin (sandbox
        // without `allow-same-origin`), so its JSON POST is blocked by CORS and
        // the guard navigates here for a 302 instead.
        let s = Arc::clone(&state);
        let fp_rebuild_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = s.services_for_request(&ctx)?;
                let req = ctx.into_request()?;
                Ok::<Response, EdgeError>(
                    handle_first_party_proxy_rebuild(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };
        let fp_rebuild_post_handler = fp_rebuild_handler.clone();

        // Shared fallback dispatch: routes to tsjs (GET only), integration proxy, or publisher.
        async fn dispatch(
            state: Arc<AppState>,
            ctx: RequestContext,
        ) -> Result<Response, EdgeError> {
            let services = state.services_for_request(&ctx)?;
            let mut req = ctx.into_request()?;
            if req.uri().path() == trusted_server_core::ec::consent_update::CONSENT_UPDATE_PATH {
                // Reserve this path without an identity graph; never forward
                // consent snapshots or acknowledge an unpersisted revocation.
                return Ok(
                    match trusted_server_core::ec::consent_update::prepare_consent_update(
                        &services, req,
                    )
                    .await
                    {
                        Ok(req) => trusted_server_core::ec::consent_update::handle_consent_update(
                            &req, None,
                        ),
                        Err(response) => *response,
                    },
                );
            }
            if let Some(response) = deny_admin_diagnostic_fallback(&req) {
                return Ok(response);
            }
            if let Err(error) = trusted_server_core::integrations::gpt_diagnostics::prepare_request(
                &state.settings,
                &mut req,
            ) {
                return Ok(http_error(&error));
            }

            let path = req.uri().path().to_owned();
            let method = req.method().clone();

            // Dynamic tsjs serving is GET-only; other methods fall through to the
            // integration/publisher fallback.
            let result = if method == Method::GET && path.starts_with("/static/tsjs=") {
                handle_tsjs_dynamic(&req, &state.registry, EdgeCacheHeader::SMaxageFallback)
            } else if state.registry.has_route(&method, &path) {
                let mut ec_context = EcContext::default();
                state
                    .registry
                    .handle_proxy(ProxyDispatchInput {
                        method: &method,
                        path: &path,
                        settings: &state.settings,
                        kv: None,
                        allow_generation: true,
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
                let mut ec_context = build_ec_context(&state.settings, &services, &req);
                let auction = AuctionDispatch {
                    integration_registry: &state.registry,
                    orchestrator: &state.orchestrator,
                    slots: state.settings.creative_opportunity_slots(),
                    registry: None,
                };
                let input_cap = publisher_input_cap(&state.settings);
                match handle_publisher_request(
                    &state.settings,
                    &services,
                    None,
                    &mut ec_context,
                    auction,
                    req,
                    PublisherOriginOptions {
                        edge_header: EdgeCacheHeader::SMaxageFallback,
                        input_cap: Some(input_cap),
                        response_decoding: ResponseDecoding::Original,
                    },
                )
                .await
                {
                    Ok((pr, owner)) => resolve_publisher_response(
                        pr,
                        &method,
                        &state.settings,
                        &state.registry,
                        &state.orchestrator,
                        &services,
                        input_cap,
                    )
                    .await
                    .and_then(|response| owner.attach(response).map_err(publisher_owner_error)),
                    Err(e) => Err(e),
                }
            };

            Ok(result.unwrap_or_else(|e| http_error(&e)))
        }

        // Single publisher/integration fallback used for every method. The method
        // is read inside `dispatch`, so the same closure serves GET (tsjs/publisher)
        // and the other supported methods (integration proxy / publisher origin).
        let s = Arc::clone(&state);
        let fallback = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            dispatch(s, ctx)
        };
        let legacy_admin_deny =
            |_ctx: RequestContext| async { Ok::<Response, EdgeError>(legacy_admin_alias_denied()) };

        let trace_state = Arc::clone(&state);
        let mut builder = RouterService::builder()
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
            .middleware(AuthMiddleware::new(Arc::clone(&state.settings)))
            // Innermost middleware: normalize every routed request (strip
            // spoofable forwarded headers, derive the trusted Host/scheme/client-IP
            // from Spin's synthetic runtime headers) so no handler can opt out of
            // the de-spoofing invariant. Runs after auth so the basic-auth gate
            // continues to see the original request, matching prior behaviour.
            .middleware(NormalizeMiddleware::new())
            // Cheap liveness probe, matching the Fastly/Axum adapters. Registered
            // explicitly so it is not absorbed by the publisher `/{*rest}` fallback.
            .get("/health", |_ctx: RequestContext| async {
                Ok::<Response, EdgeError>(health_response())
            })
            .get("/.well-known/trusted-server.json", discovery_handler)
            .post("/verify-signature", verify_handler)
            // Canonical admin key routes. These match `Settings::ADMIN_ENDPOINTS`
            // and the production basic-auth handler regex (`^/_ts/admin`), so they
            // are auth-gated under a production-shaped config.
            //
            // The legacy non-`/_ts` aliases (`/admin/keys/*`) are registered below
            // to a local 404 for every publisher-fallback method: the production
            // handler regex `^/_ts/admin` does not match them, and letting them
            // fall through to the publisher fallback would forward admin
            // credentials and key-management payloads to the origin.
            .post("/_ts/admin/keys/rotate", admin_not_supported_handler)
            .post("/_ts/admin/keys/deactivate", admin_not_supported_handler)
            // Admin EC lookup routes. Registered explicitly (like the key
            // routes above) so they never fall through to the publisher
            // fallback, and they match `Settings::ADMIN_ENDPOINTS` for auth
            // coverage. The EC identity graph is Fastly KV backed, so this
            // adapter has no store to read.
            .get("/_ts/admin/ec", admin_ec_not_supported_handler)
            .get("/_ts/admin/ec/{id}", admin_ec_not_supported_handler)
            .get("/_ts/admin/eids", admin_eids_handler)
            .post("/auction", auction_handler)
            .get(PAGE_BIDS_PATH, page_bids_handler.clone())
            .route(PAGE_BIDS_PATH, Method::OPTIONS, page_bids_options_handler)
            // Deprecated double-underscore alias, kept so tsjs bundles served
            // before the `/_ts/page-bids` rename keep getting ads on SPA
            // navigations until they age out of browser caches. See
            // `PAGE_BIDS_LEGACY_PATH`.
            .get(PAGE_BIDS_LEGACY_PATH, page_bids_handler)
            .route(
                PAGE_BIDS_LEGACY_PATH,
                Method::OPTIONS,
                page_bids_options_handler,
            )
            .get("/first-party/proxy", fp_proxy_handler)
            .get("/first-party/click", fp_click_handler)
            .get("/first-party/sign", fp_sign_handler)
            .post("/first-party/sign", fp_sign_post_handler)
            .get("/first-party/proxy-rebuild", fp_rebuild_handler)
            .post("/first-party/proxy-rebuild", fp_rebuild_post_handler);

        for method in LEGACY_ADMIN_DENY_METHODS {
            builder = builder.route(
                "/_ts/admin/cache/purge",
                method.clone(),
                cache_purge_unsupported_handler,
            );
        }

        for method in LEGACY_ADMIN_DENY_METHODS {
            builder = builder.route("/admin/keys/rotate", method.clone(), legacy_admin_deny);
            builder = builder.route("/admin/keys/deactivate", method.clone(), legacy_admin_deny);
        }

        // Mirror the Fastly/Axum publisher fallback: every supported method that is
        // not a named route's primary method falls through to the publisher origin
        // (e.g. HEAD /, OPTIONS /page preflight, HEAD /first-party/proxy) instead of
        // returning a router-level 405. tsjs handling stays GET-only (see dispatch).
        for (path, primary_methods) in named_fallback_paths() {
            for method in publisher_fallback_methods() {
                if !primary_methods.contains(&method) {
                    builder = builder.route(path, method, fallback.clone());
                }
            }
        }
        for method in publisher_fallback_methods() {
            builder = builder.route("/", method.clone(), fallback.clone());
            builder = builder.route("/{*rest}", method, fallback.clone());
        }

        builder
    }
}

fn trace_metadata(state: &AppState, request: &RequestParts) -> TraceMetadata {
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
    let client_ip = request
        .headers
        .get_all("spin-client-addr")
        .iter()
        .next_back()
        .and_then(|value| value.to_str().ok())
        .and_then(parse_client_addr);
    TraceMetadata {
        client_info: ClientInfo {
            client_ip,
            ..ClientInfo::default()
        },
        geo: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Runs the production normalizer on the head of a whole test request.
    fn normalize_request(req: &mut Request) {
        let request = std::mem::replace(req, Request::new(edgezero_core::body::Body::empty()));
        let (mut parts, body) = request.into_parts();
        normalize_spin_request(&mut parts);
        *req = Request::from_parts(parts, body);
    }

    #[test]
    fn explicit_ec_storage_is_rejected_before_route_or_provider_work() {
        for storage in [
            serde_json::json!({"provider":"fastly","store":"exact-fixture-binding"}),
            serde_json::json!({"provider":"dynamodb","region":"us-east-1","table":"example-identity","namespace":"fixture"}),
            serde_json::json!({"provider":"celld","endpoint":"http://127.0.0.1:7780","store":"identity","access":{"mode":"trusted-network"}}),
        ] {
            let mut settings = Settings::default();
            settings.ec.storage =
                Some(serde_json::from_value(storage).expect("should parse pure provider settings"));
            let error = build_state_with_settings(settings)
                .err()
                .expect("should reject unsupported EC provider before route work");
            assert!(format!("{error:?}").contains("ec.storage.provider is unsupported"));
        }
        let mut settings = Settings::default();
        settings.ec.ec_store = Some("legacy-inactive-binding".to_owned());
        assert!(
            settings
                .ec
                .resolve_storage(trusted_server_core::ec::storage::EcRuntime::Spin)
                .expect("should accept inactive legacy field")
                .is_none()
        );
    }

    fn multi_provider_settings() -> Settings {
        let mut settings = Settings::from_toml(
            r#"
                [[handlers]]
                path = "^/_ts/admin"
                username = "admin"
                password = "admin-password"

                [publisher]
                domain = "publisher.example"
                cookie_domain = ".publisher.example"
                origin_url = "https://origin.publisher.example"
                proxy_secret = "fictional-proxy-secret"

                [ec]
                passphrase = "fictional-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse startup test settings");
        settings.auction.enabled = true;
        settings.auction.providers =
            std::iter::IntoIterator::into_iter(["provider-a", "provider-b"])
                .map(|id| {
                    (
                        id.parse().expect("should parse provider ID"),
                        trusted_server_core::auction::ProviderConfig {
                            allow_insecure_http: false,
                            protocol: "openrtb-2.6".to_string(),
                            profile: "standard".to_string(),
                            endpoint: format!("https://{id}.example/openrtb"),
                            timeout_ms: None,
                            routing: trusted_server_core::auction::RoutingMode::AllEligible,
                            notifications:
                                trusted_server_core::auction::NotificationConfig::default(),
                            profile_config: "{}"
                                .parse()
                                .expect("should parse empty profile config object"),
                        },
                    )
                })
                .collect();
        settings
    }

    #[test]
    fn startup_registers_aps_renderer_route() {
        let mut settings = multi_provider_settings();
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
                profile_config: "{\"account_id\":\"example-account\"}"
                    .parse()
                    .expect("should parse APS profile config"),
            },
        );

        let state =
            build_state_with_settings(settings).expect("Spin startup should register APS renderer");
        assert!(
            state.registry.has_route(
                &edgezero_core::http::Method::GET,
                "/integrations/aps/renderer"
            ),
            "Spin startup registry should expose the APS renderer"
        );
    }

    #[test]
    fn disabled_startup_accepts_dormant_multi_provider_auction_plan() {
        let mut settings = multi_provider_settings();
        settings.auction.enabled = false;

        build_state_with_settings(settings)
            .expect("disabled Spin auction should accept dormant fanout");
    }

    #[test]
    fn cloudflare_and_spin_load_multi_provider_plan() {
        let state = build_state_with_settings(multi_provider_settings())
            .expect("should load a multi-provider auction plan on Spin");

        assert_eq!(
            state.orchestrator.provider_count(),
            2,
            "should fan out to both configured providers"
        );
    }

    #[test]
    fn scheme_host_from_spin_url_extracts_localhost_with_port() {
        assert_eq!(
            scheme_host_from_spin_url("http://localhost:3000/some/path"),
            Some(("http".to_string(), "localhost:3000".to_string())),
            "should extract scheme and host:port from http URL"
        );
    }

    #[test]
    fn scheme_host_from_spin_url_extracts_production_domain() {
        assert_eq!(
            scheme_host_from_spin_url("https://www.publisher.example/cars/"),
            Some(("https".to_string(), "www.publisher.example".to_string())),
            "should extract https scheme and domain without port"
        );
    }

    #[test]
    fn scheme_host_from_spin_url_handles_root_path() {
        assert_eq!(
            scheme_host_from_spin_url("http://127.0.0.1:3000/"),
            Some(("http".to_string(), "127.0.0.1:3000".to_string())),
            "should extract scheme and host from root path URL"
        );
    }

    #[test]
    fn scheme_host_from_spin_url_rejects_no_scheme() {
        assert_eq!(
            scheme_host_from_spin_url("localhost:3000/path"),
            None,
            "should return None when no scheme separator"
        );
    }

    fn request_with(headers: &[(&str, &str)]) -> Request {
        let mut builder = edgezero_core::http::request_builder()
            .method("GET")
            .uri("/");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build request")
    }

    #[test]
    fn normalize_spin_request_rejects_invalid_last_runtime_origin() {
        for value in [
            "ftp://publisher.example.com/article",
            // Deliberately malformed port exercises runtime-origin rejection.
            "https://publisher.example.com:invalid/article", // allow-domain: publisher.example.com:invalid
        ] {
            let mut req = request_with(&[
                (
                    "spin-full-url",
                    "https://client-supplied.example.com/article",
                ),
                ("spin-full-url", value),
            ]);

            normalize_request(&mut req);

            assert!(
                req.extensions().get::<RuntimeRequestOrigin>().is_none(),
                "should not recover origin from a client field when trusted last URL is invalid"
            );
            assert_eq!(
                req.uri().to_string(),
                "/",
                "should retain original path when runtime origin is invalid"
            );
        }
    }

    #[tokio::test]
    async fn normalized_runtime_origin_does_not_authorize_trace_without_ingress() {
        let mut settings = multi_provider_settings();
        settings.auction.enabled = false;
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &std::collections::BTreeMap::from([
                    ("enabled", true),
                    ("trace_page_enabled", true),
                ]),
            )
            .expect("should enable trace actions");
        let router =
            TrustedServerApp::routes_with_settings(settings).expect("should build trace router");
        let mut req = request_with(&[
            (
                "spin-full-url",
                "https://publisher.example.com:8443/_ts/trace/enable",
            ),
            ("origin", "https://publisher.example.com:8443"),
            ("sec-fetch-site", "same-origin"),
            ("x-ts-trace-action", "enable"),
        ]);
        *req.method_mut() = Method::POST;
        *req.uri_mut() = "/_ts/trace/enable".parse().expect("should set action path");
        normalize_request(&mut req);

        let response = RouterService::oneshot(&router, req)
            .await
            .expect("should reject unbound trace action");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should require immutable ingress even with typed runtime origin"
        );
        assert!(
            !response.headers().contains_key(header::SET_COOKIE),
            "should not mutate trace cookies without ingress origin"
        );
    }

    #[test]
    fn normalize_spin_request_preserves_https_request_info_without_ingress_origin() {
        let mut req = request_with(&[
            (
                "spin-full-url",
                "https://publisher.example.com:8443/article",
            ),
            ("host", "client-supplied.example.com"),
            ("x-forwarded-proto", "http"),
        ]);
        req.extensions_mut()
            .insert(edgezero_core::ingress::RequestIngress::new(
                edgezero_core::ingress::CapturedTarget::Unavailable(
                    edgezero_core::ingress::TargetUnavailable::NotExposed,
                ),
                None,
                edgezero_core::ingress::HeaderFidelity::default(),
            ));

        normalize_request(&mut req);
        let (mut parts, body) = req.into_parts();
        trusted_server_core::forwarder::prepare_trusted_forwarder(
            &mut parts,
            &multi_provider_settings(),
        );
        let req = Request::from_parts(parts, body);

        let info = trusted_server_core::http_util::RequestInfo::from_request(
            &req,
            &trusted_server_core::platform::ClientInfo::default(),
        )
        .expect("should accept trusted Spin request metadata");
        assert_eq!(
            info.scheme, "https",
            "should retain trusted Spin HTTPS scheme after forwarding sanitation"
        );
        assert_eq!(
            info.host, "publisher.example.com:8443",
            "should retain trusted runtime authority and port"
        );
        assert!(
            req.extensions()
                .get::<edgezero_core::ingress::RequestIngress>()
                .expect("should retain original ingress")
                .origin()
                .is_none(),
            "should not grant immutable ingress origin from normalized URL"
        );
        assert!(
            !req.headers().contains_key("x-forwarded-proto"),
            "should not require a synthetic forwarding header"
        );
    }

    #[test]
    fn normalize_spin_request_strips_spoofed_headers_and_uses_runtime_url() {
        // Client sends an HTTPS spin-full-url but tries to spoof a downgraded
        // http scheme and an attacker-controlled host via forwarded headers.
        let mut req = request_with(&[
            ("spin-full-url", "https://www.publisher.example/cars/"),
            ("spin-client-addr", "203.0.113.7:5000"),
            ("x-forwarded-proto", "http"),
            ("x-forwarded-host", "evil.example"),
            ("forwarded", "host=evil.example;proto=http"),
        ]);

        normalize_request(&mut req);

        // Spoofable host overrides are stripped, leaving only the trusted Host.
        assert!(
            req.headers().get("x-forwarded-host").is_none(),
            "should strip spoofable x-forwarded-host"
        );
        assert!(
            req.headers().get("forwarded").is_none(),
            "should strip spoofable forwarded header"
        );
        // Spin runtime synthetic headers must not survive onto the request that
        // is forwarded to publisher origins or shared integration handlers.
        assert!(
            req.headers().get("spin-full-url").is_none(),
            "should strip the consumed spin-full-url synthetic header"
        );
        assert!(
            req.headers().get("spin-client-addr").is_none(),
            "should strip the spin-client-addr synthetic header"
        );
        assert_eq!(
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok()),
            Some("www.publisher.example"),
            "should set trusted Host from spin-full-url"
        );
        assert!(
            !req.headers().contains_key("x-forwarded-proto"),
            "should use typed runtime origin instead of a forwarding header"
        );
        // The path-only request URI is promoted to an absolute URI using the
        // trusted scheme+host so the first-party proxy/click/sign handlers can
        // parse it with url::Url::parse.
        assert_eq!(
            req.uri().to_string(),
            "https://www.publisher.example/",
            "should absolutize the path-only URI from the trusted scheme+host"
        );
    }

    #[test]
    fn normalize_spin_request_overrides_existing_host_with_trusted_authority() {
        // A client-supplied Host must not survive: it would diverge from the
        // absolute req.uri() rebuilt from the trusted spin-full-url host, leaving
        // RequestInfo and the first-party handlers reading different authorities.
        let mut req = request_with(&[
            ("host", "client-supplied.example"),
            ("spin-full-url", "https://www.publisher.example/cars/"),
        ]);

        normalize_request(&mut req);

        assert_eq!(
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok()),
            Some("www.publisher.example"),
            "should override an existing Host with the trusted spin-full-url host"
        );
        assert_eq!(
            req.uri().host(),
            Some("www.publisher.example"),
            "should rebuild the absolute URI with the same trusted authority"
        );
        assert_eq!(
            trusted_server_core::http_util::RequestInfo::from_request(
                &req,
                &ClientInfo::default(),
            )
            .expect("should accept trusted Spin request metadata")
            .scheme,
            "https",
            "should retain trusted runtime scheme"
        );
    }

    #[test]
    fn normalize_spin_request_uses_trusted_spin_full_url_over_client_duplicate() {
        // Spin appends its synthetic `spin-full-url` after the client's headers,
        // so a client that prepends its own `spin-full-url` would win a
        // first-match lookup. The trusted authority must come from the last
        // (Spin-supplied) value, never the attacker's prepended one.
        let mut builder = edgezero_core::http::request_builder()
            .method("GET")
            .uri("/first-party/proxy");
        // Attacker-controlled value first, trusted Spin synthetic value last.
        builder = builder.header("spin-full-url", "https://evil.example/attacker/path");
        builder = builder.header("spin-full-url", "https://www.publisher.example/cars/");
        let mut req = builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        SpinRequestContext::insert(
            &mut req,
            SpinRequestContext {
                client_addr: None,
                full_url: Some("https://evil.example/attacker/path".to_string()),
            },
        );

        normalize_request(&mut req);

        // All `spin-full-url` copies are stripped after normalization.
        assert!(
            req.headers().get("spin-full-url").is_none(),
            "should strip every spin-full-url copy"
        );
        // The trusted (last) authority wins — the attacker's host never appears.
        assert_eq!(
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok()),
            Some("www.publisher.example"),
            "should use the trusted last spin-full-url host, not the client duplicate"
        );
        assert_eq!(
            req.uri().to_string(),
            "https://www.publisher.example/first-party/proxy",
            "should rebuild the absolute URI from the trusted authority, not evil.example"
        );
        assert_eq!(
            SpinRequestContext::get(&req).and_then(|c| c.full_url.clone()),
            Some("https://www.publisher.example/cars/".to_string()),
            "should update SpinRequestContext::full_url from the trusted last spin-full-url"
        );
        assert_ne!(
            req.uri().host(),
            Some("evil.example"),
            "the attacker-supplied spin-full-url must never become the runtime authority"
        );
    }

    #[test]
    fn normalize_spin_request_uses_trusted_last_client_addr_over_client_duplicate() {
        // Spin appends its synthetic `spin-client-addr` after the client's
        // headers, but edgezero parses `SpinRequestContext::client_addr` from the
        // *first* match — so a client can prepend its own `spin-client-addr` and
        // forge the IP. Normalization must re-derive the trusted client IP from the
        // last (Spin-supplied) value so `build_runtime_services` cannot be fooled.
        let mut builder = edgezero_core::http::request_builder()
            .method("GET")
            .uri("/");
        // Attacker-controlled value first, trusted Spin synthetic value last.
        builder = builder.header("spin-client-addr", "203.0.113.10:1234");
        builder = builder.header("spin-client-addr", "198.51.100.7:5678");
        let mut req = builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        // Simulate edgezero's first-match population of the context.
        SpinRequestContext::insert(
            &mut req,
            SpinRequestContext {
                client_addr: Some("203.0.113.10".parse().expect("should parse spoofed IP")),
                full_url: None,
            },
        );

        normalize_request(&mut req);

        assert_eq!(
            SpinRequestContext::get(&req).and_then(|c| c.client_addr),
            Some("198.51.100.7".parse().expect("should parse trusted IP")),
            "should use the trusted last spin-client-addr, not the client-spoofed first value"
        );
        assert!(
            req.headers().get("spin-client-addr").is_none(),
            "should strip every spin-client-addr copy after capturing the trusted value"
        );
    }

    #[test]
    fn normalize_spin_request_strips_client_supplied_x_forwarded_for() {
        // A Spin client supplies a spoofed x-forwarded-for alongside the trusted
        // synthetic spin-client-addr. The spoofed forwarded-for must be stripped
        // so shared proxy/Prebid code cannot forward an attacker-controlled IP to
        // downstream services, while the trusted client IP is taken from
        // spin-client-addr.
        let mut req = request_with(&[
            ("spin-client-addr", "203.0.113.7:5000"),
            ("x-forwarded-for", "198.51.100.99, 10.0.0.1"),
        ]);

        normalize_request(&mut req);

        assert!(
            req.headers().get("x-forwarded-for").is_none(),
            "should strip client-supplied x-forwarded-for"
        );
        assert_eq!(
            SpinRequestContext::get(&req).and_then(|c| c.client_addr),
            Some("203.0.113.7".parse().expect("should parse trusted IP")),
            "trusted client IP must come from spin-client-addr, not the spoofed x-forwarded-for"
        );
    }

    #[test]
    fn scheme_host_from_spin_url_strips_userinfo() {
        assert_eq!(
            scheme_host_from_spin_url("https://user:pass@www.publisher.example/path"),
            Some(("https".to_string(), "www.publisher.example".to_string())),
            "should strip userinfo and keep only the host authority"
        );
    }

    #[test]
    fn routes_answer_startup_failure_with_generic_service_unavailable() {
        let Err(error) = TrustedServerApp::routes() else {
            panic!("should fail startup without the production config store");
        };

        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "should answer 503 for every startup failure"
        );
        assert_eq!(
            error.message(),
            "Service Unavailable",
            "should not echo the startup report to callers"
        );
    }
}
#[cfg(test)]
mod trace_dispatch_tests {
    use super::*;
    use futures::executor::block_on;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedClientIpConfig;
    use trusted_server_core::trace::TraceTerminalResponse;

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

    #[test]
    fn trace_dispatch_setup_uses_last_runtime_client_addr_without_normalizing_request() {
        let router = router(true);
        let mut request = edgezero_core::http::request_builder()
            .uri("/_ts/trace")
            .header("spin-client-addr", "203.0.113.22:1234")
            .header("spin-full-url", "https://untrusted.example.com/other")
            .body(edgezero_core::body::Body::empty())
            .expect("should build Spin setup");
        request.headers_mut().append(
            "spin-client-addr",
            HeaderValue::from_static("192.0.2.99:4321"),
        );
        SpinRequestContext::insert(
            &mut request,
            SpinRequestContext {
                client_addr: Some(
                    "203.0.113.22"
                        .parse()
                        .expect("should parse spoofed example IP"),
                ),
                full_url: Some("https://untrusted.example.com/other".to_owned()),
            },
        );
        let response = block_on(router.oneshot(request)).expect("should render setup locally");
        let bytes = response
            .into_body()
            .into_bytes()
            .expect("should buffer setup");
        let html = String::from_utf8(bytes.to_vec()).expect("should render UTF8 setup");
        assert!(
            html.contains("192.0.2.0/24"),
            "should project the trusted last synthetic address"
        );
        assert!(
            !html.contains("203.0.113."),
            "should not use the spoofed first SDK-context address"
        );
        assert!(
            !html.contains("untrusted.example.com"),
            "should never project or trust spin-full-url"
        );
    }

    #[test]
    fn trace_setup_honors_authenticated_client_ip_and_preserves_runtime_fallback() {
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
                request.headers_mut().append(
                    "spin-client-addr",
                    HeaderValue::from_static("203.0.113.22:1234"),
                );
                request.headers_mut().append(
                    "spin-client-addr",
                    HeaderValue::from_static("192.0.2.99:4321"),
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
