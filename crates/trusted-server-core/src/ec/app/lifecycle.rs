//! Request-local identity operations used by every serving adapter.

use std::sync::Arc;

use edgezero_core::http::{Method, Request, Response, StatusCode, header};
use error_stack::Report;

use super::IdentityApp;
use crate::constants::{
    COOKIE_TS_EC, COOKIE_TS_EC_PULL_COMPLETE, COOKIE_TS_EIDS, COOKIE_TS_EIDS_MIRROR,
};
use crate::ec::batch_sync::{answer_batch_sync_locally, handle_batch_sync};
use crate::ec::consent::ec_consent_withdrawn;
use crate::ec::device::DeviceSignals;
use crate::ec::finalize::ec_finalize_response;
use crate::ec::identify::{answer_identify_locally, cors_preflight_identify, handle_identify};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_backend::EcKvStore;
use crate::ec::page_load::NavigationPermit;
use crate::ec::sharedid::SharedIdRequest;
use crate::ec::{EcContext, EidSyncSource};
use crate::error::{TrustedServerError, http_error};
use crate::http_util::is_navigation_request;
use crate::platform::RuntimeServices;
use crate::response_privacy::{
    enforce_set_cookie_cache_privacy, enforce_terminal_private_cache_privacy,
};

/// The identity inputs and graph used throughout one request.
pub struct IdentityRequest {
    /// Consent and record snapshot for this request only.
    pub context: EcContext,
    /// The app's shared record service, if configured.
    pub graph: Option<KvIdentityGraph>,
    /// Whether available trusted device signals allow creation.
    pub creation_allowed: bool,
    /// Browser EIDs captured before routing consumes the request.
    pub eids_cookie: Option<String>,
    /// `SharedID` inputs and configured write-back destination.
    pub sharedid: SharedIdRequest,
}

impl IdentityApp {
    /// Binds a prepared record service before the application starts serving.
    ///
    /// Axum's deployment applies its own rate limits; its default is unlimited.
    pub fn bind_record_store(&mut self, store: Arc<dyn EcKvStore + Send + Sync>) {
        self.record_store = Some(store);
    }

    /// Reads consent, identity and trusted platform signals once for a request.
    ///
    /// Missing JA4 never prevents creation. User-Agent is analytics metadata,
    /// not an independent authorization gate.
    ///
    /// # Errors
    ///
    /// Returns a request error for malformed identity cookies or consent inputs.
    pub fn read_request(
        &self,
        services: &RuntimeServices,
        request: &Request,
    ) -> Result<IdentityRequest, Report<TrustedServerError>> {
        let geo = services
            .geo()
            .lookup(services.client_info().client_ip)
            .unwrap_or_else(|error| {
                log::warn!("Identity geo lookup fails: {error}");
                None
            });
        let mut context =
            EcContext::read_from_request_with_geo(&self.settings, request, services, geo.as_ref())?;
        let signals = request
            .extensions()
            .get::<DeviceSignals>()
            .cloned()
            .unwrap_or_else(|| {
                let user_agent = request
                    .headers()
                    .get(header::USER_AGENT)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                DeviceSignals::derive(
                    user_agent,
                    services.client_info().tls_ja4.as_deref(),
                    services.client_info().h2_fingerprint.as_deref(),
                )
            });
        let creation_allowed = signals.known_browser != Some(false);
        context.set_device_signals(signals);
        Ok(IdentityRequest {
            context,
            graph: self.identity_graph(),
            creation_allowed,
            eids_cookie: cookie_value(request, COOKIE_TS_EIDS),
            sharedid: SharedIdRequest::for_write_back(&self.sharedid_storage, request),
        })
    }

    /// Prepares identity creation on a publisher document navigation.
    ///
    /// A client refused creation gets no navigation write permission, so its
    /// existing EC is never refreshed and no browser EIDs or `SharedID` are
    /// ingested for it.
    ///
    /// Call only after publisher route selection and request filters succeed.
    pub async fn prepare_navigation(
        &self,
        context: &mut EcContext,
        request: &Request,
        allowed: bool,
    ) {
        if !allowed
            || !is_navigation_request(request)
            || (matches!(*request.method(), Method::GET | Method::HEAD)
                && self
                    .settings
                    .asset_route_for_path(request.uri().path())
                    .is_some())
        {
            return;
        }
        context.set_eid_sync_source(EidSyncSource::Navigation);
        if let Some(permit) = NavigationPermit::classify(request) {
            context.set_navigation_permit(permit);
        }
        self.prepare_creation(context, allowed).await;
    }

    /// Prepares creation using this application's record service and consent gates.
    pub async fn prepare_creation(&self, context: &mut EcContext, allowed: bool) {
        if allowed
            && let Some(graph) = self.identity_graph()
            && let Err(error) = context
                .generate_if_needed(&self.settings, Some(&graph))
                .await
        {
            log::warn!("Identity generation fails: {error:?}");
        }
    }

    /// Finalizes one request before its response headers are released.
    ///
    /// Positive identity cookies are restricted to document and EC responses.
    /// Core-generated EC, configured `SharedID` and mirror expiries may accompany
    /// every finalized response (S2S design §10, WB-1). Origin identity cookies
    /// are filtered first on ineligible responses; every expiry remains private.
    pub async fn finalize(
        &self,
        context: &mut EcContext,
        eids_cookie: Option<&str>,
        sharedid: &SharedIdRequest,
        use_store: bool,
        response: &mut Response,
    ) {
        let ec_route = response.extensions().get::<EcRouteResponse>().is_some();
        // A document revalidation inherits the cached representation: 304
        // commonly omits Content-Type, and keeps the navigation permission.
        let document = context.has_navigation_permit()
            && !NavigationPermit::is_redirect(response.status())
            && (response.status() == StatusCode::NOT_MODIFIED
                || response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| {
                        value.split(';').next().is_some_and(|mime| {
                            mime.trim().eq_ignore_ascii_case("text/html")
                                || mime.trim().eq_ignore_ascii_case("application/xhtml+xml")
                        })
                    }));
        let eligible = document || ec_route;
        if !eligible {
            let _ = context.take_navigation_permit();
            // Establish provenance before core finalization: neither positive
            // nor expiring origin identity cookies may pass this boundary.
            self.filter_ineligible_identity_cookies(response, false);
        }
        let graph = self.identity_graph().filter(|_| use_store);
        ec_finalize_response(
            &self.settings,
            context,
            graph.as_ref(),
            &self.partners,
            eids_cookie,
            sharedid,
            response,
        )
        .await;
        if !eligible {
            // S2S EID collection design §10, WB-1 permits core-generated
            // identity expiries on any already-finalized response.
            self.filter_ineligible_identity_cookies(response, true);
        }
        enforce_set_cookie_cache_privacy(response);
        if response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| {
                value
                    .to_str()
                    .ok()
                    .is_some_and(|value| self.identity_expiry(value))
            })
        {
            enforce_terminal_private_cache_privacy(response);
        }
    }

    fn filter_ineligible_identity_cookies(&self, response: &mut Response, core_generated: bool) {
        let cookies: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter(|value| {
                // Read the ASCII name independently of a possibly non-ASCII
                // origin value, which must not bypass identity filtering.
                let name = value
                    .as_bytes()
                    .split(|byte| *byte == b'=')
                    .next()
                    .unwrap_or_default();
                let managed = core::str::from_utf8(name.trim_ascii())
                    .is_ok_and(|name| self.identity_cookie(name));
                !managed
                    || (core_generated
                        && value
                            .to_str()
                            .ok()
                            .is_some_and(|value| self.identity_expiry(value)))
            })
            .cloned()
            .collect();
        response.headers_mut().remove(header::SET_COOKIE);
        for cookie in cookies {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }

    fn identity_expiry(&self, value: &str) -> bool {
        let Ok(cookie) = cookie::Cookie::parse(value) else {
            return false;
        };
        let name = cookie.name();
        let allowed = [COOKIE_TS_EC, COOKIE_TS_EIDS_MIRROR].contains(&name)
            || self
                .sharedid_storage
                .write_back_cookie()
                .is_some_and(|sharedid| name == sharedid || name == format!("{sharedid}_cst"));
        allowed
            && cookie.max_age().map_or_else(
                || {
                    cookie.expires_datetime().is_some_and(|expiry| {
                        i128::from(expiry.unix_timestamp())
                            <= i128::from(crate::ec::current_timestamp())
                    })
                },
                |age| age.whole_seconds() <= 0,
            )
    }

    fn identity_cookie(&self, name: &str) -> bool {
        [
            COOKIE_TS_EC,
            COOKIE_TS_EIDS,
            COOKIE_TS_EIDS_MIRROR,
            COOKIE_TS_EC_PULL_COMPLETE,
        ]
        .contains(&name)
            || self
                .sharedid_storage
                .cookie_name()
                .is_some_and(|sharedid| name == sharedid || name == format!("{sharedid}_cst"))
    }

    /// Answers identify locally using the application's one record service.
    ///
    /// # Errors
    ///
    /// Returns request decoding or partner response errors.
    pub async fn identify(
        &self,
        services: &RuntimeServices,
        request: &Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        if request.method() == Method::OPTIONS {
            return cors_preflight_identify(&self.settings, request);
        }
        let Some(_graph) = self
            .identity_graph()
            .filter(|_| request.method() == Method::GET)
        else {
            return answer_identify_locally(&self.settings, request);
        };
        let mut identity = self.read_request(services, request)?;
        self.apply_cookie_opt_outs(request, &mut identity.context);
        let response = self
            .identify_context(request, &identity.context)
            .await
            .unwrap_or_else(|error| http_error(&error));
        Ok(identity.finalize(self, response).await)
    }

    /// Dispatches identify inside an already-owned request lifecycle.
    ///
    /// # Errors
    ///
    /// Returns partner lookup or response errors. The caller must finalize the
    /// supplied context exactly once, including error responses.
    pub async fn identify_context(
        &self,
        request: &Request,
        context: &EcContext,
    ) -> Result<Response, Report<TrustedServerError>> {
        if request.method() == Method::OPTIONS {
            return cors_preflight_identify(&self.settings, request);
        }
        let Some(graph) = self
            .identity_graph()
            .filter(|_| request.method() == Method::GET)
        else {
            return answer_identify_locally(&self.settings, request);
        };
        handle_identify(&self.settings, &graph, &self.partners, request, context).await
    }

    /// Answers an identify request on a runtime without identity storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the local response cannot be constructed.
    pub fn unavailable_identify(
        &self,
        request: &Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        answer_identify_locally(&self.settings, request)
    }

    /// Answers batch sync locally; missing storage returns 501 before origin dispatch.
    pub async fn batch_sync(&self, request: Request) -> Response {
        let Some(graph) = self
            .identity_graph()
            .filter(|_| request.method() == Method::POST)
        else {
            return answer_batch_sync_locally(&request);
        };
        handle_batch_sync(&graph, &self.partners, self.rate_limiter(), request)
            .await
            .unwrap_or_else(|error| http_error(&error))
    }

    /// Prepares a compatibility consent update before any lifecycle operation.
    ///
    /// # Errors
    ///
    /// Returns a private error response for invalid origin, method or payload.
    pub async fn prepare_consent_update(
        services: &RuntimeServices,
        request: Request,
    ) -> Result<Request, Box<Response>> {
        crate::ec::consent_update::prepare_consent_update(services, request).await
    }

    /// Answers an already prepared compatibility consent update locally.
    #[must_use]
    pub fn consent_update(&self, request: &Request) -> Response {
        crate::ec::consent_update::handle_consent_update(request, self.identity_graph().as_ref())
    }
}

/// Marks responses from the explicit same-origin EC route.
#[derive(Clone, Copy, Debug)]
pub(super) struct EcRouteResponse;

fn cookie_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_owned())
}

impl IdentityRequest {
    /// Whether finalization may write this request's identity record.
    ///
    /// Only clients allowed to create identity write, except that a consent
    /// withdrawal always reaches the store so its tombstone stays authoritative.
    #[must_use]
    pub fn store_writes_allowed(&self) -> bool {
        self.creation_allowed || ec_consent_withdrawn(self.context.consent())
    }

    /// Commits this request's identity state before releasing the response.
    pub async fn finalize(mut self, app: &IdentityApp, mut response: Response) -> Response {
        let use_store = self.store_writes_allowed();
        app.finalize(
            &mut self.context,
            self.eids_cookie.as_deref(),
            &self.sharedid,
            use_store,
            &mut response,
        )
        .await;
        response
    }
}

impl IdentityApp {
    /// Registers shared follow-up identity work with the runtime's drained task API.
    ///
    /// Outbound futures are created inside the task's fresh outbound scope.
    ///
    /// Registers nothing when there is no work or finalize issued no follow-up
    /// permit, so a skipped navigation never holds the shutdown drain. The
    /// permit stays in `context` for [`Self::run_after_response_lookups`].
    ///
    /// Returns whether a task was registered. A missing scheduler or a rejected
    /// registration is logged and drops the work without running it.
    #[must_use = "returns whether follow-up work was registered; failures are already logged"]
    pub fn schedule_after_response(
        self: &Arc<Self>,
        mut context: EcContext,
        work: Option<super::AfterResponseLookups>,
        scheduler: Option<edgezero_core::after_response::AfterResponseHandle>,
        client: edgezero_core::outbound::HttpClient,
    ) -> bool {
        if work.is_none() || !context.has_after_response_permit() {
            return false;
        }
        let Some(scheduler) = scheduler else {
            log::error!("Identity follow-up requires the runtime after-response capability");
            return false;
        };
        let app = Arc::clone(self);
        let budget =
            std::time::Duration::from_millis(self.settings.ec.identity_after_response_budget_ms);
        if let Err(error) = scheduler.schedule(budget, async move {
            app.run_after_response_lookups(&mut context, work, &client)
                .await;
        }) {
            log::error!("Identity follow-up registration fails: {error}");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::cookie_value;
    use crate::ec::app::{IdentityApp, IdentityPlatform};
    use crate::ec::{EcContext, EidSyncSource};
    use crate::settings::ProxyAssetRoute;
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::http::{Request, header};
    use futures::executor::block_on;
    use std::sync::Arc;

    #[test]
    fn identity_expiry_requires_an_allowed_name_and_effective_expiration() {
        let mut settings = create_test_settings();
        crate::test_support::identity_conformance::configure_http_fixture(&mut settings);
        let app = IdentityApp::build(Arc::new(settings), IdentityPlatform::default())
            .expect("should build app");
        for (cookie, expected) in [
            ("ts-ec=; Max-Age=0", true),
            ("_pubcid=; Max-Age=0", true),
            ("_pubcid_cst=; Max-Age=0", true),
            (
                "__Host-ts-eids-mirror=; Expires=Wed, 01 Jan 2020 00:00:00 GMT",
                true,
            ),
            (
                "ts-ec=positive; Max-Age=60; Expires=Wed, 01 Jan 2020 00:00:00 GMT",
                false,
            ),
            (
                "ts-ec=positive; Expires=Wed, 01 Jan 2098 00:00:00 GMT",
                false,
            ),
            ("ts-ec=positive", false),
            ("ts-eids=; Max-Age=0", false),
            ("ts-ec-pull-complete=; Max-Age=0", false),
            ("sharedId=; Max-Age=0", false),
        ] {
            assert_eq!(
                app.identity_expiry(cookie),
                expected,
                "should enforce the expiry allowlist and Max-Age precedence for {cookie}"
            );
        }
    }

    #[test]
    fn core_expiries_cannot_keep_shared_cache_headers() {
        let mut settings = create_test_settings();
        crate::test_support::identity_conformance::configure_http_fixture(&mut settings);
        let app = IdentityApp::build(Arc::new(settings), IdentityPlatform::default())
            .expect("should build app");
        let mut request = Request::default();
        request.headers_mut().insert(
            header::COOKIE,
            "__Host-ts-eids-mirror=stale"
                .parse()
                .expect("should set cookie"),
        );
        let mut identity = app
            .read_request(&crate::platform::test_support::noop_services(), &request)
            .expect("should read request");
        let mut response = edgezero_core::http::Response::default();
        for (name, value) in [
            ("cache-control", "public, max-age=600, s-maxage=600"),
            ("surrogate-control", "max-age=600"),
            ("cdn-cache-control", "public, max-age=600"),
            ("cloudflare-cdn-cache-control", "public, max-age=600"),
        ] {
            response
                .headers_mut()
                .insert(name, value.parse().expect("should set cache header"));
        }
        for (name, value) in [
            ("etag", "example-tag"),
            ("last-modified", "Wed, 01 Jan 2020 00:00:00 GMT"),
            ("expires", "Wed, 01 Jan 2030 00:00:00 GMT"),
            ("age", "120"),
        ] {
            response.headers_mut().insert(
                name,
                value.parse().expect("should seed origin cache metadata"),
            );
        }
        block_on(app.finalize(
            &mut identity.context,
            None,
            &identity.sharedid,
            false,
            &mut response,
        ));
        assert!(
            response.headers().contains_key(header::SET_COOKIE),
            "should emit a core mirror expiry"
        );
        assert!(
            response.headers()[header::CACHE_CONTROL]
                .to_str()
                .expect("should read cache policy")
                .contains("private"),
            "should make every expiry response private"
        );
        assert!(
            response
                .extensions()
                .get::<crate::response_privacy::TerminalPrivateResponse>()
                .is_some(),
            "should preserve expiry privacy through late response hooks"
        );
        for name in ["etag", "last-modified", "expires", "age"] {
            assert!(
                !response.headers().contains_key(name),
                "should remove origin cache metadata {name}"
            );
        }
        for name in [
            "surrogate-control",
            "cdn-cache-control",
            "cloudflare-cdn-cache-control",
        ] {
            assert!(
                !response.headers().contains_key(name),
                "should remove shared cache directive {name}"
            );
        }
    }

    #[test]
    fn ineligible_response_rejects_origin_identity_cookies_including_expiries() {
        let mut settings = create_test_settings();
        crate::test_support::identity_conformance::configure_http_fixture(&mut settings);
        let app = IdentityApp::build(Arc::new(settings), IdentityPlatform::default())
            .expect("should build identity app");
        let mut response = edgezero_core::http::Response::default();
        for name in [
            "ts-ec",
            "ts-eids",
            "__Host-ts-eids-mirror",
            "_pubcid",
            "_pubcid_cst",
            "ts-ec-pull-complete",
        ] {
            for age in [0, 3600] {
                response.headers_mut().append(
                    header::SET_COOKIE,
                    format!("{name}=origin-value; Path=/; Max-Age={age}")
                        .parse()
                        .expect("should build forged cookie"),
                );
            }
        }
        response.headers_mut().append(
            header::SET_COOKIE,
            edgezero_core::http::HeaderValue::from_bytes(b"ts-ec=origin\x80; Max-Age=0")
                .expect("should build an origin cookie containing non-ASCII bytes"),
        );
        response.headers_mut().append(
            header::SET_COOKIE,
            "unrelated=kept"
                .parse()
                .expect("should build unrelated cookie"),
        );
        block_on(app.finalize(
            &mut EcContext::default(),
            None,
            &crate::ec::sharedid::SharedIdRequest::default(),
            false,
            &mut response,
        ));
        let cookies: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .collect();
        assert_eq!(
            cookies,
            vec!["unrelated=kept"],
            "should discard every origin identity cookie including deletions"
        );
    }

    #[test]
    fn post_to_asset_path_retains_publisher_navigation() {
        let mut settings = create_test_settings();
        settings.proxy.asset_routes.push(ProxyAssetRoute::new(
            "/assets/",
            "https://assets.example.com",
        ));
        let app = IdentityApp::build(Arc::new(settings), IdentityPlatform::default())
            .expect("should build identity app");
        for method in [
            edgezero_core::http::Method::POST,
            edgezero_core::http::Method::GET,
            edgezero_core::http::Method::HEAD,
        ] {
            let request = http::Request::builder()
                .method(method.clone())
                .uri("/assets/article")
                .header("sec-fetch-dest", "document")
                .body(edgezero_core::body::Body::empty())
                .expect("should build publisher request");
            let mut context = EcContext::default();
            block_on(app.prepare_navigation(&mut context, &request, true));
            assert_eq!(
                context.has_navigation_permit(),
                method == edgezero_core::http::Method::POST,
                "should exclude only actual GET/HEAD asset routes"
            );
        }
    }

    #[test]
    fn refused_creation_gets_no_navigation_write_authority() {
        let app = IdentityApp::build(
            Arc::new(create_test_settings()),
            IdentityPlatform::default(),
        )
        .expect("should build identity app");
        for allowed in [false, true] {
            let request = http::Request::builder()
                .method(edgezero_core::http::Method::GET)
                .uri("/article")
                .header("sec-fetch-dest", "document")
                .header("sec-fetch-mode", "navigate")
                .body(edgezero_core::body::Body::empty())
                .expect("should build publisher navigation");
            let mut context = EcContext::default();
            block_on(app.prepare_navigation(&mut context, &request, allowed));
            assert_eq!(
                context.has_navigation_permit(),
                allowed,
                "should grant a navigation permit only when creation is allowed"
            );
            assert_eq!(
                context.eid_sync_source(),
                allowed.then_some(EidSyncSource::Navigation),
                "should mark browser EID ingestion only when creation is allowed"
            );
        }
    }

    #[test]
    fn refused_creation_writes_only_a_consent_withdrawal() {
        for (creation_allowed, withdrawn, expected) in [
            (true, false, true),
            (false, false, false),
            (false, true, true),
        ] {
            let mut context = EcContext::default();
            if withdrawn {
                let consent = context.consent_mut();
                consent.jurisdiction =
                    crate::consent::jurisdiction::Jurisdiction::UsState("CA".to_owned());
                consent.gpc = true;
            }
            let identity = super::IdentityRequest {
                context,
                graph: None,
                creation_allowed,
                eids_cookie: None,
                sharedid: crate::ec::sharedid::SharedIdRequest::default(),
            };
            assert_eq!(
                identity.store_writes_allowed(),
                expected,
                "should decide store writes for creation_allowed={creation_allowed}, withdrawn={withdrawn}"
            );
        }
    }

    #[test]
    fn captures_eids_from_later_cookie_field() {
        let mut request = Request::default();
        request.headers_mut().append(
            header::COOKIE,
            "unrelated=first".parse().expect("should parse cookie"),
        );
        request.headers_mut().append(
            header::COOKIE,
            "ts-eids=second; other=value"
                .parse()
                .expect("should parse cookie"),
        );
        request.headers_mut().append(
            header::COOKIE,
            "ts-eids=third".parse().expect("should parse cookie"),
        );
        assert_eq!(
            cookie_value(&request, "ts-eids").as_deref(),
            Some("second"),
            "should capture first matching EID across cookie fields"
        );
    }
}
