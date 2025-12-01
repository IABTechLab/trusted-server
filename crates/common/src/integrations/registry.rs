use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use fastly::http::Method;
use fastly::{Request, Response};

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Action returned by attribute rewriters to describe how the runtime should mutate the element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeRewriteAction {
    /// Leave the attribute and element untouched.
    Keep,
    /// Replace the attribute value with the provided string.
    Replace(String),
    /// Remove the entire element from the HTML stream.
    RemoveElement,
}

impl AttributeRewriteAction {
    #[must_use]
    pub fn keep() -> Self {
        Self::Keep
    }

    #[must_use]
    pub fn replace(value: impl Into<String>) -> Self {
        Self::Replace(value.into())
    }

    #[must_use]
    pub fn remove_element() -> Self {
        Self::RemoveElement
    }
}

/// Outcome returned by the registry after running every matching attribute rewriter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeRewriteOutcome {
    Unchanged,
    Replaced(String),
    RemoveElement,
}

/// Action returned by inline script rewriters to describe how to mutate the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRewriteAction {
    Keep,
    Replace(String),
    RemoveNode,
}

impl ScriptRewriteAction {
    #[must_use]
    pub fn keep() -> Self {
        Self::Keep
    }

    #[must_use]
    pub fn replace(value: impl Into<String>) -> Self {
        Self::Replace(value.into())
    }

    #[must_use]
    pub fn remove_node() -> Self {
        Self::RemoveNode
    }
}

/// Context provided to integration HTML attribute rewriters.
#[derive(Debug)]
pub struct IntegrationAttributeContext<'a> {
    pub attribute_name: &'a str,
    pub request_host: &'a str,
    pub request_scheme: &'a str,
    pub origin_host: &'a str,
}

/// Context passed to script/text rewriters for inline HTML handling.
#[derive(Debug)]
pub struct IntegrationScriptContext<'a> {
    pub selector: &'a str,
    pub request_host: &'a str,
    pub request_scheme: &'a str,
    pub origin_host: &'a str,
}

/// Describes an HTTP endpoint exposed by an integration.
#[derive(Clone, Debug)]
pub struct IntegrationEndpoint {
    pub method: Method,
    pub path: &'static str,
    pub match_type: RouteMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMatch {
    Exact,
    Prefix,
}

impl IntegrationEndpoint {
    #[must_use]
    pub fn new(method: Method, path: &'static str) -> Self {
        Self {
            method,
            path,
            match_type: RouteMatch::Exact,
        }
    }

    #[must_use]
    pub fn get(path: &'static str) -> Self {
        Self {
            method: Method::GET,
            path,
            match_type: RouteMatch::Exact,
        }
    }

    #[must_use]
    pub fn post(path: &'static str) -> Self {
        Self {
            method: Method::POST,
            path,
            match_type: RouteMatch::Exact,
        }
    }

    #[must_use]
    pub fn prefix(method: Method, path: &'static str) -> Self {
        Self {
            method,
            path,
            match_type: RouteMatch::Prefix,
        }
    }

    #[must_use]
    pub fn get_prefix(path: &'static str) -> Self {
        Self::prefix(Method::GET, path)
    }

    #[must_use]
    pub fn post_prefix(path: &'static str) -> Self {
        Self::prefix(Method::POST, path)
    }

    #[must_use]
    fn matches(&self, method: &Method, path: &str) -> bool {
        if self.method != method {
            return false;
        }
        match self.match_type {
            RouteMatch::Exact => path == self.path,
            RouteMatch::Prefix => path.starts_with(self.path),
        }
    }
}

/// Trait implemented by integration proxies that expose HTTP endpoints.
#[async_trait(?Send)]
pub trait IntegrationProxy: Send + Sync {
    /// Routes handled by this integration (e.g. `/integrations/example/auction`).
    fn routes(&self) -> Vec<IntegrationEndpoint>;

    /// Handle the proxied request.
    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>>;
}

/// Trait for integration-provided HTML attribute rewrite hooks.
pub trait IntegrationAttributeRewriter: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;
    /// Return true when this rewriter wants to inspect a given attribute.
    fn handles_attribute(&self, attribute: &str) -> bool;
    /// Attempt to rewrite the attribute value. Return `AttributeRewriteAction::Replace`
    /// to update the attribute, `Keep` to leave it untouched, or `RemoveElement` to drop the node.
    fn rewrite(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction;
}

/// Trait for integration-provided inline script/text rewrite hooks.
pub trait IntegrationScriptRewriter: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;
    /// CSS selector (e.g. `script#__NEXT_DATA__`) that should trigger this rewriter.
    fn selector(&self) -> &'static str;
    /// Attempt to rewrite the inline text content for the selector.
    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction;
}

/// Registration payload returned by integration builders.
pub struct IntegrationRegistration {
    pub integration_id: &'static str,
    pub proxies: Vec<Arc<dyn IntegrationProxy>>,
    pub attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    pub script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
}

impl IntegrationRegistration {
    #[must_use]
    pub fn builder(integration_id: &'static str) -> IntegrationRegistrationBuilder {
        IntegrationRegistrationBuilder::new(integration_id)
    }
}

pub struct IntegrationRegistrationBuilder {
    registration: IntegrationRegistration,
}

impl IntegrationRegistrationBuilder {
    fn new(integration_id: &'static str) -> Self {
        Self {
            registration: IntegrationRegistration {
                integration_id,
                proxies: Vec::new(),
                attribute_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
            },
        }
    }

    #[must_use]
    pub fn with_proxy(mut self, proxy: Arc<dyn IntegrationProxy>) -> Self {
        self.registration.proxies.push(proxy);
        self
    }

    #[must_use]
    pub fn with_attribute_rewriter(
        mut self,
        rewriter: Arc<dyn IntegrationAttributeRewriter>,
    ) -> Self {
        self.registration.attribute_rewriters.push(rewriter);
        self
    }

    #[must_use]
    pub fn with_script_rewriter(mut self, rewriter: Arc<dyn IntegrationScriptRewriter>) -> Self {
        self.registration.script_rewriters.push(rewriter);
        self
    }

    #[must_use]
    pub fn build(self) -> IntegrationRegistration {
        self.registration
    }
}

type RouteKey = (Method, String);
type RouteValue = (Arc<dyn IntegrationProxy>, &'static str);

#[derive(Default)]
struct IntegrationRegistryInner {
    route_map: HashMap<RouteKey, RouteValue>,
    prefix_routes: Vec<(IntegrationEndpoint, Arc<dyn IntegrationProxy>, &'static str)>,
    routes: Vec<(IntegrationEndpoint, &'static str)>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
}

/// Summary of registered integration capabilities.
#[derive(Debug, Clone)]
pub struct IntegrationMetadata {
    pub id: &'static str,
    pub routes: Vec<IntegrationEndpoint>,
    pub attribute_rewriters: usize,
    pub script_selectors: Vec<&'static str>,
}

impl IntegrationMetadata {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            routes: Vec::new(),
            attribute_rewriters: 0,
            script_selectors: Vec::new(),
        }
    }
}

/// In-memory registry of integrations discovered from settings.
#[derive(Clone, Default)]
pub struct IntegrationRegistry {
    inner: Arc<IntegrationRegistryInner>,
}

impl IntegrationRegistry {
    /// Build a registry from the provided settings.
    pub fn new(settings: &Settings) -> Self {
        let mut inner = IntegrationRegistryInner::default();

        for builder in crate::integrations::builders() {
            if let Some(registration) = builder(settings) {
                for proxy in registration.proxies {
                    for route in proxy.routes() {
                        match route.match_type {
                            RouteMatch::Exact => {
                                if inner
                                    .route_map
                                    .insert(
                                        (route.method.clone(), route.path.to_string()),
                                        (proxy.clone(), registration.integration_id),
                                    )
                                    .is_some()
                                {
                                    panic!(
                                        "Integration route collision detected for {} {}",
                                        route.method, route.path
                                    );
                                }
                            }
                            RouteMatch::Prefix => {
                                inner.prefix_routes.push((
                                    route.clone(),
                                    proxy.clone(),
                                    registration.integration_id,
                                ));
                            }
                        }
                        inner.routes.push((route, registration.integration_id));
                    }
                }
                inner
                    .html_rewriters
                    .extend(registration.attribute_rewriters.into_iter());
                inner
                    .script_rewriters
                    .extend(registration.script_rewriters.into_iter());
            }
        }

        Self {
            inner: Arc::new(inner),
        }
    }

    /// Return true when any proxy is registered for the provided route.
    pub fn has_route(&self, method: &Method, path: &str) -> bool {
        self.inner
            .route_map
            .contains_key(&(method.clone(), path.to_string()))
            || self
                .inner
                .prefix_routes
                .iter()
                .any(|(route, _, _)| route.matches(method, path))
    }

    /// Dispatch a proxy request when an integration handles the path.
    pub async fn handle_proxy(
        &self,
        method: &Method,
        path: &str,
        settings: &Settings,
        req: Request,
    ) -> Option<Result<Response, Report<TrustedServerError>>> {
        let proxy = self
            .inner
            .route_map
            .get(&(method.clone(), path.to_string()))
            .map(|(proxy, _)| Arc::clone(proxy))
            .or_else(|| {
                self.inner
                    .prefix_routes
                    .iter()
                    .filter(|(route, _, _)| route.matches(method, path))
                    .max_by_key(|(route, _, _)| route.path.len())
                    .map(|(_, proxy, _)| Arc::clone(proxy))
            })?;

        Some(proxy.handle(settings, req).await)
    }

    /// Give integrations a chance to rewrite HTML attributes.
    pub fn rewrite_attribute(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteOutcome {
        let mut current = attr_value.to_string();
        let mut changed = false;
        for rewriter in &self.inner.html_rewriters {
            if !rewriter.handles_attribute(attr_name) {
                continue;
            }
            match rewriter.rewrite(attr_name, &current, ctx) {
                AttributeRewriteAction::Keep => {}
                AttributeRewriteAction::Replace(next_value) => {
                    current = next_value;
                    changed = true;
                }
                AttributeRewriteAction::RemoveElement => {
                    return AttributeRewriteOutcome::RemoveElement;
                }
            }
        }

        if changed {
            AttributeRewriteOutcome::Replaced(current)
        } else {
            AttributeRewriteOutcome::Unchanged
        }
    }

    /// Expose registered script/text rewriters for HTML processing.
    pub fn script_rewriters(&self) -> Vec<Arc<dyn IntegrationScriptRewriter>> {
        self.inner.script_rewriters.clone()
    }

    /// Provide a snapshot of registered integrations and their hooks.
    pub fn registered_integrations(&self) -> Vec<IntegrationMetadata> {
        let mut map: BTreeMap<&'static str, IntegrationMetadata> = BTreeMap::new();

        for (route, integration_id) in &self.inner.routes {
            let entry = map
                .entry(*integration_id)
                .or_insert_with(|| IntegrationMetadata::new(integration_id));
            entry.routes.push(route.clone());
        }

        for rewriter in &self.inner.html_rewriters {
            let entry = map
                .entry(rewriter.integration_id())
                .or_insert_with(|| IntegrationMetadata::new(rewriter.integration_id()));
            entry.attribute_rewriters += 1;
        }

        for rewriter in &self.inner.script_rewriters {
            let entry = map
                .entry(rewriter.integration_id())
                .or_insert_with(|| IntegrationMetadata::new(rewriter.integration_id()));
            entry.script_selectors.push(rewriter.selector());
        }

        map.into_values().collect()
    }

    #[cfg(test)]
    pub fn from_rewriters(
        attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
        script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    ) -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner {
                route_map: HashMap::new(),
                prefix_routes: Vec::new(),
                routes: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::HEADER_X_FORWARDED_FOR;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::{Method, StatusCode};
    use fastly::{Request, Response};
    use futures::executor::block_on;

    struct TestProxy {
        id: &'static str,
        routes: Vec<IntegrationEndpoint>,
    }

    impl TestProxy {
        fn new(id: &'static str, routes: Vec<IntegrationEndpoint>) -> Arc<Self> {
            Arc::new(Self { id, routes })
        }
    }

    #[async_trait(?Send)]
    impl IntegrationProxy for TestProxy {
        fn routes(&self) -> Vec<IntegrationEndpoint> {
            self.routes.clone()
        }

        async fn handle(
            &self,
            _settings: &Settings,
            _req: Request,
        ) -> Result<Response, Report<TrustedServerError>> {
            Ok(Response::from_status(StatusCode::OK).with_header(HEADER_X_FORWARDED_FOR, self.id))
        }
    }

    fn registry_with_proxies(proxies: Vec<Arc<dyn IntegrationProxy>>) -> IntegrationRegistry {
        let mut inner = IntegrationRegistryInner::default();
        for proxy in proxies {
            for route in proxy.routes() {
                match route.match_type {
                    RouteMatch::Exact => {
                        if inner
                            .route_map
                            .insert(
                                (route.method.clone(), route.path.to_string()),
                                (proxy.clone(), "test"),
                            )
                            .is_some()
                        {
                            panic!("duplicate route {:?}", (route.method.clone(), route.path));
                        }
                    }
                    RouteMatch::Prefix => {
                        inner
                            .prefix_routes
                            .push((route.clone(), proxy.clone(), "test"));
                    }
                }
                inner.routes.push((route, "test"));
            }
        }

        IntegrationRegistry {
            inner: Arc::new(inner),
        }
    }

    #[test]
    fn has_route_handles_prefix_routes() {
        let registry = registry_with_proxies(vec![TestProxy::new(
            "prefix",
            vec![IntegrationEndpoint::get_prefix("/consent")],
        )]);

        assert!(registry.has_route(&Method::GET, "/consent/loader.js"));
        assert!(!registry.has_route(&Method::POST, "/other/path"));
    }

    #[test]
    fn handle_proxy_prefers_exact_over_prefix() {
        let settings = create_test_settings();
        let exact = TestProxy::new(
            "exact",
            vec![IntegrationEndpoint::get("/consent/api/events")],
        );
        let prefix = TestProxy::new("prefix", vec![IntegrationEndpoint::get_prefix("/consent")]);
        let registry = registry_with_proxies(vec![prefix, exact]);

        let req = Request::new(Method::GET, "https://edge.example.com/consent/api/events");
        let result =
            block_on(registry.handle_proxy(&Method::GET, "/consent/api/events", &settings, req));
        let response = result.expect("should find route").expect("should proxy");

        assert_eq!(
            response
                .get_header(&HEADER_X_FORWARDED_FOR)
                .and_then(|v| v.to_str().ok()),
            Some("exact")
        );
    }

    #[test]
    fn handle_proxy_selects_longest_prefix_match() {
        let settings = create_test_settings();
        let shorter = TestProxy::new("short", vec![IntegrationEndpoint::get_prefix("/consent")]);
        let longer = TestProxy::new(
            "long",
            vec![IntegrationEndpoint::get_prefix("/consent/api")],
        );
        let registry = registry_with_proxies(vec![shorter, longer]);

        let req = Request::new(Method::GET, "https://edge.example.com/consent/api/events");
        let result =
            block_on(registry.handle_proxy(&Method::GET, "/consent/api/events", &settings, req));
        let response = result.expect("should find route").expect("should proxy");

        assert_eq!(
            response
                .get_header(&HEADER_X_FORWARDED_FOR)
                .and_then(|v| v.to_str().ok()),
            Some("long")
        );
    }
}
