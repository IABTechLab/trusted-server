use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use edgezero_core::body::Body;
use edgezero_core::error::EdgeError;
use edgezero_core::request::RequestIngress;
use edgezero_core::router::PreDispatchHook;
use http::{Method, Request, Response, StatusCode};
use trusted_server_js::trace_assets::trace_asset;

use super::actions::action_response;
use super::routes::{TraceRoute, state_response};
use super::shell::render_setup_shell;
use super::{TraceCookies, TracePreflight, inspect_cookies, preflight, project_request_context};
use crate::forwarder::prepare_trusted_forwarder;
use crate::integrations::gpt_diagnostics::{
    GPT_DIAGNOSTICS_INTEGRATION_ID, GptDiagnosticsConfig, GptDiagnosticsRequestDecision,
};
use crate::platform::{ClientInfo, GeoInfo};
use crate::settings::Settings;

/// Immutable incoming-cookie gate captured before ordinary request preparation.
#[derive(Clone, Copy, Debug)]
pub struct TraceCaptureGate {
    cookies: TraceCookies,
    base_active: bool,
}

impl TraceCaptureGate {
    /// Return cookie health frozen at the runtime-visible request boundary.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let observed = gate.cookies().observed_active();
    /// ```
    #[must_use]
    pub fn cookies(&self) -> &TraceCookies {
        &self.cookies
    }

    /// Whether the incoming session permits request-scoped trace evidence.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if gate.base_active() { capture_optional_evidence(); }
    /// ```
    #[must_use]
    pub fn base_active(&self) -> bool {
        self.base_active
    }

    /// Combine frozen session validity with the existing navigation decision.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let eligible = gate.document_eligible(&diagnostics_decision);
    /// ```
    #[must_use]
    pub fn document_eligible(&self, decision: &GptDiagnosticsRequestDecision) -> bool {
        self.base_active && decision.active()
    }
}

/// Read-only platform facts supplied only for an authenticated setup GET.
pub struct TraceMetadata {
    /// Trusted client connection metadata.
    pub client_info: ClientInfo,
    /// Optional read-only geolocation result.
    pub geo: Option<GeoInfo>,
}

type MetadataSupplier = dyn Fn(&Request<Body>) -> TraceMetadata + Send + Sync;

/// Intercept the trace namespace before ordinary router lookup and middleware.
pub struct TracePreDispatchHook {
    settings: Arc<Settings>,
    metadata: Arc<MetadataSupplier>,
}

impl TracePreDispatchHook {
    /// Capture application settings and a lazy read-only metadata supplier.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let hook = TracePreDispatchHook::new(settings, Arc::new(read_only_metadata));
    /// ```
    ///
    /// # Performance
    ///
    /// Metadata is requested only for authenticated setup-page GET requests.
    #[must_use]
    pub fn new(settings: Arc<Settings>, metadata: Arc<MetadataSupplier>) -> Self {
        Self { settings, metadata }
    }
}

#[async_trait(?Send)]
impl PreDispatchHook for TracePreDispatchHook {
    async fn handle(
        &self,
        request: &mut Request<Body>,
    ) -> Result<Option<Response<Body>>, EdgeError> {
        prepare_trusted_forwarder(request, &self.settings);
        let dispatch = match preflight(&self.settings, request) {
            TracePreflight::NotTrace => {
                let enabled = self
                    .settings
                    .integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)
                    .ok()
                    .flatten()
                    .is_some_and(|config| config.enabled && config.trace_page_enabled);
                if enabled {
                    let cookies = inspect_cookies(
                        request.headers(),
                        request.extensions().get::<RequestIngress>(),
                    );
                    request.extensions_mut().insert(TraceCaptureGate {
                        base_active: cookies.observed_active(),
                        cookies,
                    });
                }
                return Ok(None);
            }
            TracePreflight::Response(response) => return Ok(Some(response)),
            TracePreflight::Ready(dispatch) => dispatch,
        };
        let response = match dispatch.route {
            TraceRoute::Javascript | TraceRoute::Stylesheet => {
                return Ok(Some(match trace_asset(request.uri().path()) {
                    Some(asset) => dispatch.fixed_asset(asset.bytes),
                    None => dispatch.respond(unavailable_response()),
                }));
            }
            TraceRoute::Enable | TraceRoute::End => action_response(request, dispatch.route).await,
            TraceRoute::State => {
                let cookies = inspect_cookies(
                    request.headers(),
                    request.extensions().get::<RequestIngress>(),
                );
                state_response(&cookies)
            }
            TraceRoute::Shell if request.method() == Method::HEAD => Response::new(Body::empty()),
            TraceRoute::Shell => {
                let cookies = inspect_cookies(
                    request.headers(),
                    request.extensions().get::<RequestIngress>(),
                );
                let metadata = (self.metadata)(request);
                match project_request_context(
                    &metadata.client_info,
                    metadata.geo.as_ref(),
                    &cookies,
                    Utc::now(),
                ) {
                    Ok(context) => match render_setup_shell(&context) {
                        Ok(shell) => Response::new(Body::from(shell)),
                        Err(_) => unavailable_response(),
                    },
                    Err(_) => unavailable_response(),
                }
            }
        };
        Ok(Some(dispatch.respond(response)))
    }
}

fn unavailable_response() -> Response<Body> {
    let mut response = Response::new(Body::from(br#"{"error":"trace unavailable"}"#.as_slice()));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use edgezero_core::context::RequestContext;
    use edgezero_core::middleware::{Middleware, Next};
    use edgezero_core::router::RouterService;
    use futures::executor::block_on;
    use http::{Method, StatusCode, header};
    use serde_json::json;

    use crate::test_support::tests::create_test_settings;
    use crate::trace::TraceTerminalResponse;

    use super::*;

    fn hook(enabled: bool, calls: Arc<AtomicUsize>) -> TracePreDispatchHook {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &json!({"enabled": true,"trace_page_enabled": enabled}),
            )
            .expect("should insert trace settings");
        TracePreDispatchHook::new(
            Arc::new(settings),
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                TraceMetadata {
                    client_info: ClientInfo::default(),
                    geo: None,
                }
            }),
        )
    }

    #[test]
    fn forwarding_credentials_are_removed_before_every_preflight_outcome() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = hook(false, calls);
        for path in [
            "/_ts/trace/enable",
            "/_ts/trace/trace.js",
            "/publisher",
            "/_ts/trace/missing",
        ] {
            let mut request = Request::builder()
                .uri(path)
                .header(
                    "x-ts-forwarder-auth",
                    "fictional-forwarder-secret-0123456789",
                )
                .body(Body::empty())
                .expect("should construct a preflight request");
            let _response =
                block_on(hook.handle(&mut request)).expect("should complete local preflight");
            assert!(
                !request.headers().contains_key("x-ts-forwarder-auth"),
                "should sanitize credentials before any preflight return"
            );
        }
    }

    #[test]
    fn trace_dispatch_terminal_routes_only_project_get_shell_metadata() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = hook(true, Arc::clone(&calls));
        for (method, path, status, projected) in [
            (Method::GET, "/_ts/trace", StatusCode::OK, 1),
            (Method::HEAD, "/_ts/trace", StatusCode::OK, 0),
            (Method::GET, "/_ts/trace/state", StatusCode::OK, 0),
            (Method::GET, "/_ts/trace/assets/v1.js", StatusCode::OK, 0),
            (Method::GET, "/_ts/trace/assets/v1.css", StatusCode::OK, 0),
            (Method::POST, "/_ts/trace/enable", StatusCode::FORBIDDEN, 0),
            (Method::POST, "/_ts/trace/end", StatusCode::FORBIDDEN, 0),
            (
                Method::PATCH,
                "/_ts/trace",
                StatusCode::METHOD_NOT_ALLOWED,
                0,
            ),
            (Method::GET, "/_ts/trace/extra", StatusCode::NOT_FOUND, 0),
            (Method::GET, "/%5Fts/trace", StatusCode::BAD_REQUEST, 0),
        ] {
            calls.store(0, Ordering::SeqCst);
            let is_head = method == Method::HEAD;
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .expect("should build trace request");
            let response = block_on(hook.handle(&mut request))
                .expect("should return local policy response")
                .expect("should intercept namespace before lookup");
            assert_eq!(
                response.status(),
                status,
                "should dispatch the exact trace route"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                projected,
                "should lazily project metadata only for GET shell"
            );
            assert!(
                response
                    .extensions()
                    .get::<TraceTerminalResponse>()
                    .is_some(),
                "should bypass ordinary finalization"
            );
            assert!(
                !response.headers().contains_key(header::SET_COOKIE),
                "should not mutate cookies on read or rejected action"
            );
            if is_head {
                assert_eq!(
                    response
                        .into_body()
                        .into_bytes()
                        .expect("should buffer shell HEAD")
                        .len(),
                    0,
                    "should remove the HEAD body"
                );
            }
        }
    }

    #[test]
    fn trace_dispatch_disabled_namespace_avoids_metadata() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = hook(false, Arc::clone(&calls));
        let mut request = Request::builder()
            .uri("/_ts/trace")
            .body(Body::empty())
            .expect("should build disabled request");
        let response = block_on(hook.handle(&mut request))
            .expect("should produce local response")
            .expect("should reserve disabled namespace");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should hide disabled trace routes"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "should never fetch disabled metadata"
        );
    }

    #[test]
    fn trace_dispatch_freezes_ordinary_cookie_gate_before_preparation() {
        for (enabled, cookie, expected) in [
            (true, "__Host-ts-console=1", Some(true)),
            (true, "__Host-ts-console=1, unrelated=value", Some(false)),
            (
                true,
                "__Host-ts-console=1; __Host-ts-console=1",
                Some(false),
            ),
            (true, "__Host-ts-console=invalid", Some(false)),
            (false, "__Host-ts-console=1", None),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let hook = hook(enabled, Arc::clone(&calls));
            let mut request = Request::builder()
                .uri("/article")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("should build ordinary request");
            assert!(
                block_on(hook.handle(&mut request))
                    .expect("should continue ordinary dispatch")
                    .is_none(),
                "should continue ordinary requests"
            );
            let gate = request.extensions().get::<TraceCaptureGate>().copied();
            assert_eq!(
                gate.map(|gate| gate.base_active()),
                expected,
                "should freeze conservative incoming validity only when configured"
            );
            request.headers_mut().remove(header::COOKIE);
            assert_eq!(
                gate.map(|gate| gate.cookies().observed_active()),
                expected,
                "should retain no dependency on sanitized headers"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "should never project ordinary metadata before auth"
            );
        }
    }

    struct OrdinaryLifecycleCounter(Arc<AtomicUsize>);

    #[async_trait(?Send)]
    impl Middleware for OrdinaryLifecycleCounter {
        async fn handle(
            &self,
            context: RequestContext,
            next: Next<'_>,
        ) -> Result<Response<Body>, EdgeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            next.run(context).await
        }
    }

    #[test]
    fn trace_dispatch_router_hook_bypasses_all_ordinary_middleware_and_handlers() {
        let ordinary = Arc::new(AtomicUsize::new(0));
        let metadata = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&ordinary);
        let router = RouterService::builder()
            .pre_dispatch_hook(Arc::new(hook(true, Arc::clone(&metadata))))
            .middleware(OrdinaryLifecycleCounter(Arc::clone(&ordinary)))
            .route("/{*rest}", Method::GET, move |_context: RequestContext| {
                let handler_calls = Arc::clone(&handler_calls);
                async move {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, EdgeError>(Response::new(Body::empty()))
                }
            })
            .build();
        for path in [
            "/_ts/trace",
            "/_ts/trace/state",
            "/_ts/trace/assets/v1.js",
            "/_ts/trace/extra",
        ] {
            let request = Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("should build trace route");
            let response =
                block_on(router.oneshot(request)).expect("should serve local trace response");
            assert!(
                response
                    .extensions()
                    .get::<TraceTerminalResponse>()
                    .is_some(),
                "should terminate before ordinary lifecycle"
            );
        }
        assert_eq!(
            ordinary.load(Ordering::SeqCst),
            0,
            "should bypass every ordinary middleware and publisher handler"
        );
        assert_eq!(
            metadata.load(Ordering::SeqCst),
            1,
            "should project only the authenticated setup GET"
        );
        let request = Request::builder()
            .uri("/article")
            .body(Body::empty())
            .expect("should build ordinary route");
        let _ = block_on(router.oneshot(request)).expect("should preserve ordinary lifecycle");
        assert_eq!(
            ordinary.load(Ordering::SeqCst),
            2,
            "should run middleware and handler for ordinary requests"
        );
    }

    #[test]
    fn trace_dispatch_document_gate_combines_incoming_session_and_effective_decision() {
        for (cookie, query, expected_base, expected_document) in [
            (None, "?ts_console=1", false, false),
            (Some("__Host-ts-console=1"), "", true, true),
            (Some("__Host-ts-console=1"), "?ts_console=0", true, false),
            (
                Some("__Host-ts-console=1, unrelated=value"),
                "?ts_console=1",
                false,
                false,
            ),
        ] {
            let hook = hook(true, Arc::new(AtomicUsize::new(0)));
            let mut request = Request::builder()
                .uri(format!("https://publisher.example.com/article{query}"))
                .header("accept", "text/html")
                .body(Body::empty())
                .expect("should build navigation");
            if let Some(cookie) = cookie {
                request.headers_mut().insert(
                    header::COOKIE,
                    http::HeaderValue::from_str(cookie).expect("should parse example cookie"),
                );
            }
            let _ =
                block_on(hook.handle(&mut request)).expect("should continue ordinary navigation");
            let gate = *request
                .extensions()
                .get::<TraceCaptureGate>()
                .expect("should freeze incoming gate");
            let _ =
                crate::integrations::gpt_diagnostics::prepare_request(&hook.settings, &mut request)
                    .expect("should preserve console preparation");
            let decision = request
                .extensions()
                .get::<GptDiagnosticsRequestDecision>()
                .expect("should store existing decision");
            assert_eq!(
                gate.base_active(),
                expected_base,
                "should evaluate incoming session only once"
            );
            assert_eq!(
                gate.document_eligible(decision),
                expected_document,
                "should combine frozen validity with effective navigation eligibility"
            );
        }
    }
}
