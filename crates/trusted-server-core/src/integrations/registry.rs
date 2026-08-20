use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Method, Request, Response};
use matchit::Router;
use serde::Serialize;

use crate::constants::HEADER_X_TS_EC;
use crate::ec::EcContext;
use crate::ec::kv::KvIdentityGraph;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::is_navigation_request;
use crate::platform::RuntimeServices;
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
    pub is_last_in_text_node: bool,
    pub document_state: &'a IntegrationDocumentState,
}

type IntegrationDocumentStateMap = BTreeMap<(&'static str, TypeId), Arc<dyn Any + Send + Sync>>;

/// Per-document state shared between HTML/script rewriters and post-processors.
///
/// This exists to support multi-phase HTML processing without requiring a second HTML parse.
#[derive(Clone, Default)]
pub struct IntegrationDocumentState {
    inner: Arc<Mutex<IntegrationDocumentStateMap>>,
}

impl std::fmt::Debug for IntegrationDocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<(&'static str, TypeId)> = {
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
        let value = guard.get(&(integration_id, TypeId::of::<T>()))?;
        let cloned: Arc<dyn Any + Send + Sync> = Arc::clone(value);
        cloned.downcast::<T>().ok()
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

        let key = (integration_id, TypeId::of::<T>());
        if let Some(existing) = guard.get(&key)
            && let Ok(downcast) = Arc::clone(existing).downcast::<T>()
        {
            return downcast;
        }

        let value: Arc<T> = Arc::new(init());
        guard.insert(key, Arc::clone(&value) as Arc<dyn Any + Send + Sync>);
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
///
/// `Send + Sync` bounds are required so trait objects can be stored in
/// `Arc<dyn IntegrationProxy>` and shared across the single-threaded WASM
/// request context. The `?Send` on the async methods is intentional — see the
/// `!Send` design rationale on [`crate::platform::PlatformPendingRequest`] for
/// the full explanation. On wasm32 these bounds are compatible because the runtime is
/// single-threaded.
#[async_trait(?Send)]
pub trait IntegrationProxy: Send + Sync {
    /// Integration identifier used for logging and optional URL namespace.
    /// Use this with the `namespaced_*` helper methods to automatically prefix routes.
    fn integration_name(&self) -> &'static str;

    /// Returns the URL path prefix for this integration's proxy routes.
    ///
    /// Override this to provide a custom, customer-specific proxy path that is
    /// harder for ad blockers to target. When not overridden, defaults to
    /// `/integrations/{integration_name()}`.
    ///
    /// # Example
    /// ```ignore
    /// fn proxy_prefix(&self) -> String {
    ///     "/my-custom-path".to_string()  // instead of /integrations/didomi
    /// }
    /// ```
    fn proxy_prefix(&self) -> String {
        format!("/integrations/{}", self.integration_name())
    }

    /// Routes handled by this integration.
    /// to automatically namespace routes under the proxy prefix,
    /// or define routes manually for backwards compatibility.
    fn routes(&self) -> Vec<IntegrationEndpoint>;

    /// Handle the proxied request.
    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>>;

    /// Helper to create a namespaced GET endpoint.
    /// Automatically prefixes the path with the integration's `proxy_prefix()`.
    fn get(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("{}{}", self.proxy_prefix(), path);
        IntegrationEndpoint::get(full_path)
    }

    /// Helper to create a namespaced POST endpoint.
    /// Automatically prefixes the path with the integration's `proxy_prefix()`.
    fn post(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("{}{}", self.proxy_prefix(), path);
        IntegrationEndpoint::post(full_path)
    }

    /// Helper to create a namespaced PUT endpoint.
    /// Automatically prefixes the path with the integration's `proxy_prefix()`.
    fn put(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("{}{}", self.proxy_prefix(), path);
        IntegrationEndpoint::put(full_path)
    }

    /// Helper to create a namespaced DELETE endpoint.
    /// Automatically prefixes the path with the integration's `proxy_prefix()`.
    fn delete(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("{}{}", self.proxy_prefix(), path);
        IntegrationEndpoint::delete(full_path)
    }

    /// Helper to create a namespaced PATCH endpoint.
    /// Automatically prefixes the path with the integration's `proxy_prefix()`.
    fn patch(&self, path: &str) -> IntegrationEndpoint {
        let full_path = format!("{}{}", self.proxy_prefix(), path);
        IntegrationEndpoint::patch(full_path)
    }
}

/// Input passed to integration request filters.
pub struct RequestFilterInput<'a> {
    pub settings: &'a Settings,
    pub services: &'a RuntimeServices,
    pub request: &'a mut Request<EdgeBody>,
    pub geo_info: Option<&'a GeoInfo>,
    /// Whether the request matches a registered integration proxy route.
    pub is_integration_route: bool,
}

/// How a header mutation should be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderMutationMode {
    Set,
    Append,
}

/// Header mutation requested by an integration filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderMutation {
    pub name: String,
    pub value: String,
    pub mode: HeaderMutationMode,
}

impl HeaderMutation {
    #[must_use]
    pub fn set(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            mode: HeaderMutationMode::Set,
        }
    }

    #[must_use]
    pub fn append(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            mode: HeaderMutationMode::Append,
        }
    }
}

/// Request and response effects returned by request filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestFilterEffects {
    pub request_headers: Vec<HeaderMutation>,
    pub response_headers: Vec<HeaderMutation>,
}

impl RequestFilterEffects {
    fn extend(&mut self, next: Self) {
        self.request_headers.extend(next.request_headers);
        self.response_headers.extend(next.response_headers);
    }

    fn apply_to_request(&self, req: &mut Request<EdgeBody>) {
        for mutation in &self.request_headers {
            apply_header_mutation_to_request(req, mutation);
        }
    }

    pub fn apply_to_response(&self, response: &mut Response<EdgeBody>) {
        for mutation in &self.response_headers {
            apply_header_mutation_to_response(response, mutation);
        }
    }
}

/// Decision returned by an integration request filter.
pub enum RequestFilterDecision {
    Continue(RequestFilterEffects),
    Respond {
        response: Box<Response<EdgeBody>>,
        effects: RequestFilterEffects,
    },
}

/// Input passed to [`IntegrationRegistry::filter_request`].
pub struct RequestFilterRegistryInput<'a> {
    pub settings: &'a Settings,
    pub services: &'a RuntimeServices,
    pub req: &'a mut Request<EdgeBody>,
    pub geo_info: Option<&'a GeoInfo>,
}

/// Outcome returned by [`IntegrationRegistry::filter_request`].
pub enum RequestFilterRegistryOutcome {
    Continue(RequestFilterEffects),
    Respond {
        response: Box<Response<EdgeBody>>,
        effects: RequestFilterEffects,
    },
}

/// Trait for integration-provided pre-routing request filters.
#[async_trait(?Send)]
pub trait IntegrationRequestFilter: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;

    /// Filter an incoming request before normal route matching.
    async fn filter_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>>;
}

fn is_forbidden_filter_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "host"
    ) || lower.starts_with("x-ts-")
}

fn apply_header_mutation_to_request(req: &mut Request<EdgeBody>, mutation: &HeaderMutation) {
    if is_forbidden_filter_header(&mutation.name) {
        log::warn!(
            "Skipping forbidden request-filter header: {}",
            mutation.name
        );
        return;
    }

    let Ok(name) = http::HeaderName::from_bytes(mutation.name.as_bytes()) else {
        log::warn!("Skipping invalid request-filter header: {}", mutation.name);
        return;
    };
    let Ok(value) = http::HeaderValue::from_str(&mutation.value) else {
        log::warn!(
            "Skipping invalid request-filter header value: {}",
            mutation.name
        );
        return;
    };

    match mutation.mode {
        HeaderMutationMode::Set => {
            req.headers_mut().insert(name, value);
        }
        HeaderMutationMode::Append => {
            req.headers_mut().append(name, value);
        }
    }
}

fn apply_header_mutation_to_response(response: &mut Response<EdgeBody>, mutation: &HeaderMutation) {
    if is_forbidden_filter_header(&mutation.name) {
        log::warn!(
            "Skipping forbidden response-filter header: {}",
            mutation.name
        );
        return;
    }

    let Ok(name) = http::HeaderName::from_bytes(mutation.name.as_bytes()) else {
        log::warn!("Skipping invalid response-filter header: {}", mutation.name);
        return;
    };
    let Ok(value) = http::HeaderValue::from_str(&mutation.value) else {
        log::warn!(
            "Skipping invalid response-filter header value: {}",
            mutation.name
        );
        return;
    };

    match mutation.mode {
        HeaderMutationMode::Set => {
            response.headers_mut().insert(name, value);
        }
        HeaderMutationMode::Append => {
            response.headers_mut().append(name, value);
        }
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

/// Trait for integration-provided HTML head injections.
pub trait IntegrationHeadInjector: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;
    /// Return HTML snippets to insert at the start of `<head>`.
    fn head_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String>;

    /// Return attributes to add to the publisher TSJS bundle tag.
    fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

/// Registration payload returned by integration builders.
pub struct IntegrationRegistration {
    pub integration_id: &'static str,
    pub js_deferred: bool,
    pub js_disabled: bool,
    pub proxies: Vec<Arc<dyn IntegrationProxy>>,
    pub attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    pub script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    pub html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
    pub head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    pub request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    pub(crate) browser_config_v1: Option<serde_json::Value>,
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
                js_deferred: false,
                js_disabled: false,
                proxies: Vec::new(),
                attribute_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_post_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                browser_config_v1: None,
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
    pub fn with_head_injector(mut self, injector: Arc<dyn IntegrationHeadInjector>) -> Self {
        self.registration.head_injectors.push(injector);
        self
    }

    #[must_use]
    pub fn with_request_filter(mut self, filter: Arc<dyn IntegrationRequestFilter>) -> Self {
        self.registration.request_filters.push(filter);
        self
    }

    /// Attach this product's explicit browser-safe version-1 projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the typed projection cannot be represented as JSON.
    pub(crate) fn with_browser_config_v1<T: Serialize>(
        mut self,
        config: &T,
    ) -> Result<Self, Report<TrustedServerError>> {
        self.registration.browser_config_v1 =
            Some(serde_json::to_value(config).map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Integration {} browser config serialization failed: {error}",
                        self.registration.integration_id
                    ),
                })
            })?);
        Ok(self)
    }

    /// Attach an exact empty object for a product with no browser-selectable fields.
    pub(crate) fn with_empty_browser_config_v1(
        mut self,
    ) -> Result<Self, Report<TrustedServerError>> {
        #[derive(Serialize)]
        struct EmptyBrowserConfigV1 {}

        self.registration.browser_config_v1 = Some(
            serde_json::to_value(EmptyBrowserConfigV1 {}).map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Integration {} browser config serialization failed: {error}",
                        self.registration.integration_id
                    ),
                })
            })?,
        );
        Ok(self)
    }

    /// Mark this integration's JS module for deferred loading via
    /// `<script defer>` instead of the main synchronous bundle.
    #[must_use]
    pub fn with_deferred_js(mut self) -> Self {
        self.registration.js_deferred = true;
        self
    }

    /// Disable TSJS module inclusion for an integration that is handled by other assets.
    #[must_use]
    pub fn without_js(mut self) -> Self {
        self.registration.js_disabled = true;
        self.registration.js_deferred = false;
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
    head_router: Router<RouteValue>,
    options_router: Router<RouteValue>,
    reserved_proxies: Vec<(&'static str, Arc<dyn IntegrationProxy>)>,

    // Metadata for introspection
    routes: Vec<(IntegrationEndpoint, &'static str)>,
    enabled_integration_ids: Vec<&'static str>,
    deferred_js_ids: Vec<&'static str>,
    disabled_js_ids: Vec<&'static str>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
    head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    integration_configs_v1: crate::tsjs::IntegrationConfigsV1,
    tsjs_static_transport: TsjsStaticTransportV1,
}

impl Default for IntegrationRegistryInner {
    fn default() -> Self {
        Self {
            get_router: Router::new(),
            post_router: Router::new(),
            put_router: Router::new(),
            delete_router: Router::new(),
            patch_router: Router::new(),
            head_router: Router::new(),
            options_router: Router::new(),
            reserved_proxies: Vec::new(),
            routes: Vec::new(),
            enabled_integration_ids: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_post_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters: Vec::new(),
            integration_configs_v1: crate::tsjs::IntegrationConfigsV1::empty(),
            tsjs_static_transport: TsjsStaticTransportV1::default(),
        }
    }
}

#[derive(Default)]
struct TsjsStaticTransportV1 {
    artifacts_by_hash: HashMap<String, crate::tsjs::TsjsStaticArtifactV1>,
    takeover_hash_by_selection: HashMap<TsjsCatalogSelectionV1, String>,
    first_display_hash_by_mask: HashMap<u16, String>,
    first_display_mask_by_hash: HashMap<String, u16>,
}

impl TsjsStaticTransportV1 {
    fn new(
        inner: &IntegrationRegistryInner,
        creative_boot: crate::tsjs::CreativeBootConfigV1,
    ) -> Self {
        let mut transport = Self::default();
        let creative_artifact = crate::tsjs::creative_tsjs_static_artifact_v1().clone();
        transport
            .artifacts_by_hash
            .insert(creative_artifact.hash().to_owned(), creative_artifact);

        for render_trace_overlay in [false, true] {
            for selection in tsjs_static_transport_selections(inner, render_trace_overlay) {
                let normalized_selection = normalize_tsjs_transport_selection(inner, selection);
                let takeover_ids = tsjs_selected_catalog_metadata(inner, normalized_selection)
                    .into_iter()
                    .filter(|metadata| {
                        metadata.phase == Some(trusted_server_js::TsjsModulePhase::Takeover)
                    })
                    .map(|metadata| metadata.id)
                    .collect::<Vec<_>>();
                let artifact = if takeover_ids.as_slice() == crate::tsjs::creative_tsjs_module_ids()
                {
                    crate::tsjs::creative_tsjs_static_artifact_v1().clone()
                } else {
                    crate::tsjs::TsjsStaticArtifactV1::new(&takeover_ids)
                };
                let hash = artifact.hash().to_owned();
                transport
                    .artifacts_by_hash
                    .entry(hash.clone())
                    .or_insert(artifact);
                transport
                    .takeover_hash_by_selection
                    .insert(normalized_selection, hash);
            }
        }

        for (mask, slices) in first_display_static_transport_selections(inner, creative_boot) {
            let artifact = crate::tsjs::TsjsStaticArtifactV1::new_first_display(mask, &slices)
                .expect("catalog-derived first-display selection should compose");
            let hash = artifact.hash().to_owned();
            transport
                .artifacts_by_hash
                .entry(hash.clone())
                .or_insert(artifact);
            transport
                .first_display_hash_by_mask
                .insert(mask, hash.clone());
            assert!(
                transport
                    .first_display_mask_by_hash
                    .insert(hash, mask)
                    .is_none(),
                "distinct first-display masks should have distinct exact bytes"
            );
        }

        transport
    }
}

/// Summary of registered integration capabilities.
#[derive(Debug, Clone)]
pub struct IntegrationMetadata {
    pub id: &'static str,
    pub routes: Vec<IntegrationEndpoint>,
    pub attribute_rewriters: usize,
    pub script_selectors: Vec<&'static str>,
    pub head_injectors: usize,
    pub request_filters: usize,
}

/// Request/document-owned inputs for generated TSJS catalog predicates.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TsjsCatalogSelectionV1 {
    pub creative_enabled: bool,
    pub creative_click_guard: bool,
    pub creative_render_guard: bool,
    pub gpt_diagnostics_active: bool,
    pub render_trace_overlay: bool,
}

fn tsjs_static_transport_selections(
    inner: &IntegrationRegistryInner,
    render_trace_overlay: bool,
) -> Vec<TsjsCatalogSelectionV1> {
    let diagnostics_configured = inner.enabled_integration_ids.contains(&"gpt_diagnostics");
    let diagnostics_values: &[bool] = if diagnostics_configured {
        &[false, true]
    } else {
        &[false]
    };
    let mut selections = Vec::with_capacity(diagnostics_values.len() * 2);
    for creative_enabled in [false, true] {
        for gpt_diagnostics_active in diagnostics_values {
            selections.push(TsjsCatalogSelectionV1 {
                creative_enabled,
                creative_click_guard: creative_enabled,
                creative_render_guard: false,
                gpt_diagnostics_active: *gpt_diagnostics_active,
                render_trace_overlay,
            });
        }
    }
    selections
}

fn first_display_static_transport_selections(
    inner: &IntegrationRegistryInner,
    creative: crate::tsjs::CreativeBootConfigV1,
) -> Vec<(u16, Vec<&'static str>)> {
    if !inner.enabled_integration_ids.contains(&"gpt") {
        return Vec::new();
    }
    let aps_values: &[bool] = if inner.enabled_integration_ids.contains(&"aps") {
        &[false, true]
    } else {
        &[false]
    };
    let prebid_values: &[bool] = if inner.enabled_integration_ids.contains(&"prebid") {
        &[false, true]
    } else {
        &[false]
    };
    let creative_guard = creative.enabled && (creative.click_guard || creative.render_guard);
    let mut selections = Vec::new();
    for gpt_participates in [false, true] {
        for aps_participates in aps_values {
            if *aps_participates && !gpt_participates {
                continue;
            }
            for prebid_participates in prebid_values {
                if *prebid_participates && !gpt_participates {
                    continue;
                }
                let mut mask = 0_u16;
                let mut slices = Vec::new();
                for (index, metadata) in trusted_server_js::all_first_display_metadata()
                    .into_iter()
                    .enumerate()
                {
                    let selected = match metadata.include {
                        Some("eligible_batch") => true,
                        Some("gpt_initial") => gpt_participates,
                        Some("aps_participates") => *aps_participates,
                        Some("creative_guard") => creative_guard,
                        Some("prebid_participates") => *prebid_participates,
                        Some(predicate) => predicate
                            .strip_prefix("integration:")
                            .is_some_and(|id| inner.enabled_integration_ids.contains(&id)),
                        None => false,
                    };
                    if selected {
                        mask |= 1_u16 << index;
                        slices.push(metadata.id);
                    }
                }
                if slices.first() == Some(&"first_display")
                    && trusted_server_js::first_display_mask_is_permitted(mask)
                {
                    selections.push((mask, slices));
                }
            }
        }
    }
    selections
}

fn normalize_tsjs_transport_selection(
    inner: &IntegrationRegistryInner,
    selection: TsjsCatalogSelectionV1,
) -> TsjsCatalogSelectionV1 {
    let creative_guard = selection.creative_enabled
        && (selection.creative_click_guard || selection.creative_render_guard);
    TsjsCatalogSelectionV1 {
        creative_enabled: creative_guard,
        creative_click_guard: creative_guard,
        creative_render_guard: false,
        gpt_diagnostics_active: selection.gpt_diagnostics_active
            && inner.enabled_integration_ids.contains(&"gpt_diagnostics"),
        render_trace_overlay: selection.render_trace_overlay,
    }
}

fn tsjs_catalog_module_enabled(
    inner: &IntegrationRegistryInner,
    predicate: Option<&str>,
    selection: TsjsCatalogSelectionV1,
) -> bool {
    match predicate {
        Some("always") => true,
        Some("creative_guard") => {
            selection.creative_enabled
                && (selection.creative_click_guard || selection.creative_render_guard)
        }
        Some("gpt_diagnostics_active") => selection.gpt_diagnostics_active,
        Some("diagnostics_presentation") => {
            selection.render_trace_overlay || selection.gpt_diagnostics_active
        }
        Some("prebid_and_gpt") => {
            inner.enabled_integration_ids.contains(&"prebid")
                && inner.enabled_integration_ids.contains(&"gpt")
        }
        Some(predicate) => predicate
            .strip_prefix("integration:")
            .is_some_and(|integration_id| inner.enabled_integration_ids.contains(&integration_id)),
        None => false,
    }
}

fn tsjs_selected_catalog_metadata(
    inner: &IntegrationRegistryInner,
    selection: TsjsCatalogSelectionV1,
) -> Vec<trusted_server_js::TsjsArtifactMetadata> {
    let mut selected = Vec::new();
    let mut provided = std::collections::HashSet::from(["runtime.v1"]);
    for metadata in trusted_server_js::all_integration_metadata() {
        if !tsjs_catalog_module_enabled(inner, metadata.include, selection) {
            continue;
        }
        let requirements_available = metadata
            .inputs
            .iter()
            .all(|declaration| declaration.contains('?') || provided.contains(declaration));
        if !requirements_available {
            continue;
        }
        provided.extend(metadata.outputs.iter().copied());
        selected.push(metadata);
    }
    selected
}

impl IntegrationMetadata {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            routes: Vec::new(),
            attribute_rewriters: 0,
            script_selectors: Vec::new(),
            head_injectors: 0,
            request_filters: 0,
        }
    }
}

/// Inputs to [`IntegrationRegistry::handle_proxy`].
///
/// Bundled into a struct so the dispatch surface stays within the project's
/// 7-argument cap; `ec_context` and `req` participate in the borrow so the
/// whole thing shares one lifetime.
pub struct ProxyDispatchInput<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub settings: &'a Settings,
    pub kv: Option<&'a KvIdentityGraph>,
    pub ec_context: &'a mut EcContext,
    pub services: &'a RuntimeServices,
    pub req: Request<EdgeBody>,
}

/// In-memory registry of integrations discovered from settings.
#[derive(Clone)]
pub struct IntegrationRegistry {
    inner: Arc<IntegrationRegistryInner>,
    creative_boot: crate::tsjs::CreativeBootConfigV1,
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        let mut inner = IntegrationRegistryInner::default();
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
        }
    }
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
        let mut integration_configs_v1 = Vec::new();
        let creative_boot = crate::tsjs::creative_boot_config_v1(settings)?;
        let aps_proxy: Arc<dyn IntegrationProxy> =
            Arc::new(super::aps::ApsV1Integration::from_settings(settings)?);
        inner
            .reserved_proxies
            .push(("/integrations/aps", aps_proxy));

        for builder in crate::integrations::builders() {
            if let Some(registration) = (builder.build)(settings)? {
                debug_assert_eq!(
                    registration.integration_id, builder.id,
                    "integration builder ID should match registration ID"
                );
                inner
                    .enabled_integration_ids
                    .push(registration.integration_id);
                match (
                    crate::tsjs::is_integration_config_product_v1(registration.integration_id),
                    registration.browser_config_v1.clone(),
                ) {
                    (true, Some(config)) => {
                        integration_configs_v1.push((registration.integration_id, config));
                    }
                    (true, None) => {
                        return Err(Report::new(TrustedServerError::Configuration {
                            message: format!(
                                "Integration {} is missing its browser config projection",
                                registration.integration_id
                            ),
                        }));
                    }
                    (false, Some(_)) => {
                        return Err(Report::new(TrustedServerError::Configuration {
                            message: format!(
                                "Integration {} cannot emit a generic browser config",
                                registration.integration_id
                            ),
                        }));
                    }
                    (false, None) => {}
                }

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
                            Method::HEAD => &mut inner.head_router,
                            Method::OPTIONS => &mut inner.options_router,
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
                    .extend(registration.attribute_rewriters);
                inner.script_rewriters.extend(registration.script_rewriters);
                inner
                    .html_post_processors
                    .extend(registration.html_post_processors);
                inner.head_injectors.extend(registration.head_injectors);
                inner.request_filters.extend(registration.request_filters);
                if registration.js_disabled {
                    inner.disabled_js_ids.push(registration.integration_id);
                } else if registration.js_deferred {
                    inner.deferred_js_ids.push(registration.integration_id);
                }
            }
        }

        integration_configs_v1.sort_by_key(|(id, _)| {
            crate::tsjs::integration_config_order_v1(id)
                .expect("browser config product should have a canonical order")
        });
        inner.integration_configs_v1 =
            crate::tsjs::IntegrationConfigsV1::new(integration_configs_v1)?;
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Ok(Self {
            inner: Arc::new(inner),
            creative_boot,
        })
    }

    fn reserved_proxy(&self, path: &str) -> Option<&Arc<dyn IntegrationProxy>> {
        self.inner
            .reserved_proxies
            .iter()
            .find(|(family, _)| {
                path == *family
                    || path
                        .strip_prefix(*family)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
            .map(|(_, proxy)| proxy)
    }

    /// Return true when a hard-cutover family owns this path.
    #[must_use]
    pub fn has_reserved_path(&self, path: &str) -> bool {
        self.reserved_proxy(path).is_some()
    }

    /// Dispatch a hard-cutover family before auth, EC, filters, and fallback.
    #[must_use]
    pub async fn handle_reserved_proxy(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Option<Result<Response<EdgeBody>, Report<TrustedServerError>>> {
        let proxy = self.reserved_proxy(req.uri().path())?;
        Some(proxy.handle(settings, services, req).await)
    }

    fn find_route(&self, method: &Method, path: &str) -> Option<&RouteValue> {
        let router = match *method {
            Method::GET => &self.inner.get_router,
            Method::POST => &self.inner.post_router,
            Method::PUT => &self.inner.put_router,
            Method::DELETE => &self.inner.delete_router,
            Method::PATCH => &self.inner.patch_router,
            Method::HEAD => &self.inner.head_router,
            Method::OPTIONS => &self.inner.options_router,
            _ => return None, // Unsupported method
        };

        router.at(path).ok().map(|matched| matched.value)
    }

    /// Return true when any proxy is registered for the provided route.
    #[must_use]
    pub fn has_route(&self, method: &Method, path: &str) -> bool {
        self.find_route(method, path).is_some()
    }

    /// Run pre-routing request filters.
    ///
    /// Request header mutations are applied immediately so later filters and
    /// route handlers observe enriched headers. Response mutations are returned
    /// to the adapter so it can apply them after normal response finalization.
    ///
    /// # Errors
    ///
    /// Returns an error when an integration request filter returns an error.
    pub async fn filter_request(
        &self,
        input: RequestFilterRegistryInput<'_>,
    ) -> Result<RequestFilterRegistryOutcome, Report<TrustedServerError>> {
        let RequestFilterRegistryInput {
            settings,
            services,
            req,
            geo_info,
        } = input;
        let mut accumulated = RequestFilterEffects::default();
        let is_integration_route = self.has_route(req.method(), req.uri().path());

        for filter in &self.inner.request_filters {
            let decision = filter
                .filter_request(RequestFilterInput {
                    settings,
                    services,
                    request: req,
                    geo_info,
                    is_integration_route,
                })
                .await?;

            match decision {
                RequestFilterDecision::Continue(effects) => {
                    effects.apply_to_request(req);
                    accumulated.extend(RequestFilterEffects {
                        request_headers: Vec::new(),
                        response_headers: effects.response_headers,
                    });
                }
                RequestFilterDecision::Respond { response, effects } => {
                    accumulated.extend(RequestFilterEffects {
                        request_headers: Vec::new(),
                        response_headers: effects.response_headers,
                    });
                    return Ok(RequestFilterRegistryOutcome::Respond {
                        response,
                        effects: accumulated,
                    });
                }
            }
        }

        Ok(RequestFilterRegistryOutcome::Continue(accumulated))
    }

    /// Dispatch a proxy request when an integration handles the path.
    ///
    /// This method removes any caller-supplied `x-ts-ec` before proxying.
    /// Response-side cookie mutation is centralized in EC finalize.
    #[must_use]
    pub async fn handle_proxy(
        &self,
        input: ProxyDispatchInput<'_>,
    ) -> Option<Result<Response<EdgeBody>, Report<TrustedServerError>>> {
        let ProxyDispatchInput {
            method,
            path,
            settings,
            kv,
            ec_context,
            services,
            mut req,
        } = input;
        if let Some((proxy, _)) = self.find_route(method, path) {
            // Organic proxy handler: generate if needed (best effort).
            // Only generate for document navigations — subresource requests
            // may lack consent signals such as the Sec-GPC header.
            if is_navigation_request(&req) {
                if let Err(err) = ec_context.generate_if_needed(settings, kv) {
                    log::warn!("EC generation failed for integration proxy: {err:?}");
                }
            } else {
                log::debug!(
                    "EC generation skipped for integration proxy: non-document request (path={path})",
                );
            }

            // Remove any caller-supplied EC header rather than forwarding it.
            req.headers_mut().remove(HEADER_X_TS_EC.clone());

            Some(proxy.handle(settings, services, req).await)
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
        let mut current = attr_value.to_owned();
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

    /// Check whether any HTML post-processors are registered.
    ///
    /// Cheaper than [`html_post_processors()`](Self::html_post_processors) when
    /// only the presence check is needed — avoids cloning `Vec<Arc<…>>`.
    #[must_use]
    pub fn has_html_post_processors(&self) -> bool {
        !self.inner.html_post_processors.is_empty()
    }

    /// Expose registered HTML post-processors.
    #[must_use]
    pub fn html_post_processors(&self) -> Vec<Arc<dyn IntegrationHtmlPostProcessor>> {
        self.inner.html_post_processors.clone()
    }

    /// Collect HTML snippets for insertion at the start of `<head>`.
    #[must_use]
    pub fn head_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        let mut inserts = Vec::new();
        for injector in &self.inner.head_injectors {
            let mut next = injector.head_inserts(ctx);
            if !next.is_empty() {
                inserts.append(&mut next);
            }
        }
        inserts
    }

    /// Collect static attributes for the publisher TSJS bundle tag.
    #[must_use]
    pub fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
        self.inner
            .head_injectors
            .iter()
            .flat_map(|injector| injector.tsjs_script_tag_attributes())
            .collect()
    }

    /// Provide a snapshot of registered integrations and their hooks.
    #[must_use]
    pub fn registered_integrations(&self) -> Vec<IntegrationMetadata> {
        let mut map: BTreeMap<&'static str, IntegrationMetadata> = BTreeMap::new();

        for integration_id in &self.inner.enabled_integration_ids {
            map.entry(*integration_id)
                .or_insert_with(|| IntegrationMetadata::new(integration_id));
        }

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

        for injector in &self.inner.head_injectors {
            let entry = map
                .entry(injector.integration_id())
                .or_insert_with(|| IntegrationMetadata::new(injector.integration_id()));
            entry.head_injectors += 1;
        }

        for filter in &self.inner.request_filters {
            let entry = map
                .entry(filter.integration_id())
                .or_insert_with(|| IntegrationMetadata::new(filter.integration_id()));
            entry.request_filters += 1;
        }

        map.into_values().collect()
    }

    /// Return whether an integration is enabled in this registry.
    #[must_use]
    pub fn integration_enabled(&self, integration_id: &str) -> bool {
        self.inner.enabled_integration_ids.contains(&integration_id)
    }

    /// Return the exact server-owned creative browser boot policy.
    #[must_use]
    pub fn tsjs_creative_boot(&self) -> crate::tsjs::CreativeBootConfigV1 {
        self.creative_boot
    }

    /// Return JS module IDs that should be included in the tsjs bundle.
    ///
    /// Always includes JS-only modules with no Rust-side registration.
    /// Includes enabled integrations only when the generated TSJS registry has a
    /// corresponding browser module.
    #[must_use]
    pub fn js_module_ids(&self) -> Vec<&'static str> {
        // Core JS-only modules that do not have a Rust-side registration.
        const JS_ALWAYS: &[&str] = &["creative"];

        let mut ids: Vec<&'static str> = JS_ALWAYS.to_vec();

        for id in &self.inner.enabled_integration_ids {
            if trusted_server_js::module_bundle(id).is_some()
                && !self.inner.disabled_js_ids.contains(id)
                && !ids.contains(id)
            {
                ids.push(*id);
            }
        }

        ids
    }

    /// Return whether an integration is enabled, including integrations whose
    /// JavaScript is delivered outside the standard module bundles.
    #[must_use]
    pub fn is_enabled(&self, integration_id: &str) -> bool {
        self.inner.enabled_integration_ids.contains(&integration_id)
    }

    /// Return JS module IDs for the main (synchronous) bundle, excluding
    /// modules registered with [`with_deferred_js`](IntegrationRegistrationBuilder::with_deferred_js).
    #[must_use]
    pub fn js_module_ids_immediate(&self) -> Vec<&'static str> {
        self.js_module_ids()
            .into_iter()
            .filter(|id| !self.inner.deferred_js_ids.contains(id))
            .collect()
    }

    /// Return JS module IDs that should be loaded with `<script defer>`.
    ///
    /// Only includes modules registered with
    /// [`with_deferred_js`](IntegrationRegistrationBuilder::with_deferred_js)
    /// that are actually enabled. Returns an empty vec when no deferred
    /// integrations are configured.
    #[must_use]
    pub fn js_module_ids_deferred(&self) -> Vec<&'static str> {
        self.js_module_ids()
            .into_iter()
            .filter(|id| self.inner.deferred_js_ids.contains(id))
            .collect()
    }

    /// Return enabled TSJS catalog modules in the generated release order.
    ///
    /// This transport selector is deny-unknown and never accepts a phase or
    /// ordering override from settings. Request-scoped diagnostics activation
    /// may further filter the returned diagnostics rows when composing a manifest.
    #[must_use]
    pub fn tsjs_catalog_module_ids(&self, selection: TsjsCatalogSelectionV1) -> Vec<&'static str> {
        tsjs_selected_catalog_metadata(&self.inner, selection)
            .into_iter()
            .map(|metadata| metadata.id)
            .collect()
    }

    /// Return the exact browser-safe product configuration subset for one manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when manifest product predicates and registered projections disagree.
    pub(crate) fn tsjs_integration_configs_v1(
        &self,
        module_ids: &[&str],
    ) -> Result<crate::tsjs::IntegrationConfigsV1, Report<TrustedServerError>> {
        self.inner
            .integration_configs_v1
            .select_for_manifest(module_ids)
    }

    /// Return the enabled parser-blocking catalog slice in canonical order.
    #[must_use]
    pub fn tsjs_takeover_module_ids(&self, selection: TsjsCatalogSelectionV1) -> Vec<&'static str> {
        tsjs_selected_catalog_metadata(&self.inner, selection)
            .into_iter()
            .filter(|metadata| metadata.phase == Some(trusted_server_js::TsjsModulePhase::Takeover))
            .map(|metadata| metadata.id)
            .collect()
    }

    /// Return the enabled post-paint catalog slice in canonical order.
    #[must_use]
    pub fn tsjs_deferred_module_ids(&self, selection: TsjsCatalogSelectionV1) -> Vec<&'static str> {
        tsjs_selected_catalog_metadata(&self.inner, selection)
            .into_iter()
            .filter(|metadata| metadata.phase == Some(trusted_server_js::TsjsModulePhase::Deferred))
            .map(|metadata| metadata.id)
            .collect()
    }

    /// Return the bounded request-owned variants admitted by static transport.
    ///
    /// Integration predicates remain fixed by this registry. Creative guards and
    /// diagnostics activity are document/session bits, so the content hash is
    /// matched against only their configured, finite candidate combinations.
    #[must_use]
    pub fn tsjs_static_transport_selections(
        &self,
        render_trace_overlay: bool,
    ) -> Vec<TsjsCatalogSelectionV1> {
        tsjs_static_transport_selections(&self.inner, render_trace_overlay)
    }

    /// Return the precomputed takeover artifact for one document selection.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tsjs_takeover_artifact(
        &self,
        selection: TsjsCatalogSelectionV1,
    ) -> Option<&crate::tsjs::TsjsStaticArtifactV1> {
        let normalized_selection = normalize_tsjs_transport_selection(&self.inner, selection);
        let hash = self
            .inner
            .tsjs_static_transport
            .takeover_hash_by_selection
            .get(&normalized_selection)?;
        self.tsjs_static_artifact(hash)
    }

    /// Resolve one admitted unified artifact by its exact content hash.
    #[must_use]
    pub(crate) fn tsjs_static_artifact(
        &self,
        hash: &str,
    ) -> Option<&crate::tsjs::TsjsStaticArtifactV1> {
        self.inner.tsjs_static_transport.artifacts_by_hash.get(hash)
    }

    /// Resolve one configuration-permitted first-display artifact by exact mask and hash.
    #[must_use]
    pub(crate) fn tsjs_first_display_artifact(
        &self,
        mask: u16,
        hash: &str,
    ) -> Option<&crate::tsjs::TsjsStaticArtifactV1> {
        let indexed_hash = self
            .inner
            .tsjs_static_transport
            .first_display_hash_by_mask
            .get(&mask)?;
        if indexed_hash != hash
            || self
                .inner
                .tsjs_static_transport
                .first_display_mask_by_hash
                .get(hash)
                != Some(&mask)
        {
            return None;
        }
        self.tsjs_static_artifact(hash)
    }

    /// Return the finite precomputed first-display masks admitted by this registry.
    #[must_use]
    pub fn tsjs_first_display_masks(&self) -> Vec<u16> {
        let mut masks = self
            .inner
            .tsjs_static_transport
            .first_display_hash_by_mask
            .keys()
            .copied()
            .collect::<Vec<_>>();
        masks.sort_unstable();
        masks
    }

    #[cfg(test)]
    #[must_use]
    pub fn empty_for_tests() -> Self {
        let mut inner = IntegrationRegistryInner::default();
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn from_rewriters(
        attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
        script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    ) -> Self {
        let mut inner = IntegrationRegistryInner {
            get_router: Router::new(),
            post_router: Router::new(),
            put_router: Router::new(),
            delete_router: Router::new(),
            patch_router: Router::new(),
            head_router: Router::new(),
            options_router: Router::new(),
            reserved_proxies: Vec::new(),
            routes: Vec::new(),
            enabled_integration_ids: Vec::new(),
            html_rewriters: attribute_rewriters,
            script_rewriters,
            html_post_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            integration_configs_v1: crate::tsjs::IntegrationConfigsV1::empty(),
            tsjs_static_transport: TsjsStaticTransportV1::default(),
        };
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn from_rewriters_with_head_injectors(
        attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
        script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
        head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    ) -> Self {
        let mut inner = IntegrationRegistryInner {
            get_router: Router::new(),
            post_router: Router::new(),
            put_router: Router::new(),
            delete_router: Router::new(),
            patch_router: Router::new(),
            head_router: Router::new(),
            options_router: Router::new(),
            reserved_proxies: Vec::new(),
            routes: Vec::new(),
            enabled_integration_ids: Vec::new(),
            html_rewriters: attribute_rewriters,
            script_rewriters,
            html_post_processors: Vec::new(),
            head_injectors,
            request_filters: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            integration_configs_v1: crate::tsjs::IntegrationConfigsV1::empty(),
            tsjs_static_transport: TsjsStaticTransportV1::default(),
        };
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn from_request_filters(request_filters: Vec<Arc<dyn IntegrationRequestFilter>>) -> Self {
        let mut inner = IntegrationRegistryInner {
            get_router: Router::new(),
            post_router: Router::new(),
            put_router: Router::new(),
            delete_router: Router::new(),
            patch_router: Router::new(),
            head_router: Router::new(),
            options_router: Router::new(),
            reserved_proxies: Vec::new(),
            routes: Vec::new(),
            enabled_integration_ids: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_post_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters,
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            integration_configs_v1: crate::tsjs::IntegrationConfigsV1::empty(),
            tsjs_static_transport: TsjsStaticTransportV1::default(),
        };
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
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
        let mut head_router = Router::new();
        let mut options_router = Router::new();

        for (method, path, value) in routes {
            // Convert /* wildcard to matchit's {*rest} syntax
            let matchit_path = if path.ends_with("/*") {
                format!(
                    "{}/{{*rest}}",
                    path.strip_suffix("/*").expect("path should end with '/*'")
                )
            } else {
                path.to_owned()
            };

            let router = match method {
                Method::GET => &mut get_router,
                Method::POST => &mut post_router,
                Method::PUT => &mut put_router,
                Method::DELETE => &mut delete_router,
                Method::PATCH => &mut patch_router,
                Method::HEAD => &mut head_router,
                Method::OPTIONS => &mut options_router,
                _ => continue,
            };

            router
                .insert(&matchit_path, value)
                .expect("route registration should succeed");
        }

        let mut inner = IntegrationRegistryInner {
            get_router,
            post_router,
            put_router,
            delete_router,
            patch_router,
            head_router,
            options_router,
            reserved_proxies: Vec::new(),
            routes: Vec::new(),
            enabled_integration_ids: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_post_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            integration_configs_v1: crate::tsjs::IntegrationConfigsV1::empty(),
            tsjs_static_transport: TsjsStaticTransportV1::default(),
        };
        let creative_boot = crate::tsjs::CreativeBootConfigV1::default();
        inner.tsjs_static_transport = TsjsStaticTransportV1::new(&inner, creative_boot);
        Self {
            inner: Arc::new(inner),
            creative_boot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::COOKIE_TS_EC;
    use crate::platform::test_support::noop_services;
    use http::{HeaderValue, StatusCode, header};

    struct DefaultMetadataHeadInjector;

    impl IntegrationHeadInjector for DefaultMetadataHeadInjector {
        fn integration_id(&self) -> &'static str {
            "default-metadata"
        }

        fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
            Vec::new()
        }
    }

    struct StaticMetadataHeadInjector;

    impl IntegrationHeadInjector for StaticMetadataHeadInjector {
        fn integration_id(&self) -> &'static str {
            "static-metadata"
        }

        fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
            Vec::new()
        }

        fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
            vec![
                ("data-ts-gam-attribution", "true"),
                ("data-test-order", "second"),
            ]
        }
    }

    #[test]
    fn tsjs_script_tag_attributes_preserve_registration_order_and_default_empty() {
        let registry = IntegrationRegistry::from_rewriters_with_head_injectors(
            Vec::new(),
            Vec::new(),
            vec![
                Arc::new(DefaultMetadataHeadInjector),
                Arc::new(StaticMetadataHeadInjector),
            ],
        );

        assert_eq!(
            registry.tsjs_script_tag_attributes(),
            vec![
                ("data-ts-gam-attribution", "true"),
                ("data-test-order", "second"),
            ],
            "should omit default-empty metadata and preserve registered attribute order"
        );
    }

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
            _services: &RuntimeServices,
            _req: Request<EdgeBody>,
        ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
            Ok(Response::new(EdgeBody::empty()))
        }
    }

    struct EnrichingRequestFilter;
    #[derive(Clone, Copy)]
    struct RequestAnnotation;

    #[async_trait(?Send)]
    impl IntegrationRequestFilter for EnrichingRequestFilter {
        fn integration_id(&self) -> &'static str {
            "enriching"
        }

        async fn filter_request(
            &self,
            input: RequestFilterInput<'_>,
        ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
            input.request.extensions_mut().insert(RequestAnnotation);
            Ok(RequestFilterDecision::Continue(RequestFilterEffects {
                request_headers: vec![HeaderMutation::set("x-datadome-isbot", "1")],
                response_headers: vec![HeaderMutation::set("x-dd-b", "allowed")],
            }))
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

    struct EchoProxy;

    #[async_trait(?Send)]
    impl IntegrationProxy for EchoProxy {
        fn integration_name(&self) -> &'static str {
            "echo"
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            vec![]
        }

        async fn handle(
            &self,
            _settings: &Settings,
            _services: &RuntimeServices,
            req: http::Request<EdgeBody>,
        ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
            let response = http::Response::builder()
                .status(http::StatusCode::OK)
                .header("x-echo-path", req.uri().path())
                .body(EdgeBody::empty())
                .expect("should build echo response");
            Ok(response)
        }
    }

    #[test]
    fn document_state_keeps_multiple_types_for_one_integration() {
        let state = IntegrationDocumentState::default();
        let number = state.get_or_insert_with("test", || 7_u32);
        let label = state.get_or_insert_with("test", || "first".to_string());
        let repeated_number = state.get_or_insert_with("test", || 99_u32);

        assert!(
            Arc::ptr_eq(&number, &repeated_number),
            "repeated insertion should preserve the original typed state"
        );
        assert_eq!(
            *state.get::<u32>("test").expect("should retrieve number"),
            7,
            "should retain numeric state"
        );
        assert_eq!(
            state
                .get::<String>("test")
                .expect("should retrieve label")
                .as_str(),
            "first",
            "should retain string state under the same integration ID"
        );
        assert_eq!(
            label.as_str(),
            "first",
            "should return inserted string state"
        );
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
    fn handle_proxy_passes_http_request_without_fastly_round_trip() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::from_routes(vec![(
            http::Method::GET,
            "/integrations/test/echo",
            (Arc::new(EchoProxy) as Arc<dyn IntegrationProxy>, "echo"),
        )]);
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://test.example.com/integrations/test/echo?x=1")
            .body(EdgeBody::empty())
            .expect("should build request");

        let mut ec_context =
            EcContext::new_for_test(None, crate::consent::ConsentContext::default());
        let response = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &http::Method::GET,
            path: "/integrations/test/echo",
            settings: &settings,
            kv: None,
            ec_context: &mut ec_context,
            services: &noop_services(),
            req,
        }))
        .expect("should match route")
        .expect("proxy should succeed");

        assert_eq!(
            response.status(),
            http::StatusCode::OK,
            "should preserve HTTP status"
        );
        assert_eq!(
            response.headers()["x-echo-path"],
            "/integrations/test/echo",
            "should expose the HTTP request path to the proxy"
        );
    }

    #[test]
    fn production_registry_always_reserves_the_aps_family() {
        let settings = create_test_settings();
        let registry =
            IntegrationRegistry::new(&settings).expect("production registry should build");
        assert!(registry.has_reserved_path("/integrations/aps"));
        assert!(registry.has_reserved_path("/integrations/aps/runner.js"));
        assert!(registry.has_reserved_path("/integrations/aps/malformed/path"));
        assert!(!registry.has_reserved_path("/integrations/apsx/runner.js"));
        assert!(!registry.has_reserved_path("/integrations/aps-legacy"));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/integrations/aps/renderer/v2")
            .header(HEADER_X_TS_EC.clone(), "caller-controlled")
            .body(EdgeBody::empty())
            .expect("should build reserved APS request");
        let response = futures::executor::block_on(registry.handle_reserved_proxy(
            &settings,
            &noop_services(),
            request,
        ))
        .expect("reserved family should be handled")
        .expect("disabled APS response should be local");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn filter_request_applies_request_headers_and_returns_response_headers() {
        let registry =
            IntegrationRegistry::from_request_filters(vec![Arc::new(EnrichingRequestFilter)]);
        let settings = crate::test_support::tests::create_test_settings();
        let services = crate::platform::test_support::noop_services();
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/page")
            .body(EdgeBody::empty())
            .expect("should build request");

        let outcome =
            futures::executor::block_on(registry.filter_request(RequestFilterRegistryInput {
                settings: &settings,
                services: &services,
                req: &mut req,
                geo_info: None,
            }))
            .expect("should run request filter");

        assert_eq!(
            req.headers()
                .get("x-datadome-isbot")
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "should apply DataDome-style request enrichment before routing"
        );
        assert!(
            req.extensions().get::<RequestAnnotation>().is_some(),
            "should preserve private request annotations for downstream routing"
        );
        match outcome {
            RequestFilterRegistryOutcome::Continue(effects) => {
                assert_eq!(
                    effects.response_headers,
                    vec![HeaderMutation::set("x-dd-b", "allowed")],
                    "should return downstream response header effects for finalization"
                );
            }
            RequestFilterRegistryOutcome::Respond { .. } => panic!("should continue routing"),
        }
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

    // Tests for EC ID header on proxy responses
    use crate::test_support::tests::create_test_settings;

    /// Mock proxy that returns a simple 200 OK response
    struct EcTestProxy;

    #[async_trait(?Send)]
    impl IntegrationProxy for EcTestProxy {
        fn integration_name(&self) -> &'static str {
            "ec_test"
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            vec![
                IntegrationEndpoint {
                    method: Method::GET,
                    path: "/integrations/test/ec".to_owned(),
                },
                IntegrationEndpoint {
                    method: Method::POST,
                    path: "/integrations/test/ec".to_owned(),
                },
            ]
        }

        async fn handle(
            &self,
            _settings: &Settings,
            _services: &RuntimeServices,
            req: Request<EdgeBody>,
        ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from("test response"))
                .expect("should build test response");
            if let Some(ec) = req.headers().get(HEADER_X_TS_EC.clone()) {
                response
                    .headers_mut()
                    .insert(http::HeaderName::from_static("x-echo-ts-ec"), ec.clone());
            }
            Ok(response)
        }
    }

    #[test]
    fn handle_proxy_removes_ec_id_header_on_request() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::GET,
            "/integrations/test/ec",
            (
                Arc::new(EcTestProxy) as Arc<dyn IntegrationProxy>,
                "ec_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://test-publisher.com/integrations/test/ec")
            .body(EdgeBody::empty())
            .expect("should build request");
        req.headers_mut().insert(
            HEADER_X_TS_EC.clone(),
            HeaderValue::from_static("some-ec-value"),
        );
        let mut ec_context =
            EcContext::new_for_test(None, crate::consent::ConsentContext::default());
        let services = noop_services();

        // Call handle_proxy (uses futures executor in test environment)
        let result = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &Method::GET,
            path: "/integrations/test/ec",
            settings: &settings,
            kv: None,
            ec_context: &mut ec_context,
            services: &services,
            req,
        }));

        // Should have matched and returned a response
        assert!(result.is_some(), "should find route and handle request");
        let response = result.unwrap();
        assert!(response.is_ok(), "handler should succeed");

        let response = response.unwrap();

        assert!(
            response.headers().get("x-echo-ts-ec").is_none(),
            "should not have x-ts-ec header on integration request"
        );
    }

    #[test]
    fn handle_proxy_rejects_invalid_ec_request_header() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::GET,
            "/integrations/test/ec",
            (
                Arc::new(EcTestProxy) as Arc<dyn IntegrationProxy>,
                "ec_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://test-publisher.com/integrations/test/ec")
            .body(EdgeBody::empty())
            .expect("should build request");
        req.headers_mut().insert(
            HEADER_X_TS_EC.clone(),
            HeaderValue::from_static("evil;injected"),
        );
        let mut ec_context =
            EcContext::new_for_test(None, crate::consent::ConsentContext::default());

        let services = crate::platform::test_support::noop_services();

        let result = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &Method::GET,
            path: "/integrations/test/ec",
            settings: &settings,
            kv: None,
            ec_context: &mut ec_context,
            services: &services,
            req,
        }))
        .expect("should handle proxy request");

        let response = result.expect("handler should succeed");

        assert!(
            response.headers().get("x-echo-ts-ec").is_none(),
            "should not reflect the tampered request header to the integration"
        );
    }

    #[test]
    fn handle_proxy_removes_request_ec_header_even_when_consent_denied() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::GET,
            "/integrations/test/ec",
            (Arc::new(EcTestProxy) as Arc<dyn IntegrationProxy>, "test"),
        )];

        let registry = IntegrationRegistry::from_routes(routes);

        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://test.example.com/integrations/test/ec")
            .body(EdgeBody::empty())
            .expect("should build request");
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "{}={}",
                COOKIE_TS_EC,
                crate::test_support::tests::VALID_SYNTHETIC_ID
            ))
            .expect("should build Cookie header"),
        );
        let mut ec_context =
            EcContext::new_for_test(None, crate::consent::ConsentContext::default());

        let services = crate::platform::test_support::noop_services();

        let result = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &Method::GET,
            path: "/integrations/test/ec",
            settings: &settings,
            kv: None,
            ec_context: &mut ec_context,
            services: &services,
            req,
        }))
        .expect("should handle proxy request");

        let response = result.expect("proxy handle should succeed");

        assert!(
            response.headers().get("x-echo-ts-ec").is_none(),
            "should not set x-ts-ec on integration request"
        );
    }

    #[test]
    fn handle_proxy_works_with_post_method() {
        let settings = create_test_settings();
        let routes = vec![(
            Method::POST,
            "/integrations/test/ec",
            (
                Arc::new(EcTestProxy) as Arc<dyn IntegrationProxy>,
                "ec_test",
            ),
        )];
        let registry = IntegrationRegistry::from_routes(routes);

        let mut req = Request::builder()
            .method(Method::POST)
            .uri("https://test-publisher.com/integrations/test/ec")
            .body(EdgeBody::from("test body"))
            .expect("should build POST request");
        req.headers_mut().insert(
            HEADER_X_TS_EC.clone(),
            HeaderValue::from_static("some-ec-value"),
        );
        let mut ec_context =
            EcContext::new_for_test(None, crate::consent::ConsentContext::default());

        let services = crate::platform::test_support::noop_services();

        let result = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
            method: &Method::POST,
            path: "/integrations/test/ec",
            settings: &settings,
            kv: None,
            ec_context: &mut ec_context,
            services: &services,
            req,
        }));

        assert!(result.is_some(), "Should find POST route");
        let response = result.unwrap();
        assert!(response.is_ok(), "Handler should succeed");

        let response = response.unwrap();
        assert!(
            response.headers().get("x-echo-ts-ec").is_none(),
            "POST integration request should not include x-ts-ec"
        );
    }

    #[test]
    fn js_module_ids_defer_prebid_and_include_core_js_only_modules() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut settings_with_prebid = settings;
        settings_with_prebid
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "bidders": ["mocktioneer"],
                    "debug": false
                }),
            )
            .expect("should insert prebid config");

        let registry =
            IntegrationRegistry::new(&settings_with_prebid).expect("should create registry");

        let all = registry.js_module_ids();
        let immediate = registry.js_module_ids_immediate();
        let deferred = registry.js_module_ids_deferred();

        assert!(
            all.contains(&"prebid"),
            "should include the prebid shim in embedded TSJS module IDs"
        );
        assert!(
            immediate.contains(&"creative"),
            "should include creative in immediate IDs"
        );
        assert!(
            !immediate.contains(&"sourcepoint"),
            "should not include Sourcepoint unless explicitly enabled"
        );
        assert!(
            !immediate.contains(&"prebid"),
            "should not include prebid in immediate IDs"
        );
        assert!(
            deferred.contains(&"prebid"),
            "should serve the prebid shim as a deferred module"
        );
    }

    #[test]
    fn tsjs_catalog_selector_uses_generated_predicates_and_request_bits() {
        let registry = IntegrationRegistry::empty_for_tests();

        assert_eq!(
            registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1::default()),
            vec!["render_runtime"],
            "always must select only the mandatory runtime without other inputs"
        );
        assert_eq!(
            registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1 {
                creative_enabled: true,
                creative_click_guard: true,
                ..TsjsCatalogSelectionV1::default()
            }),
            vec!["render_runtime", "creative"],
            "creative requires enabled plus one guard"
        );
        assert_eq!(
            registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1 {
                creative_enabled: true,
                ..TsjsCatalogSelectionV1::default()
            }),
            vec!["render_runtime"],
            "creative enabled without a guard must remain absent"
        );
        assert_eq!(
            registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1 {
                gpt_diagnostics_active: true,
                ..TsjsCatalogSelectionV1::default()
            }),
            vec!["render_runtime", "diagnostics_presentation"],
            "diagnostics capture is omitted when its mandatory GPT provider is absent"
        );
        assert_eq!(
            registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1 {
                render_trace_overlay: true,
                ..TsjsCatalogSelectionV1::default()
            }),
            vec!["render_runtime", "diagnostics_presentation"],
            "overlay alone selects only presentation"
        );
    }

    #[test]
    fn tsjs_static_transport_precomputes_every_admitted_takeover_artifact() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should enable diagnostics");
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        for render_trace_overlay in [false, true] {
            for selection in registry.tsjs_static_transport_selections(render_trace_overlay) {
                let module_ids = registry.tsjs_takeover_module_ids(selection);
                let expected_body = trusted_server_js::concatenate_modules(&module_ids);
                let expected_hash = trusted_server_js::concatenated_hash(&module_ids);
                let artifact = registry
                    .tsjs_takeover_artifact(selection)
                    .expect("should precompute every admitted selection");

                assert_eq!(artifact.hash(), expected_hash, "should preserve exact hash");
                assert_eq!(
                    artifact.body().as_ref(),
                    expected_body.as_bytes(),
                    "should preserve exact response bytes"
                );
                assert_eq!(
                    artifact.src(),
                    format!("/static/tsjs=tsjs-unified.min.js?v={expected_hash}"),
                    "should precompute the exact takeover URL"
                );
                assert_eq!(
                    registry
                        .tsjs_static_artifact(&expected_hash)
                        .expect("should index artifact by hash")
                        .body()
                        .as_ptr(),
                    artifact.body().as_ptr(),
                    "selection and transport lookups should share precomputed bytes"
                );
            }
        }
    }

    #[test]
    fn tsjs_static_transport_precomputes_every_size_admitted_first_display_mask() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &serde_json::json!({}))
            .expect("should enable GPT");
        settings
            .integrations
            .insert_config(
                "aps",
                &serde_json::json!({ "enabled": true, "account_id": "test-account" }),
            )
            .expect("should enable APS");
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let masks = registry.tsjs_first_display_masks();

        let catalog = trusted_server_js::all_first_display_ids();
        let bit = |id: &str| {
            1_u16
                << catalog
                    .iter()
                    .position(|candidate| *candidate == id)
                    .expect("catalog should contain slice")
        };
        let fixed = bit("first_display") | bit("creative_initial");
        let gpt = bit("gpt_initial");
        let aps = bit("aps_initial");
        let prebid = bit("prebid_initial");
        assert_eq!(
            masks,
            vec![fixed, fixed | gpt, fixed | gpt | aps, fixed | gpt | prebid,],
            "registry should enumerate every configuration-reachable mask admitted by the fixed transfer ceiling"
        );
        for mask in masks {
            let selected = trusted_server_js::all_first_display_ids()
                .into_iter()
                .enumerate()
                .filter_map(|(index, id)| (mask & (1 << index) != 0).then_some(id))
                .collect::<Vec<_>>();
            let body = trusted_server_js::concatenate_first_display_slices(&selected[1..])
                .expect("mask should select a closed composition");
            let hash = trusted_server_js::concatenated_first_display_hash(&selected[1..])
                .expect("mask should have a stable hash");
            let artifact = registry
                .tsjs_first_display_artifact(mask, &hash)
                .expect("should resolve the precomputed mask/hash pair");

            assert_eq!(artifact.body().as_ref(), body.as_bytes());
            assert_eq!(
                artifact.src(),
                format!("/static/tsjs=tsjs-first-display.min.js?m={mask:04x}&v={hash}")
            );
            assert!(
                registry
                    .tsjs_first_display_artifact(mask ^ (1 << 1), &hash)
                    .is_none(),
                "hash-to-mask lookup must reject a mismatched selection"
            );
        }
    }

    #[test]
    fn tsjs_static_transport_includes_creative_and_rejects_unknown_hash() {
        let registry = IntegrationRegistry::empty_for_tests();
        let creative_ids = crate::tsjs::creative_tsjs_module_ids();
        let creative_hash = trusted_server_js::concatenated_hash(creative_ids);

        assert_eq!(
            registry
                .tsjs_static_artifact(&creative_hash)
                .expect("should precompute rewritten creative artifact")
                .body()
                .as_ref(),
            trusted_server_js::concatenate_modules(creative_ids).as_bytes(),
            "should admit the exact rewritten creative bundle"
        );
        assert!(
            registry.tsjs_static_artifact(&"0".repeat(64)).is_none(),
            "unknown hashes should be a lookup miss"
        );
    }

    #[test]
    fn js_module_ids_skip_enabled_integrations_without_generated_js_module() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config("nextjs", &serde_json::json!({ "enabled": true }))
            .expect("should insert nextjs config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let all = registry.js_module_ids();

        assert!(
            !all.contains(&"nextjs"),
            "should not include enabled integrations without generated JS modules"
        );

        let metadata = registry.registered_integrations();
        assert!(
            metadata
                .iter()
                .any(|integration| integration.id == "nextjs"),
            "should still register enabled Rust-only integrations"
        );
    }

    #[test]
    fn catalog_ids_split_explicitly_enabled_cmp_owners_by_phase() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config("sourcepoint", &serde_json::json!({ "enabled": true }))
            .expect("should insert sourcepoint config");
        settings
            .integrations
            .insert_config("osano", &serde_json::json!({ "enabled": true }))
            .expect("should insert osano config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let selection = TsjsCatalogSelectionV1::default();
        let takeover = registry.tsjs_takeover_module_ids(selection);
        let deferred = registry.tsjs_deferred_module_ids(selection);

        assert!(
            takeover.contains(&"sourcepoint_consent"),
            "should include the Sourcepoint consent owner when explicitly enabled"
        );
        assert!(
            takeover.contains(&"osano_consent"),
            "should include the Osano consent owner when explicitly enabled"
        );
        assert!(
            deferred.contains(&"sourcepoint_lifecycle"),
            "should defer the Sourcepoint lifecycle owner"
        );
        assert!(
            deferred.contains(&"osano_lifecycle"),
            "should defer the Osano lifecycle owner"
        );

        let metadata = registry.registered_integrations();
        assert!(
            metadata.iter().any(|integration| integration.id == "osano"),
            "should include JS-only Osano registration in metadata"
        );
    }

    #[test]
    fn js_module_ids_deferred_empty_when_prebid_disabled() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": false,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        let deferred = registry.js_module_ids_deferred();
        assert!(
            deferred.is_empty(),
            "should have no deferred IDs when prebid is disabled"
        );
    }

    #[test]
    fn js_module_ids_defer_prebid_shim_when_external_bundle_is_configured() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js"
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        assert!(
            registry.js_module_ids().contains(&"prebid"),
            "external bundle mode should include the prebid shim in embedded TSJS modules"
        );
        assert!(
            !registry.js_module_ids_immediate().contains(&"prebid"),
            "the prebid shim should not load in the immediate TSJS bundle"
        );
        assert!(
            registry.js_module_ids_deferred().contains(&"prebid"),
            "the prebid shim should load as a deferred TSJS module"
        );
        assert!(
            registry.has_route(&Method::GET, "/integrations/prebid/bundle.js"),
            "external bundle mode should register the first-party bundle route"
        );
    }

    #[test]
    fn js_module_ids_split_is_exhaustive() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut settings_with_prebid = settings;
        settings_with_prebid
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "bidders": ["mocktioneer"],
                    "debug": false
                }),
            )
            .expect("should insert prebid config");

        let registry =
            IntegrationRegistry::new(&settings_with_prebid).expect("should create registry");

        let all = registry.js_module_ids();
        let mut recombined = registry.js_module_ids_immediate();
        recombined.extend(registry.js_module_ids_deferred());
        recombined.sort_unstable();

        let mut all_sorted = all;
        all_sorted.sort_unstable();

        assert_eq!(
            recombined, all_sorted,
            "should reconstruct full module list from immediate + deferred"
        );
    }

    #[test]
    fn registry_projects_every_enabled_product_once_without_private_server_fields() {
        let mut settings = crate::test_support::tests::create_test_settings();
        for (id, config) in [
            (
                "aps",
                serde_json::json!({
                    "enabled": true,
                    "account_id": "private-aps-account",
                    "endpoint": "https://private-aps.example/bid"
                }),
            ),
            (
                "datadome",
                serde_json::json!({
                    "enabled": true,
                    "server_side_key_secret_store": "private-datadome-store",
                    "server_side_key_secret_name": "private-datadome-key"
                }),
            ),
            ("didomi", serde_json::json!({ "enabled": true })),
            (
                "google_tag_manager",
                serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-TEST"
                }),
            ),
            (
                "gpt",
                serde_json::json!({
                    "enabled": true,
                    "gam_attribution_enabled": true,
                    "script_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                    "slim_prebid_url": "https://private-prebid.example/slim.js"
                }),
            ),
            (
                "lockr",
                serde_json::json!({ "enabled": true, "app_id": "private-lockr-app" }),
            ),
            ("osano", serde_json::json!({ "enabled": true })),
            (
                "permutive",
                serde_json::json!({
                    "enabled": true,
                    "organization_id": "private-org",
                    "workspace_id": "private-workspace"
                }),
            ),
            (
                "prebid",
                serde_json::json!({
                    "enabled": true,
                    "server_url": "https://private-pbs.example/openrtb2/auction",
                    "account_id": "browser-account",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "bidders": ["mocktioneer"]
                }),
            ),
            (
                "sourcepoint",
                serde_json::json!({
                    "enabled": true,
                    "auth_cookie_name": "private_sourcepoint_cookie"
                }),
            ),
            (
                "testlight",
                serde_json::json!({
                    "enabled": true,
                    "endpoint": "https://private-testlight.example/bid"
                }),
            ),
        ] {
            settings
                .integrations
                .insert_config(id, &config)
                .expect("test integration config should insert");
        }

        let registry = IntegrationRegistry::new(&settings).expect("registry should build");
        let module_ids = registry.tsjs_catalog_module_ids(TsjsCatalogSelectionV1::default());
        let carrier = registry
            .tsjs_integration_configs_v1(&module_ids)
            .expect("selected product configs should match the generated catalog");
        let value = serde_json::to_value(carrier).expect("carrier should serialize");
        let ids = value["entries"]
            .as_array()
            .expect("carrier entries should be an array")
            .iter()
            .map(|entry| entry["id"].as_str().expect("entry id should be a string"))
            .collect::<Vec<_>>();

        assert_eq!(ids, crate::tsjs::INTEGRATION_CONFIG_IDS_V1);
        let serialized = value.to_string();
        for private_value in [
            "private-aps-account",
            "private-aps.example",
            "private-datadome-store",
            "private-datadome-key",
            "private-prebid.example",
            "private-lockr-app",
            "private-org",
            "private-workspace",
            "private-pbs.example",
            "private_sourcepoint_cookie",
            "private-testlight.example",
        ] {
            assert!(
                !serialized.contains(private_value),
                "private server field leaked into browser carrier: {private_value}"
            );
        }
        assert_eq!(
            value["entries"][0]["config"],
            serde_json::json!({}),
            "enabled APS must emit its required empty browser config"
        );
    }
}
