//! Terminal timing layer for the Axum dev server.
//!
//! [`TimingService`](crate::timing::TimingService) wraps the tower `Service`
//! boundary the Axum dev server's router sits behind: it creates a
//! [`RequestTimings`](trusted_server_core::request_timing::RequestTimings)
//! collector per request, threads it through request extensions so
//! downstream core handlers can record into it, and on the way back stamps
//! `mark_headers_ready` and appends the `Server-Timing` header via
//! [`append_server_timing_if_private`](trusted_server_core::request_timing::append_server_timing_if_private).
//!
//! This wraps *outside* `RouterService` rather than registering as
//! `RouterBuilder::middleware`. A router-generated 404/405 short-circuits
//! `RouterInner::dispatch` before its middleware chain ever runs, so
//! middleware never sees those responses. By the time a response reaches
//! this layer -- after `RouterService::oneshot` inside
//! `EdgeZeroAxumService::call` has already converted any dispatch error into
//! a plain response -- every response is covered uniformly, router-generated
//! or not.
//!
//! `/health` is excluded by path match before a
//! [`RequestTimings`](trusted_server_core::request_timing::RequestTimings)
//! collector is even created: health checks never carry timing data on any
//! adapter.
//!
//! Unlike the Fastly adapter (state built per request, adding
//! `Phase::AppBuild` to the rendered header), the Axum dev server builds its
//! application state once at startup. There is no per-request app-build
//! interval to measure, so `ts-appbuild` never appears in the header here.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body as AxumBody;
use axum::http::{Request, Response};
use tower::Service;
use trusted_server_core::request_timing::{RequestTimings, append_server_timing_if_private};

/// Path excluded from timing collection and `Server-Timing` emission: health
/// checks never carry timing data on any adapter.
const HEALTH_PATH: &str = "/health";

/// Wraps an inner Axum tower service with the request-phase timing freeze
/// point described in the module docs.
#[derive(Clone)]
pub struct TimingService<S> {
    inner: S,
    server_timing_enabled: bool,
}

impl<S> TimingService<S> {
    /// Wraps `inner`, appending `Server-Timing` when `server_timing_enabled`
    /// is set and the response is conclusively private.
    #[must_use]
    pub fn new(inner: S, server_timing_enabled: bool) -> Self {
        Self {
            inner,
            server_timing_enabled,
        }
    }
}

impl<S> Service<Request<AxumBody>> for TimingService<S>
where
    S: Service<Request<AxumBody>, Response = Response<AxumBody>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    type Response = Response<AxumBody>;

    fn call(&mut self, mut req: Request<AxumBody>) -> Self::Future {
        let mut inner = self.inner.clone();

        // Excluded before a collector is even created: `/health` never
        // carries timing data, on any adapter.
        if req.uri().path() == HEALTH_PATH {
            return Box::pin(async move { inner.call(req).await });
        }

        let server_timing_enabled = self.server_timing_enabled;
        let timings = RequestTimings::new();
        req.extensions_mut().insert(timings.clone());

        Box::pin(async move {
            let mut response = inner.call(req).await?;
            append_server_timing_if_private(&mut response, &timings, server_timing_enabled);
            Ok(response)
        })
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CACHE_CONTROL;
    use axum::http::{HeaderValue, StatusCode};
    use edgezero_adapter_axum::service::EdgeZeroAxumService;
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;
    use edgezero_core::http::response_builder;
    use edgezero_core::router::RouterService;
    use tower::{ServiceExt as _, service_fn};

    /// Builds a private (`cache-control: private, no-store`) response for a
    /// handler under test.
    fn private_ok_response() -> Result<edgezero_core::http::Response, EdgeError> {
        Ok(response_builder()
            .status(StatusCode::OK)
            .header("cache-control", "private, no-store")
            .body(EdgeBody::from("ok"))
            .expect("should build a private response fixture"))
    }

    /// Reads a response header as a UTF-8 string, or `None` if absent.
    fn header(response: &Response<AxumBody>, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn axum_emits_header_on_private_response() {
        let router = RouterService::builder()
            .get("/private", |_ctx: RequestContext| async {
                private_ok_response()
            })
            .build();
        let mut service = TimingService::new(EdgeZeroAxumService::new(router), true);

        let request = Request::builder()
            .uri("/private")
            .body(AxumBody::empty())
            .expect("should build request");
        let response = service
            .ready()
            .await
            .expect("should be ready")
            .call(request)
            .await
            .expect("should not fail");

        let server_timing = header(&response, "server-timing").expect("should emit header");
        assert!(
            server_timing.contains("ts-total;dur="),
            "should carry the collected total: {server_timing}"
        );
        assert!(
            !server_timing.contains("ts-appbuild"),
            "the Axum dev server builds state once at startup, so there is no \
             per-request app-build interval to render: {server_timing}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn axum_404_carries_header_when_private() {
        // An empty router has no routes at all, so any path dispatches
        // through `RouterInner::dispatch`'s `NotFound` branch -- exactly the
        // path that bypasses `RouterBuilder::middleware`. The router's own
        // `EdgeError::into_response` does not attach `Cache-Control`, so a
        // small wrapping service forces the response private here, standing
        // in for whatever upstream layer would normally mark a genuinely
        // private 404. This proves the freeze point still runs for a
        // router-generated response without weakening
        // `append_server_timing_if_private`'s real gating logic.
        let empty_router = RouterService::builder().build();
        let inner = EdgeZeroAxumService::new(empty_router);
        let force_private = service_fn(move |req: Request<AxumBody>| {
            let mut svc = inner.clone();
            async move {
                let mut response = svc.call(req).await?;
                response
                    .headers_mut()
                    .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
                Ok::<_, Infallible>(response)
            }
        });
        let mut service = TimingService::new(force_private, true);

        let request = Request::builder()
            .uri("/does-not-exist")
            .body(AxumBody::empty())
            .expect("should build request");
        let response = service
            .ready()
            .await
            .expect("should be ready")
            .call(request)
            .await
            .expect("should not fail");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should still be the router's own not-found response"
        );
        let server_timing = header(&response, "server-timing")
            .expect("a router-generated 404 must still carry the header when private");
        assert!(
            server_timing.contains("ts-total;dur="),
            "should carry the collected total: {server_timing}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn axum_health_is_excluded() {
        let router = RouterService::builder()
            .get("/health", |_ctx: RequestContext| async {
                private_ok_response()
            })
            .build();
        let mut service = TimingService::new(EdgeZeroAxumService::new(router), true);

        let request = Request::builder()
            .uri("/health")
            .body(AxumBody::empty())
            .expect("should build request");
        let response = service
            .ready()
            .await
            .expect("should be ready")
            .call(request)
            .await
            .expect("should not fail");

        assert!(
            header(&response, "server-timing").is_none(),
            "/health must never carry a server-timing header"
        );
    }
}
