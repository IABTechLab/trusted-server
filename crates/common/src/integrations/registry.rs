use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use fastly::http::Method;
use fastly::{Request, Response};

use crate::error::TrustedServerError;
use crate::settings::Settings;

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
    /// Attempt to rewrite the attribute value. Return `Some(new_value)` to
    /// update the attribute or `None` to keep the original value.
    fn rewrite(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<String>;
}

/// Trait for integration-provided inline script/text rewrite hooks.
pub trait IntegrationScriptRewriter: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;
    /// CSS selector (e.g. `script#__NEXT_DATA__`) that should trigger this rewriter.
    fn selector(&self) -> &'static str;
    /// Attempt to rewrite the inline text content for the selector.
    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> Option<String>;
}

/// Registration payload returned by integration builders.
pub struct IntegrationRegistration {
    pub integration_id: &'static str,
    pub proxies: Vec<Arc<dyn IntegrationProxy>>,
    pub attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    pub script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    pub assets: Vec<String>,
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
                assets: Vec::new(),
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
    pub fn with_asset(mut self, asset: impl Into<String>) -> Self {
        self.registration.assets.push(asset.into());
        self
    }

    #[must_use]
    pub fn build(self) -> IntegrationRegistration {
        self.registration
    }
}

struct RegisteredRoute {
    method: Method,
    path: &'static str,
    proxy: Arc<dyn IntegrationProxy>,
    integration_id: &'static str,
}

#[derive(Default)]
struct IntegrationRegistryInner {
    routes: Vec<RegisteredRoute>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    assets: Vec<(&'static str, String)>,
}

/// Summary of registered integration capabilities.
#[derive(Debug, Clone)]
pub struct IntegrationMetadata {
    pub id: &'static str,
    pub routes: Vec<IntegrationEndpoint>,
    pub attribute_rewriters: usize,
    pub script_selectors: Vec<&'static str>,
    pub assets: Vec<String>,
}

impl IntegrationMetadata {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            routes: Vec::new(),
            attribute_rewriters: 0,
            script_selectors: Vec::new(),
            assets: Vec::new(),
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
                        inner.routes.push(RegisteredRoute {
                            method: route.method.clone(),
                            path: route.path,
                            proxy: proxy.clone(),
                            integration_id: registration.integration_id,
                        });
                    }
                }
                inner
                    .html_rewriters
                    .extend(registration.attribute_rewriters.into_iter());
                inner
                    .script_rewriters
                    .extend(registration.script_rewriters.into_iter());
                inner.assets.extend(
                    registration
                        .assets
                        .into_iter()
                        .map(|asset| (registration.integration_id, asset)),
                );
            }
        }

        Self {
            inner: Arc::new(inner),
        }
    }

    /// Return true when any proxy is registered for the provided route.
    pub fn has_route(&self, method: &Method, path: &str) -> bool {
        self.inner
            .routes
            .iter()
            .any(|r| r.method == method && r.path == path)
    }

    /// Dispatch a proxy request when an integration handles the path.
    pub async fn handle_proxy(
        &self,
        method: &Method,
        path: &str,
        settings: &Settings,
        req: Request,
    ) -> Option<Result<Response, Report<TrustedServerError>>> {
        for route in &self.inner.routes {
            if route.method == method && route.path == path {
                return Some(route.proxy.handle(settings, req).await);
            }
        }
        None
    }

    /// Give integrations a chance to rewrite HTML attributes.
    pub fn rewrite_attribute(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<String> {
        let mut current = attr_value.to_string();
        let mut changed = false;
        for rewriter in &self.inner.html_rewriters {
            if !rewriter.handles_attribute(attr_name) {
                continue;
            }
            if let Some(next_value) = rewriter.rewrite(attr_name, &current, ctx) {
                current = next_value;
                changed = true;
            }
        }

        if changed {
            Some(current)
        } else {
            None
        }
    }

    /// Expose registered script/text rewriters for HTML processing.
    pub fn script_rewriters(&self) -> Vec<Arc<dyn IntegrationScriptRewriter>> {
        self.inner.script_rewriters.clone()
    }

    /// Provide a snapshot of registered integrations and their hooks.
    pub fn registered_integrations(&self) -> Vec<IntegrationMetadata> {
        let mut map: BTreeMap<&'static str, IntegrationMetadata> = BTreeMap::new();

        for route in &self.inner.routes {
            let entry = map
                .entry(route.integration_id)
                .or_insert_with(|| IntegrationMetadata::new(route.integration_id));
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

        for (integration_id, asset) in &self.inner.assets {
            let entry = map
                .entry(*integration_id)
                .or_insert_with(|| IntegrationMetadata::new(integration_id));
            entry.assets.push(asset.clone());
        }

        map.into_values().collect()
    }
}
