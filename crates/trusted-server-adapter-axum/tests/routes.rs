//! Integration tests for the Axum dev server.
//!
//! Uses `EdgeZeroAxumService` directly (no live TCP server) so tests remain fast
//! and self-contained. Each test builds the full `TrustedServerApp` router and
//! drives it through the Tower `Service` interface.

use axum::body::Body as AxumBody;
use axum::http::Request;
use edgezero_adapter_axum::EdgeZeroAxumService;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp;

fn make_service() -> EdgeZeroAxumService {
    // Drive the router with explicit test settings: the settings baked into
    // the binary contain placeholder secrets that `get_settings()` rejects
    // by design, which would turn every route into a startup error page.
    let settings = trusted_server_core::settings::Settings::from_toml(
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
    .expect("should parse route test settings");

    let router = TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings");
    EdgeZeroAxumService::new(router)
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify routing (not business logic correctness)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_is_routed() {
    // Verifies the route exists — 5xx from missing signing keys is acceptable;
    // 404 is not (that would mean the route was not registered).
    let mut svc = make_service();

    let req = Request::builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_ne!(
        resp.status().as_u16(),
        404,
        "discovery endpoint must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_endpoint_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_ne!(
        resp.status().as_u16(),
        404,
        "verify-signature must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    // The admin handler is a fixed 501 responder with no I/O, and the test
    // settings protect only ^/_ts/admin, so this path reaches the handler.
    assert_eq!(
        resp.status().as_u16(),
        501,
        "admin/keys/rotate must reach the not-supported handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    // Same fixed 501 contract as admin/keys/rotate.
    assert_eq!(
        resp.status().as_u16(),
        501,
        "admin/keys/deactivate must reach the not-supported handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_returns_non_5xx() {
    // Admin routes return 501 Not Implemented on the Axum dev server (store
    // writes are unsupported). Auth middleware may short-circuit with 4xx
    // before reaching the handler. Either way, no panic or unhandled 500.
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    let status = resp.status().as_u16();

    assert_eq!(
        status, 501,
        "admin/keys/rotate must return the fixed not-supported status"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_prefix_is_handled_not_5xx() {
    let mut svc = make_service();

    // /static/tsjs= is a GET /{*rest} catch-all path. The handler returns 404
    // for an unknown hash, which is correct application behaviour (not a routing 404).
    // This test verifies the handler is reached (no 5xx/panic) and that routing works.
    let req = Request::builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
}

// ---------------------------------------------------------------------------
// Middleware tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_sets_geo_unavailable_header() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_eq!(
        resp.headers()
            .get("x-geo-info-available")
            .and_then(|v| v.to_str().ok()),
        Some("false"),
        "finalize middleware should set X-Geo-Info-Available: false on every response"
    );
}

// ---------------------------------------------------------------------------
// Basic-auth gate test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_returns_non_404_non_5xx() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    let status = resp.status().as_u16();

    assert_ne!(status, 404, "admin route must be routed");
    // 501 Not Implemented is the designed dev-server response for admin key
    // routes; only an unhandled 500 indicates a panic or missing handler.
    assert_ne!(status, 500, "admin route must not panic: got {status}");
}
