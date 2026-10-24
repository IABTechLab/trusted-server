//! Runtime KV wiring for the Axum dev server.
//!
//! Each test builds the KV handle exactly as `main` does (`open_kv_handle` on
//! the `TRUSTED_SERVER_KV_PATH` value) and attaches it with
//! `EdgeZeroAxumService::with_kv_handle`, the same call the dev server makes.
//! A probe route reads and writes through `build_runtime_services`, so the
//! adapter's own request-to-`RuntimeServices` path is what is exercised.

use axum::body::{Body as AxumBody, to_bytes};
use axum::http::Request;
use bytes::Bytes;
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::http::{HandlerFuture, StatusCode, response_builder};
use edgezero_core::router::RouterService;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::platform::{build_runtime_services, open_kv_handle};

const PROBE_KEY: &str = "probe-key";

fn reply(
    status: StatusCode,
    body: Body,
) -> Result<edgezero_core::http::Response, edgezero_core::error::EdgeError> {
    Ok(response_builder()
        .status(status)
        .body(body)
        .expect("should build probe response"))
}

/// The services the request path builds for `ctx`, with default settings and
/// no permission signal module.
fn services_for(ctx: &RequestContext) -> trusted_server_core::platform::RuntimeServices {
    build_runtime_services(
        ctx,
        &trusted_server_core::settings::Settings::default(),
        &std::sync::Arc::default(),
    )
}

fn probe_router() -> RouterService {
    RouterService::builder()
        .put("/probe", |ctx: RequestContext| -> HandlerFuture {
            Box::pin(async move {
                let services = services_for(&ctx);
                match services
                    .kv_store()
                    .put_bytes(PROBE_KEY, Bytes::from_static(b"stored"))
                    .await
                {
                    Ok(()) => reply(StatusCode::NO_CONTENT, Body::empty()),
                    Err(_) => reply(StatusCode::SERVICE_UNAVAILABLE, Body::empty()),
                }
            })
        })
        .get("/probe", |ctx: RequestContext| -> HandlerFuture {
            Box::pin(async move {
                let services = services_for(&ctx);
                match services.kv_store().get_bytes(PROBE_KEY).await {
                    Ok(Some(value)) => reply(StatusCode::OK, Body::from(value.to_vec())),
                    Ok(None) => reply(StatusCode::NOT_FOUND, Body::empty()),
                    Err(_) => reply(StatusCode::SERVICE_UNAVAILABLE, Body::empty()),
                }
            })
        })
        .build()
}

async fn call(service: &mut EdgeZeroAxumService, method: &str) -> (u16, Bytes) {
    let request = Request::builder()
        .method(method)
        .uri("/probe")
        .body(AxumBody::empty())
        .expect("should build probe request");
    let response = service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should serve probe request");
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 1024)
        .await
        .expect("should read probe body");
    (status, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_kv_persists_across_requests_and_restarts() {
    let dir = tempfile::tempdir().expect("should create temp dir");
    let path = dir.path().join("nested").join("kv.redb");

    {
        let handle = open_kv_handle(&path).expect("should open KV and create its directory");
        let mut service = EdgeZeroAxumService::new(probe_router()).with_kv_handle(handle);
        assert_eq!(call(&mut service, "PUT").await.0, 204, "should write");
        let (status, body) = call(&mut service, "GET").await;
        assert_eq!(status, 200, "a second request should see the write");
        assert_eq!(body, Bytes::from_static(b"stored"));
    }

    let handle = open_kv_handle(&path).expect("should reopen the same KV file");
    let mut restarted = EdgeZeroAxumService::new(probe_router()).with_kv_handle(handle);
    let (status, body) = call(&mut restarted, "GET").await;
    assert_eq!(status, 200, "data should survive a process restart");
    assert_eq!(body, Bytes::from_static(b"stored"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_kv_is_isolated_between_server_instances() {
    let dir = tempfile::tempdir().expect("should create temp dir");
    let mut first = EdgeZeroAxumService::new(probe_router())
        .with_kv_handle(open_kv_handle(&dir.path().join("first.redb")).expect("should open"));
    let mut second = EdgeZeroAxumService::new(probe_router())
        .with_kv_handle(open_kv_handle(&dir.path().join("second.redb")).expect("should open"));

    assert_eq!(
        call(&mut first, "PUT").await.0,
        204,
        "should write to first"
    );

    assert_eq!(
        call(&mut second, "GET").await.0,
        404,
        "a write to one instance must not be visible to another"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_kv_is_unavailable_without_a_configured_path() {
    let mut service = EdgeZeroAxumService::new(probe_router());

    assert_eq!(
        call(&mut service, "GET").await.0,
        503,
        "with no KV handle injected, KV should report unavailable"
    );
}

#[test]
fn open_kv_handle_rejects_a_path_that_cannot_be_created() {
    let dir = tempfile::tempdir().expect("should create temp dir");
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"not a directory").expect("should write blocker file");

    let result = open_kv_handle(&blocker.join("kv.redb"));

    assert!(result.is_err(), "a path under a regular file should fail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_serves_requests_with_and_without_runtime_kv() {
    let settings = trusted_server_core::html_processor::test_support::page_settings("");
    let dir = tempfile::tempdir().expect("should create temp dir");
    let handle = open_kv_handle(&dir.path().join("kv.redb")).expect("should open KV");

    let mut statuses = Vec::new();
    for kv in [None, Some(handle)] {
        let router =
            TrustedServerApp::routes_with_settings(settings.clone()).expect("should build router");
        let mut service = EdgeZeroAxumService::new(router);
        if let Some(handle) = kv {
            service = service.with_kv_handle(handle);
        }
        // A closed path core answers itself, so no origin is asked.
        let request = Request::builder()
            .method("POST")
            .uri("/_ts/admin/keys/rotate")
            .body(AxumBody::empty())
            .expect("should build request");

        let response = service
            .ready()
            .await
            .expect("should be ready")
            .call(request)
            .await
            .expect("should serve request");
        statuses.push(response.status().as_u16());
    }

    assert_eq!(
        statuses[0], statuses[1],
        "the app should answer the same with or without runtime KV"
    );
    assert!(
        statuses[0] < 500,
        "should not fail the request: {statuses:?}"
    );
}
