//! Identity app shared by every adapter.
//!
//! [`IdentityApp`] holds the identity state that every adapter needs: the
//! settings, the partner registry, the EC record store and the rate limiter.
//! An adapter builds it once per process or isolate from the platform
//! services it supplies through [`IdentityPlatform`], so no identity route
//! rebuilds that state per request.

use std::sync::Arc;

use error_stack::Report;

use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::kv::KvIdentityGraph;
use super::rate_limiter::{NoRateLimit, RateLimiter};
use super::registry::PartnerRegistry;

/// Platform services an adapter supplies to the [`IdentityApp`].
///
/// Each field is optional: an adapter without an EC record store or a rate
/// limiter leaves it `None`, and the app answers or skips accordingly.
#[derive(Default)]
pub struct IdentityPlatform {
    /// EC record store backing the identity graph, when the adapter has one.
    pub identity_graph: Option<KvIdentityGraph>,
    /// Rate limiter for sync endpoints. `None` applies no limit.
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
    /// configuration cannot build a [`PartnerRegistry`].
    pub fn build(
        settings: Arc<Settings>,
        platform: IdentityPlatform,
    ) -> Result<Arc<Self>, Report<TrustedServerError>> {
        let partners = Arc::new(PartnerRegistry::from_config(&settings.ec.partners)?);
        let rate_limiter = platform
            .rate_limiter
            .unwrap_or_else(|| Arc::new(NoRateLimit));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::test_support::tests::create_test_settings;

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
        let platform = IdentityPlatform {
            identity_graph: Some(KvIdentityGraph::new(InMemoryEcKv::new("ec_store"))),
            rate_limiter: None,
        };

        let app = IdentityApp::build(settings, platform).expect("should build the identity app");

        assert_eq!(
            app.identity_graph().map(KvIdentityGraph::store_name),
            Some("ec_store"),
            "should keep the supplied record store"
        );
    }

    #[test]
    fn partner_registry_is_built_once() {
        let app = IdentityApp::build(
            Arc::new(create_test_settings()),
            IdentityPlatform::default(),
        )
        .expect("should build the identity app");

        let first = Arc::clone(app.partners());
        let second = Arc::clone(app.partners());

        assert!(
            Arc::ptr_eq(&first, &second),
            "should share one partner registry across requests"
        );
    }
}
