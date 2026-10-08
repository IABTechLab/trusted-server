use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Method, Request, Response};
use matchit::Router;
use sha2::{Digest as _, Sha256};

use crate::auction::AuctionPlan;
use crate::constants::HEADER_X_TS_EC;
use crate::ec::EcContext;
use crate::ec::device::DeviceModule;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::module::{EcModuleSelection, EdgeCookieModule};
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::is_navigation_request;
use crate::platform::{DisabledGeo, PlatformGeo, RuntimeServices};
use crate::settings::Settings;
use crate::streaming_processor::StreamProcessor;

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
    pub element_name: &'a str,
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
    pub max_buffered_script_bytes: usize,
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

/// Values a module's request hooks leave for its own page hooks, for one
/// request.
///
/// A request preparer or a request filter leaves a value under its
/// integration id with [`IntegrationRequestState::insert`]. When the request
/// produces an HTML document for that one reader, every value is copied into
/// the document's [`IntegrationDocumentState`] before parsing starts, so the
/// module's head injector, rewriters and stream processors read it there, and
/// the same values are handed to the module's response finalizer.
///
/// A request that carries any value keeps to the origin path. Its document is
/// never read from a shared template and never stored as one, and an HTML
/// response with a body is sent `private, no-store`. A document built to be
/// shared starts with none of these values.
#[derive(Clone, Default)]
pub struct IntegrationRequestState {
    values: IntegrationDocumentStateMap,
}

impl std::fmt::Debug for IntegrationRequestState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntegrationRequestState")
            .field("keys", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl IntegrationRequestState {
    /// Leaves `value` on `request` for the page hooks of `integration_id`,
    /// in place of a value of the same type left earlier.
    pub fn insert<T>(request: &mut Request<EdgeBody>, integration_id: &'static str, value: T)
    where
        T: Any + Send + Sync + 'static,
    {
        let mut state = request
            .extensions_mut()
            .remove::<Self>()
            .unwrap_or_default();
        state.set(integration_id, value);
        request.extensions_mut().insert(state);
    }

    /// Holds `value` for the page hooks of `integration_id`, in place of a
    /// value of the same type held earlier.
    pub fn set<T>(&mut self, integration_id: &'static str, value: T)
    where
        T: Any + Send + Sync + 'static,
    {
        self.values
            .insert((integration_id, TypeId::of::<T>()), Arc::new(value));
    }

    /// The values left on `request`, which is none for a request no module
    /// left one on.
    #[must_use]
    pub fn of(request: &Request<EdgeBody>) -> Self {
        request
            .extensions()
            .get::<Self>()
            .cloned()
            .unwrap_or_default()
    }

    /// The value of type `T` left for `integration_id`.
    #[must_use]
    pub fn get<T>(&self, integration_id: &'static str) -> Option<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let value = self.values.get(&(integration_id, TypeId::of::<T>()))?;
        Arc::clone(value).downcast::<T>().ok()
    }

    /// Whether no module left a value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Copies every value into the state of a document that is starting.
    ///
    /// # Panics
    ///
    /// Panics if the document state's lock is poisoned.
    pub(crate) fn seed(&self, document: &IntegrationDocumentState) {
        if self.values.is_empty() {
            return;
        }
        let mut guard = document
            .inner
            .lock()
            .expect("should lock integration document state");
        for (key, value) in &self.values {
            guard.insert(*key, Arc::clone(value));
        }
    }
}

/// Per-document buffer for script text fragments split across chunks.
///
/// `lol_html` can deliver one text node as several chunks, so a rewriter that
/// needs the whole script must accumulate until `is_last_in_text_node`.
///
/// This lives in [`IntegrationDocumentState`] rather than on the rewriter.
/// Rewriters are registered once as `Arc<dyn IntegrationScriptRewriter>` and
/// live as long as the [`IntegrationRegistry`], so a buffer owned by a
/// rewriter is shared by every document that registry serves. A document whose
/// stream ends before the final fragment — client disconnect, origin error,
/// truncated body — leaves its partial script in that buffer, and the next
/// document prepends the residue to its own accumulation. That corrupts the
/// response and can disclose the previous document's content.
///
/// Keyed per integration id, so each integration gets its own buffer, and
/// dropped with the document state at end of document.
#[derive(Debug, Default)]
pub struct ScriptTextAccumulator {
    buffer: Mutex<String>,
}

impl ScriptTextAccumulator {
    /// Locks the buffer.
    ///
    /// Recovers from poisoning rather than panicking: a poisoned buffer holds
    /// at worst a partial script, and the caller's `is_last_in_text_node`
    /// handling already tolerates unexpected contents.
    pub fn buffer(&self) -> MutexGuard<'_, String> {
        self.buffer.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Owned request data supplied when an integration creates an HTML stream processor.
#[derive(Clone)]
pub struct IntegrationHtmlStreamContext {
    /// Publisher-facing host used for rewritten URLs.
    pub request_host: String,
    /// Publisher-facing scheme used for rewritten URLs.
    pub request_scheme: String,
    /// Origin host whose URLs may be rewritten.
    pub origin_host: String,
    /// Request-local state shared with the document's integration rewriters.
    pub document_state: IntegrationDocumentState,
}

/// Creates one mutable HTML output processor for each document.
pub trait IntegrationHtmlStreamProcessorFactory: Send + Sync {
    /// Identifier for logging and diagnostics.
    fn integration_id(&self) -> &'static str;

    /// Create a request-local streaming processor.
    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor>;
}

/// Trait for integration-provided HTML head injections.
pub trait IntegrationHeadInjector: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;
    /// Return HTML snippets to insert at the start of `<head>`.
    fn head_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String>;

    /// Return HTML snippets to insert straight after the main script bundle
    /// and before any deferred one, for a script that needs the bundle to
    /// have run and has to run before the page's own scripts.
    fn after_bundle_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        Vec::new()
    }

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
    pub html_stream_processors: Vec<Arc<dyn IntegrationHtmlStreamProcessorFactory>>,
    pub head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    pub request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    /// Geo module this registration supplies, with the name `[geo] module`
    /// selects it by.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to resolve location when `[geo] module` names it.
    pub geo_module: Option<(&'static str, Arc<dyn PlatformGeo>)>,
    /// Edge Cookie module this registration supplies, with the name
    /// `[ec] module` selects it by.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to create an identifier when `[ec] module` names it. This is the same
    /// route geo takes, so identity is not a second extension mechanism
    /// sitting beside the integration system.
    pub ec_module: Option<(&'static str, Arc<dyn EdgeCookieModule>)>,
    /// Device module this registration supplies, with the name
    /// `[device] module` selects it by.
    ///
    /// Declaring one does not make it active, because the module is only asked
    /// to classify a request when `[device] module` names it.
    pub device_module: Option<(&'static str, Arc<dyn DeviceModule>)>,
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
                html_stream_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
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
    pub fn with_html_stream_processor(
        mut self,
        processor: Arc<dyn IntegrationHtmlStreamProcessorFactory>,
    ) -> Self {
        self.registration.html_stream_processors.push(processor);
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

    /// Declare the geo module this registration supplies, under its `name`.
    ///
    /// The name is the path under `crates` of the crate the module lives in,
    /// as [`crate::module_name!`] gives it, so a module from
    /// `crates/geo/example` is selected by `[geo] module = "example"`. One
    /// registration can therefore supply a module of each type, each under
    /// the name of its own crate. The module only resolves location when
    /// `[geo] module` names it, and a module the selector does not choose is
    /// logged as a warning when the registry is built.
    #[must_use]
    pub fn with_geo_module(mut self, name: &'static str, module: Arc<dyn PlatformGeo>) -> Self {
        self.registration.geo_module = Some((name, module));
        self
    }

    /// Declare the Edge Cookie module this registration supplies, under its
    /// `name`, which is also what the module's
    /// [`id`](EdgeCookieModule::id) returns.
    ///
    /// The module only creates identifiers when `[ec] module` names it, and
    /// a module the selector does not choose is logged as a warning when the
    /// registry is built, the same as geo.
    #[must_use]
    pub fn with_ec_module(mut self, name: &'static str, module: Arc<dyn EdgeCookieModule>) -> Self {
        self.registration.ec_module = Some((name, module));
        self
    }

    /// Declare the device module this registration supplies, under its
    /// `name`.
    ///
    /// The module only classifies requests when `[device] module` names it,
    /// and a module the selector does not choose is logged as a warning when
    /// the registry is built, the same as geo.
    #[must_use]
    pub fn with_device_module(mut self, name: &'static str, module: Arc<dyn DeviceModule>) -> Self {
        self.registration.device_module = Some((name, module));
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

/// Marks a request the preparers have run for.
#[derive(Clone, Copy)]
struct RequestPrepared;

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
    // Every builder considered at construction, named or not, in order.
    builder_ids: Vec<(&'static str, &'static str)>,
    running_integration_ids: Vec<&'static str>,
    deferred_js_ids: Vec<&'static str>,
    disabled_js_ids: Vec<&'static str>,
    // Modules that run, served only on their own path, never in the bundle.
    standalone_js_ids: Vec<&'static str>,
    // Modules carried by their registrations, verified against their
    // declared hash at construction.
    carried_js: Vec<(&'static str, CarriedJsModule)>,
    html_rewriters: Vec<Arc<dyn IntegrationAttributeRewriter>>,
    script_rewriters: Vec<Arc<dyn IntegrationScriptRewriter>>,
    html_stream_processors: Vec<Arc<dyn IntegrationHtmlStreamProcessorFactory>>,
    head_injectors: Vec<Arc<dyn IntegrationHeadInjector>>,
    request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    /// JS module IDs to include in the bundle that come from a source other than
    /// a registered integration, for example a module tied to the selected Edge
    /// Cookie module. Populated in [`IntegrationRegistry::new`] from settings.
    extra_js_module_ids: Vec<&'static str>,
    // Preparers from every builder, named or not, in registration order.
    request_preparers: Vec<crate::integrations::IntegrationPrepareRequestFn>,
    // Finalizers from every builder, named or not, in registration order.
    response_finalizers: Vec<crate::integrations::IntegrationFinalizeResponseFn>,
    // Geo modules the running registrations supply, each with the name
    // `[geo] module` selects it by, in registration order. Declaring one does
    // not activate it.
    geo_modules: Vec<(&'static str, Arc<dyn PlatformGeo>)>,
    // Edge Cookie modules the running registrations supply, each with its
    // name, in registration order. `[ec] module` picks at most one of them.
    ec_modules: Vec<(&'static str, Arc<dyn EdgeCookieModule>)>,
    // Device modules the running registrations supply, each with its name, in
    // registration order. `[device] module` picks at most one of them.
    device_modules: Vec<(&'static str, Arc<dyn DeviceModule>)>,
    // Every builder's module name, with whether a section selects it, so the
    // refusal of a name a selector wrote can say what that name is.
    builder_modules: Vec<(&'static str, bool)>,
    // The module `[geo] module` resolved to, or `None` when the selector
    // is `platform` and the adapter's own host lookup stands. Unset and `none`
    // both resolve the disabled module.
    geo_module: Option<Arc<dyn PlatformGeo>>,
    ec_module: Option<Arc<dyn EdgeCookieModule>>,
    device_module: Option<Arc<dyn DeviceModule>>,
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
            running_integration_ids: Vec::new(),
            deferred_js_ids: Vec::new(),
            disabled_js_ids: Vec::new(),
            standalone_js_ids: Vec::new(),
            carried_js: Vec::new(),
            html_rewriters: Vec::new(),
            script_rewriters: Vec::new(),
            html_stream_processors: Vec::new(),
            head_injectors: Vec::new(),
            request_filters: Vec::new(),
            extra_js_module_ids: Vec::new(),
            request_preparers: Vec::new(),
            response_finalizers: Vec::new(),
            geo_modules: Vec::new(),
            ec_modules: Vec::new(),
            device_modules: Vec::new(),
            builder_modules: Vec::new(),
            geo_module: None,
            ec_module: None,
            device_module: None,
        }
    }
}

/// Reserved value of `[geo] module` that resolves no location at all.
const GEO_MODULE_NONE: &str = "none";

/// `[geo] module` value opting in to the adapter's own host geo lookup.
const GEO_MODULE_PLATFORM: &str = "platform";

/// `[device] module` value naming the User-Agent-only module core supplies.
///
/// The same choice as leaving the selector unset, spelled explicitly.
const DEVICE_MODULE_BUILTIN: &str = "builtin";

/// `[device] module` value opting in to the host module the adapter builds.
///
/// Resolved by `build_device_module` in [`device`](crate::ec::device) rather
/// than by a module, so the registry supplies nothing for it.
const DEVICE_MODULE_FASTLY: &str = "fastly";

/// The type folder each of these kinds of module is selected in, which a name
/// written in its section may leave off.
const EDGECOOKIE_TYPE: &str = crate::ec::module::MODULE_TYPE;
const DEVICE_TYPE: &str = "device";
const GEO_TYPE: &str = "geo";

impl IntegrationRegistryInner {
    /// A sentence to follow the refusal of a name a selector wrote, when the
    /// name is a builder's module: one no section selects, which is why
    /// nothing it supplies is running, or one that is selected and supplies no
    /// module of this kind. Empty when no builder's module answers to the
    /// name.
    fn builder_note(&self, type_folder: &str, kind: &str, written: &str) -> String {
        let names: Vec<&str> = self.builder_modules.iter().map(|(name, _)| *name).collect();
        let Some(name) = crate::module_name::resolve(type_folder, written, &names) else {
            return String::new();
        };
        let selected = self
            .builder_modules
            .iter()
            .any(|(module, selected)| *module == name && *selected);
        if selected {
            format!(". `{written}` is selected and supplies no {kind} module")
        } else {
            format!(
                ". `{written}` is a module no section selects, so nothing it supplies is running"
            )
        }
    }
}

/// The module a name written in a section means, among those the running
/// registrations supply, written in full or with the section's type folder
/// left off.
fn find_named<'a, T: ?Sized>(
    type_folder: &str,
    written: &str,
    offered: &'a [(&'static str, Arc<T>)],
) -> Option<&'a Arc<T>> {
    let names: Vec<&str> = offered.iter().map(|(name, _)| *name).collect();
    let name = crate::module_name::resolve(type_folder, written, &names)?;
    offered
        .iter()
        .find(|(offered_name, _)| *offered_name == name)
        .map(|(_, module)| module)
}

/// Warns about each module on offer that the section does not select, so an
/// operator can see a capability the deployment never uses.
fn warn_unselected<T: ?Sized>(
    section: &str,
    type_folder: &str,
    selected: Option<&Arc<T>>,
    offered: &[(&'static str, Arc<T>)],
) {
    for (name, module) in offered {
        if !selected.is_some_and(|chosen| Arc::ptr_eq(chosen, module)) {
            log::warn!(
                "the module `{}` is offered, and `[{section}] module` does not select it",
                crate::module_name::short_form(type_folder, name)
            );
        }
    }
}

/// The registered module a section names.
///
/// # Errors
///
/// A configuration error naming the section and the modules on offer when no
/// registered module answers to the name, because otherwise a mistyped name
/// would fall back to core's own module with nothing said.
fn named_module<T: ?Sized>(
    type_folder: &str,
    section: &str,
    written: &str,
    offered: &[(&'static str, Arc<T>)],
    inner: &IntegrationRegistryInner,
) -> Result<Arc<T>, Report<TrustedServerError>> {
    if let Some(module) = find_named(type_folder, written, offered) {
        return Ok(Arc::clone(module));
    }
    let names: Vec<&str> = offered
        .iter()
        .map(|(name, _)| crate::module_name::short_form(type_folder, name))
        .collect();
    let runs = if names.is_empty() {
        format!("It runs no {section} module")
    } else {
        format!("The {section} modules it runs are [{}]", names.join(", "))
    };
    Err(Report::new(TrustedServerError::Configuration {
        message: format!(
            "`[{section}] module` names `{written}`, which no module this deployment runs \
             supplies. {runs}{}",
            inner.builder_note(type_folder, section, written)
        ),
    }))
}

/// Records that `integration` supplies the `kind` module `name`.
///
/// # Errors
///
/// When another registration already supplies a module of that kind under the
/// name, naming both, because a selector could then mean either.
fn claim_module_name(
    claimed: &mut Vec<(&'static str, &'static str, &'static str)>,
    kind: &'static str,
    name: &'static str,
    integration: &'static str,
) -> Result<(), Report<TrustedServerError>> {
    if let Some((_, _, first)) = claimed
        .iter()
        .find(|(claimed_kind, claimed_name, _)| *claimed_kind == kind && *claimed_name == name)
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "the {kind} module `{name}` is declared twice, by integration `{first}` and by \
                 integration `{integration}`"
            ),
        }));
    }
    claimed.push((kind, name, integration));
    Ok(())
}

/// Resolves the Edge Cookie module `[ec] module` names, when a registered
/// module supplies it.
///
/// Core has Edge Cookie modules of its own, so a name no registered module
/// answers to is not an error here. This returns `Some` only for a registered
/// module, and core resolves the rest. The name looked for is the
/// implementation the selection's `[ec.<name>]` block names, or the selection
/// itself, which is the name core checks the module's id against.
fn resolve_ec_module(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Option<Arc<dyn EdgeCookieModule>> {
    // `None` spells statelessness and never names a module, so only a named
    // selection can match one.
    let implementation = match settings.ec.module.as_ref() {
        Some(EcModuleSelection::Named(key)) => Some(settings.ec.module_blocks.implementation(key)),
        Some(EcModuleSelection::None) | None => None,
    };
    let resolved = implementation
        .and_then(|name| find_named(EDGECOOKIE_TYPE, name, &inner.ec_modules))
        .map(Arc::clone);
    warn_unselected("ec", EDGECOOKIE_TYPE, resolved.as_ref(), &inner.ec_modules);
    resolved
}

/// Resolves the device module `[device] module` names, when a registered
/// module supplies it.
///
/// As with identity, core has a device module of its own, so a selector naming
/// it is not an error here.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the selector names no
/// registered module, or names one that declares a permission nothing can
/// enforce.
fn resolve_device_module(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<Option<Arc<dyn DeviceModule>>, Report<TrustedServerError>> {
    let selector = settings.device.module.as_deref();
    let resolved = match selector {
        // Unset and `builtin` both name the module core supplies itself, and
        // `fastly` names the host module the adapter builds, so the registry
        // supplies nothing for any of the three.
        None | Some(DEVICE_MODULE_BUILTIN) | Some(DEVICE_MODULE_FASTLY) => None,
        Some(written) => Some(named_module(
            DEVICE_TYPE,
            "device",
            written,
            &inner.device_modules,
            inner,
        )?),
    };

    // A module-supplied device module may declare the permissions its data
    // use requires, but nothing enforces that declaration yet. Device
    // classification runs before the permission set for the request is
    // assembled, so there is no per-request gate to check it against. Selecting
    // a module is an operator decision and not a per-request permission
    // decision, so honoring the declaration by silently ignoring it would let a
    // vendor state a requirement that never binds. Refuse the selection instead,
    // loudly and at startup, until a real per-request device gate exists.
    if let Some(module) = resolved.as_ref()
        && module.required_permissions() != crate::permissions::PermissionSet::none()
    {
        let declared = module
            .required_permissions()
            .iter()
            .map(crate::permissions::Permission::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "`[device] module` selects `{}`, a device module requiring `{declared}`, and \
             Trusted Server has no per-request gate that can enforce that yet, so the \
             selection is refused rather than silently ignored",
            selector.unwrap_or_default(),
        );
        return Err(Report::new(TrustedServerError::Configuration { message }));
    }

    warn_unselected(
        "device",
        DEVICE_TYPE,
        resolved.as_ref(),
        &inner.device_modules,
    );

    Ok(resolved)
}

/// Resolves `[geo] module` against the registered geo modules.
///
/// Returns the disabled module when the selector is unset or `none`, `None`
/// when it is `platform` so the adapter's own host lookup stands, and the
/// registered module otherwise. A module on offer that the selector does not
/// choose is logged as a warning when the registry is built.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the selector names no
/// registered module.
fn resolve_geo_module(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<Option<Arc<dyn PlatformGeo>>, Report<TrustedServerError>> {
    let selector = settings.geo.module.as_deref();
    let resolved = match selector {
        // Unset resolves nothing and makes no host geo call, so a default
        // deployment is not tied to any host geo service. `none` spells the
        // same choice explicitly.
        None | Some(GEO_MODULE_NONE) => Some(Arc::new(DisabledGeo) as Arc<dyn PlatformGeo>),
        // `platform` opts in to the adapter's own host lookup, so the registry
        // supplies nothing and the adapter's module stands.
        Some(GEO_MODULE_PLATFORM) => None,
        Some(written) => Some(named_module(
            GEO_TYPE,
            "geo",
            written,
            &inner.geo_modules,
            inner,
        )?),
    };
    warn_unselected("geo", GEO_TYPE, resolved.as_ref(), &inner.geo_modules);
    Ok(resolved)
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
    plan: Option<Arc<AuctionPlan>>,
}

/// Refuses a name a section selects that no module in this deployment
/// supplies, listing what the section could select instead.
///
/// # Errors
///
/// Naming the first section and name at fault.
fn check_section_selections(
    settings: &Settings,
    extra: &[crate::integrations::IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    let offered: Vec<&'static str> = crate::integrations::all_builders(extra)
        .filter_map(|builder| builder.module_name())
        .collect();
    for (section, modules) in settings.module_sections() {
        for written in modules.selected() {
            if crate::module_name::resolve(section, written, &offered).is_some() {
                continue;
            }
            let of_type: Vec<&str> = offered
                .iter()
                .map(|name| crate::module_name::short_form(section, name))
                .filter(|short| !offered.contains(short))
                .collect();
            let supplied = if of_type.is_empty() {
                let mut types: Vec<&str> = offered
                    .iter()
                    .filter_map(|name| name.split_once('.').map(|(folder, _)| folder))
                    .collect();
                types.sort_unstable();
                types.dedup();
                format!(
                    "It supplies no {section} module. The types it supplies modules of are [{}]",
                    types.join(", ")
                )
            } else {
                format!(
                    "The {section} modules it supplies are [{}]",
                    of_type.join(", ")
                )
            };
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{section}] selects `{written}`, which no module this deployment \
                     supplies. {supplied}"
                ),
            }));
        }
    }
    Ok(())
}

impl IntegrationRegistry {
    /// Build a registry and auction plan from the provided settings for tests.
    ///
    /// Runtime adapters should compile one plan and pass it to
    /// [`Self::with_plan`], or to [`Self::with_plan_and_registrations`] when
    /// they also supply integrations of their own.
    ///
    /// # Errors
    ///
    /// Returns an error if the auction plan or integration registry is invalid.
    #[cfg(test)]
    pub fn new(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let plan = Arc::new(crate::auction::compile_auction_plan(settings)?);
        Self::with_plan(settings, plan)
    }

    /// Build a registry from the built-in integrations followed by `extra`,
    /// compiling the auction plan from `settings`, for tests.
    ///
    /// Runtime adapters compile one plan and pass it to
    /// [`Self::with_plan_and_registrations`], so the plan is shared rather than
    /// compiled twice.
    ///
    /// # Errors
    ///
    /// Returns an error if the auction plan or the integration registry is
    /// invalid.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_registrations(
        settings: &Settings,
        extra: &[crate::integrations::IntegrationBuilder],
    ) -> Result<Self, Report<TrustedServerError>> {
        let plan = Arc::new(crate::auction::compile_auction_plan_with(settings, extra)?);
        Self::with_plan_and_registrations(settings, plan, extra)
    }

    /// Build a registry from the built-in integrations, against one shared
    /// compiled auction plan.
    ///
    /// # Errors
    ///
    /// Returns an error if route registration fails due to duplicate routes or
    /// invalid paths, or when a registration carries a browser module whose
    /// declared SHA-256 does not match its source.
    pub fn with_plan(
        settings: &Settings,
        plan: Arc<AuctionPlan>,
    ) -> Result<Self, Report<TrustedServerError>> {
        Self::with_plan_and_registrations(settings, plan, &[])
    }

    /// Build a registry from the built-in integrations followed by `extra`,
    /// the builders an adapter or a vendor crate supplies, against one shared
    /// compiled auction plan.
    ///
    /// The plan-backed auction providers, Prebid then APS, register before the
    /// builders, so opening the builder table changes no existing hook order.
    /// Their ids are reserved for core whether or not they run, so an
    /// outside builder claiming either is refused the way two builders claiming
    /// one id are.
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
    pub fn with_plan_and_registrations(
        settings: &Settings,
        plan: Arc<AuctionPlan>,
        extra: &[crate::integrations::IntegrationBuilder],
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut inner = IntegrationRegistryInner::default();
        // What a builder registers from the auction plan goes first, so its
        // hooks run ahead of every module a section selects.
        let mut plan_registrations: Vec<IntegrationRegistration> = Vec::new();
        let mut registrations: Vec<IntegrationRegistration> = Vec::new();

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
            // Preparers are collected before the selection check, so an
            // integration can sanitize its own reserved query or cookie in a
            // deployment that does not run it.
            if let Some(prepare) = builder.prepare_request() {
                inner.request_preparers.push(prepare);
            }
            if let Some(finalize) = builder.finalize_response() {
                inner.response_finalizers.push(finalize);
            }
            // A registration from the plan is made whatever the sections
            // select, because the function decides from the plan and the
            // settings whether the module runs.
            if let Some(register) = builder.plan_registration()
                && let Some(registration) = register(settings, &plan)?
            {
                debug_assert_eq!(
                    registration.integration_id,
                    builder.id(),
                    "integration builder ID should match registration ID"
                );
                plan_registrations.push(registration);
            }

            // Only a builder whose module a section selects is built, so an
            // integration runs exactly when an operator selects it, whatever
            // its builder would otherwise make of the settings.
            let selected = builder
                .module_name()
                .is_some_and(|name| settings.selects_module(name));
            if let Some(name) = builder.module_name() {
                inner.builder_modules.push((name, selected));
            }
            if !selected {
                continue;
            }

            if let Some(registration) = builder.build(settings)? {
                debug_assert_eq!(
                    registration.integration_id,
                    builder.id(),
                    "integration builder ID should match registration ID"
                );
                registrations.push(registration);
            }
        }

        // Which names a section can select is only knowable here, where the
        // adapter's and a vendor crate's builders have been handed over, so
        // the selections are checked against them rather than in the
        // settings. Deploy validation deliberately does not make this check,
        // because a vendor crate the CLI never links may supply the name.
        check_section_selections(settings, extra)?;

        // The geo, Edge Cookie and device module names taken so far, with the
        // integration that supplies each.
        let mut claimed = Vec::new();
        for registration in plan_registrations.into_iter().chain(registrations) {
            inner
                .running_integration_ids
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
                .html_stream_processors
                .extend(registration.html_stream_processors);
            inner.head_injectors.extend(registration.head_injectors);
            inner.request_filters.extend(registration.request_filters);
            if let Some((name, module)) = registration.geo_module {
                claim_module_name(&mut claimed, "geo", name, registration.integration_id)?;
                inner.geo_modules.push((name, module));
            }
            if let Some((name, module)) = registration.ec_module {
                claim_module_name(
                    &mut claimed,
                    "Edge Cookie",
                    name,
                    registration.integration_id,
                )?;
                // Core checks the selected module by its own id, so a module
                // declared under another name would be selected here and
                // refused there.
                if module.id() != name {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "integration `{}` declares an Edge Cookie module under `{name}`, \
                             and the module's own id is `{}`. The two must be the same name",
                            registration.integration_id,
                            module.id()
                        ),
                    }));
                }
                inner.ec_modules.push((name, module));
            }
            if let Some((name, module)) = registration.device_module {
                claim_module_name(&mut claimed, "device", name, registration.integration_id)?;
                inner.device_modules.push((name, module));
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

        // A client-cycle Edge Cookie module ships a page script that posts its
        // result to the resolve endpoint. The script rides the tsjs bundle, so
        // include its module when that module is selected. The same module
        // list drives both the served bundle and the injected `<script>` hash,
        // so they stay consistent.
        if settings
            .ec
            .module
            .as_ref()
            .is_some_and(|selection| selection.key() == crate::ec::module::CLIENT_FIXED_MODULE_KEY)
        {
            inner.extra_js_module_ids.push("ec_client_fixed");
        }
        let geo_module = resolve_geo_module(settings, &inner)?;
        inner.ec_module = resolve_ec_module(settings, &inner);
        inner.device_module = resolve_device_module(settings, &inner)?;
        inner.geo_module = geo_module;

        Ok(Self {
            inner: Arc::new(inner),
            plan: Some(plan),
        })
    }

    /// Return whether this registry and another consumer share the same plan allocation.
    #[must_use]
    pub fn shares_plan(&self, plan: &Arc<AuctionPlan>) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|owned| Arc::ptr_eq(owned, plan))
    }

    /// The geo module `[geo] module` selected, or `None` when the selector
    /// is `platform` and the adapter's own host lookup stands. Unset and `none`
    /// both resolve the disabled module.
    #[must_use]
    pub fn geo_module(&self) -> Option<Arc<dyn PlatformGeo>> {
        self.inner.geo_module.clone()
    }

    /// The Edge Cookie module `[ec] module` selected from a module, or
    /// `None` when the selector names a module built into core or nothing.
    #[must_use]
    pub fn ec_module(&self) -> Option<Arc<dyn EdgeCookieModule>> {
        self.inner.ec_module.clone()
    }

    /// The device module `[device] module` selected from a module, or
    /// `None` when the selector names a module built into core or nothing.
    #[must_use]
    pub fn device_module(&self) -> Option<Arc<dyn DeviceModule>> {
        self.inner.device_module.clone()
    }

    /// Every integration id the registry was built from, named or not, in
    /// registration order. A registry test enumerates this to check every
    /// builder was considered, while the deploy validation test iterates the
    /// builder lists themselves rather than this method.
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
    /// Preparers run whether or not their integration runs, so an
    /// integration can sanitize its own reserved query or cookie in a
    /// deployment that has it switched off. They run once for a request, so
    /// a caller further along the request path can call this again and be
    /// sure the request is prepared without running any of them twice.
    ///
    /// # Errors
    ///
    /// Returns the first preparer's error.
    pub fn prepare_request(
        &self,
        settings: &Settings,
        request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        if request.extensions().get::<RequestPrepared>().is_some() {
            return Ok(());
        }
        for prepare in &self.inner.request_preparers {
            prepare(settings, request)?;
        }
        request.extensions_mut().insert(RequestPrepared);
        Ok(())
    }

    /// Marks `request` as one the preparers have run for, for a test that
    /// hands a handler the request an adapter would have prepared.
    #[cfg(test)]
    pub(crate) fn mark_prepared_for_tests(request: &mut Request<EdgeBody>) {
        request.extensions_mut().insert(RequestPrepared);
    }

    /// Runs every registered integration's response finalizer, in
    /// registration order, on the response the page path is about to return.
    ///
    /// Each is handed what the module's request hooks left for this request,
    /// and one with nothing left for it has nothing to do.
    pub fn finalize_response(
        &self,
        request_state: &IntegrationRequestState,
        response: &mut Response<EdgeBody>,
    ) {
        for finalize in &self.inner.response_finalizers {
            finalize(request_state, response);
        }
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
                if let Err(err) = ec_context.generate_if_needed(settings, kv, services).await {
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

    /// Expose registered per-document HTML stream processor factories.
    #[must_use]
    pub fn html_stream_processor_factories(
        &self,
    ) -> Vec<Arc<dyn IntegrationHtmlStreamProcessorFactory>> {
        self.inner.html_stream_processors.clone()
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

    /// Collect HTML snippets for insertion straight after the main script
    /// bundle.
    #[must_use]
    pub fn after_bundle_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        self.inner
            .head_injectors
            .iter()
            .flat_map(|injector| injector.after_bundle_inserts(ctx))
            .collect()
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

        for integration_id in &self.inner.running_integration_ids {
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

    /// Return whether an integration runs in this registry.
    #[must_use]
    pub fn integration_runs(&self, integration_id: &str) -> bool {
        self.inner.running_integration_ids.contains(&integration_id)
    }

    /// Return JS module IDs that should be included in the tsjs bundle.
    ///
    /// Always includes JS-only modules with no Rust-side registration.
    /// Includes an integration that runs only when a browser module serves its
    /// id, either compiled into `trusted-server-js` or carried by the
    /// registration, and excludes modules served standalone only.
    #[must_use]
    pub fn js_module_ids(&self) -> Vec<&'static str> {
        // Core JS-only modules that do not have a Rust-side registration.
        const JS_ALWAYS: &[&str] = &["creative"];

        let mut ids: Vec<&'static str> = JS_ALWAYS.to_vec();

        for id in &self.inner.running_integration_ids {
            if self.js_part(id).is_some()
                && !self.inner.standalone_js_ids.contains(id)
                && !ids.contains(id)
            {
                ids.push(*id);
            }
        }

        // Modules not tied to a registered integration, for example the
        // client-cycle module's page script.
        for id in &self.inner.extra_js_module_ids {
            if !ids.contains(id) {
                ids.push(id);
            }
        }

        ids
    }

    /// The module part for one id: the registration that carries it, else
    /// the compile-time module, for an integration that runs or an always-on
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
        if id != "core" && id != "creative" && !self.integration_runs(id) {
            return None;
        }
        crate::tsjs_bundle::JsModulePart::compile_time(id)
    }

    /// Ids of modules that run and are served standalone only. Only a
    /// registration a section selects reaches the construction
    /// loop, so every id here runs.
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
    /// for cache hashes. Each id appears once.
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
    /// that actually run. Returns an empty vec when no deferred integration
    /// runs.
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
            plan: None,
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
                running_integration_ids: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
                html_stream_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                response_finalizers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_modules: Vec::new(),
                ec_modules: Vec::new(),
                device_modules: Vec::new(),
                builder_modules: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
            }),
            plan: None,
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
                running_integration_ids: Vec::new(),
                html_rewriters: attribute_rewriters,
                script_rewriters,
                html_stream_processors: Vec::new(),
                head_injectors,
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                response_finalizers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_modules: Vec::new(),
                ec_modules: Vec::new(),
                device_modules: Vec::new(),
                builder_modules: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
            }),
            plan: None,
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
                running_integration_ids: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_stream_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters,
                request_preparers: Vec::new(),
                response_finalizers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_modules: Vec::new(),
                ec_modules: Vec::new(),
                device_modules: Vec::new(),
                builder_modules: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
            }),
            plan: None,
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
                running_integration_ids: Vec::new(),
                html_rewriters: Vec::new(),
                script_rewriters: Vec::new(),
                html_stream_processors: Vec::new(),
                head_injectors: Vec::new(),
                request_filters: Vec::new(),
                request_preparers: Vec::new(),
                response_finalizers: Vec::new(),
                deferred_js_ids: Vec::new(),
                disabled_js_ids: Vec::new(),
                extra_js_module_ids: Vec::new(),
                standalone_js_ids: Vec::new(),
                carried_js: Vec::new(),
                geo_modules: Vec::new(),
                ec_modules: Vec::new(),
                device_modules: Vec::new(),
                builder_modules: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
            }),
            plan: None,
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

    /// Validates nothing and reports the integration as named.
    pub(crate) fn validate_nothing(
        _settings: &Settings,
    ) -> Result<bool, Report<TrustedServerError>> {
        Ok(true)
    }

    /// A stand-in for an integration that acts on one request, for core's
    /// own tests of the page path.
    ///
    /// Its preparer runs for every request, as any preparer does, and strips
    /// the query `fixture_request=1`. Where that query arrived on a document
    /// navigation it also leaves a mark for the page path. It strips the
    /// cookie it reserves too, and writes the Cookie header out again as it
    /// does, the way a module that reserves a cookie does. Selected, its
    /// request filter leaves the same mark on a request that carries its
    /// header, which is the route a module that decides in its filter takes.
    /// Selected, the
    /// stand-in's head injector reads the mark from the document's state and
    /// writes one script at the start of `<head>` and the tag of its
    /// standalone module after the bundle. Its response finalizer sets a
    /// cookie on the response to a marked request, and its builder declares
    /// that it reads the auction token.
    pub(crate) mod request_fixture {
        use std::sync::Arc;

        use edgezero_core::body::Body as EdgeBody;
        use error_stack::Report;
        use http::{HeaderValue, Method, Request, Response, Uri, header};

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{
            CarriedJsModule, IntegrationHeadInjector, IntegrationHtmlContext,
            IntegrationRegistration, IntegrationRequestFilter, IntegrationRequestState,
            RequestFilterDecision, RequestFilterEffects, RequestFilterInput,
        };
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::settings::Settings;
        use crate::tsjs_bundle::JsModulePart;

        /// The integration id the stand-in registers under.
        pub(crate) const ID: &str = "request_fixture";
        /// The name a test's settings select the stand-in by, in `[testing]`.
        pub(crate) const MODULE: &str = "testing.request-fixture";
        /// The query a navigation asks the stand-in to act with.
        pub(crate) const QUERY: &str = "fixture_request=1";
        /// What the stand-in's head insert sets for a marked request.
        pub(crate) const HEAD_FLAG: &str = "window.__ts_request_fixture=true;";
        /// The file of the stand-in's standalone module, which the document
        /// of a marked request loads after the bundle.
        pub(crate) const MODULE_FILE: &str = "tsjs-request_fixture.min.js";
        /// The cookie the finalizer sets on the response to a marked request.
        pub(crate) const COOKIE: &str = "ts-request-fixture=1; Path=/";
        /// The name of that cookie, which the preparer strips from a request.
        pub(crate) const COOKIE_NAME: &str = "ts-request-fixture";
        /// The request header that has the stand-in's request filter leave
        /// the mark.
        pub(crate) const FILTER_HEADER: &str = "x-ts-request-fixture";

        const JS: &str = "(function(){window.__ts_request_fixture_loaded=1;})();";
        // SHA-256 of JS, hex. The registry refuses a literal that is not.
        const JS_SHA256: &str = "7160e730ce9301ed134c84fa13605d7cd979cf929679f1c3176ea88b1989477e";

        /// The builder core's test build lists beside its own.
        pub(crate) const BUILDER: IntegrationBuilder =
            IntegrationBuilder::new(ID, CORE_SOURCE, register, validate)
                .with_module_name(MODULE)
                .with_request_preparer(prepare)
                .with_response_finalizer(finalize)
                .with_auction_token();

        #[derive(Debug, serde::Deserialize, validator::Validate)]
        #[serde(deny_unknown_fields)]
        struct FixtureSettings {}

        impl crate::settings::IntegrationConfig for FixtureSettings {}

        /// What the preparer leaves for the page path.
        #[derive(Debug, Clone, Copy)]
        pub(crate) struct Mark;

        /// Leaves the mark on `request`, as the preparer does for the query.
        pub(crate) fn mark(request: &mut Request<EdgeBody>) {
            IntegrationRequestState::insert(request, ID, Mark);
        }

        /// The request state of a marked request.
        pub(crate) fn marked() -> IntegrationRequestState {
            let mut state = IntegrationRequestState::default();
            state.set(ID, Mark);
            state
        }

        /// Strips the reserved cookie, which means writing the header again
        /// from the pairs that can be read. A request without the cookie is
        /// left exactly as it arrived.
        fn strip_reserved_cookie(request: &mut Request<EdgeBody>) {
            let is_reserved =
                |pair: &str| pair.split('=').next().map(str::trim) == Some(COOKIE_NAME);
            let pairs = request
                .headers()
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(';'))
                .map(str::trim)
                .filter(|pair| !pair.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !pairs.iter().any(|pair| is_reserved(pair)) {
                return;
            }
            let kept = pairs
                .into_iter()
                .filter(|pair| !is_reserved(pair))
                .collect::<Vec<_>>();
            request.headers_mut().remove(header::COOKIE);
            if !kept.is_empty() {
                request.headers_mut().insert(
                    header::COOKIE,
                    HeaderValue::from_str(&kept.join("; "))
                        .expect("should keep already valid cookie pairs"),
                );
            }
        }

        fn prepare(
            _settings: &Settings,
            request: &mut Request<EdgeBody>,
        ) -> Result<(), Report<TrustedServerError>> {
            strip_reserved_cookie(request);
            let query = request.uri().query().unwrap_or_default();
            if !query.split('&').any(|pair| pair == QUERY) {
                return Ok(());
            }
            let retained = query
                .split('&')
                .filter(|pair| *pair != QUERY)
                .collect::<Vec<_>>()
                .join("&");
            let mut path_and_query = request.uri().path().to_owned();
            if !retained.is_empty() {
                path_and_query.push('?');
                path_and_query.push_str(&retained);
            }
            let mut parts = request.uri().clone().into_parts();
            parts.path_and_query = Some(
                path_and_query
                    .parse()
                    .expect("should keep a valid path and query"),
            );
            *request.uri_mut() = Uri::from_parts(parts).expect("should keep a valid URI");

            if request.method() == Method::GET && crate::http_util::is_navigation_request(request) {
                mark(request);
            }
            Ok(())
        }

        fn finalize(request_state: &IntegrationRequestState, response: &mut Response<EdgeBody>) {
            if request_state.get::<Mark>(ID).is_some() {
                response
                    .headers_mut()
                    .append(header::SET_COOKIE, HeaderValue::from_static(COOKIE));
            }
        }

        struct Filter;

        #[async_trait::async_trait(?Send)]
        impl IntegrationRequestFilter for Filter {
            fn integration_id(&self) -> &'static str {
                ID
            }

            async fn filter_request(
                &self,
                input: RequestFilterInput<'_>,
            ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
                if input.request.headers().contains_key(FILTER_HEADER) {
                    mark(input.request);
                }
                Ok(RequestFilterDecision::Continue(
                    RequestFilterEffects::default(),
                ))
            }
        }

        struct Head;

        impl Head {
            fn marked(ctx: &IntegrationHtmlContext<'_>) -> bool {
                ctx.document_state.get::<Mark>(ID).is_some()
            }
        }

        impl IntegrationHeadInjector for Head {
            fn integration_id(&self) -> &'static str {
                ID
            }

            fn head_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
                if !Self::marked(ctx) {
                    return Vec::new();
                }
                vec![format!("<script>{HEAD_FLAG}</script>")]
            }

            fn after_bundle_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
                if !Self::marked(ctx) {
                    return Vec::new();
                }
                let module = JsModulePart {
                    id: ID,
                    source: JS,
                    sha256: JS_SHA256,
                };
                vec![format!(
                    "<script src=\"{}\"></script>",
                    crate::tsjs::tsjs_single_module_script_src(&module)
                )]
            }
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            let Some(_config) = settings.module_config::<FixtureSettings>(MODULE)? else {
                return Ok(None);
            };
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .with_js_module(CarriedJsModule {
                        source: JS,
                        sha256: JS_SHA256,
                    })
                    .with_standalone_js()
                    .with_head_injector(Arc::new(Head))
                    .with_request_filter(Arc::new(Filter))
                    .build(),
            ))
        }

        fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
            settings
                .module_config::<FixtureSettings>(MODULE)
                .map(|config| config.is_some())
        }
    }

    /// A stand-in for an integration that tags a page, for core's own tests
    /// of the HTML processor and the JavaScript asset proxy.
    ///
    /// Selected, it inserts one script at the start of `<head>` and rewrites
    /// the address of its own script to a first-party path. With
    /// `mark_bundle` set it also asks for an attribute on the publisher
    /// bundle tag.
    pub(crate) mod tag_fixture {
        use std::sync::Arc;

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{
            AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
            IntegrationHeadInjector, IntegrationHtmlContext, IntegrationRegistration,
        };
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::settings::Settings;

        /// The integration id the stand-in registers under.
        pub(crate) const ID: &str = "tag_fixture";
        /// The name a test's settings select the stand-in by, in `[testing]`.
        pub(crate) const MODULE: &str = "testing.tag-fixture";
        /// The script the stand-in's vendor would serve.
        pub(crate) const SCRIPT_URL: &str = "https://cdn.tag-fixture.example/sdk.js";
        /// The first-party path the stand-in rewrites that script to.
        pub(crate) const FIRST_PARTY_SCRIPT: &str = "/integrations/tag_fixture/script";
        /// What the stand-in's head insert sets, so a test can find it.
        pub(crate) const HEAD_FLAG: &str = "window.__ts_tag_fixture=true;";
        /// The attribute the stand-in asks for on the publisher bundle tag.
        pub(crate) const BUNDLE_ATTRIBUTE: &str = "data-ts-tag-fixture";

        /// The builder core's test build lists beside its own.
        pub(crate) const BUILDER: IntegrationBuilder =
            IntegrationBuilder::new(ID, CORE_SOURCE, register, validate).with_module_name(MODULE);

        #[derive(Debug, serde::Deserialize, validator::Validate)]
        #[serde(deny_unknown_fields)]
        struct FixtureSettings {
            #[serde(default)]
            mark_bundle: bool,
        }

        impl crate::settings::IntegrationConfig for FixtureSettings {}

        struct Tag {
            mark_bundle: bool,
        }

        impl IntegrationHeadInjector for Tag {
            fn integration_id(&self) -> &'static str {
                ID
            }

            fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
                vec![format!("<script>{HEAD_FLAG}</script>")]
            }

            fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
                if self.mark_bundle {
                    vec![(BUNDLE_ATTRIBUTE, "true")]
                } else {
                    Vec::new()
                }
            }
        }

        impl IntegrationAttributeRewriter for Tag {
            fn integration_id(&self) -> &'static str {
                ID
            }

            fn handles_attribute(&self, attribute: &str) -> bool {
                attribute == "src"
            }

            fn rewrite(
                &self,
                _attr_name: &str,
                attr_value: &str,
                _ctx: &IntegrationAttributeContext<'_>,
            ) -> AttributeRewriteAction {
                if attr_value == SCRIPT_URL {
                    AttributeRewriteAction::Replace(FIRST_PARTY_SCRIPT.to_owned())
                } else {
                    AttributeRewriteAction::Keep
                }
            }
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            let Some(config) = settings.module_config::<FixtureSettings>(MODULE)? else {
                return Ok(None);
            };
            let tag = Arc::new(Tag {
                mark_bundle: config.mark_bundle,
            });
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .without_js()
                    .with_attribute_rewriter(tag.clone())
                    .with_head_injector(tag)
                    .build(),
            ))
        }

        fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
            settings
                .module_config::<FixtureSettings>(MODULE)
                .map(|config| config.is_some())
        }
    }

    /// A stand-in for an integration that rewrites script payloads in two
    /// passes, for core's own tests of the page pipeline.
    ///
    /// The script rewriter swaps the payload of each `fixture_payload("...")`
    /// call for a placeholder that carries a namespace made per document, and
    /// keeps the payload in the document state. The stream processor swaps
    /// each placeholder back for its payload with the origin host rewritten.
    /// A payload pushed with `fixture_payload_open` leaves its group
    /// unresolved, and the processor holds its output from that placeholder
    /// on until a `fixture_payload_close` arrives.
    pub(crate) mod payload_fixture {
        use std::io;
        use std::sync::{Arc, Mutex, PoisonError};

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{
            IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory,
            IntegrationRegistration, IntegrationScriptContext, IntegrationScriptRewriter,
            ScriptRewriteAction, ScriptTextAccumulator,
        };
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::settings::Settings;
        use crate::streaming_processor::StreamProcessor;

        /// The integration id the stand-in registers under.
        pub(crate) const ID: &str = "payload_fixture";
        /// The name a test's settings select the stand-in by, in `[testing]`.
        pub(crate) const MODULE: &str = "testing.payload-fixture";
        /// What every placeholder starts with, so a test can check that none
        /// reaches a reader.
        pub(crate) const PLACEHOLDER_PREFIX: &str = "__ts_fixture_";
        const PLACEHOLDER_END: &str = "__";

        /// The builder core's test build lists beside its own.
        pub(crate) const BUILDER: IntegrationBuilder =
            IntegrationBuilder::new(ID, CORE_SOURCE, register, validate).with_module_name(MODULE);

        struct Payload {
            placeholder: String,
            original: String,
            /// Whether the group this payload opened is still waiting for its
            /// close.
            unresolved: bool,
        }

        /// What one document's script rewriter has captured so far.
        struct Captured {
            namespace: String,
            payloads: Vec<Payload>,
        }

        impl Default for Captured {
            fn default() -> Self {
                Self {
                    namespace: uuid::Uuid::new_v4().simple().to_string(),
                    payloads: Vec::new(),
                }
            }
        }

        fn captured(
            state: &crate::integrations::registry::IntegrationDocumentState,
        ) -> Arc<Mutex<Captured>> {
            state.get_or_insert_with(ID, || Mutex::new(Captured::default()))
        }

        struct ScriptRewriter;

        impl ScriptRewriter {
            /// Swaps the payload of a whole script for a placeholder, or
            /// leaves a script that pushes no payload as it is.
            fn rewrite_whole(script: &str, ctx: &IntegrationScriptContext<'_>) -> Option<String> {
                let (call, opens, closes) = [
                    ("fixture_payload_open(\"", true, false),
                    ("fixture_payload_close(\"", false, true),
                    ("fixture_payload(\"", false, false),
                ]
                .into_iter()
                .find(|(call, _, _)| script.contains(*call))?;
                let start = script.find(call)? + call.len();
                let end = script.rfind("\")")?;
                if end < start {
                    return None;
                }

                let shared = captured(ctx.document_state);
                let mut captured = shared.lock().unwrap_or_else(PoisonError::into_inner);
                let placeholder = format!(
                    "{PLACEHOLDER_PREFIX}{}_{}{PLACEHOLDER_END}",
                    captured.namespace,
                    captured.payloads.len()
                );
                if closes {
                    for payload in &mut captured.payloads {
                        payload.unresolved = false;
                    }
                }
                captured.payloads.push(Payload {
                    placeholder: placeholder.clone(),
                    original: script[start..end].to_owned(),
                    unresolved: opens,
                });

                let mut rewritten = script.to_owned();
                rewritten.replace_range(start..end, &placeholder);
                Some(rewritten)
            }
        }

        impl IntegrationScriptRewriter for ScriptRewriter {
            fn integration_id(&self) -> &'static str {
                ID
            }

            fn selector(&self) -> &'static str {
                "script"
            }

            fn rewrite(
                &self,
                content: &str,
                ctx: &IntegrationScriptContext<'_>,
            ) -> ScriptRewriteAction {
                let accumulator = ctx
                    .document_state
                    .get_or_insert_with(ID, ScriptTextAccumulator::default);
                let mut buffer = accumulator.buffer();
                let claimed = !buffer.is_empty() || content.contains("fixture_payload");
                if !claimed {
                    return ScriptRewriteAction::Keep;
                }
                buffer.push_str(content);
                if !ctx.is_last_in_text_node {
                    return ScriptRewriteAction::RemoveNode;
                }
                let script = std::mem::take(&mut *buffer);
                let rewritten = Self::rewrite_whole(&script, ctx).unwrap_or(script);
                ScriptRewriteAction::replace(rewritten)
            }
        }

        struct StreamFactory;

        impl IntegrationHtmlStreamProcessorFactory for StreamFactory {
            fn integration_id(&self) -> &'static str {
                ID
            }

            fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
                Box::new(Processor {
                    context,
                    held: Vec::new(),
                })
            }
        }

        struct Processor {
            context: IntegrationHtmlStreamContext,
            /// Output not yet released, because it ends inside a placeholder
            /// or starts at one whose group is unresolved.
            held: Vec<u8>,
        }

        fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
            haystack
                .get(from..)?
                .windows(needle.len())
                .position(|window| window == needle)
                .map(|at| at + from)
        }

        impl StreamProcessor for Processor {
            fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
                self.held.extend_from_slice(chunk);
                let shared = captured(&self.context.document_state);
                let captured = shared.lock().unwrap_or_else(PoisonError::into_inner);
                let prefix = PLACEHOLDER_PREFIX.as_bytes();
                let mut out = Vec::with_capacity(self.held.len());
                let mut at = 0;

                while let Some(start) = find(&self.held, prefix, at) {
                    let Some(end) =
                        find(&self.held, PLACEHOLDER_END.as_bytes(), start + prefix.len())
                    else {
                        // The placeholder runs past this chunk.
                        out.extend_from_slice(&self.held[at..start]);
                        at = start;
                        break;
                    };
                    let end = end + PLACEHOLDER_END.len();
                    let found = std::str::from_utf8(&self.held[start..end])
                        .ok()
                        .and_then(|text| {
                            captured
                                .payloads
                                .iter()
                                .find(|payload| payload.placeholder == text)
                        });
                    match found {
                        Some(payload) if payload.unresolved && !is_last => {
                            out.extend_from_slice(&self.held[at..start]);
                            at = start;
                            break;
                        }
                        Some(payload) => {
                            out.extend_from_slice(&self.held[at..start]);
                            out.extend_from_slice(
                                payload
                                    .original
                                    .replace(&self.context.origin_host, &self.context.request_host)
                                    .as_bytes(),
                            );
                            at = end;
                        }
                        None => {
                            out.extend_from_slice(&self.held[at..end]);
                            at = end;
                        }
                    }
                }

                let stopped_at_placeholder = find(&self.held, prefix, at) == Some(at);
                if is_last {
                    out.extend_from_slice(&self.held[at..]);
                    self.held.clear();
                } else if stopped_at_placeholder {
                    self.held.drain(..at);
                } else {
                    // Keep back a tail that could be the start of a
                    // placeholder split across chunks.
                    let rest = &self.held[at..];
                    let keep = (1..prefix.len().min(rest.len() + 1))
                        .rev()
                        .find(|len| rest.ends_with(&prefix[..*len]))
                        .unwrap_or(0);
                    out.extend_from_slice(&rest[..rest.len() - keep]);
                    let tail = rest[rest.len() - keep..].to_vec();
                    self.held = tail;
                }
                Ok(out)
            }
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            if settings.module_config::<FixtureSettings>(MODULE)?.is_none() {
                return Ok(None);
            }
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .with_script_rewriter(Arc::new(ScriptRewriter))
                    .with_html_stream_processor(Arc::new(StreamFactory))
                    .build(),
            ))
        }

        /// The stand-in takes no settings, and refuses one it does not know
        /// as any module does.
        #[derive(Debug, serde::Deserialize, validator::Validate)]
        #[serde(deny_unknown_fields)]
        struct FixtureSettings {}

        impl crate::settings::IntegrationConfig for FixtureSettings {}

        fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
            settings
                .module_config::<FixtureSettings>(MODULE)
                .map(|config| config.is_some())
        }
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
                request_headers: vec![HeaderMutation::set("x-probe-isbot", "1")],
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
            self.seen
                .lock()
                .expect("should lock the recorded permission state")
                .clone()
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
                Some(input.permissions.cloned());
            Ok(RequestFilterDecision::Continue(
                RequestFilterEffects::default(),
            ))
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

    struct CountingStreamFactory(&'static str);

    impl IntegrationHtmlStreamProcessorFactory for CountingStreamFactory {
        fn integration_id(&self) -> &'static str {
            self.0
        }

        fn create(&self, _context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
            struct CountingStreamProcessor(usize);

            impl StreamProcessor for CountingStreamProcessor {
                fn process_chunk(
                    &mut self,
                    chunk: &[u8],
                    _is_last: bool,
                ) -> std::io::Result<Vec<u8>> {
                    self.0 += 1;
                    let mut output = self.0.to_string().into_bytes();
                    output.extend_from_slice(chunk);
                    Ok(output)
                }
            }

            Box::new(CountingStreamProcessor(0))
        }
    }

    #[test]
    fn html_stream_factories_preserve_order_and_create_isolated_sessions() {
        let registration = IntegrationRegistration::builder("test")
            .with_html_stream_processor(Arc::new(CountingStreamFactory("first")))
            .with_html_stream_processor(Arc::new(CountingStreamFactory("second")))
            .build();
        let identifiers: Vec<_> = registration
            .html_stream_processors
            .iter()
            .map(|factory| factory.integration_id())
            .collect();
        assert_eq!(
            identifiers,
            ["first", "second"],
            "should preserve factory registration order",
        );

        let context = IntegrationHtmlStreamContext {
            request_host: "proxy.example.com".to_owned(),
            request_scheme: "https".to_owned(),
            origin_host: "origin.example.com".to_owned(),
            document_state: IntegrationDocumentState::default(),
        };
        let factory = &registration.html_stream_processors[0];
        let mut first = factory.create(context.clone());
        let mut second = factory.create(context);

        assert_eq!(
            first
                .process_chunk(b"a", false)
                .expect("should process first session"),
            b"1a",
            "should initialize the first session counter",
        );
        assert_eq!(
            second
                .process_chunk(b"b", true)
                .expect("should process second session"),
            b"1b",
            "should initialize an independent second session counter",
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
                .get("x-probe-isbot")
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "should apply a filter's request enrichment before routing"
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
            .insert_module_config(
                "auction",
                "auction.prebid",
                &serde_json::json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "debug": false
                }),
            )
            .expect("should insert prebid config");

        let registry = IntegrationRegistry::with_plan(
            &settings_with_prebid,
            Arc::new(
                crate::auction::compile_auction_plan(&settings_with_prebid)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

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
            "should not include Sourcepoint unless it is named"
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
    fn js_module_ids_skip_named_integrations_without_generated_js_module() {
        let settings = settings_naming("testing.probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should create registry");
        let all = registry.js_module_ids();

        assert!(
            !all.contains(&"probe"),
            "should not include named integrations without generated JS modules"
        );

        let metadata = registry.registered_integrations();
        assert!(
            metadata.iter().any(|integration| integration.id == "probe"),
            "should still register named Rust-only integrations"
        );
    }

    #[test]
    fn js_module_ids_include_client_fixed_when_module_selected() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.ec.module = Some(crate::ec::module::EcModuleSelection::from(
            crate::ec::module::CLIENT_FIXED_MODULE_KEY,
        ));
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        assert!(
            registry
                .js_module_ids_immediate()
                .contains(&"ec_client_fixed"),
            "selecting the `client_fixed` module should inject its demo page script"
        );
    }

    #[test]
    fn js_module_ids_exclude_client_fixed_without_module() {
        let registry =
            IntegrationRegistry::new(&crate::test_support::tests::create_test_settings())
                .expect("should create registry");

        assert!(
            !registry
                .js_module_ids_immediate()
                .contains(&"ec_client_fixed"),
            "the demo page script should not ship unless the `client_fixed` module is selected"
        );
    }

    #[test]
    fn js_module_ids_deferred_empty_when_prebid_is_not_named() {
        let mut settings = crate::test_support::tests::create_test_settings();
        // The shared fixture names prebid, and this asks what the registry
        // serves when it does not.
        settings.auction.modules.clear();

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

        let deferred = registry.js_module_ids_deferred();
        assert!(
            deferred.is_empty(),
            "should have no deferred IDs when prebid is not named"
        );
    }

    #[test]
    fn js_module_ids_defer_prebid_shim_when_external_bundle_is_configured() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &serde_json::json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js"
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

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
            .insert_module_config(
                "auction",
                "auction.prebid",
                &serde_json::json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "debug": false
                }),
            )
            .expect("should insert prebid config");

        let registry = IntegrationRegistry::with_plan(
            &settings_with_prebid,
            Arc::new(
                crate::auction::compile_auction_plan(&settings_with_prebid)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

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

    /// Claims the id of the JavaScript asset proxy, which is core's own.
    fn duplicate_core_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder(
                crate::integrations::js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID,
            )
            .build(),
        ))
    }

    /// The shared fixture with the module `name` selected in its section, so a
    /// test builder is built the way any integration an operator names is.
    fn settings_naming(name: &str) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        let section = name.split_once('.').map_or("testing", |(folder, _)| folder);
        settings.select_module(section, name);
        settings
    }

    #[test]
    fn with_registrations_adds_an_external_builder_after_the_built_ins() {
        let settings = settings_naming("probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with an external builder");

        assert!(
            registry.integration_runs("probe"),
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
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [crate::integrations::IntegrationBuilder::new(
            crate::integrations::js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID,
            "seam-probe",
            duplicate_core_registration,
            validate_nothing,
        )
        .with_module_name("testing.duplicate")];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a duplicate integration id");

        let message = error.to_string();
        assert!(
            message.contains(crate::integrations::js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID)
                && message.contains("trusted-server-core")
                && message.contains("seam-probe"),
            "error should name the id and both sources: {message}"
        );
    }

    /// Prebid and APS are not builders, because the auction plan registers
    /// them, yet an outside builder claiming either id is still refused, naming
    /// both sources, whether or not the plan-backed integration runs.
    #[test]
    fn with_registrations_rejects_an_outside_builder_claiming_a_plan_backed_id() {
        fn register_nothing(
            _settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            Ok(None)
        }

        let settings = crate::test_support::tests::create_test_settings();
        for id in [
            crate::integrations::prebid::PREBID_INTEGRATION_ID,
            crate::integrations::aps::APS_INTEGRATION_ID,
        ] {
            let extra = [crate::integrations::IntegrationBuilder::new(
                id,
                "seam-probe",
                register_nothing,
                validate_nothing,
            )];

            let Err(error) = IntegrationRegistry::with_registrations(&settings, &extra) else {
                panic!("should refuse an outside builder claiming `{id}`");
            };

            let message = error.to_string();
            assert!(
                message.contains(id)
                    && message.contains("trusted-server-core")
                    && message.contains("seam-probe"),
                "error should name `{id}` and both sources: {message}"
            );
        }
    }

    fn enable_prebid(settings: &mut Settings) {
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &serde_json::json!({
                    "external_bundle_url": "https://assets.example.com/prebid/trusted-prebid.js",
                }),
            )
            .expect("should insert prebid config");
    }

    /// Selects core's stand-in, which registers a standalone browser module.
    fn enable_standalone_module(settings: &mut Settings) {
        settings.select_module("testing", test_support::request_fixture::MODULE);
    }

    fn carried_probe_builder() -> crate::integrations::IntegrationBuilder {
        crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            carried_probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")
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
        let settings = settings_naming("probe");
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
        let settings = settings_naming("probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            lying_probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

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
        // without-JS check runs first. No built-in integration can stand in, because
        // the only `without_js` built-in with a Rust registration, `aps`, has
        // no compile-time module either.
        let settings = settings_naming("probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            disabled_carried_probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");

        assert!(
            registry.integration_runs("probe"),
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
        enable_standalone_module(&mut settings);

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");

        assert!(
            !registry
                .js_module_ids()
                .contains(&test_support::request_fixture::ID),
            "should keep a standalone module out of the bundle module ids"
        );
        assert!(
            registry
                .js_part(test_support::request_fixture::ID)
                .is_some(),
            "should serve the standalone module as a part"
        );
        assert_eq!(
            registry.js_standalone_ids(),
            vec![test_support::request_fixture::ID],
            "should list the standalone module that runs"
        );
        assert!(
            registry.js_part("lockr").is_none(),
            "should not serve a module for an integration that does not run"
        );
    }

    #[test]
    fn js_parts_all_covers_bundle_deferred_and_standalone_modules() {
        let mut settings = settings_naming("probe");
        enable_prebid(&mut settings);
        enable_standalone_module(&mut settings);
        let extra = [carried_probe_builder()];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");

        let ids = registry
            .js_parts_all()
            .into_iter()
            .map(|part| part.id)
            .collect::<Vec<_>>();
        for expected in [
            "core",
            "creative",
            "probe",
            "prebid",
            test_support::request_fixture::ID,
        ] {
            assert_eq!(
                ids.iter().filter(|id| **id == expected).count(),
                1,
                "should list `{expected}` exactly once in {ids:?}"
            );
        }
    }

    /// A builder function for an integration that never registers, so its
    /// preparer is the only thing the registry can take from it.
    fn never_registering_builder(
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
    fn prepare_request_runs_every_preparer_in_registration_order_named_or_not() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-first",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-first")
            .with_request_preparer(record_first_preparer),
            crate::integrations::IntegrationBuilder::new(
                "probe-second",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-second")
            .with_request_preparer(record_second_preparer),
        ];
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with request preparers");
        assert!(
            !registry.integration_runs("probe-first"),
            "should leave the first probe integration unnamed, so it does not run"
        );
        assert!(
            !registry.integration_runs("probe-second"),
            "should leave the second probe integration unnamed, so it does not run"
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
            "should run both preparers in registration order even though neither integration runs"
        );
    }

    #[test]
    fn prepare_request_surfaces_the_first_preparer_error() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-failing",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-failing")
            .with_request_preparer(failing_preparer),
            crate::integrations::IntegrationBuilder::new(
                "probe-after-failure",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-after-failure")
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

    /// Writes one fixed insert, so a test can read the order hooks ran in.
    struct FixedHeadInsert {
        id: &'static str,
        insert: &'static str,
    }

    impl IntegrationHeadInjector for FixedHeadInsert {
        fn integration_id(&self) -> &'static str {
            self.id
        }

        fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
            vec![self.insert.to_owned()]
        }
    }

    const SECTION_INSERT: &str = "<!--from the section-->";
    const PLAN_INSERT: &str = "<!--from the plan-->";

    fn section_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("probe-section")
                .with_head_injector(Arc::new(FixedHeadInsert {
                    id: "probe-section",
                    insert: SECTION_INSERT,
                }))
                .build(),
        ))
    }

    /// Registers when the plan's auction is enabled, whatever the sections
    /// select.
    fn plan_probe_registration(
        _settings: &Settings,
        plan: &AuctionPlan,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(plan.enabled().then(|| {
            IntegrationRegistration::builder("probe-plan")
                .with_head_injector(Arc::new(FixedHeadInsert {
                    id: "probe-plan",
                    insert: PLAN_INSERT,
                }))
                .build()
        }))
    }

    #[test]
    fn a_registration_from_the_plan_runs_unselected_and_ahead_of_a_section_s_module() {
        let mut settings = settings_naming("testing.probe-section");
        // The section's module is listed first, and what the plan registers
        // still goes ahead of it.
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-section",
                "seam-probe",
                section_probe_registration,
                validate_nothing,
            )
            .with_module_name("testing.probe-section"),
            crate::integrations::IntegrationBuilder::new(
                "probe-plan",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-plan")
            .with_plan_registration(plan_probe_registration),
        ];
        let probe_inserts = |registry: &IntegrationRegistry| {
            let document_state = IntegrationDocumentState::default();
            registry
                .head_inserts(&IntegrationHtmlContext {
                    request_host: "publisher.example.com",
                    request_scheme: "https",
                    origin_host: "origin.example.com",
                    document_state: &document_state,
                })
                .into_iter()
                .filter(|insert| insert == SECTION_INSERT || insert == PLAN_INSERT)
                .collect::<Vec<_>>()
        };

        settings.auction.enabled = false;
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");
        assert!(
            !registry.integration_runs("probe-plan"),
            "should register nothing where the function finds nothing in the plan"
        );
        assert_eq!(probe_inserts(&registry), vec![SECTION_INSERT]);

        settings.auction.enabled = true;
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");
        assert!(
            registry.integration_runs("probe-plan"),
            "should register from the plan a module no section selects"
        );
        assert_eq!(
            probe_inserts(&registry),
            vec![PLAN_INSERT, SECTION_INSERT],
            "should run the hooks of what the plan registered ahead of a section's module"
        );
    }

    /// Appends to a header on each run, so a test can count the runs on the
    /// request itself.
    fn counting_preparer(
        _settings: &Settings,
        request: &mut Request<EdgeBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        request
            .headers_mut()
            .append("x-probe-prepared", HeaderValue::from_static("1"));
        Ok(())
    }

    #[test]
    fn prepare_request_runs_the_preparers_once_for_a_request() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe-counting",
            "seam-probe",
            never_registering_builder,
            validate_nothing,
        )
        .with_module_name("testing.probe-counting")
        .with_request_preparer(counting_preparer)];
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with a request preparer");
        let mut request = plain_request();

        registry
            .prepare_request(&settings, &mut request)
            .expect("should prepare the request");
        registry
            .prepare_request(&settings, &mut request)
            .expect("should accept a request that is already prepared");

        assert_eq!(
            request.headers().get_all("x-probe-prepared").iter().count(),
            1,
            "should run a preparer once however often the request is prepared"
        );

        let mut next = plain_request();
        registry
            .prepare_request(&settings, &mut next)
            .expect("should prepare the next request");
        assert_eq!(
            next.headers().get_all("x-probe-prepared").iter().count(),
            1,
            "should prepare each request"
        );
    }

    /// What the second probe's request hook leaves for its finalizer.
    #[derive(Debug, PartialEq)]
    struct ProbeNote(&'static str);

    fn first_finalizer(_state: &IntegrationRequestState, response: &mut Response<EdgeBody>) {
        response
            .headers_mut()
            .append("x-probe-finalized", HeaderValue::from_static("first"));
    }

    fn second_finalizer(state: &IntegrationRequestState, response: &mut Response<EdgeBody>) {
        let value = match state.get::<ProbeNote>("probe-second") {
            Some(_) => "second-with-its-note",
            None => "second",
        };
        response
            .headers_mut()
            .append("x-probe-finalized", HeaderValue::from_static(value));
    }

    #[test]
    fn finalize_response_runs_every_finalizer_in_registration_order_with_the_request_state() {
        let settings = crate::test_support::tests::create_test_settings();
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                "probe-first",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-first")
            .with_response_finalizer(first_finalizer),
            crate::integrations::IntegrationBuilder::new(
                "probe-second",
                "seam-probe",
                never_registering_builder,
                validate_nothing,
            )
            .with_module_name("testing.probe-second")
            .with_response_finalizer(second_finalizer),
        ];
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry with response finalizers");
        let finalized = |response: &Response<EdgeBody>| {
            response
                .headers()
                .get_all("x-probe-finalized")
                .iter()
                .map(|value| value.to_str().expect("should be text").to_owned())
                .collect::<Vec<_>>()
        };

        let mut request = plain_request();
        IntegrationRequestState::insert(&mut request, "probe-second", ProbeNote("left"));
        let mut response = Response::builder()
            .body(EdgeBody::empty())
            .expect("should build response");
        registry.finalize_response(&IntegrationRequestState::of(&request), &mut response);

        assert_eq!(
            finalized(&response),
            vec!["first", "second-with-its-note"],
            "should run both finalizers in registration order, neither integration \
             running, and hand each what was left on the request"
        );

        let mut unmarked = Response::builder()
            .body(EdgeBody::empty())
            .expect("should build response");
        registry.finalize_response(&IntegrationRequestState::default(), &mut unmarked);
        assert_eq!(
            finalized(&unmarked),
            vec!["first", "second"],
            "should hand a finalizer no state for a request nothing was left on"
        );
    }

    #[test]
    fn what_a_module_leaves_on_a_request_reaches_a_document_when_it_is_seeded() {
        let mut request = plain_request();
        assert!(
            IntegrationRequestState::of(&request).is_empty(),
            "should hold nothing for a request no module left a value on"
        );

        IntegrationRequestState::insert(&mut request, "probe", ProbeNote("first"));
        IntegrationRequestState::insert(&mut request, "probe", ProbeNote("second"));
        IntegrationRequestState::insert(&mut request, "other", ProbeNote("other"));
        let state = IntegrationRequestState::of(&request);

        assert!(!state.is_empty(), "should hold what was left");
        assert_eq!(
            state.get::<ProbeNote>("probe").as_deref(),
            Some(&ProbeNote("second")),
            "should keep the last value of one type left for an integration"
        );
        assert!(
            state.get::<ProbeNote>("absent").is_none(),
            "should hold nothing for an integration that left nothing"
        );

        let document = IntegrationDocumentState::default();
        assert!(
            document.get::<ProbeNote>("probe").is_none(),
            "should start a document with none of the request's values"
        );
        state.seed(&document);
        assert_eq!(
            document.get::<ProbeNote>("probe").as_deref(),
            Some(&ProbeNote("second")),
            "should copy the value into the document's state"
        );
        assert_eq!(
            document.get::<ProbeNote>("other").as_deref(),
            Some(&ProbeNote("other")),
            "should copy every integration's value"
        );
    }

    #[test]
    fn the_request_stand_in_marks_a_navigation_and_strips_its_query() {
        use super::test_support::request_fixture;

        let settings = crate::test_support::tests::create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let mut navigation = Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article?fixture_request=1&keep=yes")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build request");
        let mut subresource = Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/data.json?fixture_request=1")
            .header("sec-fetch-dest", "empty")
            .body(EdgeBody::empty())
            .expect("should build request");

        registry
            .prepare_request(&settings, &mut navigation)
            .expect("should prepare the navigation");
        registry
            .prepare_request(&settings, &mut subresource)
            .expect("should prepare the subresource request");

        assert_eq!(
            navigation.uri().query(),
            Some("keep=yes"),
            "should strip the stand-in's query and keep the rest"
        );
        assert!(
            IntegrationRequestState::of(&navigation)
                .get::<request_fixture::Mark>(request_fixture::ID)
                .is_some(),
            "should leave the mark on a document navigation, selected or not"
        );
        assert_eq!(
            subresource.uri().query(),
            None,
            "should strip the stand-in's query from any request"
        );
        assert!(
            IntegrationRequestState::of(&subresource).is_empty(),
            "should leave no mark on a request for something other than a document"
        );
    }

    /// Example country code returned by the test geo module. `ZZ` is the
    /// user-assigned code, so it names no real place.
    const GEO_PROBE_COUNTRY: &str = "ZZ";

    /// A geo module that resolves one fixed location, so a test can tell the
    /// module's module apart from the "no location" one.
    #[derive(Debug)]
    struct FixedCountryGeo;

    #[async_trait::async_trait(?Send)]
    impl crate::platform::PlatformGeo for FixedCountryGeo {
        async fn lookup(
            &self,
            _client_ip: Option<std::net::IpAddr>,
            _services: &crate::platform::RuntimeServices,
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

    /// The name the `geo-probe` registration's geo module is selected by.
    const GEO_PROBE_MODULE: &str = "testing.geo-probe";

    /// Builds a `geo-probe` registration that declares a geo module.
    fn geo_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("geo-probe")
                .with_geo_module(GEO_PROBE_MODULE, Arc::new(FixedCountryGeo))
                .build(),
        ))
    }

    /// A device module that declares it needs a permission, which nothing can
    /// enforce per request yet.
    #[derive(Debug)]
    struct PermissionDemandingDevice;

    /// The name the `device-probe` registration's device module is selected
    /// by.
    const DEVICE_PROBE_MODULE: &str = "testing.device-probe";

    #[async_trait::async_trait(?Send)]
    impl crate::ec::device::DeviceModule for PermissionDemandingDevice {
        fn id(&self) -> &'static str {
            DEVICE_PROBE_MODULE
        }

        async fn detect(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _services: &crate::platform::RuntimeServices,
        ) -> crate::ec::device::DeviceSignals {
            crate::ec::device::DeviceSignals {
                is_mobile: 2,
                ja4_class: None,
                platform_class: None,
                h2_fp_hash: None,
                known_browser: None,
                looks_like_browser: false,
            }
        }

        fn required_permissions(&self) -> crate::permissions::PermissionSet {
            crate::permissions::PermissionSet::none()
                .with(crate::permissions::Permission::StoreOnDevice)
        }
    }

    fn device_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("device-probe")
                .with_device_module(DEVICE_PROBE_MODULE, Arc::new(PermissionDemandingDevice))
                .build(),
        ))
    }

    fn device_probe_builders() -> [crate::integrations::IntegrationBuilder; 1] {
        [crate::integrations::IntegrationBuilder::new(
            "device-probe",
            "seam-probe",
            device_probe_registration,
            validate_nothing,
        )
        .with_module_name(DEVICE_PROBE_MODULE)]
    }

    #[test]
    fn a_modules_device_module_declaring_permissions_is_refused_at_startup() {
        let mut settings = settings_naming("device-probe");
        settings.device.module = Some(DEVICE_PROBE_MODULE.to_owned());

        let error = IntegrationRegistry::with_registrations(&settings, &device_probe_builders())
            .err()
            .expect("should refuse a device module whose permissions nothing can enforce");

        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("device-probe"),
            "the failure should name the module, got: {rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "requiring `{}`",
                crate::permissions::Permission::StoreOnDevice.as_str()
            )),
            "the failure should say why it was refused, got: {rendered}"
        );
    }

    fn geo_probe_builders() -> [crate::integrations::IntegrationBuilder; 1] {
        [crate::integrations::IntegrationBuilder::new(
            "geo-probe",
            "seam-probe",
            geo_probe_registration,
            validate_nothing,
        )
        .with_module_name(GEO_PROBE_MODULE)]
    }

    /// Settings whose `[geo] module` names `module`, which may be a value
    /// core resolves itself, such as `none` or `platform`.
    fn settings_with_geo_selector(module: &str) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.geo.module = Some(module.to_owned());
        settings
    }

    /// The same, with the module selected in its section as well, which is
    /// what a deployment running a module's geo module writes.
    fn settings_selecting_geo_module(name: &str) -> Settings {
        let mut settings = settings_with_geo_selector(name);
        let section = name.split_once('.').map_or("testing", |(folder, _)| folder);
        settings.select_module(section, name);
        settings
    }

    #[tokio::test]
    async fn geo_selector_resolves_the_geo_module_the_selected_module_declares() {
        let settings = settings_selecting_geo_module(GEO_PROBE_MODULE);

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with a module geo module");

        let module = registry
            .geo_module()
            .expect("should resolve the module's geo module");
        let resolved = module
            .lookup(None, &noop_services())
            .await
            .expect("should look up without failing")
            .expect("should resolve a location");
        assert_eq!(
            resolved.country, GEO_PROBE_COUNTRY,
            "should resolve through the module's own module"
        );
    }

    #[tokio::test]
    async fn geo_module_none_resolves_no_location() {
        let settings = settings_with_geo_selector("none");

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with the disabled geo module");

        let module = registry
            .geo_module()
            .expect("should resolve the disabled geo module");
        assert!(
            module
                .lookup(None, &noop_services())
                .await
                .expect("should look up without failing")
                .is_none(),
            "should resolve no location when the selector is `none`"
        );
    }

    #[tokio::test]
    async fn an_unset_geo_selector_resolves_the_disabled_module_and_platform_opts_in() {
        // Unset is the permission model's privacy default: it resolves the
        // disabled module, so no client IP ever reaches a host geo service.
        // It is deliberately not the same as leaving the adapter's own lookup
        // in place, which is what `platform` spells.
        let settings = crate::test_support::tests::create_test_settings();
        assert!(
            settings.geo.module.is_none(),
            "the shared test settings should leave the geo selector unset"
        );

        let registry = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .expect("should build registry with an unset geo selector");

        let module = registry
            .geo_module()
            .expect("an unset selector should resolve the disabled module, not nothing");
        assert!(
            module
                .lookup(None, &noop_services())
                .await
                .expect("should look up without failing")
                .is_none(),
            "the disabled module should resolve no location and make no host call"
        );

        // `platform` is the explicit opt-in to the adapter's own lookup, and it
        // is the one case that resolves nothing here so the adapter's module
        // stands.
        let platform = settings_with_geo_selector(GEO_MODULE_PLATFORM);
        let registry = IntegrationRegistry::with_registrations(&platform, &geo_probe_builders())
            .expect("should build registry with the platform geo selector");
        assert!(
            registry.geo_module().is_none(),
            "`platform` should leave the adapter's own host lookup in place"
        );
    }

    #[test]
    fn geo_selector_rejects_a_module_that_declares_no_geo_module() {
        let settings = settings_selecting_geo_module("testing.probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a module that declares no geo module");

        let message = error.to_string();
        assert!(
            message.contains("`[geo] module` names `testing.probe`")
                && message.contains("It runs no geo module")
                && message.contains("is selected and supplies no geo module"),
            "error should name the module and say it supplies no geo module: {message}"
        );
    }

    #[test]
    fn geo_selector_rejects_a_module_no_section_selects() {
        // The module is registered, but no section selects it, so its geo
        // module is not there to select.
        let settings = settings_with_geo_selector("testing.probe-unnamed");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe-unnamed",
            "seam-probe",
            never_registering_builder,
            validate_nothing,
        )
        .with_module_name("testing.probe-unnamed")];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should reject a module no section selects");

        let message = error.to_string();
        assert!(
            message.contains("`[geo] module` names `testing.probe-unnamed`")
                && message.contains("is a module no section selects"),
            "error should name the module and say no section selects it: {message}"
        );
    }

    /// An id no builder supplies is refused here, where the adapter's and a
    /// vendor crate's builders are known, rather than in the settings.
    #[test]
    fn an_id_no_builder_supplies_is_refused_at_registry_build() {
        let settings = settings_naming("a_vendors_own_integration");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse an id no builder in this deployment supplies");

        let message = error.to_string();
        assert!(
            message.contains("a_vendors_own_integration"),
            "should name the id nothing supplies: {message}"
        );
        assert!(
            message.contains("The testing modules it supplies are") && message.contains("probe"),
            "should list the modules of that type this deployment does supply: {message}"
        );
    }

    /// A demand or ad server implementation is not a page integration, so a
    /// section naming one is refused like any name no builder supplies.
    #[test]
    fn an_auction_implementation_named_in_a_section_is_refused() {
        for (section, name) in [
            ("auction", "prebid-server"),
            ("auction", "aps"),
            ("auction", "ad-server.fixture"),
            ("auction-protocol", "openrtb"),
        ] {
            let mut settings = crate::test_support::tests::create_test_settings();
            settings.select_module(section, name);

            let error = IntegrationRegistry::new(&settings)
                .err()
                .expect("should refuse an implementation named as a page module");

            let message = error.to_string();
            assert!(
                message.contains(&format!("[{section}] selects `{name}`")),
                "should name the section and the implementation: {message}"
            );
        }
    }

    /// The same id is accepted once a builder supplies it, so a vendor crate
    /// an adapter composes in is named the same way a built-in is.
    #[test]
    fn an_id_a_supplied_builder_claims_is_accepted() {
        let settings = settings_naming("probe");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "probe",
            "seam-probe",
            probe_registration,
            validate_nothing,
        )
        .with_module_name("testing.probe")];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should accept an id a supplied builder claims");

        assert!(
            registry.integration_runs("probe"),
            "the named module should run"
        );
    }

    #[test]
    fn geo_selector_rejects_a_name_no_module_supplies() {
        // Nothing supplies the name, as a geo module or as a module at all.
        let settings = settings_with_geo_selector("absent-module");

        let error = IntegrationRegistry::with_registrations(&settings, &geo_probe_builders())
            .err()
            .expect("should reject a selector naming nothing this deployment runs");

        let message = error.to_string();
        assert!(
            message.contains("`[geo] module` names `absent-module`")
                && message.contains("which no module this deployment runs supplies")
                && !message.contains("no section selects"),
            "error should name what was written and say nothing supplies it: {message}"
        );
    }

    /// A device module that needs no permission, for a registration that
    /// supplies more than one type of module.
    #[derive(Debug)]
    struct ExampleDevice;

    #[async_trait::async_trait(?Send)]
    impl crate::ec::device::DeviceModule for ExampleDevice {
        fn id(&self) -> &'static str {
            "device.example"
        }

        async fn detect(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _services: &crate::platform::RuntimeServices,
        ) -> crate::ec::device::DeviceSignals {
            crate::ec::device::DeviceSignals {
                is_mobile: 1,
                ja4_class: None,
                platform_class: Some("example".to_owned()),
                h2_fp_hash: None,
                known_browser: None,
                looks_like_browser: true,
            }
        }
    }

    /// An Edge Cookie module that answers to `edgecookie.example`.
    #[derive(Debug)]
    struct ExampleEc;

    #[async_trait::async_trait(?Send)]
    impl EdgeCookieModule for ExampleEc {
        fn id(&self) -> &'static str {
            "edgecookie.example"
        }

        fn code(&self) -> crate::ec::module::ModuleCode {
            crate::module_code!("t0rg")
        }

        async fn generate(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _input: &crate::ec::module::IdentityInput<'_>,
            _services: &crate::platform::RuntimeServices,
        ) -> Result<crate::ec::module::GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(crate::ec::module::GeneratedEdgeCookie::default())
        }
    }

    /// Builds one registration that supplies a geo, a device and an Edge
    /// Cookie module, each under the name of its own type.
    fn example_vendor_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("example-vendor")
                .with_geo_module("geo.example", Arc::new(FixedCountryGeo))
                .with_device_module("device.example", Arc::new(ExampleDevice))
                .with_ec_module("edgecookie.example", Arc::new(ExampleEc))
                .build(),
        ))
    }

    fn example_vendor_builders() -> [crate::integrations::IntegrationBuilder; 1] {
        [crate::integrations::IntegrationBuilder::new(
            "example-vendor",
            "seam-probe",
            example_vendor_registration,
            validate_nothing,
        )
        .with_module_name("testing.example-vendor")]
    }

    /// One registration can supply a module of each type, and each selector
    /// finds its own by the short name, the type folder left off.
    #[test]
    fn each_selector_resolves_its_own_type_of_module_from_one_registration() {
        let mut settings = settings_naming("example-vendor");
        settings.geo.module = Some("example".to_owned());
        settings.device.module = Some("example".to_owned());
        settings.ec.module = Some(EcModuleSelection::from("example"));

        let registry =
            IntegrationRegistry::with_registrations(&settings, &example_vendor_builders())
                .expect("should build registry with one module of each type");

        assert!(
            registry.geo_module().is_some(),
            "`[geo] module = \"example\"` should resolve `geo.example`"
        );
        assert_eq!(
            registry
                .device_module()
                .expect("`[device] module = \"example\"` should resolve `device.example`")
                .id(),
            "device.example",
            "should resolve the registration's device module"
        );
        assert_eq!(
            registry
                .ec_module()
                .expect("`[ec] module = \"example\"` should resolve `edgecookie.example`")
                .id(),
            "edgecookie.example",
            "should resolve the registration's Edge Cookie module"
        );
    }

    /// A name written in full selects the same module, and a short name is
    /// read within the selector's own type only.
    #[test]
    fn a_selector_reads_a_full_name_and_keeps_a_short_name_within_its_type() {
        let mut settings = settings_naming("example-vendor");
        settings.geo.module = Some("geo.example".to_owned());

        let registry =
            IntegrationRegistry::with_registrations(&settings, &example_vendor_builders())
                .expect("should resolve a geo module written in full");
        assert!(
            registry.geo_module().is_some(),
            "`[geo] module = \"geo.example\"` should resolve the module"
        );

        // `device.example` is a device module, so `[geo]` does not find it
        // under the short name of another type.
        let mut settings = settings_naming("example-vendor");
        settings.geo.module = Some("device.example".to_owned());
        let error = IntegrationRegistry::with_registrations(&settings, &example_vendor_builders())
            .err()
            .expect("should refuse a device module named as a geo module");
        let message = error.to_string();
        assert!(
            message.contains("`[geo] module` names `device.example`")
                && message.contains("The geo modules it runs are [example]"),
            "error should list the geo modules by the name the section writes: {message}"
        );
    }

    /// `[ec] module` may name a label whose block names the implementation,
    /// and the registry resolves the implementation, as core does.
    #[test]
    fn ec_selector_resolves_the_implementation_a_labelled_block_names() {
        let mut settings = settings_naming("example-vendor");
        settings.ec.module = Some(EcModuleSelection::from("primary"));
        settings.ec.module_blocks.insert(
            "primary".to_owned(),
            crate::settings::EcModuleBlock {
                implementation: Some("example".to_owned()),
                settings: crate::settings::EcModuleSettings::Injected(serde_json::Map::new()),
            },
        );

        let registry =
            IntegrationRegistry::with_registrations(&settings, &example_vendor_builders())
                .expect("should build registry with a labelled Edge Cookie selection");

        assert_eq!(
            registry
                .ec_module()
                .expect("should resolve the implementation the block names")
                .id(),
            "edgecookie.example",
            "should resolve the registration's Edge Cookie module"
        );
    }

    /// Core's own Edge Cookie modules are not the registry's to supply, so a
    /// selection naming one resolves nothing here and is not an error.
    #[test]
    fn ec_selector_naming_a_module_of_cores_resolves_nothing_here() {
        let settings = settings_naming("example-vendor");
        assert!(
            settings
                .ec
                .module
                .as_ref()
                .is_some_and(|selection| selection.key() == crate::ec::module::HMAC_MODULE_KEY),
            "the shared test settings should select core's HMAC module"
        );

        let registry =
            IntegrationRegistry::with_registrations(&settings, &example_vendor_builders())
                .expect("should build registry with core's own Edge Cookie module selected");

        assert!(
            registry.ec_module().is_none(),
            "core resolves its own module, so the registry should supply none"
        );
    }

    fn misnamed_ec_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("misnamed")
                .with_ec_module("edgecookie.another", Arc::new(ExampleEc))
                .build(),
        ))
    }

    /// Core checks the selected Edge Cookie module by its own id, so one
    /// declared under another name is refused where the mistake is made.
    #[test]
    fn an_edge_cookie_module_declared_under_another_name_is_refused() {
        let settings = settings_naming("misnamed");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "misnamed",
            "seam-probe",
            misnamed_ec_registration,
            validate_nothing,
        )
        .with_module_name("testing.misnamed")];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse an Edge Cookie module declared under another name");

        let message = error.to_string();
        assert!(
            message.contains("`edgecookie.another`") && message.contains("`edgecookie.example`"),
            "error should give both names: {message}"
        );
    }

    fn second_geo_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("geo-second")
                .with_geo_module(GEO_PROBE_MODULE, Arc::new(FixedCountryGeo))
                .build(),
        ))
    }

    /// Two registrations supplying a module of one type under one name would
    /// leave a selector meaning either, so the pair is refused.
    #[test]
    fn two_registrations_declaring_one_geo_module_name_are_refused() {
        let mut settings = settings_naming("geo-probe");
        settings.select_module("testing", "testing.geo-second");
        let extra = [
            geo_probe_builders()[0],
            crate::integrations::IntegrationBuilder::new(
                "geo-second",
                "seam-probe",
                second_geo_registration,
                validate_nothing,
            )
            .with_module_name("testing.geo-second"),
        ];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse one geo module name declared twice");

        let message = error.to_string();
        assert!(
            message.contains("the geo module `testing.geo-probe` is declared twice")
                && message.contains("`geo-probe`")
                && message.contains("`geo-second`"),
            "error should name the module and both integrations: {message}"
        );
    }
}
