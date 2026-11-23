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
use crate::ec::cookies::expire_ec_cookie;
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
    /// Call only after publisher route selection and request filters succeed.
    pub async fn prepare_navigation(
        &self,
        context: &mut EcContext,
        request: &Request,
        allowed: bool,
    ) {
        if !is_navigation_request(request)
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
    /// Withdrawal expires the EC cookie on every finalized response, including
    /// when authoritative storage is unavailable.
    pub async fn finalize(
        &self,
        context: &mut EcContext,
        eids_cookie: Option<&str>,
        sharedid: &SharedIdRequest,
        use_store: bool,
        response: &mut Response,
    ) {
        let withdrawal = ec_consent_withdrawn(context.consent()) && context.cookie_was_present();
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
        if self.settings.ec.sliding_refresh {
            context.sliding_document = document && !ec_route;
        }
        if !eligible {
            let _ = context.take_navigation_permit();
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
            let cookies: Vec<_> = response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .filter(|value| {
                    value
                        .to_str()
                        .ok()
                        .and_then(|value| value.split_once('='))
                        .is_none_or(|(name, _)| !self.identity_cookie(name.trim()))
                })
                .cloned()
                .collect();
            response.headers_mut().remove(header::SET_COOKIE);
            for cookie in cookies {
                response.headers_mut().append(header::SET_COOKIE, cookie);
            }
            // Rebuild only our withdrawal deletion after filtering, so an
            // origin-supplied positive identity cookie cannot escape the gate.
            if withdrawal {
                expire_ec_cookie(&self.settings, response);
            }
        }
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
    /// Commits this request's identity state before releasing the response.
    pub async fn finalize(mut self, app: &IdentityApp, mut response: Response) -> Response {
        app.finalize(
            &mut self.context,
            self.eids_cookie.as_deref(),
            &self.sharedid,
            true,
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
    pub fn schedule_after_response(
        self: &Arc<Self>,
        mut context: EcContext,
        work: Option<super::AfterResponseLookups>,
        scheduler: Option<edgezero_core::after_response::AfterResponseHandle>,
        client: edgezero_core::outbound::HttpClient,
    ) {
        if work.is_none() {
            return;
        }
        let Some(scheduler) = scheduler else {
            log::error!("Identity follow-up requires the runtime after-response capability");
            return;
        };
        let app = Arc::clone(self);
        let budget =
            std::time::Duration::from_millis(self.settings.ec.identity_after_response_budget_ms);
        if let Err(error) = scheduler.schedule(budget, async move {
            app.run_after_response_lookups(&mut context, work, &client)
                .await;
        }) {
            log::error!("Identity follow-up registration fails: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cookie_value;
    use crate::ec::EcContext;
    use crate::ec::app::{IdentityApp, IdentityPlatform};
    use crate::settings::ProxyAssetRoute;
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::http::{Request, header};
    use futures::executor::block_on;
    use std::sync::Arc;

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
            block_on(app.prepare_navigation(&mut context, &request, false));
            assert_eq!(
                context.has_navigation_permit(),
                method == edgezero_core::http::Method::POST,
                "should exclude only actual GET/HEAD asset routes"
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
