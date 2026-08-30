use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Method, Request, Response};
use matchit::Router;
use sha2::{Digest as _, Sha256};

use crate::constants::HEADER_X_TS_EC;
use crate::ec::EcContext;
use crate::ec::device::DeviceProvider;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::provider::{EcProviderSelection, EdgeCookieProvider};
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::is_navigation_request;
use crate::platform::{DisabledGeo, PlatformGeo, RuntimeServices};
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
    /// The permission state resolved for this request at the start of the
    /// request cycle, so a filter reads the same permissions the rest of the
    /// request uses rather than resolving its own. `None` only on paths that
    /// build no EC context, such as batch sync and admin diagnostics.
    pub permissions: Option<&'a crate::permissions::PermissionState>,
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
    /// The permission state resolved for this request at the start of the
    /// request cycle, passed on to every filter. `None` only on paths that
    /// build no EC context, such as batch sync and admin diagnostics.
    pub permissions: Option<&'a crate::permissions::PermissionState>,
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

/// A browser module a registration carries, for a module built outside
/// `trusted-server-js`. The crate embeds its built IIFE with `include_str!`
/// and states its SHA-256 as a literal next to it; the registry verifies the
/// two agree when it is built, and the served `?v=` hash is derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarriedJsModule {
    /// The built IIFE.
    pub source: &'static str,
    /// SHA-256 of `source`, hex encoded, lower case.
    pub sha256: &'static str,
}

/// Registration payload returned by integration builders.
pub struct IntegrationRegistration {
    pub integration_id: &'static str,
    pub js_deferred: bool,
    pub js_disabled: bool,
    /// Browser module carried by the registration, when the module is not
    /// compiled into `trusted-server-js`.
    pub js_module: Option<CarriedJsModule>,
    /// Serve the module only on its own `/static/tsjs=tsjs-<id>.min.js` path,
    /// never in the unified bundle and never as a deferred tag; the
    /// integration injects the tag itself when it decides to.
    pub js_standalone: bool,
    pub proxies: Vec<Arc<dyn IntegrationProxy>>,
    pub attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    pub script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    pub html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
    pub head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    pub request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    /// Geo provider this module supplies, selectable by `[geo] provider`.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to resolve location when `[geo] provider` names this module's id.
    pub geo_provider: Option<Arc<dyn PlatformGeo>>,
    /// Edge Cookie provider this module supplies, selectable by `[ec] provider`.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to mint an identifier when `[ec] provider` names this module's id. This
    /// is the same route geo takes, so identity is not a second extension
    /// mechanism sitting beside the integration system.
    pub ec_provider: Option<Arc<dyn EdgeCookieProvider>>,
    /// Device provider this module supplies, selectable by `[device] provider`.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to classify a request when `[device] provider` names this module's id.
    pub device_provider: Option<Arc<dyn DeviceProvider>>,
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
                js_module: None,
                js_standalone: false,
                proxies: Vec::new(),
                attribute_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_post_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                geo_provider: None,
                ec_provider: None,
                device_provider: None,
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

    /// Declare the geo provider this module supplies.
    ///
    /// The provider only resolves location when `[geo] provider` names this
    /// module's id, and a declared provider the selector does not choose is
    /// reported as a startup warning.
    #[must_use]
    pub fn with_geo_provider(mut self, provider: Arc<dyn PlatformGeo>) -> Self {
        self.registration.geo_provider = Some(provider);
        self
    }

    /// Declare the Edge Cookie provider this module supplies.
    ///
    /// The provider only mints identifiers when `[ec] provider` names this
    /// module's id, and a declared provider the selector does not choose is
    /// reported as a startup warning, the same as geo.
    #[must_use]
    pub fn with_ec_provider(mut self, provider: Arc<dyn EdgeCookieProvider>) -> Self {
        self.registration.ec_provider = Some(provider);
        self
    }

    /// Declare the device provider this module supplies.
    ///
    /// The provider only classifies requests when `[device] provider` names
    /// this module's id, and a declared provider the selector does not choose
    /// is reported as a startup warning, the same as geo.
    #[must_use]
    pub fn with_device_provider(mut self, provider: Arc<dyn DeviceProvider>) -> Self {
        self.registration.device_provider = Some(provider);
        self
    }

    /// Mark this integration's JS module for deferred loading via
    /// `<script defer>` instead of the main synchronous bundle.
    #[must_use]
    pub fn with_deferred_js(mut self) -> Self {
        self.registration.js_deferred = true;
        self.registration.js_disabled = false;
        self.registration.js_standalone = false;
        self
    }

    /// Disable TSJS module inclusion for an integration that is handled by other assets.
    #[must_use]
    pub fn without_js(mut self) -> Self {
        self.registration.js_disabled = true;
        self.registration.js_deferred = false;
        self.registration.js_standalone = false;
        self
    }

    /// Carry a browser module built outside `trusted-server-js`.
    #[must_use]
    pub fn with_js_module(mut self, module: CarriedJsModule) -> Self {
        self.registration.js_module = Some(module);
        self
    }

    /// Serve the module standalone only; see
    /// [`IntegrationRegistration::js_standalone`].
    ///
    /// The three delivery flags are exclusive and the last builder call wins,
    /// so this clears the disabled and deferred flags as those methods clear
    /// this one.
    #[must_use]
    pub fn with_standalone_js(mut self) -> Self {
        self.registration.js_standalone = true;
        self.registration.js_disabled = false;
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

    // Metadata for introspection
    routes: Vec<(IntegrationEndpoint, &'static str)>,
    // Every builder considered at construction, enabled or not, in order.
    builder_ids: Vec<(&'static str, &'static str)>,
    enabled_integration_ids: Vec<&'static str>,
    deferred_js_ids: Vec<&'static str>,
    disabled_js_ids: Vec<&'static str>,
    // Enabled modules served only on their own path, never in the bundle.
    standalone_js_ids: Vec<&'static str>,
    // Modules carried by their registrations, verified against their
    // declared hash at construction.
    carried_js: Vec<(&'static str, CarriedJsModule)>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    html_post_processors: Vec<Arc<dyn IntegrationHtmlPostProcessor>>,
    head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    /// JS module IDs to include in the bundle that come from a source other than
    /// a registered integration, for example a module tied to the selected Edge
    /// Cookie provider. Populated in [`IntegrationRegistry::new`] from settings.
    extra_js_module_ids: Vec<&'static str>,
    // Preparers from every builder, enabled or not, in registration order.
    request_preparers: Vec<crate::integrations::IntegrationPrepareRequestFn>,
    // Geo providers declared by the enabled registrations, in registration
    // order. Declaring one does not activate it.
    geo_providers: Vec<(&'static str, Arc<dyn PlatformGeo>)>,
    // Edge Cookie providers each declaring module supplies, in registration
    // order. `[ec] provider` picks at most one of them.
    ec_providers: Vec<(&'static str, Arc<dyn EdgeCookieProvider>)>,
    // Device providers each declaring module supplies, in registration order.
    // `[device] provider` picks at most one of them.
    device_providers: Vec<(&'static str, Arc<dyn DeviceProvider>)>,
    // The provider `[geo] provider` resolved to, or `None` when the selector
    // is unset and the adapter's own host lookup stands.
    geo_provider: Option<Arc<dyn PlatformGeo>>,
    ec_provider: Option<Arc<dyn EdgeCookieProvider>>,
    device_provider: Option<Arc<dyn DeviceProvider>>,
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
            routes: Vec::new(),
            builder_ids: Vec::new(),
            enabled_integration_ids: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            standalone_js_ids: Vec::new(),
            carried_js: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_post_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters: Vec::new(),
            extra_js_module_ids: Vec::new(),
            request_preparers: Vec::new(),
            geo_providers: Vec::new(),
            ec_providers: Vec::new(),
            device_providers: Vec::new(),
            geo_provider: None,
            ec_provider: None,
            device_provider: None,
        }
    }
}

/// Reserved value of `[geo] provider` that resolves no location at all.
const GEO_PROVIDER_NONE: &str = "none";

/// `[geo] provider` value opting in to the adapter's own host geo lookup.
const GEO_PROVIDER_PLATFORM: &str = "platform";

/// `[device] provider` value naming the User-Agent-only provider core supplies.
///
/// The same choice as leaving the selector unset, spelled explicitly.
const DEVICE_PROVIDER_BUILTIN: &str = "builtin";

/// `[device] provider` value opting in to the host provider the adapter builds.
///
/// Resolved by `build_device_provider` in [`device`](crate::ec::device) rather
/// than by a module, so the registry supplies nothing for it.
const DEVICE_PROVIDER_FASTLY: &str = "fastly";

/// Resolves `[geo] provider` against the modules that declared a geo provider.
///
/// Returns `None` when the selector is unset, which leaves the adapter's own
/// host lookup in place. A module that declares a geo provider the selector
/// does not choose is reported as a warning, so an operator can see a module
/// shipping a capability the deployment never uses.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the selector names a
/// module that is not registered, is registered but not enabled, or is enabled
/// and declares no geo provider.
/// Resolves the Edge Cookie provider `[ec] provider` names, when a module
/// supplies it.
///
/// Unlike geo, identity has providers built into core, so a selector naming one
/// of those is not an error here. This returns `Some` only when a registered
/// module declared a provider under that name, and core resolves the rest.
fn resolve_ec_provider(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Option<Arc<dyn EdgeCookieProvider>> {
    // `None` spells statelessness and never names a module, so only a named
    // selection can match one.
    let selector = match settings.ec.provider.as_ref() {
        Some(EcProviderSelection::Named(key)) => Some(key.as_str()),
        Some(EcProviderSelection::None) | None => None,
    };
    let resolved = selector.and_then(|key| {
        inner
            .ec_providers
            .iter()
            .find(|(id, _)| *id == key)
            .map(|(_, provider)| Arc::clone(provider))
    });

    for (id, _) in &inner.ec_providers {
        if selector != Some(*id) {
            log::warn!(
                "integration module `{id}` declares an Edge Cookie provider that `[ec] provider` does not select"
            );
        }
    }

    resolved
}

/// Resolves the device provider `[device] provider` names, when a module
/// supplies it.
///
/// As with identity, core has a built-in device provider, so a selector naming
/// it is not an error here.
fn resolve_device_provider(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<Option<Arc<dyn DeviceProvider>>, Report<TrustedServerError>> {
    let selector = settings.device.provider.as_deref();
    let resolved = match selector {
        // Unset and `builtin` both name the provider core supplies itself, and
        // `fastly` names the host provider the adapter builds, so the registry
        // supplies nothing for any of the three.
        None | Some(DEVICE_PROVIDER_BUILTIN) | Some(DEVICE_PROVIDER_FASTLY) => None,
        Some(module_id) => Some(module_device_provider(module_id, inner)?),
    };

    for (id, _) in &inner.device_providers {
        if selector != Some(*id) {
            log::warn!(
                "integration module `{id}` declares a device provider that `[device] provider` does not select"
            );
        }
    }

    Ok(resolved)
}

/// Looks up the device provider declared by the module `module_id`.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming the module and the
/// capability when the module declares no device provider, is registered but
/// not enabled, or is not registered at all. Without this a mistyped selector
/// would fall back to core's built-in provider with nothing said, which is the
/// silent wrong answer the geo selector already refuses to give.
fn module_device_provider(
    module_id: &str,
    inner: &IntegrationRegistryInner,
) -> Result<Arc<dyn DeviceProvider>, Report<TrustedServerError>> {
    if let Some((_, provider)) = inner
        .device_providers
        .iter()
        .find(|(id, _)| *id == module_id)
    {
        return Ok(Arc::clone(provider));
    }

    let message = if inner
        .enabled_integration_ids
        .iter()
        .copied()
        .any(|id| id == module_id)
    {
        format!(
            "`[device] provider` selects integration module `{module_id}`, which declares no device provider"
        )
    } else if inner.builder_ids.iter().any(|(id, _)| *id == module_id) {
        format!(
            "`[device] provider` selects integration module `{module_id}`, which is registered but not enabled, so its device provider is unavailable"
        )
    } else {
        format!(
            "`[device] provider` selects integration module `{module_id}`, which is not registered; the registered modules that declare a device provider are [{}]",
            inner
                .device_providers
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    Err(Report::new(TrustedServerError::Configuration { message }))
}

fn resolve_geo_provider(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<Option<Arc<dyn PlatformGeo>>, Report<TrustedServerError>> {
    let selector = settings.geo.provider.as_deref();
    let resolved = match selector {
        // Unset resolves nothing and makes no host geo call, so a default
        // deployment is not tied to any host geo service. `none` spells the
        // same choice explicitly.
        None | Some(GEO_PROVIDER_NONE) => Some(Arc::new(DisabledGeo) as Arc<dyn PlatformGeo>),
        // `platform` opts in to the adapter's own host lookup, so the registry
        // supplies nothing and the adapter's provider stands.
        Some(GEO_PROVIDER_PLATFORM) => None,
        Some(module_id) => Some(module_geo_provider(module_id, inner)?),
    };

    for (id, _) in &inner.geo_providers {
        if selector != Some(*id) {
            log::warn!(
                "integration module `{id}` declares a geo provider that `[geo] provider` does not select"
            );
        }
    }

    Ok(resolved)
}

/// Looks up the geo provider declared by the module `module_id`.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming the module and the
/// capability when the module declares no geo provider, is registered but not
/// enabled, or is not registered at all.
fn module_geo_provider(
    module_id: &str,
    inner: &IntegrationRegistryInner,
) -> Result<Arc<dyn PlatformGeo>, Report<TrustedServerError>> {
    if let Some((_, provider)) = inner.geo_providers.iter().find(|(id, _)| *id == module_id) {
        return Ok(Arc::clone(provider));
    }

    // Only an enabled registration reaches the collection loop, so a module
    // that exists but is switched off must say so rather than read as a module
    // that never declared the capability.
    let message = if inner
        .enabled_integration_ids
        .iter()
        .copied()
        .any(|id| id == module_id)
    {
        format!(
            "`[geo] provider` selects integration module `{module_id}`, which declares no geo provider"
        )
    } else if inner.builder_ids.iter().any(|(id, _)| *id == module_id) {
        format!(
            "`[geo] provider` selects integration module `{module_id}`, which is registered but not enabled, so its geo provider is unavailable"
        )
    } else {
        format!(
            "`[geo] provider` selects integration module `{module_id}`, which is not registered; the registered modules that declare a geo provider are [{}]",
            inner
                .geo_providers
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    Err(Report::new(TrustedServerError::Configuration { message }))
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
#[derive(Clone, Default)]
pub struct IntegrationRegistry {
    inner: Arc<IntegrationRegistryInner>,
}

impl IntegrationRegistry {
    /// Build a registry from the built-in integrations.
    ///
    /// # Errors
    ///
    /// Returns an error if route registration fails due to duplicate routes or
    /// invalid paths, or when a registration carries a browser module whose
    /// declared SHA-256 does not match its source.
    ///
    /// # Panics
    ///
    /// Panics if a route path ends with `/*` but `strip_suffix` unexpectedly fails (invariant violation).
    pub fn new(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        Self::with_registrations(settings, &[])
    }

    /// Build a registry from the built-in integrations followed by `extra`,
    /// the builders an adapter or a vendor crate supplies.
    ///
    /// # Errors
    ///
    /// Returns an error when two builders claim the same integration id, when
    /// route registration fails due to duplicate routes or invalid paths, when
    /// a builder fails, or when a registration carries a browser module whose
    /// declared SHA-256 does not match its source.
    ///
    /// # Panics
    ///
    /// Panics if a route path ends with `/*` but `strip_suffix` unexpectedly fails (invariant violation).
    pub fn with_registrations(
        settings: &Settings,
        extra: &[crate::integrations::IntegrationBuilder],
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut inner = IntegrationRegistryInner::default();

        for builder in crate::integrations::all_builders(extra) {
            if let Some((_, first_source)) =
                inner.builder_ids.iter().find(|(id, _)| *id == builder.id())
            {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "integration id `{}` is registered twice, by `{first_source}` and by `{}`",
                        builder.id(),
                        builder.source()
                    ),
                }));
            }
            inner.builder_ids.push((builder.id(), builder.source()));
            // Preparers are collected before the enabled check, so an
            // integration can sanitize its own reserved query or cookie in a
            // deployment that has it switched off.
            if let Some(prepare) = builder.prepare_request() {
                inner.request_preparers.push(prepare);
            }

            if let Some(registration) = builder.build(settings)? {
                debug_assert_eq!(
                    registration.integration_id,
                    builder.id(),
                    "integration builder ID should match registration ID"
                );
                inner
                    .enabled_integration_ids
                    .push(registration.integration_id);

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
                if let Some(provider) = registration.geo_provider {
                    inner
                        .geo_providers
                        .push((registration.integration_id, provider));
                }
                if let Some(provider) = registration.ec_provider {
                    inner
                        .ec_providers
                        .push((registration.integration_id, provider));
                }
                if let Some(provider) = registration.device_provider {
                    inner
                        .device_providers
                        .push((registration.integration_id, provider));
                }
                if registration.js_disabled {
                    inner.disabled_js_ids.push(registration.integration_id);
                } else if registration.js_deferred {
                    inner.deferred_js_ids.push(registration.integration_id);
                }
                if registration.js_standalone {
                    inner.standalone_js_ids.push(registration.integration_id);
                }
                if let Some(module) = registration.js_module {
                    // The served `?v=` hash and its memo trust this value, so a
                    // stale literal is a startup error rather than a stale
                    // script. The cost is one SHA-256 of each carried module
                    // per registry build, and only the Axum dev server builds
                    // the registry once, because its `main` builds the router
                    // before serving. Fastly starts a fresh Wasm instance per
                    // request, and `edgezero_adapter_cloudflare::run_app` and
                    // `edgezero_adapter_spin::run_app` both call `build_app`
                    // inside the per-request entry point, so those three pay
                    // it on every request.
                    let actual = hex::encode(Sha256::digest(module.source.as_bytes()));
                    if actual != module.sha256 {
                        return Err(Report::new(TrustedServerError::Configuration {
                            message: format!(
                                "Integration `{}` carries a browser module whose declared SHA-256 does not match its source (declared {}, actual {actual})",
                                registration.integration_id, module.sha256
                            ),
                        }));
                    }
                    inner.carried_js.push((registration.integration_id, module));
                }
            }
        }

        // A client-cycle Edge Cookie provider ships a page script that posts its
        // result to the resolve endpoint. The script rides the tsjs bundle, so
        // include its module when that provider is selected. The same module
        // list drives both the served bundle and the injected `<script>` hash,
        // so they stay consistent.
        if settings.ec.provider.as_ref().is_some_and(|selection| {
            selection.key() == crate::ec::provider::CLIENT_FIXED_PROVIDER_KEY
        }) {
            inner.extra_js_module_ids.push("ec_client_fixed");
        }
        let geo_provider = resolve_geo_provider(settings, &inner)?;
        inner.ec_provider = resolve_ec_provider(settings, &inner);
        inner.device_provider = resolve_device_provider(settings, &inner)?;
        inner.geo_provider = geo_provider;

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// The geo provider `[geo] provider` selected, or `None` when the selector
    /// is unset and the adapter's own host lookup stands.
    #[must_use]
    pub fn geo_provider(&self) -> Option<Arc<dyn PlatformGeo>> {
        self.inner.geo_provider.clone()
    }

    /// The Edge Cookie provider `[ec] provider` selected from a module, or
    /// `None` when the selector names a provider built into core or nothing.
    #[must_use]
    pub fn ec_provider(&self) -> Option<Arc<dyn EdgeCookieProvider>> {
        self.inner.ec_provider.clone()
    }

    /// The device provider `[device] provider` selected from a module, or
    /// `None` when the selector names a provider built into core or nothing.
    #[must_use]
    pub fn device_provider(&self) -> Option<Arc<dyn DeviceProvider>> {
        self.inner.device_provider.clone()
    }

    /// Every integration id the registry was built from, enabled or not, in
    /// registration order. The deploy validation and registry tests enumerate
    /// this to check every builder was considered.
    #[must_use]
    pub fn registered_builder_ids(&self) -> Vec<&'static str> {
        self.inner.builder_ids.iter().map(|(id, _)| *id).collect()
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

    /// Runs every registered integration's request preparer, in registration
    /// order, before routing.
    ///
    /// Preparers run whether or not their integration is enabled, so an
    /// integration can sanitize its own reserved query or cookie in a
    /// deployment that has it switched off.
    ///
    /// # Errors
    ///
    /// Returns the first preparer's error.
    pub fn prepare_request(
        &self,
        settings: &Settings,
        request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        for prepare in &self.inner.request_preparers {
            prepare(settings, request)?;
        }
        Ok(())
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
            permissions,
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
                    permissions,
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
                    log::error!("EC generation failed for integration proxy: {err:?}");
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
        let mut attributes: Vec<(&'static str, &'static str)> = Vec::new();
        for injector in &self.inner.head_injectors {
            for attribute in injector.tsjs_script_tag_attributes() {
                let existing = attributes
                    .iter()
                    .find(|(name, _)| *name == attribute.0)
                    .copied();
                match existing {
                    None => attributes.push(attribute),
                    Some((_, kept_value)) if kept_value != attribute.1 => log::warn!(
                        "Integration `{}` emits conflicting value for publisher tag attribute `{}`; keeping the first",
                        injector.integration_id(),
                        attribute.0
                    ),
                    Some(_) => {}
                }
            }
        }
        attributes
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

    /// Return JS module IDs that should be included in the tsjs bundle.
    ///
    /// Always includes JS-only modules with no Rust-side registration.
    /// Includes enabled integrations only when a browser module serves their
    /// id, either compiled into `trusted-server-js` or carried by the
    /// registration, and excludes modules served standalone only.
    #[must_use]
    pub fn js_module_ids(&self) -> Vec<&'static str> {
        // Core JS-only modules that do not have a Rust-side registration.
        const JS_ALWAYS: &[&str] = &["creative"];

        let mut ids: Vec<&'static str> = JS_ALWAYS.to_vec();

        for id in &self.inner.enabled_integration_ids {
            if self.js_part(id).is_some()
                && !self.inner.standalone_js_ids.contains(id)
                && !ids.contains(id)
            {
                ids.push(*id);
            }
        }

        // Modules not tied to a registered integration, for example the
        // client-cycle provider's page script.
        for id in &self.inner.extra_js_module_ids {
            if !ids.contains(id) {
                ids.push(id);
            }
        }

        ids
    }

    /// The module part for one id: the registration that carries it, else
    /// the compile-time module, for an enabled integration or an always-on
    /// core module (`core`, `creative`). `None` when nothing serves that id
    /// or the integration registered without JS.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::integrations::IntegrationRegistry;
    ///
    /// let registry = IntegrationRegistry::default();
    ///
    /// assert!(registry.js_part("core").is_some());
    /// assert!(registry.js_part("lockr").is_none());
    /// ```
    #[must_use]
    pub fn js_part(&self, id: &'static str) -> Option<crate::tsjs_bundle::JsModulePart> {
        if self.inner.disabled_js_ids.contains(&id) {
            return None;
        }
        if let Some((carried_id, module)) = self
            .inner
            .carried_js
            .iter()
            .find(|(carried_id, _)| *carried_id == id)
        {
            return Some(crate::tsjs_bundle::JsModulePart {
                id: carried_id,
                source: module.source,
                sha256: module.sha256,
            });
        }
        if id != "core" && id != "creative" && !self.integration_enabled(id) {
            return None;
        }
        crate::tsjs_bundle::JsModulePart::compile_time(id)
    }

    /// Ids of enabled modules served standalone only. Only enabled
    /// registrations reach the construction loop, so every id here is enabled.
    #[must_use]
    pub fn js_standalone_ids(&self) -> Vec<&'static str> {
        self.inner.standalone_js_ids.clone()
    }

    /// Resolves a module id given as request text (the stem of a
    /// `tsjs-<id>.min.js` filename) to this registry's own `&'static str`
    /// id, covering bundle, deferred and standalone modules.
    ///
    /// `trusted_server_js::all_module_ids` knows only compile-time modules,
    /// so a carried module could never be looked up through it. Returns
    /// `None` for an id this registry serves nowhere, which includes a
    /// disabled or unknown integration.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::integrations::IntegrationRegistry;
    ///
    /// let registry = IntegrationRegistry::default();
    ///
    /// assert_eq!(registry.js_module_id("creative"), Some("creative"));
    /// assert_eq!(registry.js_module_id("not-a-module"), None);
    /// ```
    #[must_use]
    pub fn js_module_id(&self, stem: &str) -> Option<&'static str> {
        self.js_module_ids()
            .into_iter()
            .chain(self.js_standalone_ids())
            .find(|id| *id == stem)
    }

    /// Parts of the unified bundle: core, then every immediate module.
    #[must_use]
    pub fn js_parts_immediate(&self) -> Vec<crate::tsjs_bundle::JsModulePart> {
        self.parts_for(&self.js_module_ids_immediate())
    }

    /// Parts served with `<script defer>`, one file each. Core is not
    /// included.
    #[must_use]
    pub fn js_parts_deferred(&self) -> Vec<crate::tsjs_bundle::JsModulePart> {
        self.js_module_ids_deferred()
            .into_iter()
            .filter_map(|id| self.js_part(id))
            .collect()
    }

    /// Every part this registry can serve (bundle, deferred and standalone),
    /// for cache fingerprints. Each id appears once.
    #[must_use]
    pub fn js_parts_all(&self) -> Vec<crate::tsjs_bundle::JsModulePart> {
        let mut ids = self.js_module_ids();
        for id in self.js_standalone_ids() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        self.parts_for(&ids)
    }

    /// Core first, then the part for each id that has one.
    fn parts_for(&self, ids: &[&'static str]) -> Vec<crate::tsjs_bundle::JsModulePart> {
        let mut parts = Vec::with_capacity(ids.len() + 1);
        if let Some(core) = crate::tsjs_bundle::JsModulePart::compile_time("core") {
            parts.push(core);
        }
        parts.extend(ids.iter().filter_map(|id| self.js_part(id)));
        parts
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

    #[cfg(test)]
    #[must_use]
    pub fn empty_for_tests() -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner::default()),
        }
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
                head_router: Router::new(),
                options_router: Router::new(),
                routes: Vec::new(),
                builder_ids: Vec::new(),
                enabled_integration_ids: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
                html_post_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_providers: Vec::new(),
                ec_providers: Vec::new(),
                device_providers: Vec::new(),
                geo_provider: None,
                ec_provider: None,
                device_provider: None,
            }),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn from_rewriters_with_head_injectors(
        attribute_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
        script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
        head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    ) -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner {
                get_router: Router::new(),
                post_router: Router::new(),
                put_router: Router::new(),
                delete_router: Router::new(),
                patch_router: Router::new(),
                head_router: Router::new(),
                options_router: Router::new(),
                routes: Vec::new(),
                builder_ids: Vec::new(),
                enabled_integration_ids: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
                html_post_processors: Vec::new(),
                head_injectors,
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_providers: Vec::new(),
                ec_providers: Vec::new(),
                device_providers: Vec::new(),
                geo_provider: None,
                ec_provider: None,
                device_provider: None,
            }),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn from_request_filters(request_filters: Vec<Arc<dyn IntegrationRequestFilter>>) -> Self {
        Self {
            inner: Arc::new(IntegrationRegistryInner {
                get_router: Router::new(),
                post_router: Router::new(),
                put_router: Router::new(),
                delete_router: Router::new(),
                patch_router: Router::new(),
                head_router: Router::new(),
                options_router: Router::new(),
                routes: Vec::new(),
                builder_ids: Vec::new(),
                enabled_integration_ids: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_post_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters,
                request_preparers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_providers: Vec::new(),
                ec_providers: Vec::new(),
                device_providers: Vec::new(),
                geo_provider: None,
                ec_provider: None,
                device_provider: None,
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

        Self {
            inner: Arc::new(IntegrationRegistryInner {
                get_router,
                post_router,
                put_router,
                delete_router,
                patch_router,
                head_router,
                options_router,
                routes: Vec::new(),
                builder_ids: Vec::new(),
                enabled_integration_ids: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_post_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_providers: Vec::new(),
                ec_providers: Vec::new(),
                device_providers: Vec::new(),
                geo_provider: None,
                ec_provider: None,
                device_provider: None,
            }),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use error_stack::Report;

    use super::{CarriedJsModule, IntegrationRegistration};
    use crate::error::TrustedServerError;
    use crate::settings::Settings;

    /// A browser module built outside `trusted-server-js`, carried by the
    /// `probe` registration.
    pub(crate) const PROBE_JS: &str = "(function(){window.__probe=1;})();";
    // SHA-256 of PROBE_JS, hex; `probe_js_hash_literal_matches_its_source`
    // keeps it honest.
    pub(crate) const PROBE_JS_SHA256: &str =
        "4a8781b3f95646b33f2d4aa92eeaa93ce4b93e87b3d3f20333682ce33eb2f961";

    /// Builds a registration for the `probe` integration on every call.
    pub(crate) fn probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(IntegrationRegistration::builder("probe").build()))
    }

    /// Builds a `probe` registration that carries [`PROBE_JS`] on every call.
    pub(crate) fn carried_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("probe")
                .with_js_module(CarriedJsModule {
                    source: PROBE_JS,
                    sha256: PROBE_JS_SHA256,
                })
                .build(),
        ))
    }

    /// Validates nothing and reports the integration as enabled.
    pub(crate) fn validate_nothing(
        _settings: &Settings,
    ) -> Result<bool, Report<TrustedServerError>> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        PROBE_JS, PROBE_JS_SHA256, carried_probe_registration, probe_registration, validate_nothing,
    };
    use super::*;
    use crate::constants::COOKIE_TS_EC;
    use crate::permissions::{Permission, PermissionSet, PermissionState};
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

    struct ConflictingMetadataHeadInjector;

    impl IntegrationHeadInjector for ConflictingMetadataHeadInjector {
        fn integration_id(&self) -> &'static str {
            "conflicting-metadata"
        }

        fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
            Vec::new()
        }

        fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
            vec![
                ("data-ts-gam-attribution", "false"),
                ("data-third-attribute", "third"),
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
                Arc::new(ConflictingMetadataHeadInjector),
            ],
        );

        assert_eq!(
            registry.tsjs_script_tag_attributes(),
            vec![
                ("data-ts-gam-attribution", "true"),
                ("data-test-order", "second"),
                ("data-third-attribute", "third"),
            ],
            "should keep the first value for duplicate names and preserve attribute order"
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

    /// Records the permission state each filter invocation received, so a test
    /// can assert what the registry handed to the filter.
    #[derive(Default)]
    struct RecordingPermissionsFilter {
        seen: std::sync::Mutex<Option<Option<crate::permissions::PermissionState>>>,
    }

    impl RecordingPermissionsFilter {
        /// The permission state observed by the last invocation, or `None` when
        /// the filter has not run.
        fn seen(&self) -> Option<Option<crate::permissions::PermissionState>> {
            *self
                .seen
                .lock()
                .expect("should lock the recorded permission state")
        }
    }

    #[async_trait(?Send)]
    impl IntegrationRequestFilter for RecordingPermissionsFilter {
        fn integration_id(&self) -> &'static str {
            "recording-permissions"
        }

        async fn filter_request(
            &self,
            input: RequestFilterInput<'_>,
        ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
            *self
                .seen
                .lock()
                .expect("should lock the recorded permission state") =
                Some(input.permissions.copied());
            Ok(RequestFilterDecision::Continue(
                RequestFilterEffects::default(),
            ))
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
                permissions: None,
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
    fn filter_request_passes_the_resolved_permission_state_to_each_filter() {
        let filter = Arc::new(RecordingPermissionsFilter::default());
        let registry = IntegrationRegistry::from_request_filters(vec![
            filter.clone() as Arc<dyn IntegrationRequestFilter>
        ]);
        let settings = crate::test_support::tests::create_test_settings();
        let services = crate::platform::test_support::noop_services();
        let permissions =
            PermissionState::new(PermissionSet::none().with(Permission::StoreOnDevice));
        let mut req = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/page")
            .body(EdgeBody::empty())
            .expect("should build request");

        futures::executor::block_on(registry.filter_request(RequestFilterRegistryInput {
            settings: &settings,
            services: &services,
            req: &mut req,
            geo_info: None,
            permissions: Some(&permissions),
        }))
        .expect("should run request filter");

        assert_eq!(
            filter.seen(),
            Some(Some(permissions)),
            "the filter should observe exactly the permission state resolved for the request"
        );

        // A path that builds no EC context passes no permissions, and the
        // filter must see that absence rather than an empty state.
        futures::executor::block_on(registry.filter_request(RequestFilterRegistryInput {
            settings: &settings,
            services: &services,
            req: &mut req,
            geo_info: None,
            permissions: None,
        }))
        .expect("should run request filter");

        assert_eq!(
            filter.seen(),
            Some(None),
            "an absent permission state should reach the filter as `None`"
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
    fn js_module_ids_include_client_fixed_when_provider_selected() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.ec.provider = Some(crate::ec::provider::EcProviderSelection::from(
            crate::ec::provider::CLIENT_FIXED_PROVIDER_KEY,
        ));
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        assert!(
            registry
                .js_module_ids_immediate()
                .contains(&"ec_client_fixed"),
            "selecting the client-fixed provider should inject its demo page script"
        );
    }

    #[test]
    fn js_module_ids_include_explicitly_enabled_cmp_mirrors() {
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
        let immediate = registry.js_module_ids_immediate();

        assert!(
            immediate.contains(&"sourcepoint"),
            "should include Sourcepoint when explicitly enabled"
        );
        assert!(
            immediate.contains(&"osano"),
            "should include Osano when explicitly enabled"
        );

        let metadata = registry.registered_integrations();
        assert!(
            metadata.iter().any(|integration| integration.id == "osano"),
            "should include JS-only Osano registration in metadata"
        );
    }

    #[test]
    fn js_module_ids_exclude_client_fixed_without_provider() {
        let registry =
            IntegrationRegistry::new(&crate::test_support::tests::create_test_settings())
                .expect("should create registry");

        assert!(
            !registry
                .js_module_ids_immediate()
                .contains(&"ec_client_fixed"),
            "the demo page script should not ship unless the client-fixed provider is selected"
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

    fn duplicate_lockr_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(IntegrationRegistration::builder("lockr").build()))
    }

    #[test]
    fn with_registrations_adds_an_external_builder_after_the_built_ins() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with an external builder");

        assert!(
            registry.integration_enabled("probe"),
            "should register the external integration"
        );
        let ids = registry.registered_builder_ids();
        assert_eq!(
            ids.last().copied(),
            Some("probe"),
            "should order the external builder after every built-in"
        );
    }

    #[test]
    fn with_registrations_rejects_a_duplicate_integration_id_naming_both_sources() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "lockr",
                &serde_json::json!({ "enabled": true, "app_id": "test-app-id" }),
            )
            .expect("should insert lockr config");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "lockr",
            "seam-probe",
            duplicate_lockr_registration,
            validate_nothing,
        )];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a duplicate integration id");

        let message = error.to_string();
        assert!(
            message.contains("lockr")
                && message.contains("trusted-server-core")
                && message.contains("seam-probe"),
            "error should name the id and both sources: {message}"
        );
    }

    fn enable_prebid(settings: &mut Settings) {
        settings
            .integrations
            .insert_config(
                "prebid",
                &serde_json::json!({
                    "enabled": true,
                    "server_url": "https://prebid.example.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example.com/prebid/trusted-prebid.js",
                }),
            )
            .expect("should insert prebid config");
    }

    fn enable_gpt_diagnostics(settings: &mut Settings) {
        settings
            .integrations
            .insert_config("gpt_diagnostics", &serde_json::json!({ "enabled": true }))
            .expect("should insert gpt_diagnostics config");
    }

    fn carried_probe_builder() -> crate::integrations::IntegrationBuilder {
        crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            carried_probe_registration,
            validate_nothing,
        )
    }

    const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn lying_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("probe")
                .with_js_module(CarriedJsModule {
                    source: PROBE_JS,
                    sha256: ZERO_SHA256,
                })
                .build(),
        ))
    }

    fn disabled_carried_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("probe")
                .with_js_module(CarriedJsModule {
                    source: PROBE_JS,
                    sha256: PROBE_JS_SHA256,
                })
                .without_js()
                .build(),
        ))
    }

    #[test]
    fn probe_js_hash_literal_matches_its_source() {
        assert_eq!(
            hex::encode(Sha256::digest(PROBE_JS.as_bytes())),
            PROBE_JS_SHA256,
            "should keep the PROBE_JS_SHA256 literal equal to the hash of PROBE_JS"
        );
    }

    #[test]
    fn a_carried_js_module_is_served_in_the_immediate_parts() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [carried_probe_builder()];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with a carried module");

        let parts = registry.js_parts_immediate();
        assert_eq!(
            parts.first().map(|part| part.id),
            Some("core"),
            "should put core first in the immediate parts"
        );
        let probe = parts
            .iter()
            .find(|part| part.id == "probe")
            .expect("should include the carried probe module in the immediate parts");
        assert_eq!(
            probe.source, PROBE_JS,
            "should serve the carried source verbatim"
        );
        assert_eq!(
            probe.sha256, PROBE_JS_SHA256,
            "should carry the declared hash on the part"
        );
        assert!(
            registry.js_module_ids_immediate().contains(&"probe"),
            "should list the carried module among the immediate module ids"
        );
    }

    #[test]
    fn a_carried_js_module_with_a_wrong_hash_is_rejected_at_registry_build() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            lying_probe_registration,
            validate_nothing,
        )];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a carried module whose declared hash is wrong");

        let message = error.to_string();
        assert!(
            message.contains("probe") && message.contains("does not match"),
            "error should name the integration and say the hash does not match: {message}"
        );
        assert!(
            message.contains(ZERO_SHA256) && message.contains(PROBE_JS_SHA256),
            "error should quote the declared and the actual hash: {message}"
        );
    }

    #[test]
    fn js_part_is_none_for_an_integration_registered_without_js() {
        // The carried lookup would answer `Some` on its own, so this proves the
        // disabled check runs first. No built-in integration can stand in: the
        // only `without_js` built-in with a Rust registration, `aps`, has no
        // compile-time module either.
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            disabled_carried_probe_registration,
            validate_nothing,
        )];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");

        assert!(
            registry.integration_enabled("probe"),
            "should still register the integration"
        );
        assert!(
            registry.js_part("probe").is_none(),
            "should not serve a module for an integration registered without JS"
        );
        assert!(
            !registry.js_module_ids().contains(&"probe"),
            "should keep a without-JS integration out of the bundle module ids"
        );
    }

    #[test]
    fn the_last_js_delivery_flag_set_on_the_builder_wins() {
        let disabled_last = IntegrationRegistration::builder("probe")
            .with_standalone_js()
            .without_js()
            .build();
        assert!(
            disabled_last.js_disabled && !disabled_last.js_standalone && !disabled_last.js_deferred,
            "without_js after with_standalone_js should leave only disabled set"
        );

        let deferred_last = IntegrationRegistration::builder("probe")
            .with_standalone_js()
            .with_deferred_js()
            .build();
        assert!(
            deferred_last.js_deferred && !deferred_last.js_standalone && !deferred_last.js_disabled,
            "with_deferred_js after with_standalone_js should leave only deferred set"
        );

        let deferred_after_disabled = IntegrationRegistration::builder("probe")
            .without_js()
            .with_deferred_js()
            .build();
        assert!(
            deferred_after_disabled.js_deferred
                && !deferred_after_disabled.js_disabled
                && !deferred_after_disabled.js_standalone,
            "with_deferred_js after without_js should leave only deferred set"
        );

        let standalone_last = IntegrationRegistration::builder("probe")
            .without_js()
            .with_deferred_js()
            .with_standalone_js()
            .build();
        assert!(
            standalone_last.js_standalone
                && !standalone_last.js_disabled
                && !standalone_last.js_deferred,
            "with_standalone_js last should leave only standalone set"
        );
    }

    #[test]
    fn a_standalone_js_module_is_served_alone_and_not_in_the_bundle() {
        let mut settings = crate::test_support::tests::create_test_settings();
        enable_gpt_diagnostics(&mut settings);

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        assert!(
            !registry.js_module_ids().contains(&"gpt_diagnostics"),
            "should keep a standalone module out of the bundle module ids"
        );
        assert!(
            registry.js_part("gpt_diagnostics").is_some(),
            "should serve the standalone module as a part"
        );
        assert_eq!(
            registry.js_standalone_ids(),
            vec!["gpt_diagnostics"],
            "should list the enabled standalone module"
        );
        assert!(
            registry.js_part("lockr").is_none(),
            "should not serve a module for an integration that is not enabled"
        );
    }

    #[test]
    fn js_parts_all_covers_bundle_deferred_and_standalone_modules() {
        let mut settings = crate::test_support::tests::create_test_settings();
        enable_prebid(&mut settings);
        enable_gpt_diagnostics(&mut settings);
        let extra = [carried_probe_builder()];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");

        let ids = registry
            .js_parts_all()
            .into_iter()
            .map(|part| part.id)
            .collect::<Vec<_>>();
        for expected in ["core", "creative", "probe", "prebid", "gpt_diagnostics"] {
            assert_eq!(
                ids.iter().filter(|id| **id == expected).count(),
                1,
                "should list `{expected}` exactly once in {ids:?}"
            );
        }
    }

    /// A builder function for an integration that is never enabled, so its
    /// preparer is the only thing the registry can take from it.
    fn never_enabled_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(None)
    }

    /// Preparers cannot capture, because a builder holds plain function
    /// pointers, so they record the order they ran in here.
    static PREPARER_ORDER: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    fn record_first_preparer(
        _settings: &Settings,
        _request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        PREPARER_ORDER
            .lock()
            .expect("should lock the preparer order")
            .push("first");
        Ok(())
    }

    fn record_second_preparer(
        _settings: &Settings,
        _request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        PREPARER_ORDER
            .lock()
            .expect("should lock the preparer order")
            .push("second");
        Ok(())
    }

    /// Records whether the preparer registered after the failing one ran.
    static PREPARER_AFTER_FAILURE: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    const FAILING_PREPARER_MESSAGE: &str = "probe preparer refused the request";

    fn failing_preparer(
        _settings: &Settings,
        _request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Configuration {
            message: FAILING_PREPARER_MESSAGE.to_owned(),
        }))
    }

    fn record_after_failure_preparer(
        _settings: &Settings,
        _request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        PREPARER_AFTER_FAILURE
            .lock()
            .expect("should lock the after-failure record")
            .push("after");
        Ok(())
    }

    fn plain_request() -> Request<EdgeBody> {
        Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article")
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    #[test]
    fn prepare_request_runs_every_preparer_in_registration_order_enabled_or_not() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-first",
                "seam-probe",
                never_enabled_registration,
                validate_nothing,
            )
            .with_request_preparer(record_first_preparer),
            crate::integrations::IntegrationBuilder::new(
                "probe-second",
                "seam-probe",
                never_enabled_registration,
                validate_nothing,
            )
            .with_request_preparer(record_second_preparer),
        ];
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with request preparers");
        assert!(
            !registry.integration_enabled("probe-first"),
            "should leave the first probe integration disabled"
        );
        assert!(
            !registry.integration_enabled("probe-second"),
            "should leave the second probe integration disabled"
        );
        PREPARER_ORDER
            .lock()
            .expect("should lock the preparer order")
            .clear();
        let mut request = plain_request();

        registry
            .prepare_request(&settings, &mut request)
            .expect("should run every request preparer");

        assert_eq!(
            *PREPARER_ORDER
                .lock()
                .expect("should lock the preparer order"),
            vec!["first", "second"],
            "should run both preparers in registration order even though neither integration is enabled"
        );
    }

    #[test]
    fn prepare_request_surfaces_the_first_preparer_error() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-failing",
                "seam-probe",
                never_enabled_registration,
                validate_nothing,
            )
            .with_request_preparer(failing_preparer),
            crate::integrations::IntegrationBuilder::new(
                "probe-after-failure",
                "seam-probe",
                never_enabled_registration,
                validate_nothing,
            )
            .with_request_preparer(record_after_failure_preparer),
        ];
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with request preparers");
        PREPARER_AFTER_FAILURE
            .lock()
            .expect("should lock the after-failure record")
            .clear();
        let mut request = plain_request();

        let error = registry
            .prepare_request(&settings, &mut request)
            .expect_err("should surface the failing preparer's error");

        assert!(
            error.to_string().contains(FAILING_PREPARER_MESSAGE),
            "should keep the preparer's message intact: {error}"
        );
        assert!(
            PREPARER_AFTER_FAILURE
                .lock()
                .expect("should lock the after-failure record")
                .is_empty(),
            "should not run a preparer registered after the failing one"
        );
    }

    #[test]
    fn prepare_request_runs_the_built_in_gpt_diagnostics_preparer() {
        // The adapters call the registry rather than naming GPT diagnostics, so
        // the built-in table is what attaches the sanitizing preparer. This is
        // asserted here rather than through an adapter route test because the
        // stripped query is only visible on the publisher HTML path, which needs
        // a live origin the adapter test harnesses do not have.
        let settings = crate::test_support::tests::create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article?ts_console=1&keep=yes")
            .header(header::COOKIE, "__Host-ts-console=1; keep-me=yes")
            .body(EdgeBody::empty())
            .expect("should build request");

        registry
            .prepare_request(&settings, &mut request)
            .expect("should run the built-in preparers");

        assert_eq!(
            request.uri().query(),
            Some("keep=yes"),
            "should strip the reserved diagnostics query and keep the rest"
        );
        assert_eq!(
            request
                .headers()
                .get(header::COOKIE)
                .map(|value| value.to_str().expect("cookie should be text")),
            Some("keep-me=yes"),
            "should strip the reserved diagnostics cookie and keep the rest"
        );
    }

    /// Example country code returned by the test geo provider. `ZZ` is the
    /// user-assigned code, so it names no real place.
    const GEO_PROBE_COUNTRY: &str = "ZZ";

    /// A geo provider that resolves one fixed location, so a test can tell the
    /// module's provider apart from the "no location" one.
    #[derive(Debug)]
    struct FixedCountryGeo;

    impl crate::platform::PlatformGeo for FixedCountryGeo {
        fn lookup(
            &self,
            _client_ip: Option<std::net::IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<crate::platform::PlatformError>> {
            Ok(Some(GeoInfo {
                city: "Example City".to_owned(),
                country: GEO_PROBE_COUNTRY.to_owned(),
                continent: "Example".to_owned(),
                latitude: 0.0,
                longitude: 0.0,
                metro_code: 0,
                region: None,
                asn: None,
            }))
        }
    }

    /// Builds a `geo-probe` registration that declares a geo provider.
    fn geo_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("geo-probe")
                .with_geo_provider(Arc::new(FixedCountryGeo))
                .build(),
        ))
    }

    fn geo_probe_builders() -> [crate::integrations::IntegrationBuilder; 1] {
        [crate::integrations::IntegrationBuilder::new(
            "geo-probe",
            "seam-probe",
            geo_probe_registration,
            validate_nothing,
        )]
    }

    fn settings_selecting_geo_provider(provider: &str) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.geo.provider = Some(provider.to_owned());
        settings
    }

    #[test]
    fn geo_provider_resolves_the_provider_declared_by_the_selected_module() {
        let settings = settings_selecting_geo_provider("geo-probe");

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with a module geo provider");

        let provider = registry
            .geo_provider()
            .expect("should resolve the module's geo provider");
        let resolved = provider
            .lookup(None)
            .expect("should look up without failing")
            .expect("should resolve a location");
        assert_eq!(
            resolved.country, GEO_PROBE_COUNTRY,
            "should resolve through the module's own provider"
        );
    }

    #[test]
    fn geo_provider_none_resolves_no_location() {
        let settings = settings_selecting_geo_provider("none");

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with the disabled geo provider");

        let provider = registry
            .geo_provider()
            .expect("should resolve the disabled geo provider");
        assert!(
            provider
                .lookup(None)
                .expect("should look up without failing")
                .is_none(),
            "should resolve no location when the selector is `none`"
        );
    }

    #[test]
    fn an_unset_geo_selector_resolves_the_disabled_provider_and_platform_opts_in() {
        // Unset is the permission model's privacy default: it resolves the
        // disabled provider, so no client IP ever reaches a host geo service.
        // It is deliberately not the same as leaving the adapter's own lookup
        // in place, which is what `platform` now spells.
        let settings = crate::test_support::tests::create_test_settings();
        assert!(
            settings.geo.provider.is_none(),
            "the shared test settings should leave the geo selector unset"
        );

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with an unset geo selector");

        let provider = registry
            .geo_provider()
            .expect("an unset selector should resolve the disabled provider, not nothing");
        assert!(
            provider
                .lookup(None)
                .expect("should look up without failing")
                .is_none(),
            "the disabled provider should resolve no location and make no host call"
        );

        // `platform` is the explicit opt-in to the adapter's own lookup, and it
        // is the one case that resolves nothing here so the adapter's provider
        // stands.
        let platform = settings_selecting_geo_provider(GEO_PROVIDER_PLATFORM);
        let registry = IntegrationRegistry::with_registrations(&platform, &geo_probe_builders())
            .expect("should build registry with the platform geo selector");
        assert!(
            registry.geo_provider().is_none(),
            "`platform` should leave the adapter's own host lookup in place"
        );
    }

    #[test]
    fn geo_provider_rejects_a_module_that_declares_no_geo_provider() {
        let settings = settings_selecting_geo_provider("probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a module that declares no geo provider");

        let message = error.to_string();
        assert!(
            message.contains("probe") && message.contains("geo provider"),
            "error should name the module and the capability: {message}"
        );
    }

    #[test]
    fn geo_provider_rejects_a_module_that_is_registered_but_not_enabled() {
        let settings = settings_selecting_geo_provider("probe-disabled");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe-disabled",
            "seam-probe",
            never_enabled_registration,
            validate_nothing,
        )];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a module that is registered but not enabled");

        let message = error.to_string();
        assert!(
            message.contains("probe-disabled")
                && message.contains("not enabled")
                && message.contains("geo provider"),
            "error should name the module, that it is not enabled, and the capability: {message}"
        );
    }

    #[test]
    fn geo_provider_rejects_a_module_that_is_not_registered() {
        let settings = settings_selecting_geo_provider("absent-module");

        let error = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .err()
            .expect("should reject a selector naming nothing registered");

        let message = error.to_string();
        assert!(
            message.contains("absent-module") && message.contains("not registered"),
            "error should name the module and say it is not registered: {message}"
        );
    }
}
