//! Identity app shared by every adapter.
//!
//! [`IdentityApp`] holds the identity state that every adapter needs: the
//! settings, the partner registry, the EC record store and the rate limiter.
//! An adapter builds it once per process or isolate from the platform
//! services it supplies through [`IdentityPlatform`], so no identity route
//! rebuilds that state per request.
//!
//! [`IdentityApp::mount`] registers the identity routes the core app owns on
//! the adapter's router. Today these are the admin lookups; identify and
//! batch sync stay in the adapters until the EC lifecycle moves into core.

use std::sync::Arc;

use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Request, Response};
use edgezero_core::router::RouterBuilder;
use error_stack::Report;

use crate::error::{TrustedServerError, http_error};
use crate::integrations::gpt_diagnostics::prepare_request;
use crate::settings::Settings;

use super::admin::{
    ADMIN_EC_PATH, ADMIN_EIDS_PATH, handle_admin_ec_lookup, handle_admin_eids_lookup,
};
use super::kv::KvIdentityGraph;
use super::rate_limiter::{NoRateLimit, RateLimiter};
use super::registry::PartnerRegistry;

/// Platform services an adapter supplies to the [`IdentityApp`].
///
/// An adapter without an EC record store leaves both fields `None`, and the
/// app answers locally. An adapter that supplies a record store must also
/// supply a rate limiter, so store-backed sync endpoints are never unlimited
/// by omission.
#[derive(Default)]
pub struct IdentityPlatform {
    /// EC record store backing the identity graph, when the adapter has one.
    pub identity_graph: Option<KvIdentityGraph>,
    /// Rate limiter for sync endpoints. `None` applies no limit and is only
    /// accepted when `identity_graph` is also `None`.
    pub rate_limiter: Option<Arc<dyn RateLimiter>>,
}

/// Identity state built once and shared by every identity route.
pub struct IdentityApp {
    settings: Arc<Settings>,
    partners: Arc<PartnerRegistry>,
    identity_graph: Option<KvIdentityGraph>,
    rate_limiter: Arc<dyn RateLimiter>,
}

impl IdentityApp {
    /// Builds the identity app from settings and adapter platform services.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let app = IdentityApp::build(settings, IdentityPlatform::default())?;
    /// assert!(app.identity_graph().is_none());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when the partner
    /// configuration cannot build a [`PartnerRegistry`], or when the platform
    /// supplies a record store without a rate limiter.
    pub fn build(
        settings: Arc<Settings>,
        platform: IdentityPlatform,
    ) -> Result<Arc<Self>, Report<TrustedServerError>> {
        let rate_limiter = match (&platform.identity_graph, platform.rate_limiter) {
            (Some(_), None) => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message:
                        "identity app needs a rate limiter when an EC record store is supplied"
                            .to_string(),
                }));
            }
            (_, Some(rate_limiter)) => rate_limiter,
            (None, None) => Arc::new(NoRateLimit) as Arc<dyn RateLimiter>,
        };
        let partners = Arc::new(PartnerRegistry::from_config(&settings.ec.partners)?);
        Ok(Arc::new(Self {
            settings,
            partners,
            identity_graph: platform.identity_graph,
            rate_limiter,
        }))
    }

    /// Returns the settings the app was built from.
    #[must_use]
    pub fn settings(&self) -> &Arc<Settings> {
        &self.settings
    }

    /// Returns the partner registry built once at startup.
    #[must_use]
    pub fn partners(&self) -> &Arc<PartnerRegistry> {
        &self.partners
    }

    /// Returns the EC identity graph, when the adapter supplied a record store.
    #[must_use]
    pub fn identity_graph(&self) -> Option<&KvIdentityGraph> {
        self.identity_graph.as_ref()
    }

    /// Returns the rate limiter for sync endpoints.
    #[must_use]
    pub fn rate_limiter(&self) -> &dyn RateLimiter {
        self.rate_limiter.as_ref()
    }

    /// Registers the identity routes the core app owns on `router`.
    ///
    /// Registers `GET` on `/_ts/admin/ec`, `/_ts/admin/ec/{id}` and
    /// `/_ts/admin/eids`. Admin EC lookup answers from the record store, or
    /// with `501 Not Implemented` when the adapter supplied none. Every route
    /// first runs GPT diagnostics request preparation, which strips the
    /// reserved console query and cookie and answers with an error response
    /// when preparation fails. The routes run no EC lifecycle, so a read-only
    /// diagnostic never ingests the operator's cookies. Other methods on these
    /// paths reach the adapter's fallback, which denies admin diagnostics.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let router = app.mount(RouterService::builder()).build();
    /// ```
    #[must_use]
    pub fn mount(self: &Arc<Self>, router: RouterBuilder) -> RouterBuilder {
        let ec_lookup = Arc::clone(self);
        let ec_lookup_by_id = Arc::clone(self);
        let eids_lookup = Arc::clone(self);
        router
            .route(ADMIN_EC_PATH, Method::GET, move |ctx: RequestContext| {
                let app = Arc::clone(&ec_lookup);
                async move { Ok::<_, EdgeError>(app.admin_ec_lookup(ctx.into_request()?)) }
            })
            .route(
                ADMIN_EC_BY_ID_PATH,
                Method::GET,
                move |ctx: RequestContext| {
                    let app = Arc::clone(&ec_lookup_by_id);
                    async move { Ok::<_, EdgeError>(app.admin_ec_lookup(ctx.into_request()?)) }
                },
            )
            .route(ADMIN_EIDS_PATH, Method::GET, move |ctx: RequestContext| {
                let app = Arc::clone(&eids_lookup);
                async move { Ok::<_, EdgeError>(app.admin_eids_lookup(ctx.into_request()?)) }
            })
    }

    fn admin_ec_lookup(&self, request: Request) -> Response {
        self.prepare(request)
            .and_then(|request| {
                handle_admin_ec_lookup(self.identity_graph.as_ref(), &self.partners, &request)
            })
            .unwrap_or_else(|report| http_error(&report))
    }

    fn admin_eids_lookup(&self, request: Request) -> Response {
        self.prepare(request)
            .and_then(|request| handle_admin_eids_lookup(&self.partners, &request))
            .unwrap_or_else(|report| http_error(&report))
    }

    /// Runs GPT diagnostics request preparation, as every adapter route does
    /// before its handler, so the reserved console query and cookie never
    /// reach an identity handler.
    fn prepare(&self, mut request: Request) -> Result<Request, Report<TrustedServerError>> {
        prepare_request(&self.settings, &mut request)?;
        Ok(request)
    }
}

const ADMIN_EC_BY_ID_PATH: &str = "/_ts/admin/ec/{id}";

#[cfg(test)]
mod tests {
    use edgezero_core::body::Body;
    use edgezero_core::http::{StatusCode, header};
    use edgezero_core::router::RouterService;
    use futures::executor::block_on;
    use serde_json::json;

    use super::*;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_types::KvEntry;
    use crate::integrations::gpt_diagnostics::{
        GPT_DIAGNOSTICS_COOKIE, GPT_DIAGNOSTICS_INTEGRATION_ID, GPT_DIAGNOSTICS_QUERY,
    };
    use crate::test_support::tests::create_test_settings;

    const MISSING_EC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.abc123";
    const SEEDED_EC_ID: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.abc123";

    fn store_platform() -> IdentityPlatform {
        IdentityPlatform {
            identity_graph: Some(KvIdentityGraph::new(InMemoryEcKv::new("ec_store"))),
            rate_limiter: Some(Arc::new(NoRateLimit)),
        }
    }

    fn router_for(platform: IdentityPlatform) -> RouterService {
        let app = IdentityApp::build(Arc::new(create_test_settings()), platform)
            .expect("should build the identity app");
        app.mount(RouterService::builder()).build()
    }

    fn get(router: &RouterService, path: &str) -> Response {
        let request = http::Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .expect("should build request");
        block_on(router.oneshot(request)).expect("should dispatch request")
    }

    #[test]
    fn mounted_admin_ec_lookup_reads_the_record_store() {
        let router = router_for(store_platform());

        let response = get(&router, &format!("/_ts/admin/ec/{MISSING_EC_ID}"));

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "should look the EC up in the record store"
        );
    }

    #[test]
    fn mounted_admin_ec_lookup_without_store_answers_not_implemented() {
        let router = router_for(IdentityPlatform::default());

        for path in [
            "/_ts/admin/ec".to_owned(),
            format!("/_ts/admin/ec/{MISSING_EC_ID}"),
        ] {
            let response = get(&router, &path);

            assert_eq!(
                response.status(),
                StatusCode::NOT_IMPLEMENTED,
                "should answer 501 for {path} without a record store"
            );
        }
    }

    #[test]
    fn prepare_strips_the_reserved_console_query_and_cookie() {
        let app = IdentityApp::build(
            Arc::new(create_test_settings()),
            IdentityPlatform::default(),
        )
        .expect("should build the identity app");
        let request = http::Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/_ts/admin/eids?{GPT_DIAGNOSTICS_QUERY}=true&keep=1"
            ))
            .header(
                header::COOKIE,
                format!("{GPT_DIAGNOSTICS_COOKIE}=1; other=2"),
            )
            .body(Body::empty())
            .expect("should build request");

        let prepared = app.prepare(request).expect("should prepare the request");

        assert_eq!(
            prepared.uri().query(),
            Some("keep=1"),
            "should strip the reserved console query"
        );
        let cookies = prepared
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            !cookies.contains(GPT_DIAGNOSTICS_COOKIE),
            "should strip the reserved console cookie"
        );
    }

    #[test]
    fn mounted_admin_routes_fail_closed_when_diagnostics_preparation_fails() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_DIAGNOSTICS_INTEGRATION_ID,
                &json!({ "enabled": true, "unknown_field": true }),
            )
            .expect("should insert an invalid diagnostics configuration");
        let app = IdentityApp::build(Arc::new(settings), store_platform())
            .expect("should build the identity app");
        let router = app.mount(RouterService::builder()).build();

        for path in [
            "/_ts/admin/eids".to_owned(),
            "/_ts/admin/ec".to_owned(),
            format!("/_ts/admin/ec/{MISSING_EC_ID}"),
        ] {
            let response = get(&router, &path);

            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "should answer {path} with the preparation error instead of the handler"
            );
        }
    }

    #[test]
    fn mounted_admin_eids_lookup_answers_locally() {
        let router = router_for(IdentityPlatform::default());

        let response = get(&router, "/_ts/admin/eids");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should answer the EIDs echo without a record store"
        );
    }

    #[test]
    fn build_without_platform_services_has_no_store_and_no_limit() {
        let settings = Arc::new(create_test_settings());

        let app = IdentityApp::build(settings, IdentityPlatform::default())
            .expect("should build the identity app");

        assert!(
            app.identity_graph().is_none(),
            "should have no identity graph without a record store"
        );
        assert!(
            !app.rate_limiter()
                .exceeded("partner", 1)
                .expect("should check the rate limit"),
            "should apply no rate limit without a limiter"
        );
    }

    #[test]
    fn build_keeps_the_supplied_record_store() {
        let settings = Arc::new(create_test_settings());
        let app =
            IdentityApp::build(settings, store_platform()).expect("should build the identity app");

        assert_eq!(
            app.identity_graph().map(KvIdentityGraph::store_name),
            Some("ec_store"),
            "should keep the supplied record store"
        );
    }

    #[test]
    fn build_refuses_a_record_store_without_a_rate_limiter() {
        let settings = Arc::new(create_test_settings());
        let platform = IdentityPlatform {
            identity_graph: Some(KvIdentityGraph::new(InMemoryEcKv::new("ec_store"))),
            rate_limiter: None,
        };

        let result = IdentityApp::build(settings, platform);

        let Err(report) = result else {
            panic!("should refuse a record store without a rate limiter");
        };
        assert!(
            matches!(
                report.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should report a configuration error"
        );
    }

    #[test]
    fn mounted_routes_share_the_supplied_store_across_requests() {
        let graph = KvIdentityGraph::in_memory("ec_store");
        let router = router_for(IdentityPlatform {
            identity_graph: Some(graph.clone()),
            rate_limiter: Some(Arc::new(NoRateLimit)),
        });
        graph
            .create(
                SEEDED_EC_ID,
                &KvEntry::minimal("partner.example", "uid-1", 1_741_824_000),
            )
            .expect("should seed the supplied record store");

        let statuses: Vec<_> = (0..2)
            .map(|_| get(&router, &format!("/_ts/admin/ec/{SEEDED_EC_ID}")).status())
            .collect();

        assert_eq!(
            statuses,
            vec![StatusCode::OK, StatusCode::OK],
            "should read the record written after mounting on every request, so no request builds its own store"
        );
    }
}
