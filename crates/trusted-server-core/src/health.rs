//! Configuration-independent liveness, initialization readiness, and startup errors.

use async_trait::async_trait;
use edgezero_core::body::Body;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Method, Response, StatusCode, header};
use edgezero_core::pre_dispatch::PreDispatchRequest;
use edgezero_core::router::{BoxPreDispatchHook, PreDispatchHook};

use crate::version_header::apply_git_version_header;

/// Outcome of the application's existing initialization attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupState {
    /// Application state was constructed successfully.
    Ready,
    /// Application initialization failed.
    Unavailable,
}

/// Local operational probe selected by its exact path and method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Probe {
    /// The running adapter can answer a request.
    Liveness,
    /// Application initialization succeeded for this instance.
    Readiness,
}

/// Marks a fixed operational response that must bypass adapter finalization.
#[derive(Clone, Copy, Debug)]
pub struct OperationalResponse;

/// Recognizes GET/HEAD probes without normalizing paths.
///
/// Pass the URI path, excluding its query.
///
/// # Examples
///
/// ```
/// use edgezero_core::http::Method;
/// use trusted_server_core::health::{Probe, classify_probe};
/// assert_eq!(classify_probe(&Method::GET, "/ready"), Some(Probe::Readiness));
/// ```
#[must_use]
pub fn classify_probe(method: &Method, path: &str) -> Option<Probe> {
    if method != Method::GET && method != Method::HEAD {
        return None;
    }
    match path {
        "/health" => Some(Probe::Liveness),
        "/ready" => Some(Probe::Readiness),
        _ => None,
    }
}

/// Builds a probe response without reading settings or dependencies.
///
/// HEAD has the GET status and contract headers with an empty body.
///
/// # Examples
///
/// ```
/// use edgezero_core::http::Method;
/// use trusted_server_core::health::{Probe, StartupState, probe_response};
/// let response = probe_response(&Method::HEAD, Probe::Readiness, StartupState::Ready);
/// assert_eq!(response.status().as_u16(), 200);
/// ```
#[must_use]
pub fn probe_response(method: &Method, probe: Probe, state: StartupState) -> Response {
    match (probe, state) {
        (Probe::Liveness, _) => operational_response(method, StatusCode::OK, "text/plain", "ok"),
        (Probe::Readiness, StartupState::Ready) => {
            operational_response(method, StatusCode::OK, "text/plain", "ready")
        }
        (Probe::Readiness, StartupState::Unavailable) => startup_unavailable_response(method),
    }
}

/// Builds the generic public response for a caught application startup failure.
///
/// Error details belong in server diagnostics, independently of this response.
///
/// # Examples
///
/// ```
/// use edgezero_core::http::Method;
/// use trusted_server_core::health::startup_unavailable_response;
/// assert_eq!(startup_unavailable_response(&Method::GET).status().as_u16(), 503);
/// ```
#[must_use]
pub fn startup_unavailable_response(method: &Method) -> Response {
    operational_response(
        method,
        StatusCode::SERVICE_UNAVAILABLE,
        "text/plain; charset=utf-8",
        "Service Unavailable\n",
    )
}

fn operational_response(
    method: &Method,
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
) -> Response {
    let mut response = Response::new(if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    });
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    apply_git_version_header(&mut response);
    response.extensions_mut().insert(OperationalResponse);
    response
}

/// Handles operational probes before delegating ordinary requests to another hook.
///
/// Captures only the initialization outcome; it never constructs application state.
pub struct ProbePreDispatchHook {
    state: StartupState,
    delegate: Option<BoxPreDispatchHook>,
}

impl ProbePreDispatchHook {
    /// Composes probes with an optional existing pre-dispatch hook.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::health::{ProbePreDispatchHook, StartupState};
    /// let hook = ProbePreDispatchHook::new(StartupState::Unavailable, None);
    /// ```
    #[must_use]
    pub fn new(state: StartupState, delegate: Option<BoxPreDispatchHook>) -> Self {
        Self { state, delegate }
    }
}

#[async_trait(?Send)]
impl PreDispatchHook for ProbePreDispatchHook {
    async fn handle(
        &self,
        request: &mut PreDispatchRequest,
    ) -> Result<Option<Response>, EdgeError> {
        if let Some(probe) = classify_probe(request.method(), request.uri().path()) {
            return Ok(Some(probe_response(request.method(), probe, self.state)));
        }
        match &self.delegate {
            Some(delegate) => delegate.handle(request).await,
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::run_pre_dispatch_hook;
    use edgezero_core::http::request_builder;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Delegate(Arc<AtomicUsize>);

    #[async_trait(?Send)]
    impl PreDispatchHook for Delegate {
        async fn handle(
            &self,
            _request: &mut PreDispatchRequest,
        ) -> Result<Option<Response>, EdgeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut response = Response::new(Body::from("delegate"));
            *response.status_mut() = StatusCode::ACCEPTED;
            Ok(Some(response))
        }
    }

    #[test]
    fn classifier_preserves_exact_paths_and_ordinary_methods() {
        for method in [Method::GET, Method::HEAD] {
            assert_eq!(
                classify_probe(&method, "/health"),
                Some(Probe::Liveness),
                "should recognize liveness"
            );
            assert_eq!(
                classify_probe(&method, "/ready"),
                Some(Probe::Readiness),
                "should recognize readiness"
            );
            for path in ["/health/", "/ready/", "/HEALTH", "/ready/nested"] {
                assert_eq!(
                    classify_probe(&method, path),
                    None,
                    "should preserve ordinary {path}"
                );
            }
        }
        for method in [Method::POST, Method::OPTIONS, Method::TRACE] {
            assert_eq!(
                classify_probe(&method, "/health"),
                None,
                "should preserve ordinary methods"
            );
        }
    }

    #[test]
    fn hook_short_circuits_probes_and_preserves_ordinary_delegation() {
        futures::executor::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            for state in [StartupState::Ready, StartupState::Unavailable] {
                for method in [Method::GET, Method::HEAD] {
                    for (path, live) in [("/health?probe=1", true), ("/ready?probe=1", false)] {
                        let hook = ProbePreDispatchHook::new(
                            state,
                            Some(Arc::new(Delegate(Arc::clone(&calls)))),
                        );
                        let request = request_builder()
                            .method(method.clone())
                            .uri(path)
                            .body(Body::empty())
                            .expect("should build request");
                        let Err(response) = run_pre_dispatch_hook(hook, request).await else {
                            panic!("should intercept probe");
                        };
                        let ready = live || state == StartupState::Ready;
                        assert_eq!(
                            response.status().as_u16(),
                            if ready { 200 } else { 503 },
                            "should report startup outcome"
                        );
                        assert_eq!(
                            response.headers()[header::CACHE_CONTROL],
                            "no-store",
                            "should disable caching"
                        );
                        assert!(
                            response.extensions().get::<OperationalResponse>().is_some(),
                            "should mark terminal response"
                        );
                        let body = response.into_body().into_bytes().expect("should read body");
                        let expected = if method == Method::HEAD {
                            ""
                        } else if live {
                            "ok"
                        } else if ready {
                            "ready"
                        } else {
                            "Service Unavailable\n"
                        };
                        assert_eq!(
                            body.as_ref(),
                            expected.as_bytes(),
                            "should serve exact body"
                        );
                    }
                }
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "should skip delegate for probes"
            );
            let hook = ProbePreDispatchHook::new(
                StartupState::Ready,
                Some(Arc::new(Delegate(Arc::clone(&calls)))),
            );
            let request = request_builder()
                .uri("/ordinary?query=1")
                .body(Body::empty())
                .expect("should build ordinary request");
            let Err(response) = run_pre_dispatch_hook(hook, request).await else {
                panic!("should return delegate response");
            };
            assert_eq!(
                response.status(),
                StatusCode::ACCEPTED,
                "should preserve delegate response"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "should delegate exactly once"
            );
            let hook = ProbePreDispatchHook::new(StartupState::Unavailable, None);
            let request = request_builder()
                .uri("/ordinary?query=1")
                .body(Body::empty())
                .expect("should build ordinary request");
            let continued = run_pre_dispatch_hook(hook, request)
                .await
                .expect("should continue without a delegate");
            assert_eq!(continued.uri(), "/ordinary?query=1", "should preserve URI");
        });
    }
}
