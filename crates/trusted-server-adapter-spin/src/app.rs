use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use edgezero_adapter_spin::SpinRequestContext;
use edgezero_core::app::Hooks;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Method, Request, Response, StatusCode, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::{AuctionOrchestrator, build_orchestrator};
use trusted_server_core::ec::EcContext;
use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
use trusted_server_core::http_util::sanitize_forwarded_headers;
use trusted_server_core::integrations::{IntegrationRegistry, ProxyDispatchInput};
use trusted_server_core::proxy::{
    handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild,
    handle_first_party_proxy_sign,
};
use trusted_server_core::publisher::{
    PublisherResponse, buffer_publisher_response, handle_publisher_request, handle_tsjs_dynamic,
};
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings;

use crate::middleware::{AuthMiddleware, FinalizeResponseMiddleware, NormalizeMiddleware};
use crate::platform::build_runtime_services;

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state built once at startup and shared across all requests.
pub struct AppState {
    settings: Arc<Settings>,
    orchestrator: Arc<AuctionOrchestrator>,
    registry: Arc<IntegrationRegistry>,
}

/// Build the application state, loading settings and constructing all per-application components.
///
/// # Errors
///
/// Returns an error when settings, the auction orchestrator, or the integration
/// registry fail to initialise.
fn build_state() -> Result<Arc<AppState>, Report<TrustedServerError>> {
    let settings = get_settings()?;
    build_state_with_settings(settings)
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
    let orchestrator = build_orchestrator(&settings)?;
    let registry = IntegrationRegistry::new(&settings)?;

    Ok(Arc::new(AppState {
        settings: Arc::new(settings),
        orchestrator: Arc::new(orchestrator),
        registry: Arc::new(registry),
    }))
}

// ---------------------------------------------------------------------------
// Publisher response helper
// ---------------------------------------------------------------------------

/// Collapse a [`PublisherResponse`] into a plain [`Response`].
///
/// Delegates to the shared [`buffer_publisher_response`], which enforces
/// `settings.publisher.max_buffered_body_bytes` so a large processable
/// origin response fails safely instead of exhausting the Wasm heap.
fn resolve_publisher_response(
    publisher_response: PublisherResponse,
    settings: &Settings,
    registry: &IntegrationRegistry,
) -> Result<Response, Report<TrustedServerError>> {
    buffer_publisher_response(publisher_response, settings, registry)
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
fn named_fallback_paths() -> [(&'static str, &'static [Method]); 9] {
    [
        ("/.well-known/trusted-server.json", &[Method::GET]),
        ("/verify-signature", &[Method::POST]),
        ("/_ts/admin/keys/rotate", &[Method::POST]),
        ("/_ts/admin/keys/deactivate", &[Method::POST]),
        ("/auction", &[Method::POST]),
        ("/first-party/proxy", &[Method::GET]),
        ("/first-party/click", &[Method::GET]),
        ("/first-party/sign", &[Method::GET, Method::POST]),
        ("/first-party/proxy-rebuild", &[Method::POST]),
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
// sanitization) means the value `detect_request_scheme`/`extract_request_host`
// read originates from the trusted runtime URL rather than the client.
//
// Spin builds the core request URI from `IncomingRequest::path_with_query()`, so
// it is path-only (e.g. "/first-party/proxy?..."). The shared first-party
// proxy/click/sign handlers parse `req.uri().to_string()` with `url::Url::parse`,
// which rejects a relative path. Rebuilding an absolute URI from the trusted
// scheme+host lets those handlers validate the signed target instead of failing
// with "Invalid URL".
pub(crate) fn normalize_spin_request(req: &mut Request) {
    sanitize_forwarded_headers(req);

    // Re-derive the trusted client IP from the *last* `spin-client-addr` header
    // and overwrite `SpinRequestContext`, which `build_runtime_services` reads
    // for `ClientInfo::client_ip`. edgezero populates that context from the
    // *first* `spin-client-addr` match, but Spin's WASI HTTP bridge appends its
    // synthetic header *after* the original client headers — so a client can send
    // its own `spin-client-addr` ahead of Spin's and forge the IP that EC ID
    // hashing, `/auction` `device.ip`, and integration `X-Forwarded-For` consume.
    // Selecting the last occurrence pins the value to the trusted runtime one.
    let trusted_client_addr = req
        .headers()
        .get_all("spin-client-addr")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_addr);
    let existing_full_url = SpinRequestContext::get(req).and_then(|c| c.full_url.clone());
    SpinRequestContext::insert(
        req,
        SpinRequestContext {
            client_addr: trusted_client_addr,
            full_url: existing_full_url,
        },
    );

    // Spin's WASI HTTP bridge copies synthetic `spin-client-addr` and
    // `spin-full-url` headers onto the core request. They are runtime metadata,
    // not client- or application-supplied headers. The trusted `spin-client-addr`
    // value is captured into `SpinRequestContext` above; remove every copy here so
    // the shared publisher/integration handlers neither forward them to publisher
    // origins nor mistake them for client-controlled input. `spin-full-url` is
    // consumed below to derive the trusted scheme/host.
    req.headers_mut().remove("spin-client-addr");

    // Spin's WASI HTTP bridge appends its synthetic `spin-full-url` *after* the
    // original client headers, so when duplicate names are present the trusted
    // runtime value is the last one. `HeaderMap::remove` returns the *first*
    // value, which a client could control by sending its own `spin-full-url`
    // ahead of Spin's synthetic one — letting it choose the host/scheme consumed
    // by publisher HTML rewriting, integration URL rewriting, and request
    // signing. Select the last occurrence as the trusted authority, then strip
    // every copy so none reach publisher origins or the shared handlers.
    let trusted_full_url = req
        .headers()
        .get_all("spin-full-url")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    req.headers_mut().remove("spin-full-url");
    let Some((scheme, host)) = trusted_full_url
        .as_deref()
        .and_then(scheme_host_from_spin_url)
    else {
        return;
    };

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
        req.headers_mut().insert(header::HOST, hval);
    }

    // Without a trusted scheme signal, detect_request_scheme defaults to http and
    // rewrites HTTPS URLs as http.
    if let Ok(pval) = HeaderValue::from_str(&scheme) {
        req.headers_mut().insert("x-forwarded-proto", pval);
    }

    // Promote the path-only URI to an absolute one so the shared first-party
    // proxy/click/sign handlers can parse `req.uri()` as a full URL.
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_string());
    if let Ok(uri) = format!("{scheme}://{host}{path_and_query}").parse() {
        *req.uri_mut() = uri;
    }
}

// ---------------------------------------------------------------------------
// Health probe
// ---------------------------------------------------------------------------

/// Builds the `GET /health` liveness response (`200 ok`, `text/plain`).
///
/// Mirrors the Fastly entry point and Axum adapter so deployments reusing
/// Trusted Server health probes see identical behaviour on Spin. Served from
/// both the healthy router and the startup-error fallback so the probe answers
/// even before (or when) application state is usable, leaving Spin's
/// platform-provided `/.well-known/spin/health` untouched.
fn health_response() -> Response {
    let mut resp = Response::new(edgezero_core::body::Body::from("ok"));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    resp
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

/// Returns a [`RouterService`] that responds to every route with a generic
/// 503 Service Unavailable. The startup error is logged but not echoed in the
/// response body so that deployment state is not leaked to anonymous callers.
fn startup_error_router(e: &Report<TrustedServerError>) -> RouterService {
    log::error!("startup failed, serving error fallback: {:?}", e);

    let handler = |_ctx: RequestContext| {
        let body = edgezero_core::body::Body::from("Service Unavailable\n");
        let mut resp = Response::new(body);
        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        async move { Ok::<Response, EdgeError>(resp) }
    };

    // Cover the full publisher fallback method set (GET, POST, HEAD, OPTIONS,
    // PUT, PATCH, DELETE) so degraded behaviour stays consistent with the
    // healthy router: every method on `/` and `/{*rest}` returns the generic
    // 503 instead of a router-level 405 for HEAD/OPTIONS/PATCH.
    let mut builder = RouterService::builder().middleware(FinalizeResponseMiddleware::new(
        Arc::new(Settings::default()),
    ));
    // Keep the liveness probe answering 200 even while state construction is
    // failing, matching the Fastly/Axum health behaviour.
    builder = builder.get("/health", |_ctx: RequestContext| async {
        Ok::<Response, EdgeError>(health_response())
    });
    for method in publisher_fallback_methods() {
        builder = builder.route("/", method.clone(), handler);
        builder = builder.route("/{*rest}", method, handler);
    }
    builder.build()
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
}

fn build_router(state: &Arc<AppState>) -> RouterService {
    {
        let state = Arc::clone(state);

        // /.well-known/trusted-server.json
        let s = Arc::clone(&state);
        let discovery_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
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
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_verify_signature(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /_ts/admin/keys/rotate
        let s = Arc::clone(&state);
        let rotate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_rotate_key(&s.settings, &services, req)
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // /_ts/admin/keys/deactivate
        let s = Arc::clone(&state);
        let deactivate_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
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
                let services = build_runtime_services(&ctx);
                // Request normalization (forwarded-header stripping, trusted
                // Host/scheme/client-IP derivation) is applied centrally by
                // `NormalizeMiddleware` before this handler runs, so the signed
                // OpenRTB metadata that auction signing derives from
                // `RequestInfo::from_request` uses the trusted runtime authority.
                let req = ctx.into_request();
                let ec_context = EcContext::default();
                Ok(handle_auction(
                    &s.settings,
                    &s.orchestrator,
                    None,
                    None,
                    &ec_context,
                    &services,
                    req,
                )
                .await
                .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // GET /first-party/proxy
        let s = Arc::clone(&state);
        let fp_proxy_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
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
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_click(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };

        // GET + POST /first-party/sign — identical handler, cloned for both bindings
        let s = Arc::clone(&state);
        let fp_sign_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(handle_first_party_proxy_sign(&s.settings, &services, req)
                    .await
                    .unwrap_or_else(|e| http_error(&e)))
            }
        };
        let fp_sign_post_handler = fp_sign_handler.clone();

        // /first-party/proxy-rebuild
        let s = Arc::clone(&state);
        let fp_rebuild_handler = move |ctx: RequestContext| {
            let s = Arc::clone(&s);
            async move {
                let services = build_runtime_services(&ctx);
                let req = ctx.into_request();
                Ok(
                    handle_first_party_proxy_rebuild(&s.settings, &services, req)
                        .await
                        .unwrap_or_else(|e| http_error(&e)),
                )
            }
        };

        // Shared fallback dispatch: routes to tsjs (GET only), integration proxy, or publisher.
        async fn dispatch(
            state: Arc<AppState>,
            ctx: RequestContext,
        ) -> Result<Response, EdgeError> {
            let services = build_runtime_services(&ctx);
            let req = ctx.into_request();

            let path = req.uri().path().to_owned();
            let method = req.method().clone();

            // Dynamic tsjs serving is GET-only; other methods fall through to the
            // integration/publisher fallback.
            let result = if method == Method::GET && path.starts_with("/static/tsjs=") {
                handle_tsjs_dynamic(&req, &state.registry)
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
                handle_publisher_request(&state.settings, &state.registry, &services, req)
                    .await
                    .and_then(|pr| resolve_publisher_response(pr, &state.settings, &state.registry))
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

        let mut builder = RouterService::builder()
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
            // The legacy non-`/_ts` aliases (`/admin/keys/*`) are deliberately not
            // registered: the production handler regex `^/_ts/admin` does not match
            // them, so registering them as admin routes would expose the key
            // handlers to unauthenticated callers. Unrouted, they fall through to
            // the publisher fallback like any other unknown path.
            .post("/_ts/admin/keys/rotate", rotate_handler)
            .post("/_ts/admin/keys/deactivate", deactivate_handler)
            .post("/auction", auction_handler)
            .get("/first-party/proxy", fp_proxy_handler)
            .get("/first-party/click", fp_click_handler)
            .get("/first-party/sign", fp_sign_handler)
            .post("/first-party/sign", fp_sign_post_handler)
            .post("/first-party/proxy-rebuild", fp_rebuild_handler);

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

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        normalize_spin_request(&mut req);

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
        // The only surviving x-forwarded-proto is the trusted scheme we injected.
        assert_eq!(
            req.headers()
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok()),
            Some("https"),
            "should override spoofed scheme with the trusted https scheme"
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

        normalize_spin_request(&mut req);

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
            req.headers()
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok()),
            Some("https"),
            "should still inject the trusted scheme"
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

        normalize_spin_request(&mut req);

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

        normalize_spin_request(&mut req);

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
    fn scheme_host_from_spin_url_strips_userinfo() {
        assert_eq!(
            scheme_host_from_spin_url("https://user:pass@www.publisher.example/path"),
            Some(("https".to_string(), "www.publisher.example".to_string())),
            "should strip userinfo and keep only the host authority"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_error_router_serves_503_for_all_fallback_methods() {
        // The degraded router must answer every publisher-fallback method
        // (including HEAD/OPTIONS/PATCH) on both "/" and nested paths with the
        // generic 503, never a router-level 405, so startup-failure behaviour
        // stays consistent with the healthy router.
        let report = Report::new(TrustedServerError::BadRequest {
            message: "startup failure".to_string(),
        });
        let router = startup_error_router(&report);

        for method in ["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"] {
            for path in ["/", "/some/nested/page"] {
                let req = edgezero_core::http::request_builder()
                    .method(method)
                    .uri(path)
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build request");
                let status = router.oneshot(req).await.status().as_u16();
                assert_eq!(
                    status, 503,
                    "{method} {path} must return 503 from the startup fallback, got {status}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_error_router_answers_health_with_200() {
        // The liveness probe must keep returning 200 even while application state
        // construction is failing, matching the Fastly/Axum health behaviour.
        let report = Report::new(TrustedServerError::BadRequest {
            message: "startup failure".to_string(),
        });
        let router = startup_error_router(&report);

        let req = edgezero_core::http::request_builder()
            .method("GET")
            .uri("/health")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = router.oneshot(req).await;

        assert_eq!(
            resp.status().as_u16(),
            200,
            "GET /health must return 200 from the startup fallback"
        );
        let body = resp.into_body().into_bytes();
        assert_eq!(
            &body[..],
            b"ok",
            "startup-fallback health body should be `ok`"
        );
    }
}
