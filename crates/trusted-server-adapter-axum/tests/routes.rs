//! Integration tests for the Axum dev server.
//!
//! Each route test assembles the full `TrustedServerApp` from explicit test
//! settings, serves it through `EdgeZero`'s `AxumTestServer` on a loopback port,
//! and sends real HTTP/1 requests over TCP. The production listener therefore
//! applies its ingress and egress handling to every request. The startup test
//! runs the compiled `trusted-server-axum` binary.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use edgezero_adapter_axum::AXUM_PLATFORM;
use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use edgezero_adapter_axum::test_utils::AxumTestServer;
use edgezero_core::app::App;
use edgezero_core::router::RouterService;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::platform::HttpClient;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::nextjs_auction;
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

const LEGACY_ADMIN_DENY_METHODS: &[&str] =
    &["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// Environment variable from which the Axum binary reads its app config.
const AXUM_CONFIG_ENV: &str = "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG";

/// How long the startup test waits for the binary to exit.
const STARTUP_EXIT_DEADLINE: Duration = Duration::from_secs(10);

/// Settings the route tests build the application from.
///
/// The settings baked into the binary contain placeholder secrets that
/// `get_settings()` rejects by design, which would fail application startup.
fn test_settings() -> Settings {
    Settings::from_toml(
        r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "integration-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
        "#,
    )
    .expect("should parse route test settings")
}

fn test_router() -> RouterService {
    TrustedServerApp::routes_with_settings(test_settings())
        .expect("should build router from test settings")
}

fn registered_routes() -> Vec<(String, String)> {
    test_router()
        .routes()
        .into_iter()
        .map(|r| (r.method().to_string(), r.path().to_string()))
        .collect()
}

fn assert_route_registered(method: &str, path: &str) {
    let routes = registered_routes();
    assert!(
        routes.iter().any(|(m, p)| m == method && p == path),
        "{method} {path} must be explicitly registered; registered routes: {routes:?}"
    );
}

/// A `TrustedServerApp` served over loopback TCP, with an HTTP client for it.
///
/// The app has no `EdgeZero` stores attached. The Axum adapter reads its
/// config and secrets from environment-backed platform stores, and these tests
/// inject settings directly, so no route needs one.
struct TestServer {
    server: AxumTestServer,
    client: reqwest::Client,
    /// The script in router state, when the test supplied one.
    outbound: Option<ScriptedHttpClient>,
}

impl TestServer {
    /// Serves the app built from `settings`, with a script that fails every
    /// outbound request in router state, so no real `EdgeZero` transport starts.
    async fn start(settings: Settings) -> Self {
        Self::with_origin(settings, ScriptedHttpClient::forbidding()).await
    }

    /// Serves the app built from `settings`, with `outbound` in router state.
    async fn with_origin(settings: Settings, outbound: ScriptedHttpClient) -> Self {
        let router = TrustedServerApp::routes_with_settings_and_outbound_client(
            settings,
            HttpClient::with_client(outbound.clone()),
        )
        .expect("should build router from test settings");
        Self {
            outbound: Some(outbound),
            ..Self::serve(router).await
        }
    }

    /// Assembles `router` as the Trusted Server app and serves it on loopback port 0.
    async fn serve(router: RouterService) -> Self {
        let app = App::from_router::<TrustedServerApp>(router, AXUM_PLATFORM)
            .expect("should assemble the Trusted Server app");
        let config = AxumDevServerConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            enable_ctrl_c: false,
            ..AxumDevServerConfig::default()
        };
        let server = AxumTestServer::start(AxumDevServer::with_config(app, config))
            .await
            .expect("should start the Axum test server");
        // No proxy, redirect following, or transparent decompression: the
        // tests assert on exactly what the listener sends.
        let client = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("should build the test HTTP client");
        Self {
            server,
            client,
            outbound: None,
        }
    }

    /// Fails the test if the app sent any outbound request through its router-state script.
    fn assert_no_outbound_requests(&self) {
        self.outbound
            .as_ref()
            .expect("should serve with a router-state script")
            .assert_no_requests();
    }

    /// Serves the router built from the default test settings.
    async fn with_test_settings() -> Self {
        Self::start(test_settings()).await
    }

    /// Starts a request for `path` on the served app.
    fn request(&self, method: &str, path: &str) -> reqwest::RequestBuilder {
        let method =
            reqwest::Method::from_bytes(method.as_bytes()).expect("should parse the test method");
        self.client
            .request(method, format!("http://{}{path}", self.server.address()))
    }
}

/// Sends `request` and returns the response, with a bound on the wait.
async fn send(request: reqwest::RequestBuilder) -> reqwest::Response {
    tokio::time::timeout(Duration::from_secs(10), request.send())
        .await
        .expect("should answer promptly")
        .expect("should respond")
}

/// Returns a loopback port that is free at the time of the call.
fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("should reserve a loopback port")
        .local_addr()
        .expect("should read the reserved port")
        .port()
}

/// Waits up to `deadline` for `child` to exit, killing it if it does not.
///
/// Returns `None` when the child was still running at the deadline.
fn wait_for_exit(child: &mut Child, deadline: Duration) -> Option<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("should poll the Axum binary") {
            return Some(status);
        }
        if started.elapsed() >= deadline {
            child.kill().expect("should stop the Axum binary");
            child.wait().expect("should reap the Axum binary");
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reads everything left in a child's output pipe.
fn read_pipe(pipe: Option<impl std::io::Read>) -> String {
    let mut output = String::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_string(&mut output)
            .expect("should read the Axum binary output");
    }
    output
}

#[test]
fn axum_main_exits_non_zero_when_settings_fail() {
    // A clean environment keeps developer store-name overrides out of the
    // test. The config value is not a Trusted Server app-config envelope.
    let mut child = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"))
        .env_clear()
        .env(AXUM_CONFIG_ENV, "not-a-trusted-server-config-envelope")
        .env("PORT", free_loopback_port().to_string())
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("should spawn the Axum binary");

    let status = wait_for_exit(&mut child, STARTUP_EXIT_DEADLINE);
    let output = format!(
        "{}{}",
        read_pipe(child.stdout.take()),
        read_pipe(child.stderr.take())
    );

    let status = status.unwrap_or_else(|| {
        panic!("should exit instead of serving after a settings failure; output:\n{output}")
    });
    assert!(
        !status.success(),
        "should exit non-zero after a settings failure, got {status}; output:\n{output}"
    );
    assert!(
        output.contains("failed to parse Trusted Server app-config blob envelope"),
        "should log the settings failure report; output:\n{output}"
    );
    assert!(
        output.contains("trusted-server-adapter-axum failed to start"),
        "should log the startup failure before exiting; output:\n{output}"
    );
    assert!(
        !output.contains("Listening on"),
        "should not reach the listener; output:\n{output}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aps_profile_serves_renderer_through_adapter_fallback() {
    let mut settings = test_settings();
    settings.auction.providers.insert(
        "aps-main".parse().expect("should parse APS provider ID"),
        trusted_server_core::auction::ProviderConfig {
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
    let server = TestServer::start(settings).await;

    let response = send(server.request("GET", "/integrations/aps/renderer")).await;
    assert_eq!(response.status().as_u16(), 200);
    server.assert_no_outbound_requests();
}

/// Verify that every expected explicit route is registered in the route table.
///
/// Uses [`RouterService::routes()`] for introspection rather than checking
/// response status codes — wildcards (`/{*rest}`) can return non-404 even when
/// an explicit registration is missing, making status-based checks false positives.
#[test]
fn all_explicit_routes_are_registered() {
    let expected: &[(&str, &str)] = &[
        ("GET", "/.well-known/trusted-server.json"),
        ("POST", "/verify-signature"),
        ("POST", "/_ts/admin/keys/rotate"),
        ("POST", "/_ts/admin/keys/deactivate"),
        ("GET", "/_ts/admin/ec"),
        ("GET", "/_ts/admin/ec/{id}"),
        ("GET", "/_ts/admin/eids"),
        ("POST", "/admin/keys/rotate"),
        ("POST", "/admin/keys/deactivate"),
        ("POST", "/auction"),
        // SPA re-auction endpoint, plus its deprecated `/__ts/` alias. Both
        // paths are spelled out as literals rather than referencing
        // `PAGE_BIDS_PATH` / `PAGE_BIDS_LEGACY_PATH` so this test pins the
        // actual URL the tsjs client fetches — asserting a const against itself
        // would still pass if the const's value changed out from under the
        // client.
        ("GET", "/_ts/page-bids"),
        ("OPTIONS", "/_ts/page-bids"),
        ("GET", "/__ts/page-bids"),
        ("OPTIONS", "/__ts/page-bids"),
        ("GET", "/first-party/proxy"),
        ("GET", "/first-party/click"),
        ("GET", "/first-party/sign"),
        ("POST", "/first-party/sign"),
        ("GET", "/first-party/proxy-rebuild"),
        ("POST", "/first-party/proxy-rebuild"),
    ];

    for (method, path) in expected {
        assert_route_registered(method, path);
    }
}

/// Verify the legacy non-`/_ts` admin aliases ARE registered — to the local
/// deny handler — matching the Fastly and Cloudflare adapters.
///
/// The production basic-auth handler regex (`^/_ts/admin`) does not match
/// `/admin/keys/*`, so these aliases are not auth-gated. Leaving them unrouted
/// would let them fall through to the publisher fallback, which forwards the
/// request (including `Authorization` and key body) to the origin, leaking admin
/// credentials. Registering them to a local 404 deny fails closed instead. This
/// guard pins the cross-adapter agreement so the divergence cannot silently
/// reappear.
#[test]
fn legacy_admin_aliases_are_registered_to_local_deny() {
    let routes = registered_routes();
    for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        for method in LEGACY_ADMIN_DENY_METHODS {
            assert!(
                routes.iter().any(|(m, p)| m == method && p == path),
                "legacy {method} {path} must be registered (to the local deny) so it never reaches the publisher fallback; registered routes: {routes:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify routing (not business logic correctness)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_is_routed() {
    // Verifies the route exists — 5xx from missing signing keys is acceptable;
    // 404 is not (that would mean the route was not registered).
    let server = TestServer::with_test_settings().await;

    let resp = send(server.request("GET", "/.well-known/trusted-server.json")).await;

    assert_ne!(
        resp.status().as_u16(),
        404,
        "discovery endpoint must be routed"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_endpoint_is_routed() {
    let server = TestServer::with_test_settings().await;

    let resp = send(
        server
            .request("POST", "/verify-signature")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;

    assert_ne!(
        resp.status().as_u16(),
        404,
        "verify-signature must be routed"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_prefix_is_handled_not_5xx() {
    let server = TestServer::with_test_settings().await;

    // /static/tsjs= is a GET /{*rest} catch-all path. The handler returns 404
    // for an unknown hash, which is correct application behaviour (not a routing 404).
    // This test verifies the handler is reached (no 5xx/panic) and that routing works.
    let resp = send(server.request("GET", "/static/tsjs=0000000000000000")).await;
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_matching_hash_uses_s_maxage_fallback() {
    let server = TestServer::with_test_settings().await;
    let src = trusted_server_core::tsjs::tsjs_script_src(&["creative"]);

    let resp = send(server.request("GET", &src)).await;

    assert_eq!(
        resp.status().as_u16(),
        200,
        "matching TSJS hash should serve OK"
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=31536000, s-maxage=31536000, immutable"),
        "Axum adapter should render the portable s-maxage fallback"
    );
    assert!(
        resp.headers().get("surrogate-control").is_none(),
        "s-maxage fallback must not emit Fastly Surrogate-Control"
    );
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// Middleware tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_sets_geo_unavailable_header() {
    let server = TestServer::with_test_settings().await;

    let resp = send(
        server
            .request("POST", "/verify-signature")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;

    assert_eq!(
        resp.headers()
            .get("x-geo-info-available")
            .and_then(|v| v.to_str().ok()),
        Some("false"),
        "finalize middleware should set X-Geo-Info-Available: false on every response"
    );
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_returns_401() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/rotate")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must return 401 without credentials"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_ec_routes_return_501() {
    // The EC identity graph is Fastly KV backed, so the Axum dev server
    // answers the admin EC lookup routes locally with 501 instead of letting
    // them fall through to the publisher fallback.
    let sample_ec_id = format!("{}.abc123", "a".repeat(64));
    for path in [
        "/_ts/admin/ec".to_owned(),
        format!("/_ts/admin/ec/{sample_ec_id}"),
    ] {
        let server = TestServer::with_test_settings().await;
        let resp = send(
            server
                .request("GET", &path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw=="),
        )
        .await;
        assert_eq!(
            resp.status().as_u16(),
            501,
            "{path} should report that Axum EC lookup is unsupported"
        );
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_ec_route_without_credentials_returns_401() {
    let server = TestServer::with_test_settings().await;
    let resp = send(server.request("GET", "/_ts/admin/ec")).await;

    assert_eq!(resp.status().as_u16(), 401);
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "admin EC 401 should include the Basic authentication challenge"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_eids_route_returns_200() {
    // The EIDs echo is pure request inspection (no KV), so the dev server
    // serves the real handler.
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("GET", "/_ts/admin/eids")
            .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw=="),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        200,
        "/_ts/admin/eids should serve the real EIDs echo handler"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_diagnostic_fallback_is_denied_locally() {
    let ec_id = format!("{}.abc123", "a".repeat(64));
    let valid_paths = [
        "/_ts/admin/ec".to_owned(),
        format!("/_ts/admin/ec/{ec_id}"),
        "/_ts/admin/eids".to_owned(),
    ];
    let server = TestServer::with_test_settings().await;

    for path in valid_paths {
        for method in ["POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"] {
            let response = send(
                server
                    .request(method, &path)
                    .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body("sensitive-admin-body"),
            )
            .await;

            assert_eq!(response.status().as_u16(), 405);
            assert_eq!(
                response
                    .headers()
                    .get("allow")
                    .and_then(|v| v.to_str().ok()),
                Some("GET")
            );
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
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
        // Multi-encoded separators survive a single decode, so the reservation
        // decodes to a fixed point before the publisher fallback runs.
        "/admin%252Fkeys/rotate".to_owned(),
        "/_ts%252Fadmin/ec".to_owned(),
    ] {
        for method in ["GET", "POST"] {
            let response = send(
                server
                    .request(method, &path)
                    .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .body("sensitive-admin-body"),
            )
            .await;

            assert_eq!(response.status().as_u16(), 404);
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("no-store")
            );
        }
    }
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_admin_aliases_denied_locally_not_proxied_to_publisher() {
    // Regression for the credential-leak finding: the production basic-auth regex
    // `^/_ts/admin` does not match `/admin/keys/*`, so those aliases are not
    // auth-gated. Any publisher-fallback method carrying an `Authorization`
    // header must be denied locally with 404, never proxied to the publisher
    // origin (which would leak the admin credentials and key body). A
    // publisher-fallback proxy without a backend would surface as a 5xx, so 404
    // proves the local deny ran.
    let server = TestServer::with_test_settings().await;
    for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        for method in LEGACY_ADMIN_DENY_METHODS {
            let resp = send(
                server
                    .request(method, path)
                    .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                    .header("content-type", "application/json")
                    .body("{\"key_id\":\"leak-me\"}"),
            )
            .await;
            assert_eq!(
                resp.status().as_u16(),
                404,
                "legacy {method} {path} with Authorization must be denied locally (404), not proxied to publisher"
            );
        }
    }
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_includes_www_authenticate_header() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/rotate")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "should be 401 before checking header"
    );
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "401 response must include WWW-Authenticate header"
    );
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .expect("should have www-authenticate header")
        .to_str()
        .expect("should be valid UTF-8");
    assert!(
        www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme, got: {www_auth}"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_with_wrong_credentials_returns_401() {
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong-password");
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/rotate")
            .header("content-type", "application/json")
            .header("authorization", format!("Basic {creds}"))
            .body("{}"),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must reject wrong credentials with 401"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_does_not_require_auth() {
    let server = TestServer::with_test_settings().await;
    let resp = send(server.request("GET", "/.well-known/trusted-server.json")).await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/.well-known/trusted-server.json must not require auth"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_endpoint_does_not_require_auth() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/auction")
            .header("content-type", "application/json")
            .body(r#"{"adUnits":[]}"#),
    )
    .await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/auction must not apply admin basic-auth gate"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_returns_non_404_non_5xx() {
    let server = TestServer::with_test_settings().await;

    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/rotate")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;
    let status = resp.status().as_u16();

    assert_ne!(status, 404, "admin route must be routed");
    // The auth gate short-circuits with 401 before the handler; only an
    // unhandled 500 indicates a panic or missing handler.
    assert_ne!(status, 500, "admin route must not panic: got {status}");
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// Admin key route full path coverage
// ---------------------------------------------------------------------------

// Exercises the auth-fail path with a realistic key body (complements the
// generic `admin_route_without_credentials_returns_401` above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_auth_fail_returns_401() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/rotate")
            .header("content-type", "application/json")
            .body(r#"{"keyId":"test-key"}"#),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/rotate without credentials must return 401"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_auth_fail_returns_401() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/_ts/admin/keys/deactivate")
            .header("content-type", "application/json")
            .body(r#"{"keyId":"test-key"}"#),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/deactivate without credentials must return 401"
    );
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// First-party route smoke tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_is_routed() {
    let server = TestServer::with_test_settings().await;
    let resp = send(server.request("GET", "/first-party/proxy")).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy must be routed"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_click_is_routed() {
    let server = TestServer::with_test_settings().await;
    let resp = send(server.request("GET", "/first-party/click")).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/click must be routed"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_is_routed() {
    let server = TestServer::with_test_settings().await;
    let resp = send(server.request("GET", "/first-party/sign")).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "GET /first-party/sign must be routed"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_is_routed() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/first-party/sign")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "POST /first-party/sign must be routed"
    );
    server.assert_no_outbound_requests();
}

/// The listener hands every POST body to the handler as a stream, so a signed
/// URL in the answer proves the bounded reader collected it end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_signs_the_url_in_its_body() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/first-party/sign")
            .header("content-type", "application/json")
            .body(r#"{"url":"https://cdn.example.com/pixel.gif?a=1"}"#),
    )
    .await;

    let status = resp.status().as_u16();
    let body = resp.text().await.expect("should read the sign response");
    assert_eq!(status, 200, "POST /first-party/sign should sign: {body}");
    assert!(
        body.contains(r#""base":"https://cdn.example.com/pixel.gif""#),
        "should sign the URL from the request body: {body}"
    );
    assert!(
        body.contains(r#""href":"https://test-publisher.example.com/first-party/proxy?tsurl="#),
        "should return the first-party proxy URL on the default public origin: {body}"
    );
    server.assert_no_outbound_requests();
}

/// A JSON auction body reaches the handler instead of being read as empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_post_parses_its_json_body() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/auction")
            .header("content-type", "application/json")
            .body(
                r#"{"adUnits":[{"code":"slot-1","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}"#,
            ),
    )
    .await;

    let status = resp.status().as_u16();
    let body = resp.text().await.expect("should read the auction response");
    assert_eq!(status, 200, "POST /auction should parse its body: {body}");
    assert!(
        body.contains(r#""id":""#),
        "should return an OpenRTB response with an auction id: {body}"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_is_routed() {
    let server = TestServer::with_test_settings().await;
    let resp = send(
        server
            .request("POST", "/first-party/proxy-rebuild")
            .header("content-type", "application/json")
            .body("{}"),
    )
    .await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy-rebuild must be routed"
    );
    server.assert_no_outbound_requests();
}

/// Regression test: a Next.js navigation with a pending auction must buffer to
/// the structural body close. The Flight payload carries a literal `</body>`, so
/// a parser-blind seam would inject bids early and split the RSC data.
///
/// This covers the buffered path only. This adapter routes navigations through
/// `buffer_publisher_response_async`, which resolves the body close without the
/// deferred inline seam marker, so the streaming seam token is exercised by the
/// Fastly adapter alone and not by this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nextjs_auction_output_holds_until_the_structural_body_close() {
    let client = nextjs_auction::NextJsAuctionOrigin::default();
    let router = TrustedServerApp::routes_with_settings_and_services(
        nextjs_auction::settings(),
        nextjs_auction::services(&client),
    )
    .expect("should build router with fixture services");
    let server = TestServer::serve(router).await;

    let response = send(
        server
            .request("GET", "/article")
            .header("host", "test-publisher.example.com")
            .header("accept", "text/html"),
    )
    .await;
    assert_eq!(response.status(), 200, "should serve fixture HTML");
    let body = response
        .bytes()
        .await
        .expect("should buffer adapter output");
    let html = String::from_utf8(body.to_vec()).expect("should emit UTF-8 HTML");

    assert_eq!(
        client.auction_requests(),
        1,
        "should dispatch exactly one auction"
    );
    let bids = html
        .find("var b=JSON.parse(")
        .unwrap_or_else(|| panic!("should inject auction bids: {html}"));
    let close = html
        .rfind("</body>")
        .unwrap_or_else(|| panic!("should retain structural close: {html}"));
    assert!(
        bids < close && html[bids..].ends_with("</script></body></html>"),
        "should inject bids immediately before the structural body close: {html}"
    );
    // The fixture splits the URL across two scripts, so the rewritten payload
    // never appears contiguously. Assert on the recomputed `T` length instead:
    // it shrinks only when the origin URL was actually replaced.
    assert!(
        html.contains(&nextjs_auction::expected_rewritten_flight_header()),
        "should recompute the Flight T length after rewriting the URL: {html}"
    );
    assert!(
        !html.contains(nextjs_auction::ORIGIN_HOST),
        "should leave no origin host in the rewritten payload: {html}"
    );
    assert!(
        !html.contains("__ts_rsc_") && !html.contains("<!--ts-inline-body-close-"),
        "should not leak generated placeholders: {html}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nextjs_gtm_rejects_malformed_request_hosts() {
    // A hostile `Host` is rejected. Unauthenticated forwarded fields are
    // stripped before routing, so their hostile values never reach rewriting.
    for header in ["host", "x-forwarded-host", "forwarded"] {
        let origin = nextjs_auction::NextJsAuctionOrigin::with_html_response(
            &nextjs_auction::script_composition_html(),
            false,
            true,
        );
        let router = TrustedServerApp::routes_with_settings_and_services(
            nextjs_auction::script_composition_settings(),
            nextjs_auction::services(&origin),
        )
        .expect("should build script composition router");
        let server = TestServer::serve(router).await;
        let hostile_host = "example.com</script><script>alert(1)</script>";
        let value = if header == "forwarded" {
            format!("host=\"{hostile_host}\"")
        } else {
            hostile_host.to_owned()
        };
        let mut request = server.request("GET", "/script-composition");
        if header != "host" {
            request = request.header("host", nextjs_auction::PUBLISHER_HOST);
        }
        let response = send(request.header(header, value)).await;
        let expected_status = if header == "host" { 400 } else { 200 };
        assert_eq!(
            response.status().as_u16(),
            expected_status,
            "should reject malformed Host and ignore unauthenticated {header}"
        );
        let body = response
            .bytes()
            .await
            .expect("should consume hostile host response");
        assert!(
            !String::from_utf8_lossy(&body).contains(hostile_host),
            "should not emit hostile {header} bytes in the response"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nextjs_gtm_accepts_local_request_hosts_and_ports() {
    for host in [
        "localhost:3000",
        "example.com:8443",
        "[::1]:3000",
        "my_service:8080",
        "_service_.example.com:8443",
    ] {
        let origin = nextjs_auction::NextJsAuctionOrigin::with_html_response(
            &nextjs_auction::script_composition_html(),
            false,
            true,
        );
        let router = TrustedServerApp::routes_with_settings_and_services(
            nextjs_auction::script_composition_settings(),
            nextjs_auction::services(&origin),
        )
        .expect("should build script composition router");
        let server = TestServer::serve(router).await;
        let response = send(
            server
                .request("GET", "/script-composition")
                .header("host", host),
        )
        .await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "should accept host: {host}"
        );
        let body = response
            .bytes()
            .await
            .expect("should consume local host response");
        let html = String::from_utf8(body.to_vec()).expect("should emit UTF-8 HTML");
        assert!(
            html.contains(&format!("http://{host}/app")),
            "should preserve host and port in rewritten Flight URL: {host}"
        );
        assert!(
            !html.contains("__ts_rsc_"),
            "should resolve every local host payload"
        );
        assert!(
            html.ends_with("</body></html>"),
            "should complete local response"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nextjs_gtm_html_response_is_complete() {
    for gzip in [false, true] {
        let origin = nextjs_auction::NextJsAuctionOrigin::with_html_response(
            &nextjs_auction::script_composition_html(),
            gzip,
            false,
        );
        let router = TrustedServerApp::routes_with_settings_and_services(
            nextjs_auction::script_composition_settings(),
            nextjs_auction::services(&origin),
        )
        .expect("should build mixed integration router");
        let server = TestServer::serve(router).await;
        let response = send(
            server
                .request("GET", "/article")
                .header("host", nextjs_auction::PUBLISHER_HOST)
                .header("accept", "text/html"),
        )
        .await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "should return a successful composed publisher response"
        );
        assert_eq!(
            response
                .headers()
                .get("content-encoding")
                .map(|value| value.to_str().expect("should have ASCII coding")),
            gzip.then_some("gzip"),
            "should retain the requested response compression"
        );
        let body = response
            .bytes()
            .await
            .expect("should consume entire adapter body");
        nextjs_auction::assert_script_composition_response(&body, gzip);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_reports_git_version() {
    let server = TestServer::with_test_settings().await;

    let response = send(server.request("GET", "/health")).await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "should return 200 on /health"
    );
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|v| v.to_str().ok()),
        trusted_server_core::constants::TS_GIT_VERSION,
        "should report the compiled-in git version on /health"
    );
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// Operator response headers that egress framing owns
// ---------------------------------------------------------------------------

/// The route test settings with `headers` as the operator's `response_headers`.
fn settings_with_response_headers(headers: &[(&str, &str)]) -> Settings {
    let mut settings = test_settings();
    settings.response_headers = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    settings
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_skips_malformed_connection_entry_and_serves_real_status() {
    // A live config that predates validation can hold any of these. `x example`
    // is not a field name, so egress would refuse it with a framing 500.
    for value in ["keep-alive,", "", "x example"] {
        let server =
            TestServer::start(settings_with_response_headers(&[("connection", value)])).await;

        let response = send(server.request("GET", "/health")).await;

        assert_eq!(
            response.status().as_u16(),
            200,
            "should skip the operator `connection: {value}` entry and serve the real status"
        );
        server.assert_no_outbound_requests();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_skip_of_connection_x_example_serves_x_example() {
    let server = TestServer::start(settings_with_response_headers(&[
        ("connection", "x-example"),
        ("x-example", "example-value"),
    ]))
    .await;

    let response = send(server.request("GET", "/health")).await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "should serve the real status"
    );
    // Skipping the `connection` entry also drops its nomination, so egress no
    // longer removes the field it named.
    assert_eq!(
        response
            .headers()
            .get("x-example")
            .and_then(|value| value.to_str().ok()),
        Some("example-value"),
        "should serve x-example once the connection entry naming it is skipped"
    );
    server.assert_no_outbound_requests();
}

// ---------------------------------------------------------------------------
// Synthesized headers on EdgeZero's canonical responses
// ---------------------------------------------------------------------------

/// Sends `request` as raw bytes to `address` and returns the whole response.
///
/// For request targets the HTTP client cannot express, such as asterisk-form.
/// The request must ask the server to close the connection.
fn send_raw(address: SocketAddr, request: &'static str) -> String {
    let mut stream = TcpStream::connect(address).expect("should connect to the Axum test server");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("should set a read timeout");
    stream
        .write_all(request.as_bytes())
        .expect("should send the raw request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("should read the raw response");
    String::from_utf8(response).expect("should answer with a UTF-8 response head")
}

/// Returns the value of header `name` from a raw HTTP/1 response.
fn raw_header<'response>(response: &'response str, name: &str) -> Option<&'response str> {
    response
        .split("\r\n\r\n")
        .next()
        .into_iter()
        .flat_map(|head| head.split("\r\n").skip(1))
        .filter_map(|line| line.split_once(':'))
        .find(|(field, _)| field.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

/// Returns the git version compiled into this build.
///
/// # Panics
///
/// Panics when the build has none, so a version assertion cannot pass on an
/// absent header.
fn compiled_git_version() -> &'static str {
    trusted_server_core::constants::TS_GIT_VERSION
        .expect("should compile a git version into the test build")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_not_found_carries_x_ts_version() {
    let server = TestServer::with_test_settings().await;
    let address = server.server.address();

    // The publisher fallback claims every origin-form path, so only an
    // asterisk-form target has no route and gets EdgeZero's canonical 404.
    let response = tokio::task::spawn_blocking(move || {
        send_raw(
            address,
            "OPTIONS * HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
    })
    .await
    .expect("should finish the raw request");

    assert!(
        response.starts_with("HTTP/1.1 404 "),
        "should answer an unrouted target with the canonical 404; response:\n{response}"
    );
    assert_eq!(
        raw_header(&response, "x-ts-version"),
        Some(compiled_git_version()),
        "should add the compiled-in git version to the canonical 404; response:\n{response}"
    );
    server.assert_no_outbound_requests();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_method_not_allowed_carries_allow_and_x_ts_version() {
    let server = TestServer::with_test_settings().await;

    let response = send(server.request("TRACE", "/auction")).await;

    assert_eq!(
        response.status().as_u16(),
        405,
        "should answer an unrouted method on a known path with the canonical 405"
    );
    assert_eq!(
        response
            .headers()
            .get("allow")
            .and_then(|value| value.to_str().ok()),
        Some("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"),
        "should list the path's methods in sorted order"
    );
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|value| value.to_str().ok()),
        Some(compiled_git_version()),
        "should add the compiled-in git version to the canonical 405"
    );
    server.assert_no_outbound_requests();
}

/// Origin of [`test_settings`].
const ORIGIN: &str = "https://origin.test-publisher.example.com";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_body_at_cap_passes_and_one_over_fails_502_axum() {
    // Axum's cap is `max_buffered_body_bytes` alone, 16 MiB by default.
    let cap = test_settings().publisher.max_buffered_body_bytes;
    let origin = ScriptedHttpClient::new(&TestClock::new());
    for (path, len) in [("/at-cap.bin", cap), ("/over-cap.bin", cap + 1)] {
        origin.respond_to(
            edgezero_core::http::Method::GET,
            &format!("{ORIGIN}{path}"),
            ScriptedReply::status(edgezero_core::http::StatusCode::OK)
                .header("content-type", "application/octet-stream")
                .body(vec![b'x'; len]),
        );
    }
    let server = TestServer::with_origin(test_settings(), origin).await;

    let at_cap = send(server.request("GET", "/at-cap.bin")).await;
    assert_eq!(at_cap.status(), 200, "should serve a raw body at the cap");
    let body = at_cap.bytes().await.expect("should read the at-cap body");
    assert_eq!(body.len(), cap, "should serve every byte at the cap");

    let over_cap = send(server.request("GET", "/over-cap.bin")).await;
    assert_eq!(
        over_cap.status(),
        502,
        "should fail a raw body one byte over the cap with 502 before egress"
    );
}
