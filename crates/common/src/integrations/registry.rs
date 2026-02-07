use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use error_stack::Report;
use fastly::http::{header, Method};
use fastly::{Request, Response};
use matchit::Router;

use crate::constants::{COOKIE_SYNTHETIC_ID, HEADER_X_SYNTHETIC_ID};
use crate::cookies::{create_synthetic_cookie, handle_request_cookies};
use crate::error::TrustedServerError;
use crate::settings::Settings;
use crate::synthetic::get_or_generate_synthetic_id;

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
    pub is_last_in_text_node: bool,
    pub document_state: &'a IntegrationDocumentState,
}

/// Per-document state shared between HTML/script rewriters and post-processors.
///
/// This exists to support multi-phase HTML processing without requiring a second HTML parse.
#[derive(Clone, Default)]
pub struct IntegrationDocumentState {
    inner: Arc<Mutex<BTreeMap<&'static str, Arc<dyn Any + Send + Sync>>>>,
}

impl std::fmt::Debug for IntegrationDocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&'static str> = {
            let guard = self
                .inner
                .lock()
                .expect("should lock integration document state");
            guard.keys().copied().collect()
        };
        f.debug_struct("IntegrationDocumentState")
            .field("keys", &keys)
            .finish()
    }
}

impl IntegrationDocumentState {
    #[must_use]
    /// Retrieves a value stored for an integration.
    ///
    /// # Panics
    ///
    /// Panics if the inner lock is poisoned.
    pub fn get<T>(&self, integration_id: &'static str) -> Option<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let guard = self
            .inner
            .lock()
            .expect("should lock integration document state");
        guard.get(integration_id).and_then(|value| {
            let cloned: Arc<dyn Any + Send + Sync> = Arc::clone(value);
            cloned.downcast::<T>().ok()
        })
    }

    /// Retrieves or initializes a value for an integration.
    ///
    /// # Panics
    ///
    /// Panics if the inner lock is poisoned.
    pub fn get_or_insert_with<T>(
        &self,
        integration_id: &'static str,
        init: impl FnOnce() -> T,
    ) -> Arc<T>
    where
        T: Any + Send + Sync + 'static,
    {
        let mut guard = self
            .inner
            .lock()
            .expect("should lock integration document state");

        if let Some(existing) = guard.get(integration_id) {
            if let Ok(downcast) = Arc::clone(existing).downcast::<T>() {
                return downcast;
            }
        }

        let value: Arc<T> = Arc::new(init());
        guard.insert(
            integration_id,
            Arc::clone(&value) as Arc<dyn Any + Send + Sync>,
        );
        value
    }

    /// Clears all stored values.
    ///
    /// # Panics
    ///
    /// Panics if the inner lock is poisoned.
    pub fn clear(&self) {
        let mut guard = self
            .inner
            .lock()
            .expect("should lock integration document state");
        guard.clear();
    }
}

/// Describes an HTTP endpoint exposed by an integration.
#[derive(Clone, Debug)]
pub struct IntegrationEndpoint {
    pub method: Method,
    pub path: String,
}

impl IntegrationEndpoint {
    #[must_use]
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn post(path: impl Into<String>) -> Self {
        Self {
            method: Method::POST,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn put(path: impl Into<String>) -> Self {
        Self {
            method: Method::PUT,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            method: Method::DELETE,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn patch(path: impl Into<String>) -> Self {
        Self {
            method: Method::PATCH,
            path: path.into(),
        }
    }
}

/// Trait implemented by integration proxies that expose HTTP endpoints.
#[async_trait(?Send)]
pub trait IntegrationProxy: Send + Sync {
    /// Integration identifier used for logging and optional URL namespace.
    /// Use this with the `namespaced_*` helper methods to automatically prefix routes.
    fn integration_name(&self) -> &'static str;

    /// Routes handled by this integration.
    /// to automatically namespace routes under `/integrations/{integration_name()}/`,
    /// or define routes manually for backwards compatibility.
    fn routes(&self) -> Vec<IntegrationEndpoint>;

    /// Handle the proxied request.
    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>>;

    /// Helper to create a namespaced GET endpoint.
    /// Automatically prefixes the path with `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// self.namespaced_get("/auction")  // becomes /integrations/my_integration/auction
    /// ```
    fn get(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("/integrations/{}{}", self.integration_name(), path);
        IntegrationEndpoint::get(full_path)
    }

    /// Helper to create a namespaced POST endpoint.
    /// Automatically prefixes the path with `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// self.post("/auction")  // becomes /integrations/my_integration/auction
    /// ```
    fn post(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("/integrations/{}{}", self.integration_name(), path);
        IntegrationEndpoint::post(full_path)
    }

    /// Helper to create a namespaced PUT endpoint.
    /// Automatically prefixes the path with `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// self.put("/users")  // becomes /integrations/my_integration/users
    /// ```
    fn put(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("/integrations/{}{}", self.integration_name(), path);
        IntegrationEndpoint::put(full_path)
    }

    /// Helper to create a namespaced DELETE endpoint.
    /// Automatically prefixes the path with `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// self.delete("/users/123")  // becomes /integrations/my_integration/users/123
    /// ```
    fn delete(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("/integrations/{}{}", self.integration_name(), path);
        IntegrationEndpoint::delete(full_path)
    }

    /// Helper to create a namespaced PATCH endpoint.
    /// Automatically prefixes the path with `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// self.patch("/settings")  // becomes /integrations/my_integration/settings
    /// ```
    fn patch(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("/integrations/{}{}", self.integration_name(), path);
        IntegrationEndpoint::patch(full_path)
    }
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

/// Context for HTML post-processors.
#[derive(Debug)]
pub struct IntegrationHtmlContext<'a> {
    pub request_host: &'a str,
    pub request_scheme: &'a str,
    pub origin_host: &'a str,
    pub document_state: &'a IntegrationDocumentState,
}

/// Trait for integration-provided HTML post-processors.
/// These run after streaming HTML processing to handle cases that require
/// access to the complete HTML (e.g., cross-script RSC T-chunks).
pub trait IntegrationHtmlPostProcessor: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;

    /// Fast preflight check to decide whether post-processing should run for this document.
    ///
    /// Implementations should keep this cheap (e.g., a substring check) because it may run on
    /// every HTML response when the integration is enabled.
    fn should_process(&self, html: &str, ctx: &IntegrationHtmlContext<'_>) -> bool {
        let _ = (html, ctx);
        false
    }

    /// Post-process complete HTML content.
    /// This is called after streaming HTML processing with the complete HTML.
    /// Implementations should mutate `html` in-place and return `true` when changes were made.
    fn post_process(&self, html: &mut String, ctx: &IntegrationHtmlContext<'_>) -> bool;
}

/// Registration payload returned by integration builders.
pub struct IntegrationRegistration {
    pub integration_id: &'static str,
    pub proxies: Vec<Arc<dyn IntegrationProxy>>,
    pub attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    pub script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    pub html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
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
                html_post_processors: Vec::new(),
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
    pub fn with_html_post_processor(
        mut self,
        processor: Arc<dyn IntegrationHtmlPostProcessor>,
    ) -> Self {
        self.registration.html_post_processors.push(processor);
        self
    }

    #[must_use]
    pub fn build(self) -> IntegrationRegistration {
        self.registration
    }
}

type RouteValue = (Arc<dyn IntegrationProxy>, &'static str);

struct IntegrationRegistryInner {
    // Method-specific routers for O(log n) lookups
    get_router: Router<RouteValue>,
    post_router: Router<RouteValue>,
    put_router: Router<RouteValue>,
    delete_router: Router<RouteValue>,
    patch_router: Router<RouteValue>,

    // Metadata for introspection
    routes: Vec<(IntegrationEndpoint, &'static str)>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
}

impl Default for IntegrationRegistryInner {
    fn default() -> Self {
        Self {
            get_router: Router::new(),
            post_router: Router::new(),
            put_router: Router::new(),
            delete_router: Router::new(),
            patch_router: Router::new(),
            routes: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_post_processors: Vec::new(),
        }
    }
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
    ///
    /// # Errors
    ///
    /// Returns an error if route registration fails due to duplicate routes or invalid paths.
    ///
    /// # Panics
    ///
    /// Panics if a route path ends with `/*` but `strip_suffix` unexpectedly fails (invariant violation).
    pub fn new(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let mut inner = IntegrationRegistryInner::default();

        for builder in crate::integrations::builders() {
            if let Some(registration) = builder(settings) {
                for proxy in registration.proxies {
                    for route in proxy.routes() {
                        let value = (proxy.clone(), registration.integration_id);

                        // Convert /* wildcard to matchit's {*rest} syntax
                        let matchit_path = if route.path.ends_with("/*") {
                            format!(
                                "{}/{{*rest}}",
                                route
                                    .path
                                    .strip_suffix("/*")
                                    .expect("path should end with '/*'")
                            )
                        } else {
                            route.path.clone()
                        };

                        // Select appropriate router and insert
                        let router = match route.method {
                            Method::GET => &mut inner.get_router,
                            Method::POST => &mut inner.post_router,
                            Method::PUT => &mut inner.put_router,
                            Method::DELETE => &mut inner.delete_router,
                            Method::PATCH => &mut inner.patch_router,
                            _ => {
                                log::warn!(
                                    "Unsupported HTTP method {} for route {}",
                                    route.method,
                                    route.path
                                );
                                continue;
                            }
                        };

                        if let Err(e) = router.insert(&matchit_path, value) {
                            return Err(Report::new(TrustedServerError::Configuration {
                                message: format!(
                                    "Integration route registration failed for {} {}: {:?}",
                                    route.method, route.path, e
                                ),
                            }));
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
                inner
                    .html_post_processors
                    .extend(registration.html_post_processors.into_iter());
            }
        }

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    fn find_route(&self, method: &Method, path: &str) -> Option<&RouteValue> {
        let router = match *method {
            Method::GET => &self.inner.get_router,
            Method::POST => &self.inner.post_router,
            Method::PUT => &self.inner.put_router,
            Method::DELETE => &self.inner.delete_router,
            Method::PATCH => &self.inner.patch_router,
            _ => return None, // Unsupported method
        };

        router.at(path).ok().map(|matched| matched.value)
    }

    /// Return true when any proxy is registered for the provided route.
    #[must_use]
    pub fn has_route(&self, method: &Method, path: &str) -> bool {
        self.find_route(method, path).is_some()
    }

    /// Dispatch a proxy request when an integration handles the path.
    ///
    /// This method automatically sets the `x-synthetic-id` header and
    /// `synthetic_id` cookie on successful responses.
    #[must_use]
    pub async fn handle_proxy(
        &self,
        method: &Method,
        path: &str,
        settings: &Settings,
        req: Request,
    ) -> Option<Result<Response, Report<TrustedServerError>>> {
        if let Some((proxy, _)) = self.find_route(method, path) {
            // Generate synthetic ID before consuming request
            let synthetic_id_result = get_or_generate_synthetic_id(settings, &req);
            let has_synthetic_cookie = handle_request_cookies(&req)
                .ok()
                .flatten()
                .and_then(|jar| jar.get(COOKIE_SYNTHETIC_ID).map(|_| true))
                .unwrap_or(false);

            let mut result = proxy.handle(settings, req).await;

            // Set synthetic ID header on successful responses
            if let Ok(ref mut response) = result {
                if let Ok(ref synthetic_id) = synthetic_id_result {
                    response.set_header(HEADER_X_SYNTHETIC_ID, synthetic_id.as_str());
                    if !has_synthetic_cookie {
                        response.set_header(
                            header::SET_COOKIE,
                            create_synthetic_cookie(settings, synthetic_id.as_str()),
                        );
                    }
                }
            }
            Some(result)
        } else {
            None
        }
    }

    /// Give integrations a chance to rewrite HTML attributes.
    #[must_use]
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
    #[must_use]
    pub fn script_rewriters(&self) -> Vec<Arc<dyn IntegrationScriptRewriter>> {
        self.inner.script_rewriters.clone()
    }

    /// Expose registered HTML post-processors.
    #[must_use]
    pub fn html_post_processors(&self) -> Vec<Arc<dyn IntegrationHtmlPostProcessor>> {
        self.inner.html_post_processors.clone()
    }

    /// Provide a snapshot of registered integrations and their hooks.
    #[must_use]
    pub fn registered_integrations(&self) -> Vec<IntegrationMetadata> {
        let mut map: BTreeMap<&'static str, IntegrationMetadata> = BTreeMap::new();

        for (route, integration_id) in &self.inner.routes {
            let entry = map
                .entry(*integration_id)
                .or_insert_with(|| IntegrationMetadata::new(integration_id));
            entry.routes.push(IntegrationEndpoint::new(
                route.method.clone(),
                route.path.clone(),
            ));
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
    #[must_use]
    pub fn from_rewriters(
        attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
        script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    ) -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner {
                get_router: Router::new(),
                post_router: Router::new(),
                put_router: Router::new(),
                delete_router: Router::new(),
                patch_router: Router::new(),
                routes: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
                html_post_processors: Vec::new(),
            }),
        }
    }

    #[cfg(test)]
    #[must_use]
    /// Test helper to create a registry from routes.
    ///
    /// # Panics
    ///
    /// Panics if route registration fails due to duplicate or invalid paths.
    pub fn from_routes(routes: Vec<(Method, &str, RouteValue)>) -> Self {
        let mut get_router = Router::new();
        let mut post_router = Router::new();
        let mut put_router = Router::new();
        let mut delete_router = Router::new();
        let mut patch_router = Router::new();

        for (method, path, value) in routes {
            // Convert /* wildcard to matchit's {*rest} syntax
            let matchit_path = if path.ends_with("/*") {
                format!(
                    "{}/{{*rest}}",
                    path.strip_suffix("/*").expect("path should end with '/*'")
                )
            } else {
                path.to_string()
            };

            let router = match method {
                Method::GET => &mut get_router,
                Method::POST => &mut post_router,
                Method::PUT => &mut put_router,
                Method::DELETE => &mut delete_router,
                Method::PATCH => &mut patch_router,
                _ => continue,
            };

            router
                .insert(&matchit_path, value)
                .expect("route registration should succeed");
        }

        Self {
            inner: Arc::new(IntegrationRegistryInner {
                get_router,
                post_router,
                put_router,
                delete_router,
                patch_router,
                routes: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_post_processors: Vec::new(),
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
        fn integration_name(&self) -> &'static str {
            "test"
        }

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

    struct NoopHtmlPostProcessor;

    impl IntegrationHtmlPostProcessor for NoopHtmlPostProcessor {
        fn integration_id(&self) -> &'static str {
            "noop"
        }

        fn post_process(&self, _html: &mut String, _ctx: &IntegrationHtmlContext<'_>) -> bool {
            false
        }
    }

    #[test]
    fn default_html_post_processor_should_process_is_false() {
        let processor = NoopHtmlPostProcessor;
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "proxy.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            document_state: &document_state,
        };

        assert!(
            !processor.should_process("<html></html>", &ctx),
            "Default `should_process` should be false to avoid running post-processing unexpectedly"
        );
    }

    #[test]
    fn test_exact_route_matching() {
        let routes = vec![(
            Method::GET,
            "/integrations/test/exact",
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        )];

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
        let routes = vec![(
            Method::GET,
            "/integrations/lockr/api/*",
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        )];

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
        let routes = vec![
            (
                Method::GET,
                "/integrations/test/api/*",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
            ),
            (
                Method::GET,
                "/integrations/test/exact",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
            ),
        ];

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
        let routes = vec![
            (
                Method::GET,
                "/integrations/lockr/api/*",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
            ),
            (
                Method::POST,
                "/integrations/lockr/api/*",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
            ),
            (
                Method::GET,
                "/integrations/testlight/api/*",
                (
                    Arc::new(MockProxy) as Arc<dyn IntegrationProxy>,
                    "testlight",
                ),
            ),
        ];

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
        let routes = vec![(
            Method::GET,
            "/integrations/lockr/api/*",
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "lockr"),
        )];

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
        let routes = vec![(
            Method::GET,
            "/api/*",
            (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
        )];

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

    #[test]
    fn test_helper_methods_create_namespaced_routes() {
        let proxy = Arc::new(MockProxy);

        // Test all HTTP method helpers
        let get_endpoint = proxy.get("/users");
        assert_eq!(get_endpoint.method, Method::GET);
        assert_eq!(get_endpoint.path, "/integrations/test/users");

        let post_endpoint = proxy.post("/users");
        assert_eq!(post_endpoint.method, Method::POST);
        assert_eq!(post_endpoint.path, "/integrations/test/users");

        let put_endpoint = proxy.put("/users");
        assert_eq!(put_endpoint.method, Method::PUT);
        assert_eq!(put_endpoint.path, "/integrations/test/users");

        let delete_endpoint = proxy.delete("/users");
        assert_eq!(delete_endpoint.method, Method::DELETE);
        assert_eq!(delete_endpoint.path, "/integrations/test/users");

        let patch_endpoint = proxy.patch("/users");
        assert_eq!(patch_endpoint.method, Method::PATCH);
        assert_eq!(patch_endpoint.path, "/integrations/test/users");
    }

    #[test]
    fn test_put_delete_patch_routes() {
        let routes = vec![
            (
                Method::PUT,
                "/integrations/test/users",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
            ),
            (
                Method::DELETE,
                "/integrations/test/users",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
            ),
            (
                Method::PATCH,
                "/integrations/test/users",
                (Arc::new(MockProxy) as Arc<dyn IntegrationProxy>, "test"),
            ),
        ];

        let registry = IntegrationRegistry::from_routes(routes);

        // Should match PUT, DELETE, and PATCH routes
        assert!(registry.has_route(&Method::PUT, "/integrations/test/users"));
        assert!(registry.has_route(&Method::DELETE, "/integrations/test/users"));
        assert!(registry.has_route(&Method::PATCH, "/integrations/test/users"));

        // Should not match other methods on same path
        assert!(!registry.has_route(&Method::GET, "/integrations/test/users"));
        assert!(!registry.has_route(&Method::POST, "/integrations/test/users"));
    }

    // Tests for synthetic ID header on proxy responses

    use crate::cookies::parse_cookies_to_jar;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn cookie_jar_finds_synthetic_id() {
        let cookies = "other=value; synthetic_id=abc123; more=stuff";
        let jar = parse_cookies_to_jar(cookies);
        assert!(
            jar.get(COOKIE_SYNTHETIC_ID).is_some(),
            "Should detect synthetic_id cookie"
        );
    }

    #[test]
    fn cookie_jar_handles_missing_cookie() {
        let cookies = "other=value; session=xyz";
        let jar = parse_cookies_to_jar(cookies);
        assert!(
            jar.get(COOKIE_SYNTHETIC_ID).is_none(),
            "Should not find synthetic_id when missing"
        );
    }

    #[test]
    fn cookie_jar_handles_empty_cookies() {
        let cookies = "";
        let jar = parse_cookies_to_jar(cookies);
        assert!(
            jar.get(COOKIE_SYNTHETIC_ID).is_none(),
            "Should handle empty cookie string"
        );
    }

    /// Mock proxy that returns a simple 200 OK response
    struct SyntheticIdTestProxy;

    #[async_trait(?Send)]
    impl IntegrationProxy for SyntheticIdTestProxy {
        fn integration_name(&self) -> &'static str {
            "synthetic_id_test"
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            vec![
                IntegrationEndpoint {
                    method: Method::GET,
                    path: "/integrations/test/synthetic".to_string(),
                },
                IntegrationEndpoint {
                    method: Method::POST,
                    path: "/integrations/test/synthetic".to_string(),
                },
            ]
        }

        async fn handle(
            &self,
            _settings: &Settings,
            _req: Request,
        ) -> Result<Response, Report<TrustedServerError>> {
            // Return a simple response without the synthetic ID header.
            // The registry's handle_proxy should add it.
            Ok(Response::from_status(fastly::http::StatusCode::OK).with_body("test response"))
        }
    }

    #[test]
    fn handle_proxy_sets_synthetic_id_header_on_response() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::GET,
            "/integrations/test/synthetic",
            (
                Arc::new(SyntheticIdTestProxy) as Arc<dyn IntegrationProxy>,
                "synthetic_id_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        // Create a request without a synthetic ID cookie
        let req = Request::get("https://test-publisher.com/integrations/test/synthetic");

        // Call handle_proxy (uses futures executor in test environment)
        let result = futures::executor::block_on(registry.handle_proxy(
            &Method::GET,
            "/integrations/test/synthetic",
            &settings,
            req,
        ));

        // Should have matched and returned a response
        assert!(result.is_some(), "Should find route and handle request");
        let response = result.unwrap();
        assert!(response.is_ok(), "Handler should succeed");

        let response = response.unwrap();

        // Verify x-synthetic-id header is present
        assert!(
            response.get_header(HEADER_X_SYNTHETIC_ID).is_some(),
            "Response should have x-synthetic-id header"
        );

        // Verify Set-Cookie header is present (since no cookie was in request)
        let set_cookie = response.get_header(header::SET_COOKIE);
        assert!(
            set_cookie.is_some(),
            "Response should have Set-Cookie header for synthetic_id"
        );

        let cookie_value = set_cookie.unwrap().to_str().unwrap();
        assert!(
            cookie_value.contains(COOKIE_SYNTHETIC_ID),
            "Set-Cookie should contain synthetic_id cookie, got: {}",
            cookie_value
        );
    }

    #[test]
    fn handle_proxy_skips_cookie_when_already_present() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::GET,
            "/integrations/test/synthetic",
            (
                Arc::new(SyntheticIdTestProxy) as Arc<dyn IntegrationProxy>,
                "synthetic_id_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        // Create a request WITH an existing synthetic_id cookie
        let mut req = Request::get("https://test-publisher.com/integrations/test/synthetic");
        req.set_header(header::COOKIE, "synthetic_id=existing_id_12345");

        let result = futures::executor::block_on(registry.handle_proxy(
            &Method::GET,
            "/integrations/test/synthetic",
            &settings,
            req,
        ));

        assert!(result.is_some(), "Should find route");
        let response = result.unwrap();
        assert!(response.is_ok(), "Handler should succeed");

        let response = response.unwrap();

        // Should still have x-synthetic-id header
        assert!(
            response.get_header(HEADER_X_SYNTHETIC_ID).is_some(),
            "Response should still have x-synthetic-id header"
        );

        // But should NOT set the cookie again (it's already present)
        let set_cookie = response.get_header(header::SET_COOKIE);

        // Either no Set-Cookie, or if present, not for synthetic_id
        if let Some(cookie) = set_cookie {
            let cookie_str = cookie.to_str().unwrap_or("");
            assert!(
                !cookie_str.contains(COOKIE_SYNTHETIC_ID),
                "Should not set duplicate synthetic_id cookie, got: {}",
                cookie_str
            );
        }
    }

    #[test]
    fn handle_proxy_works_with_post_method() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::POST,
            "/integrations/test/synthetic",
            (
                Arc::new(SyntheticIdTestProxy) as Arc<dyn IntegrationProxy>,
                "synthetic_id_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        let req = Request::post("https://test-publisher.com/integrations/test/synthetic")
            .with_body("test body");

        let result = futures::executor::block_on(registry.handle_proxy(
            &Method::POST,
            "/integrations/test/synthetic",
            &settings,
            req,
        ));

        assert!(result.is_some(), "Should find POST route");
        let response = result.unwrap();
        assert!(response.is_ok(), "Handler should succeed");

        let response = response.unwrap();
        assert!(
            response.get_header(HEADER_X_SYNTHETIC_ID).is_some(),
            "POST response should have x-synthetic-id header"
        );
    }
}
