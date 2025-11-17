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

/// Describes an HTTP endpoint exposed by an integration.
#[derive(Clone)]
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

struct RegisteredRoute {
    method: Method,
    path: &'static str,
    proxy: Arc<dyn IntegrationProxy>,
}

#[derive(Default)]
struct IntegrationRegistryInner {
    routes: Vec<RegisteredRoute>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
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

        if let Some(integration) = crate::integrations::starlight::build(settings) {
            for route in integration.routes() {
                inner.routes.push(RegisteredRoute {
                    method: route.method.clone(),
                    path: route.path,
                    proxy: integration.clone(),
                });
            }
            inner.html_rewriters.push(integration);
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
        self.inner
            .html_rewriters
            .iter()
            .find(|rewriter| rewriter.handles_attribute(attr_name))
            .and_then(|rewriter| rewriter.rewrite(attr_name, attr_value, ctx))
    }
}
