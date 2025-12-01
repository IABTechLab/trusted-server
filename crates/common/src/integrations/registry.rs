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
}

impl IntegrationEndpoint {
    #[must_use]
    pub fn new(method: Method, path: &'static str) -> Self {
        Self { method, path }
    }

    #[must_use]
    pub fn get(path: &'static str) -> Self {
        Self {
            method: Method::GET,
            path,
        }
    }

    #[must_use]
    pub fn post(path: &'static str) -> Self {
        Self {
            method: Method::POST,
            path,
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

    fn find_route(&self, method: &Method, path: &str) -> Option<&RouteValue> {
        // First try exact match
        let key = (method.clone(), path.to_string());
        if let Some(route_value) = self.inner.route_map.get(&key) {
            return Some(route_value);
        }

        // If no exact match, try wildcard matching
        // Routes ending with /* should match any path with that prefix + additional segments
        for ((route_method, route_path), route_value) in &self.inner.route_map {
            if route_method != method {
                continue;
            }

            if let Some(prefix) = route_path.strip_suffix("/*") {
                if path.starts_with(prefix)
                    && path.len() > prefix.len()
                    && path[prefix.len()..].starts_with('/')
                {
                    return Some(route_value);
                }
            }
        }

        None
    }

    /// Return true when any proxy is registered for the provided route.
    pub fn has_route(&self, method: &Method, path: &str) -> bool {
        self.find_route(method, path).is_some()
    }

    /// Dispatch a proxy request when an integration handles the path.
    pub async fn handle_proxy(
        &self,
        method: &Method,
        path: &str,
        settings: &Settings,
        req: Request,
    ) -> Option<Result<Response, Report<TrustedServerError>>> {
        if let Some((proxy, _)) = self.find_route(method, path) {
            Some(proxy.handle(settings, req).await)
        } else {
            None
        }
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
            entry
                .routes
                .push(IntegrationEndpoint::new(route.method.clone(), route.path));
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
                routes: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
            }),
        }
    }

    #[cfg(test)]
    pub fn from_routes(routes: HashMap<RouteKey, RouteValue>) -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner {
                route_map: routes,
                routes: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock integration proxy for testing
    struct MockProxy;

    #[async_trait(?Send)]
    impl IntegrationProxy for MockProxy {
        fn routes(&self) -> Vec<IntegrationEndpoint> {
            vec![]
        }

        async fn handle(
            &self,
            _settings: &Settings,
            _req: Request,
        ) -> Result<Response, Report<TrustedServerError>> {
            Ok(Response::new())
        }
    }

    #[test]
    fn test_exact_route_matching() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/integrations/test/exact".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Should match exact route
        assert!(registry.has_route(&Method::GET, "/integrations/test/exact"));

        // Should not match different paths
        assert!(!registry.has_route(&Method::GET, "/integrations/test/other"));
        assert!(!registry.has_route(&Method::GET, "/integrations/test/exact/nested"));

        // Should not match different methods
        assert!(!registry.has_route(&Method::POST, "/integrations/test/exact"));
    }

    #[test]
    fn test_wildcard_route_matching() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/integrations/lockr/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Should match paths under the wildcard prefix
        assert!(registry.has_route(&Method::GET, "/integrations/lockr/api/settings"));
        assert!(registry.has_route(
            &Method::GET,
            "/integrations/lockr/api/publisher/app/v1/identityLockr/settings"
        ));
        assert!(registry.has_route(&Method::GET, "/integrations/lockr/api/page-view"));
        assert!(registry.has_route(&Method::GET, "/integrations/lockr/api/a/b/c/d/e"));

        // Should not match paths that don't start with the prefix
        assert!(!registry.has_route(&Method::GET, "/integrations/lockr/sdk"));
        assert!(!registry.has_route(&Method::GET, "/integrations/lockr/other"));
        assert!(!registry.has_route(&Method::GET, "/integrations/other/api/settings"));

        // Should not match different methods
        assert!(!registry.has_route(&Method::POST, "/integrations/lockr/api/settings"));
    }

    #[test]
    fn test_wildcard_and_exact_routes_coexist() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/integrations/test/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        );
        routes.insert(
            (Method::GET, "/integrations/test/exact".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Exact route should match
        assert!(registry.has_route(&Method::GET, "/integrations/test/exact"));

        // Wildcard routes should match
        assert!(registry.has_route(&Method::GET, "/integrations/test/api/anything"));
        assert!(registry.has_route(&Method::GET, "/integrations/test/api/nested/path"));

        // Non-matching should fail
        assert!(!registry.has_route(&Method::GET, "/integrations/test/other"));
    }

    #[test]
    fn test_multiple_wildcard_routes() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/integrations/lockr/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        );
        routes.insert(
            (Method::POST, "/integrations/lockr/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        );
        routes.insert(
            (Method::GET, "/integrations/testlight/api/*".to_string()),
            (
                Arc::new(MockProxy) as Arc<dyn IntegrationProxy>,
                "testlight",
            ),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Lockr GET routes should match
        assert!(registry.has_route(&Method::GET, "/integrations/lockr/api/settings"));

        // Lockr POST routes should match
        assert!(registry.has_route(&Method::POST, "/integrations/lockr/api/settings"));

        // Testlight routes should match
        assert!(registry.has_route(&Method::GET, "/integrations/testlight/api/auction"));
        assert!(registry.has_route(&Method::GET, "/integrations/testlight/api/any-path"));

        // Cross-integration paths should not match
        assert!(!registry.has_route(&Method::GET, "/integrations/lockr/other-endpoint"));
        assert!(!registry.has_route(&Method::GET, "/integrations/other/api/test"));
    }

    #[test]
    fn test_wildcard_preserves_casing() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/integrations/lockr/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Should match with camelCase preserved
        assert!(registry.has_route(
            &Method::GET,
            "/integrations/lockr/api/publisher/app/v1/identityLockr/settings"
        ));
        assert!(registry.has_route(
            &Method::GET,
            "/integrations/lockr/api/publisher/app/v1/identitylockr/settings"
        ));
    }

    #[test]
    fn test_wildcard_edge_cases() {
        let mut routes = HashMap::new();
        routes.insert(
            (Method::GET, "/api/*".to_string()),
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        );

        let registry = IntegrationRegistry::from_routes(routes);

        // Should match paths under /api/
        assert!(registry.has_route(&Method::GET, "/api/v1"));
        assert!(registry.has_route(&Method::GET, "/api/v1/users"));

        // Should not match /api without trailing content
        // The current implementation requires a / after the prefix
        assert!(!registry.has_route(&Method::GET, "/api"));

        // Should not match partial prefix matches
        assert!(!registry.has_route(&Method::GET, "/apiv1"));
    }
}
