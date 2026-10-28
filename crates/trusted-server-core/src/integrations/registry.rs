use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

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
use crate::middleware::{Middleware, MiddlewareChain, MiddlewarePhase, PhaseEntries};
use crate::module_context::{ModuleCall, ModuleContext, ModuleRequest, ResolvedRequest};
use crate::platform::{DisabledGeo, PlatformGeo, PlatformIdentity, RuntimeServices};
use crate::prefix::{PrefixModule, Prefixes, SelectedPrefixModule};
use crate::robots_txt::{ALLOW_ALL, REFUSE_ALL, RobotsTxtContributor};
use crate::settings::Settings;

/// What a middleware's element handler decides about one attribute of an
/// element.
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

/// What a middleware's text handler decides about one chunk of the text
/// inside an element.
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

type IntegrationDocumentStateMap = BTreeMap<(&'static str, TypeId), Arc<dyn Any + Send + Sync>>;

/// State shared by the middleware working on one document in one phase.
///
/// What one handler leaves here a later handler or a stream processor reads,
/// so the document is parsed once. In the serve phase it starts with what
/// the request's hooks left, see [`IntegrationRequestState`].
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

/// Values a module's request hooks leave for its own serve middleware, for
/// one request.
///
/// A request preparer or a request filter leaves a value under its
/// integration id with [`IntegrationRequestState::insert`]. Where the
/// request's reader is served an HTML document, or another body a
/// `[[serve]]` entry covers, every value is copied into the
/// [`IntegrationDocumentState`] the `[[serve]]` entry's middleware share,
/// so the module's serve middleware reads it there, and the same values are
/// handed to the module's response finalizer.
///
/// A request that carries any value keeps to the origin path. Its document is
/// never read from a shared template and never stored as one, and an HTML
/// response with a body, or another response a `[[serve]]` entry covers, is
/// sent `private, no-store`. A fetch middleware is handed none of these
/// values.
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
    /// Leaves `value` on `request` for the serve middleware of
    /// `integration_id`, in place of a value of the same type left earlier.
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

    /// Holds `value` for the serve middleware of `integration_id`, in place
    /// of a value of the same type held earlier.
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

    /// The permissions this route declares, which decide whether a gated
    /// value, such as the Edge Cookie identifier, is passed to it. None by
    /// default.
    fn required_permissions(&self) -> crate::permissions::PermissionSet {
        crate::permissions::PermissionSet::none()
    }

    /// Handle the proxied request.
    ///
    /// Hand a function to [`ModuleCall::inject_with`] with `req` as its
    /// argument. A route whose use needs a permission declares it and checks
    /// it is granted, because nothing gates the route itself.
    async fn handle(
        &self,
        call: ModuleCall<'_>,
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
    /// build no EC context, such as batch sync.
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
    /// build no EC context, such as batch sync.
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
    pub request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    /// The page changes this registration supplies, see [`crate::middleware`].
    ///
    /// Declaring one does not make it run, because a middleware changes a
    /// page only where an entry of the settings names it.
    pub middleware: Vec<Arc<dyn Middleware>>,
    /// Attributes this registration puts on the script bundle's tag, each a
    /// name and a value, for a browser module that reads a setting from the
    /// tag it was loaded by.
    pub bundle_tag_attributes: Vec<(&'static str, &'static str)>,
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
    /// Prefix module this registration supplies, with the name
    /// `[prefix] module` selects it by.
    ///
    /// Declaring one does not make it active, because the module only gives
    /// the deployment's prefixes when `[prefix] module` names it.
    pub prefix_module: Option<(&'static str, Arc<dyn PrefixModule>)>,
    /// The rules this registration adds to `/robots.txt`, with the name
    /// `[robots-txt] modules` selects them by.
    ///
    /// Declaring them does not put them in the file, because the file is only
    /// built from the contributors `[robots-txt] modules` names.
    pub robots_txt_contributor: Option<(&'static str, Arc<dyn RobotsTxtContributor>)>,
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
                request_filters: Vec::new(),
                middleware: Vec::new(),
                bundle_tag_attributes: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
                prefix_module: None,
                robots_txt_contributor: None,
            },
        }
    }

    #[must_use]
    pub fn with_proxy(mut self, proxy: Arc<dyn IntegrationProxy>) -> Self {
        self.registration.proxies.push(proxy);
        self
    }

    #[must_use]
    pub fn with_request_filter(mut self, filter: Arc<dyn IntegrationRequestFilter>) -> Self {
        self.registration.request_filters.push(filter);
        self
    }

    /// Declare a page change this registration supplies, see
    /// [`crate::middleware`]. Called once for each one.
    ///
    /// The middleware changes a page only where an entry of the settings
    /// names it, and one no entry names is logged as a warning when the
    /// registry is built.
    #[must_use]
    pub fn with_middleware(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.registration.middleware.push(middleware);
        self
    }

    /// Put an attribute on the script bundle's tag, on every page the bundle
    /// is written into.
    ///
    /// The name is lower case letters, digits and hyphens, and the value
    /// holds none of `"`, `&`, `<` and `>`, or startup is refused. Where two
    /// registrations give one name different values the first is kept, and
    /// the other is logged as a warning.
    #[must_use]
    pub fn with_bundle_tag_attribute(mut self, name: &'static str, value: &'static str) -> Self {
        self.registration.bundle_tag_attributes.push((name, value));
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

    /// Declare the prefix module this registration supplies, with the name
    /// `[prefix] module` selects it by.
    ///
    /// It only gives the deployment's prefixes when `[prefix] module` names
    /// it, see [`crate::prefix`], and a module the selector does not choose is
    /// logged as a warning when the registry is built, the same as geo.
    #[must_use]
    pub fn with_prefix_module(mut self, name: &'static str, module: Arc<dyn PrefixModule>) -> Self {
        self.registration.prefix_module = Some((name, module));
        self
    }

    /// Declare the rules this registration adds to `/robots.txt`, under their
    /// `name`.
    ///
    /// They are only asked for when `[robots-txt] modules` names them. Core
    /// holds what they return and writes the file, see [`crate::robots_txt`].
    #[must_use]
    pub fn with_robots_txt_contributor(
        mut self,
        name: &'static str,
        contributor: Arc<dyn RobotsTxtContributor>,
    ) -> Self {
        self.registration.robots_txt_contributor = Some((name, contributor));
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
    request_filters: Vec<Arc<dyn IntegrationRequestFilter>>,
    // The middleware the running registrations supply, each against the
    // integration that supplied it, in registration order.
    middleware: Vec<(&'static str, Arc<dyn Middleware>)>,
    // The attributes the running registrations put on the script bundle's
    // tag, each against the integration that asked for it, in registration
    // order.
    bundle_tag_attributes: Vec<(&'static str, (&'static str, &'static str))>,
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
    // The robots.txt contributors the running registrations supply, each with
    // its name, in registration order. `[robots-txt] modules` picks any of
    // them, in its own order.
    robots_txt_contributors: Vec<(&'static str, Arc<dyn RobotsTxtContributor>)>,
    // Every builder's module name, with whether a section selects it, so the
    // refusal of a name a selector wrote can say what that name is.
    builder_modules: Vec<(&'static str, bool)>,
    // Vendors whose shared table is absent, for the refusal note.
    absent_shared: Vec<&'static str>,
    // The module `[geo] module` resolved to, or `None` when the selector
    // is `platform` and the adapter's own host lookup stands. Unset and `none`
    // both resolve the disabled module.
    geo_module: Option<Arc<dyn PlatformGeo>>,
    ec_module: Option<Arc<dyn EdgeCookieModule>>,
    device_module: Option<Arc<dyn DeviceModule>>,
    // The prefix modules the running registrations supply, each with its
    // name, in registration order. `[prefix] module` picks at most one.
    prefix_modules: Vec<(&'static str, Arc<dyn PrefixModule>)>,
    // The prefix module `[prefix] module` resolved to, core's own when
    // nothing is selected.
    prefix_module: SelectedPrefixModule,
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
            request_filters: Vec::new(),
            middleware: Vec::new(),
            bundle_tag_attributes: Vec::new(),
            extra_js_module_ids: Vec::new(),
            request_preparers: Vec::new(),
            response_finalizers: Vec::new(),
            geo_modules: Vec::new(),
            ec_modules: Vec::new(),
            device_modules: Vec::new(),
            robots_txt_contributors: Vec::new(),
            builder_modules: Vec::new(),
            absent_shared: Vec::new(),
            geo_module: None,
            ec_module: None,
            device_module: None,
            prefix_modules: Vec::new(),
            prefix_module: SelectedPrefixModule::default(),
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
const PREFIX_TYPE: &str = crate::prefix::MODULE_TYPE;
const ROBOTS_TXT_TYPE: &str = crate::robots_txt::MODULE_TYPE;

impl IntegrationRegistryInner {
    /// The registered middleware an entry means by `name`.
    fn middleware_named(&self, name: &str) -> Option<&Arc<dyn Middleware>> {
        self.middleware
            .iter()
            .map(|(_, middleware)| middleware)
            .find(|middleware| middleware.middleware_id() == name)
    }

    /// A sentence to follow the refusal of a middleware name an entry wrote,
    /// when the name is that of a module no section selects, or begins with
    /// one. Empty otherwise.
    fn unselected_module_note(&self, name: &str) -> String {
        self.builder_modules
            .iter()
            .find(|(module, selected)| {
                !*selected
                    && name
                        .strip_prefix(*module)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            })
            .map(|(module, _)| {
                format!(
                    ". `{module}` is a module no section selects, so nothing it supplies is \
                     running"
                )
            })
            .unwrap_or_default()
    }

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

    /// A sentence naming the shared tables whose absence kept their vendors'
    /// modules from running, to follow a refusal of a module nothing supplies,
    /// or nothing when every builder a shared table runs is running.
    fn absent_shared_note(&self) -> String {
        if self.absent_shared.is_empty() {
            return String::new();
        }
        let tables = self
            .absent_shared
            .iter()
            .map(|vendor| format!("[shared.{vendor}]"))
            .collect::<Vec<_>>()
            .join(" or ");
        let those = if self.absent_shared.len() == 1 {
            "that table runs"
        } else {
            "those tables run"
        };
        format!(". There is no {tables}, so the modules {those} are not running")
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
             supplies. {runs}{}{}",
            inner.builder_note(type_folder, section, written),
            inner.absent_shared_note()
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

/// Resolves `[prefix] module` against the registered prefix modules, or to
/// core's own when nothing or `ts` is selected, and asks the module for the prefixes
/// of the publisher the settings describe, so a module that names something
/// its call never carries is refused here rather than on every request.
///
/// # Errors
///
/// A configuration error when the selector names no registered module, or
/// when the module names a value other than the request and the settings.
fn resolve_prefix_module(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<SelectedPrefixModule, Report<TrustedServerError>> {
    let selected = match settings.prefix.module.as_deref() {
        None | Some(crate::prefix::TRUSTED_SERVER_MODULE) => SelectedPrefixModule::default(),
        Some(written) => {
            let module =
                named_module(PREFIX_TYPE, "prefix", written, &inner.prefix_modules, inner)?;
            let name = inner
                .prefix_modules
                .iter()
                .find(|(_, offered)| Arc::ptr_eq(offered, &module))
                .map_or(PREFIX_TYPE, |(name, _)| *name);
            SelectedPrefixModule::new(name, module)
        }
    };
    let chosen = inner
        .prefix_modules
        .iter()
        .find(|(name, _)| *name == selected.name())
        .map(|(_, module)| module);
    warn_unselected("prefix", PREFIX_TYPE, chosen, &inner.prefix_modules);
    let trial = ModuleRequest::new(&Method::GET, &settings.publisher.domain, "https", "/");
    selected.prefixes(settings, trial).map_err(|withheld| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "`[prefix] module` selects `{}`, which cannot give its prefixes: {withheld}. A \
                 prefix module is handed the request and the settings, and nothing else",
                selected.name()
            ),
        })
    })?;
    Ok(selected)
}

/// Refuses a `[robots-txt] modules` that names something nothing this
/// deployment runs supplies.
///
/// Only knowable here, where every registration has been handed over, so it
/// is checked when the registry is built rather than when the settings load.
/// The file would otherwise fail on the first request for it rather than at
/// deploy.
fn check_robots_txt_selection(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<(), Report<TrustedServerError>> {
    let Some(robots_txt) = settings.robots_txt.as_ref() else {
        return Ok(());
    };
    for written in &robots_txt.modules {
        if written == REFUSE_ALL
            || written == ALLOW_ALL
            || find_named(ROBOTS_TXT_TYPE, written, &inner.robots_txt_contributors).is_some()
        {
            continue;
        }
        let offered: Vec<&str> = [REFUSE_ALL, ALLOW_ALL]
            .into_iter()
            .chain(
                inner
                    .robots_txt_contributors
                    .iter()
                    .map(|(name, _)| crate::module_name::short_form(ROBOTS_TXT_TYPE, name)),
            )
            .collect();
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "`[robots-txt] modules` names `{written}`, which nothing this deployment runs \
                 supplies. What it can name is [{}]{}",
                offered.join(", "),
                inner.builder_note(ROBOTS_TXT_TYPE, "robots.txt", written)
            ),
        }));
    }
    Ok(())
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
    /// The names of the middleware the integration supplies.
    pub middleware: Vec<&'static str>,
    pub request_filters: usize,
}

impl IntegrationMetadata {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            routes: Vec::new(),
            middleware: Vec::new(),
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

/// Refuses a registered middleware no entry could name, and an entry this
/// deployment cannot run.
///
/// A middleware no entry names is left alone and logged.
///
/// # Errors
///
/// Naming the module or the entry at fault.
fn check_middleware(
    settings: &Settings,
    inner: &IntegrationRegistryInner,
) -> Result<(), Report<TrustedServerError>> {
    let refuse = |message: String| Report::new(TrustedServerError::Configuration { message });
    let phases_of = |middleware: &Arc<dyn Middleware>| -> Vec<String> {
        middleware
            .phases()
            .iter()
            .map(|phase| format!("[[{phase}]]"))
            .collect()
    };
    for (position, (integration, middleware)) in inner.middleware.iter().enumerate() {
        let id = middleware.middleware_id();
        if !crate::module_name::is_valid(id) {
            return Err(refuse(format!(
                "module `{integration}` supplies a middleware named `{id}`, which is not a \
                 name an entry can write. A name is parts joined by `.`, each in lower case \
                 letters, digits, `_` or `-`"
            )));
        }
        if middleware.phases().is_empty() {
            return Err(refuse(format!(
                "module `{integration}` supplies the middleware `{id}`, which says it runs \
                 in no phase, so no entry could name it"
            )));
        }
        if let Some((earlier, _)) = inner.middleware[..position]
            .iter()
            .find(|(_, earlier)| earlier.middleware_id() == id)
        {
            return Err(refuse(format!(
                "modules `{earlier}` and `{integration}` both supply a middleware named \
                 `{id}`, so an entry could not say which one it means"
            )));
        }
    }
    for phase in MiddlewarePhase::ALL {
        for (index, entry) in settings.phase_entries(phase).entries().iter().enumerate() {
            let at = format!("[[{phase}]] entry {}", index + 1);
            for name in &entry.middleware {
                let Some(middleware) = inner.middleware_named(name) else {
                    let supplied: Vec<&str> = inner
                        .middleware
                        .iter()
                        .map(|(_, middleware)| middleware.middleware_id())
                        .collect();
                    return Err(refuse(format!(
                        "{at} names `{name}`, which no module that runs supplies. The \
                         middleware the running modules supply is [{}]{}",
                        supplied.join(", "),
                        inner.unselected_module_note(name)
                    )));
                };
                if !middleware.phases().contains(&phase) {
                    return Err(refuse(format!(
                        "{at} names `{name}`, which does not run in that phase. It runs in {}",
                        phases_of(middleware).join(" and ")
                    )));
                }
                if !middleware.handles_media_type(&entry.media_type) {
                    return Err(refuse(format!(
                        "{at} covers `{}` and names `{name}`, which does not work on that media \
                         type",
                        entry.media_type
                    )));
                }
            }
        }
    }
    for (integration, middleware) in unnamed_middleware(settings, inner) {
        log::warn!(
            "Module `{integration}` supplies the middleware `{}` and no {} entry names \
             it, so it changes no page",
            middleware.middleware_id(),
            phases_of(middleware).join(" or ")
        );
    }
    Ok(())
}

/// The middleware no entry names in a phase it runs in, each with the
/// integration that supplies it, in registration order. Core's own
/// middleware is offered to every deployment, so one that no entry names
/// is not a module running for nothing, and is left out.
fn unnamed_middleware<'a>(
    settings: &Settings,
    inner: &'a IntegrationRegistryInner,
) -> Vec<(&'static str, &'a Arc<dyn Middleware>)> {
    inner
        .middleware
        .iter()
        .filter(|(integration, _)| *integration != crate::service_version::MODULE)
        .filter(|(_, middleware)| {
            let id = middleware.middleware_id();
            !middleware
                .phases()
                .iter()
                .any(|phase| settings.phase_entries(*phase).names().contains(&id))
        })
        .map(|(integration, middleware)| (*integration, middleware))
        .collect()
}

/// In-memory registry of integrations discovered from settings.
#[derive(Clone, Default)]
pub struct IntegrationRegistry {
    inner: Arc<IntegrationRegistryInner>,
    plan: Option<Arc<AuctionPlan>>,
    /// What the adapter reports about the platform the deployment runs on,
    /// which a middleware is told about every document.
    platform: Arc<PlatformIdentity>,
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
    inner: &IntegrationRegistryInner,
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
                     supplies. {supplied}{}",
                    inner.absent_shared_note()
                ),
            }));
        }
    }
    Ok(())
}

impl IntegrationRegistry {
    /// Tells the registry what the adapter reports about the platform, which
    /// a middleware is told about every document.
    #[must_use]
    pub fn with_platform(mut self, platform: PlatformIdentity) -> Self {
        self.platform = Arc::new(platform);
        self
    }

    /// What the adapter reports about the platform the deployment runs on.
    #[must_use]
    pub fn platform(&self) -> &PlatformIdentity {
        &self.platform
    }

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

        // A vendor's shared table is read by that vendor's builder alone, so
        // one no builder reads is a table the deployment cannot use.
        let unread: Vec<&str> = settings
            .shared
            .vendors()
            .filter(|vendor| {
                !crate::integrations::all_builders(extra)
                    .any(|builder| builder.shared_settings() == Some(*vendor))
            })
            .collect();
        if !unread.is_empty() {
            let read: Vec<&str> = crate::integrations::all_builders(extra)
                .filter_map(|builder| builder.shared_settings())
                .collect();
            let reads = if read.is_empty() {
                "It reads none".to_owned()
            } else {
                format!("The shared tables it reads are [{}]", read.join(", "))
            };
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[shared.{}] is read by no module this deployment supplies. {reads}",
                    unread.join("] and [shared."),
                ),
            }));
        }

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
            // A builder its shared table runs answers to no section, so a
            // module name would let a section select what it does not run.
            if let (Some(vendor), Some(name)) = (builder.shared_settings(), builder.module_name()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "integration `{}` from `{}` is run by [shared.{vendor}] and also names \
                         the module `{name}`. A builder its shared table runs is selected by \
                         no section, so it takes no module name",
                        builder.id(),
                        builder.source()
                    ),
                }));
            }
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
            // A builder a vendor's shared table runs is built when the table
            // is present, whatever any section selects.
            let runs = match builder.shared_settings() {
                Some(vendor) => {
                    let present = settings.shared.contains(vendor);
                    if !present && !inner.absent_shared.contains(&vendor) {
                        inner.absent_shared.push(vendor);
                    }
                    present
                }
                None => selected,
            };
            if !runs {
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
        check_section_selections(settings, extra, &inner)?;

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
            inner.request_filters.extend(registration.request_filters);
            for middleware in registration.middleware {
                inner
                    .middleware
                    .push((registration.integration_id, middleware));
            }
            for (name, value) in registration.bundle_tag_attributes {
                // The tag is written as markup with no escaping, so a name
                // or a value that could end the attribute is refused.
                let name_is_plain = !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    });
                let value_is_plain = !value
                    .bytes()
                    .any(|byte| matches!(byte, b'"' | b'&' | b'<' | b'>'));
                if !name_is_plain || !value_is_plain {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "integration `{}` puts the attribute `{name}` on the script \
                             bundle's tag with the value `{value}`. A name is lower case \
                             letters, digits and hyphens, and a value holds none of `\"`, `&`, \
                             `<` and `>`",
                            registration.integration_id
                        ),
                    }));
                }
                inner
                    .bundle_tag_attributes
                    .push((registration.integration_id, (name, value)));
            }
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
            if let Some((name, module)) = registration.prefix_module {
                claim_module_name(&mut claimed, "prefix", name, registration.integration_id)?;
                inner.prefix_modules.push((name, module));
            }
            if let Some((name, contributor)) = registration.robots_txt_contributor {
                claim_module_name(
                    &mut claimed,
                    "robots.txt",
                    name,
                    registration.integration_id,
                )?;
                inner.robots_txt_contributors.push((name, contributor));
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
        inner.prefix_module = resolve_prefix_module(settings, &inner)?;
        inner.geo_module = geo_module;
        check_robots_txt_selection(settings, &inner)?;
        if let Some(config) = settings.robots_txt.as_ref() {
            // Core's own middleware for the file, made once every registration is in.
            let selected = crate::robots_txt::select(config, &inner.robots_txt_contributors)?;
            inner.middleware.push((
                crate::robots_txt::middleware::MODULE,
                Arc::new(crate::robots_txt::middleware::RobotsTxt::new(
                    config.clone(),
                    selected,
                )) as Arc<dyn Middleware>,
            ));
        }
        // Core's own middleware, which an entry may name whatever the
        // sections select.
        inner.middleware.push((
            crate::service_version::MODULE,
            Arc::new(crate::service_version::ServiceVersion::default()),
        ));
        check_middleware(settings, &inner)?;

        Ok(Self {
            inner: Arc::new(inner),
            plan: Some(plan),
            platform: Arc::default(),
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

    /// The prefix module `[prefix] module` selects, or core's own when nothing
    /// is selected.
    #[must_use]
    pub fn prefix_module(&self) -> &SelectedPrefixModule {
        &self.inner.prefix_module
    }

    /// The prefixes of the publisher `request` was resolved to under
    /// `settings`, from the selected prefix module.
    ///
    /// # Panics
    ///
    /// When the module names a value its call does not carry, which the
    /// registry refused when it was built, because a request's call carries
    /// what the startup trial's did.
    #[must_use]
    pub fn prefixes(&self, settings: &Settings, request: ModuleRequest<'_>) -> Prefixes {
        self.inner
            .prefix_module
            .prefixes(settings, request)
            .expect("should give prefixes, as the startup trial proved the module names only what a call carries")
    }

    /// The name of every middleware a running module supplies, in
    /// registration order.
    #[must_use]
    pub fn middleware_ids(&self) -> Vec<&'static str> {
        self.inner
            .middleware
            .iter()
            .map(|(_, middleware)| middleware.middleware_id())
            .collect()
    }

    /// The names of the middleware that run in `phase`, in registration
    /// order.
    #[must_use]
    pub fn middleware_in(&self, phase: MiddlewarePhase) -> Vec<&'static str> {
        self.inner
            .middleware
            .iter()
            .filter(|(_, middleware)| middleware.phases().contains(&phase))
            .map(|(_, middleware)| middleware.middleware_id())
            .collect()
    }

    /// Each middleware's name with the text it keys a stored page by, in
    /// registration order. See [`Middleware::template_key`].
    #[must_use]
    pub fn middleware_template_keys(&self) -> Vec<(&'static str, &str)> {
        self.inner
            .middleware
            .iter()
            .map(|(_, middleware)| (middleware.middleware_id(), middleware.template_key()))
            .collect()
    }

    /// The middleware the first entry covering a response selects, in the
    /// order they run, for a response of `media_type` to a request for `path`,
    /// leaving out any that does not work on `path`. A response no entry
    /// covers gives an empty chain.
    #[must_use]
    pub fn middleware_chain(
        &self,
        entries: &PhaseEntries,
        phase: MiddlewarePhase,
        media_type: &str,
        path: &str,
    ) -> MiddlewareChain {
        let middleware = entries
            .for_response(media_type, path)
            .iter()
            .filter_map(|name| self.inner.middleware_named(name).map(Arc::clone))
            .filter(|middleware| middleware.handles_path(path))
            .collect();
        MiddlewareChain::new(phase, media_type, middleware)
    }

    /// Whether a middleware that an entry of either phase names for a request
    /// for `path`, under any media type, needs the origin's whole body, see
    /// [`Middleware::needs_whole_body`]. Asked before the origin answers, so
    /// before the media type of the response is known.
    #[must_use]
    pub fn middleware_needs_whole_body(&self, settings: &Settings, path: &str) -> bool {
        MiddlewarePhase::ALL.iter().any(|phase| {
            settings
                .phase_entries(*phase)
                .entries()
                .iter()
                .filter(|entry| entry.covers_path(path))
                .flat_map(|entry| &entry.middleware)
                .filter_map(|name| self.inner.middleware_named(name))
                .any(|middleware| middleware.handles_path(path) && middleware.needs_whole_body())
        })
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

            // The route takes the request, so the context holds a copy of
            // what it resolved to, beside what was resolved for it.
            let resolved = ResolvedRequest::of(&req, services.client_info());
            let context = ModuleContext::new(resolved.view())
                .with_settings(settings)
                .with_request_state(ec_context, services);
            Some(
                proxy
                    .handle(
                        context.call(proxy.integration_name(), proxy.required_permissions()),
                        req,
                    )
                    .await,
            )
        } else {
            None
        }
    }

    /// Collect the attributes the registrations put on the publisher TSJS
    /// bundle tag, each keeping the first value a name is given.
    #[must_use]
    pub fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
        let mut attributes: Vec<(&'static str, &'static str)> = Vec::new();
        for (integration, attribute) in self.inner.bundle_tag_attributes.iter().copied() {
            let existing = attributes
                .iter()
                .find(|(name, _)| *name == attribute.0)
                .copied();
            match existing {
                None => attributes.push(attribute),
                Some((_, kept_value)) if kept_value != attribute.1 => log::warn!(
                    "Integration `{integration}` emits conflicting value for publisher tag attribute `{}`; keeping the first",
                    attribute.0
                ),
                Some(_) => {}
            }
        }
        attributes
    }

    /// Provide a snapshot of registered integrations and what each supplies.
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

        for (integration_id, middleware) in &self.inner.middleware {
            let entry = map
                .entry(*integration_id)
                .or_insert_with(|| IntegrationMetadata::new(integration_id));
            entry.middleware.push(middleware.middleware_id());
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
            platform: Arc::default(),
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
                request_filters,
                middleware: Vec::new(),
                bundle_tag_attributes: Vec::new(),
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
                robots_txt_contributors: Vec::new(),
                builder_modules: Vec::new(),
                absent_shared: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
                prefix_modules: Vec::new(),
                prefix_module: SelectedPrefixModule::default(),
            }),
            plan: None,
            platform: Arc::default(),
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
                request_filters: Vec::new(),
                middleware: Vec::new(),
                bundle_tag_attributes: Vec::new(),
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
                robots_txt_contributors: Vec::new(),
                builder_modules: Vec::new(),
                absent_shared: Vec::new(),
                geo_module: None,
                ec_module: None,
                device_module: None,
                prefix_modules: Vec::new(),
                prefix_module: SelectedPrefixModule::default(),
            }),
            plan: None,
            platform: Arc::default(),
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
    /// Selected, it supplies one serve middleware under its own name, which
    /// reads the mark from the state the request left and writes one script
    /// ahead of the bundle and the tag of its standalone module after it,
    /// into that reader's copy alone. Its response finalizer sets a
    /// cookie on the response to a marked request, and its builder declares
    /// that it reads the auction token.
    pub(crate) mod request_fixture {
        use std::sync::Arc;

        use edgezero_core::body::Body as EdgeBody;
        use error_stack::Report;
        use http::{HeaderValue, Method, Request, Response, Uri, header};

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{
            CarriedJsModule, IntegrationRegistration, IntegrationRequestFilter,
            IntegrationRequestState, RequestFilterDecision, RequestFilterEffects,
            RequestFilterInput,
        };
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::middleware::{Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase};
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

        impl Middleware for Head {
            fn middleware_id(&self) -> &'static str {
                MODULE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Serve]
            }

            fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
                if context.document_state.get::<Mark>(ID).is_none() {
                    return MiddlewareAction::pass();
                }
                let module = JsModulePart {
                    id: ID,
                    source: JS,
                    sha256: JS_SHA256,
                };
                MiddlewareAction {
                    head_inserts: vec![format!("<script>{HEAD_FLAG}</script>")],
                    after_bundle_inserts: vec![format!(
                        "<script src=\"{}\"></script>",
                        crate::tsjs::tsjs_single_module_script_src(&module)
                    )],
                    ..MiddlewareAction::pass()
                }
            }
        }

        /// Has `settings` select the stand-in and run its middleware, and no
        /// other, on every reader's copy of a page.
        pub(crate) fn select_and_place(settings: &mut Settings) {
            settings.select_module("testing", MODULE);
            crate::html_processor::test_support::place_on_every_page(
                settings,
                MiddlewarePhase::Serve,
                &[MODULE],
            );
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
                    .with_middleware(Arc::new(Head))
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

    /// A stand-in for an integration whose browser module loads deferred, for
    /// core's own tests of how modules are divided between the bundle and
    /// deferred tags, and of how a module's settings reach a page template.
    ///
    /// Selected, it carries a deferred browser module and supplies one fetch
    /// middleware under its own name, which writes its two settings into
    /// `<head>`.
    pub(crate) mod deferred_fixture {
        use std::sync::Arc;

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{CarriedJsModule, IntegrationRegistration};
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::middleware::{Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase};
        use crate::settings::Settings;

        /// The integration id the stand-in registers under.
        pub(crate) const ID: &str = "deferred_fixture";
        /// The name a test's settings select the stand-in by, in `[testing]`.
        pub(crate) const MODULE: &str = "testing.deferred-fixture";
        /// The file of the stand-in's deferred module.
        pub(crate) const MODULE_FILE: &str = "tsjs-deferred_fixture.min.js";

        const JS: &str = "(function(){window.__ts_deferred_fixture_loaded=1;})();";
        // SHA-256 of JS, hex. The registry refuses a literal that is not.
        const JS_SHA256: &str = "3947bc5e93ffb5d057f0d500d20ee25d53612b6c8b59cd3f9512c1b64b45f9f6";

        /// The builder core's test build lists beside its own.
        pub(crate) const BUILDER: IntegrationBuilder =
            IntegrationBuilder::new(ID, CORE_SOURCE, register, validate).with_module_name(MODULE);

        #[derive(Debug, serde::Deserialize, validator::Validate)]
        #[serde(deny_unknown_fields)]
        struct FixtureSettings {
            #[serde(default)]
            label: String,
            #[serde(default)]
            timeout_ms: u32,
        }

        impl crate::settings::IntegrationConfig for FixtureSettings {}

        struct Head {
            label: String,
            timeout_ms: u32,
        }

        impl Middleware for Head {
            fn middleware_id(&self) -> &'static str {
                MODULE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                let config = serde_json::json!({
                    "label": self.label,
                    "timeoutMs": self.timeout_ms,
                });
                MiddlewareAction {
                    head_inserts: vec![format!(
                        "<script>window.__ts_deferred_fixture={config};</script>"
                    )],
                    ..MiddlewareAction::pass()
                }
            }
        }

        /// Has `settings` run the stand-in's middleware, and no other, on
        /// every page.
        pub(crate) fn place(settings: &mut Settings) {
            crate::html_processor::test_support::place_on_every_page(
                settings,
                MiddlewarePhase::Fetch,
                &[MODULE],
            );
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            let Some(config) = settings.module_config::<FixtureSettings>(MODULE)? else {
                return Ok(None);
            };
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .with_js_module(CarriedJsModule {
                        source: JS,
                        sha256: JS_SHA256,
                    })
                    .with_deferred_js()
                    .with_middleware(Arc::new(Head {
                        label: config.label,
                        timeout_ms: config.timeout_ms,
                    }))
                    .build(),
            ))
        }

        fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
            settings
                .module_config::<FixtureSettings>(MODULE)
                .map(|config| config.is_some())
        }
    }

    /// A stand-in for a module that changes a page through middleware, for
    /// core's own tests of the entries and of the page path.
    ///
    /// Selected, it supplies three fetch middleware. The one under the
    /// module's own name writes a marker carrying the module's `label`
    /// setting at the start of `<head>`. The one named `.links` points every
    /// link that carries `data-fixture` at a fixed path. The one named
    /// `.broken` asks for a selector that does not parse.
    ///
    /// It supplies two serve middleware as well. The one named `.reader`
    /// writes a marker ahead of the script bundle, saying which origin it was
    /// told of and whether the request stand-in left its mark, and a script
    /// straight after the bundle. The one named `.broken-reader` asks for a
    /// selector that does not parse.
    pub(crate) mod middleware_fixture {
        use std::rc::Rc;
        use std::sync::Arc;

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{AttributeRewriteAction, IntegrationRegistration};
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::middleware::{
            AttributeRewrite, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
            Preparation, Refusal,
        };
        use crate::module_context::ModuleCall;
        use crate::settings::Settings;

        /// The integration id the stand-in registers under.
        pub(crate) const ID: &str = "middleware_fixture";
        /// The name a test's settings select the stand-in by, in `[testing]`.
        pub(crate) const MODULE: &str = "testing.middleware-fixture";
        /// The name of the middleware that marks the head.
        pub(crate) const HEAD: &str = MODULE;
        /// The name of the middleware that moves links.
        pub(crate) const LINKS: &str = "testing.middleware-fixture.links";
        /// The name of the middleware whose selector does not parse.
        pub(crate) const BROKEN: &str = "testing.middleware-fixture.broken";
        /// The name of the serve middleware that marks a reader's copy.
        pub(crate) const READER: &str = "testing.middleware-fixture.reader";
        /// The name of the serve middleware whose selector does not parse.
        pub(crate) const BROKEN_READER: &str = "testing.middleware-fixture.broken-reader";
        /// What the reader middleware writes straight after the bundle.
        pub(crate) const READER_SCRIPT: &str = "<script data-fixture-reader></script>";
        /// Where the links middleware points a link.
        pub(crate) const LINK_TARGET: &str = "/fixture/link";
        /// The selector the broken middleware asks for.
        pub(crate) const BROKEN_SELECTOR: &str = "a[";
        /// The name of the fetch middleware that works on `text/plain`.
        pub(crate) const TEXT: &str = "testing.middleware-fixture.text";
        /// The media type the text middleware works on.
        pub(crate) const TEXT_MEDIA_TYPE: &str = "text/plain";
        /// The line the text middleware adds at the end of a text body.
        pub(crate) const TEXT_LINE: &str = "# added by the middleware fixture\n";
        /// The name of the fetch middleware that prepares.
        pub(crate) const PREPARING: &str = "testing.middleware-fixture.preparing";
        /// The name of the fetch middleware that refuses every request.
        pub(crate) const REFUSING: &str = "testing.middleware-fixture.refusing";
        /// How long the refusing middleware asks the caller to wait.
        pub(crate) const REFUSAL_RETRY_AFTER: std::time::Duration =
            std::time::Duration::from_secs(30);
        /// What the refusing middleware says.
        pub(crate) const REFUSAL_MESSAGE: &str = "the middleware fixture refuses";
        /// What the preparing middleware writes when nothing was prepared.
        pub(crate) const NOTHING_PREPARED: &str =
            "<meta name=\"middleware-fixture-prepared\" content=\"nothing prepared\">";

        /// What the reader marker says when the preparation could read
        /// nothing of the reader.
        pub(crate) const NOTHING_OF_THE_READER: &str = "nothing of the reader";

        /// The marker the preparing middleware writes, naming the module it
        /// was called as and the host it was told of.
        pub(crate) fn prepared_marker(module: &str, request_host: &str) -> String {
            format!(
                "<meta name=\"middleware-fixture-prepared\" content=\"{module} for {request_host}\">"
            )
        }

        /// The marker the preparing middleware writes after
        /// [`prepared_marker`], saying what its preparation could read of the
        /// reader.
        pub(crate) fn prepared_reader_marker(seen: &str) -> String {
            format!("<meta name=\"middleware-fixture-reader\" content=\"{seen}\">")
        }

        /// What a preparation called with `call` can read of the reader,
        /// through the services or as a value of its own.
        fn reader_seen(call: &ModuleCall<'_>) -> String {
            use crate::consent::ConsentContext;
            use crate::ec::device::DeviceSignals;
            use crate::evidence::RequestInfo;
            use crate::geo::GeoInfo;
            use crate::module_context::{EdgeCookie, FromModuleContext};
            use crate::permissions::PermissionState;
            use crate::platform::{ClientInfo, RuntimeServices};

            let mut seen = Vec::new();
            if let Ok(services) = <&RuntimeServices>::from_module_context(call) {
                let client = services.client_info();
                if let Some(ip) = client.client_ip {
                    seen.push(format!("client IP {ip}"));
                }
                if client.tls_protocol.is_some()
                    || client.tls_cipher.is_some()
                    || client.tls_ja4.is_some()
                    || client.h2_fingerprint.is_some()
                {
                    seen.push("TLS details".to_owned());
                }
                if services.host_signals().is_some() {
                    seen.push("client signals".to_owned());
                }
            }
            if let Ok(evidence) = <&dyn RequestInfo>::from_module_context(call) {
                seen.push(format!(
                    "evidence with user-agent {}",
                    evidence.user_agent()
                ));
            }
            for (value, handed) in [
                (
                    "connection",
                    <&ClientInfo>::from_module_context(call).is_ok(),
                ),
                (
                    "consent",
                    <&ConsentContext>::from_module_context(call).is_ok(),
                ),
                (
                    "permissions",
                    <&PermissionState>::from_module_context(call).is_ok(),
                ),
                ("geo", <&GeoInfo>::from_module_context(call).is_ok()),
                (
                    "device signals",
                    <&DeviceSignals>::from_module_context(call).is_ok(),
                ),
                ("Edge Cookie", EdgeCookie::from_module_context(call).is_ok()),
            ] {
                if handed {
                    seen.push(value.to_owned());
                }
            }
            if seen.is_empty() {
                NOTHING_OF_THE_READER.to_owned()
            } else {
                seen.join(", ")
            }
        }

        struct Preparing;

        impl Middleware for Preparing {
            fn middleware_id(&self) -> &'static str {
                PREPARING
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &MiddlewarePhase::ALL
            }

            fn prepare<'a>(
                &'a self,
                call: ModuleCall<'a>,
                context: &'a MiddlewareContext<'a>,
            ) -> Option<Preparation<'a>> {
                Some(Box::pin(async move {
                    let marker = prepared_marker(call.module(), context.request_host)
                        + &prepared_reader_marker(&reader_seen(&call));
                    Ok(Box::new(marker) as Box<dyn std::any::Any + Send>)
                }))
            }

            fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
                let marker: String = context
                    .take_prepared(PREPARING)
                    .unwrap_or_else(|| NOTHING_PREPARED.to_owned());
                MiddlewareAction {
                    head_inserts: vec![marker],
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct Refusing;

        impl Middleware for Refusing {
            fn middleware_id(&self) -> &'static str {
                REFUSING
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn prepare<'a>(
                &'a self,
                _call: ModuleCall<'a>,
                _context: &'a MiddlewareContext<'a>,
            ) -> Option<Preparation<'a>> {
                Some(Box::pin(async {
                    Err(Refusal::service_unavailable(
                        REFUSAL_RETRY_AFTER,
                        REFUSAL_MESSAGE,
                    ))
                }))
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction::pass()
            }
        }
        /// The name of the serve middleware that writes a header on a text
        /// response.
        pub(crate) const TEXT_HEADER: &str = "testing.middleware-fixture.text-header";
        /// The header the text header middleware writes, name and value.
        pub(crate) const TEXT_HEADER_NAME: &str = "x-fixture-text";
        pub(crate) const TEXT_HEADER_VALUE: &str = "served";

        /// The builder core's test build lists beside its own.
        pub(crate) const BUILDER: IntegrationBuilder =
            IntegrationBuilder::new(ID, CORE_SOURCE, register, validate).with_module_name(MODULE);

        #[derive(Debug, serde::Deserialize, validator::Validate)]
        #[serde(deny_unknown_fields)]
        struct FixtureSettings {
            #[serde(default)]
            label: String,
        }

        impl crate::settings::IntegrationConfig for FixtureSettings {}

        /// The marker the head middleware writes for `label`.
        pub(crate) fn head_marker(label: &str) -> String {
            format!("<meta name=\"middleware-fixture\" content=\"{label}\">")
        }

        struct Head {
            label: String,
        }

        impl Middleware for Head {
            fn middleware_id(&self) -> &'static str {
                HEAD
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    head_inserts: vec![head_marker(&self.label)],
                    ..MiddlewareAction::pass()
                }
            }
        }

        /// The marker the reader middleware writes when told of `origin_host`,
        /// for a request the request stand-in marked or did not.
        pub(crate) fn reader_marker(origin_host: &str, marked: bool) -> String {
            format!(
                "<meta name=\"middleware-fixture-reader\" content=\"origin {origin_host}; marked \
                 {marked}\">"
            )
        }

        struct Reader;

        impl Middleware for Reader {
            fn middleware_id(&self) -> &'static str {
                READER
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Serve]
            }

            fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
                use super::request_fixture;

                let marked = context
                    .document_state
                    .get::<request_fixture::Mark>(request_fixture::ID)
                    .is_some();
                MiddlewareAction {
                    head_inserts: vec![reader_marker(context.origin_host, marked)],
                    after_bundle_inserts: vec![READER_SCRIPT.to_owned()],
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct BrokenReader;

        impl Middleware for BrokenReader {
            fn middleware_id(&self) -> &'static str {
                BROKEN_READER
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Serve]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    element_handlers: vec![Box::new(AttributeRewrite::matching(
                        BROKEN_SELECTOR,
                        "href",
                        Rc::new(|_matched| AttributeRewriteAction::keep()),
                    ))],
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct Links;

        impl Middleware for Links {
            fn middleware_id(&self) -> &'static str {
                LINKS
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    element_handlers: vec![Box::new(AttributeRewrite::matching(
                        "a[data-fixture]",
                        "href",
                        Rc::new(|_matched| AttributeRewriteAction::replace(LINK_TARGET)),
                    ))],
                    ..MiddlewareAction::pass()
                }
            }
        }

        /// Passes a text body through and adds a line at its end.
        struct AppendLine;

        impl crate::streaming_processor::StreamProcessor for AppendLine {
            fn process_chunk(
                &mut self,
                chunk: &[u8],
                is_last: bool,
            ) -> Result<Vec<u8>, std::io::Error> {
                let mut output = chunk.to_vec();
                if is_last {
                    output.extend_from_slice(TEXT_LINE.as_bytes());
                }
                Ok(output)
            }
        }

        struct Text;

        impl Middleware for Text {
            fn middleware_id(&self) -> &'static str {
                TEXT
            }

            fn handles_media_type(&self, media_type: &str) -> bool {
                media_type == TEXT_MEDIA_TYPE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    stream: Some(Box::new(AppendLine)),
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct TextHeader;

        impl Middleware for TextHeader {
            fn middleware_id(&self) -> &'static str {
                TEXT_HEADER
            }

            fn handles_media_type(&self, media_type: &str) -> bool {
                media_type == TEXT_MEDIA_TYPE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Serve]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    response_headers: vec![(
                        http::HeaderName::from_static(TEXT_HEADER_NAME),
                        http::HeaderValue::from_static(TEXT_HEADER_VALUE),
                    )],
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct Broken;

        impl Middleware for Broken {
            fn middleware_id(&self) -> &'static str {
                BROKEN
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                MiddlewareAction {
                    element_handlers: vec![Box::new(AttributeRewrite::matching(
                        BROKEN_SELECTOR,
                        "href",
                        Rc::new(|_matched| AttributeRewriteAction::keep()),
                    ))],
                    ..MiddlewareAction::pass()
                }
            }
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            let Some(config) = settings.module_config::<FixtureSettings>(MODULE)? else {
                return Ok(None);
            };
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .without_js()
                    .with_middleware(Arc::new(Head {
                        label: config.label,
                    }))
                    .with_middleware(Arc::new(Links))
                    .with_middleware(Arc::new(Broken))
                    .with_middleware(Arc::new(Reader))
                    .with_middleware(Arc::new(BrokenReader))
                    .with_middleware(Arc::new(Text))
                    .with_middleware(Arc::new(Preparing))
                    .with_middleware(Arc::new(Refusing))
                    .with_middleware(Arc::new(TextHeader))
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
    /// Selected, it supplies one fetch middleware under its own name, which
    /// writes one script in `<head>` ahead of the bundle and rewrites the
    /// address of its own script to a first-party path. With `mark_bundle`
    /// set it also asks for an attribute on the publisher bundle tag.
    pub(crate) mod tag_fixture {
        use std::rc::Rc;
        use std::sync::Arc;

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{AttributeRewriteAction, IntegrationRegistration};
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::middleware::{
            AttributeRewrite, AttributeRewriteFn, Middleware, MiddlewareAction, MiddlewareContext,
            MiddlewarePhase,
        };
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

        struct Tag;

        impl Middleware for Tag {
            fn middleware_id(&self) -> &'static str {
                MODULE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
                let decide: Rc<AttributeRewriteFn> = Rc::new(|matched| {
                    if matched.value == SCRIPT_URL {
                        AttributeRewriteAction::Replace(FIRST_PARTY_SCRIPT.to_owned())
                    } else {
                        AttributeRewriteAction::Keep
                    }
                });
                MiddlewareAction {
                    head_inserts: vec![format!("<script>{HEAD_FLAG}</script>")],
                    element_handlers: vec![Box::new(AttributeRewrite::new("src", decide))],
                    ..MiddlewareAction::pass()
                }
            }
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            let Some(config) = settings.module_config::<FixtureSettings>(MODULE)? else {
                return Ok(None);
            };
            let mut registration = IntegrationRegistration::builder(ID)
                .without_js()
                .with_middleware(Arc::new(Tag));
            if config.mark_bundle {
                registration = registration.with_bundle_tag_attribute(BUNDLE_ATTRIBUTE, "true");
            }
            Ok(Some(registration.build()))
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
    /// Selected, it supplies one fetch middleware under its own name. Its
    /// text handler swaps the payload of each `fixture_payload("...")` call
    /// for a placeholder that carries a namespace made per document, and
    /// keeps the payload. Its stream processor swaps each placeholder back
    /// for its payload with the origin host rewritten. A payload pushed with
    /// `fixture_payload_open` leaves its group unresolved, and the processor
    /// holds its output from that placeholder on until a
    /// `fixture_payload_close` arrives.
    pub(crate) mod payload_fixture {
        use std::io;
        use std::sync::{Arc, Mutex, PoisonError};

        use error_stack::Report;

        use crate::error::TrustedServerError;
        use crate::integrations::registry::{IntegrationRegistration, ScriptRewriteAction};
        use crate::integrations::{CORE_SOURCE, IntegrationBuilder};
        use crate::middleware::{
            Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, TextHandler,
        };
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

        /// What one document's text handler has captured so far.
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

        /// The scripts of one document, each held until its last chunk.
        struct Scripts {
            captured: Arc<Mutex<Captured>>,
            held: String,
        }

        impl Scripts {
            /// Swaps the payload of a whole script for a placeholder, or
            /// leaves a script that pushes no payload as it is.
            fn rewrite_whole(&self, script: &str) -> Option<String> {
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

                let mut captured = self.captured.lock().unwrap_or_else(PoisonError::into_inner);
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

        impl TextHandler for Scripts {
            fn selector(&self) -> &str {
                "script"
            }

            fn decide(&mut self, text: &str, is_last: bool) -> ScriptRewriteAction {
                let claimed = !self.held.is_empty() || text.contains("fixture_payload");
                if !claimed {
                    return ScriptRewriteAction::Keep;
                }
                self.held.push_str(text);
                if !is_last {
                    return ScriptRewriteAction::RemoveNode;
                }
                let script = std::mem::take(&mut self.held);
                let rewritten = self.rewrite_whole(&script).unwrap_or(script);
                ScriptRewriteAction::replace(rewritten)
            }
        }

        struct Payloads;

        impl Middleware for Payloads {
            fn middleware_id(&self) -> &'static str {
                MODULE
            }

            fn phases(&self) -> &[MiddlewarePhase] {
                &[MiddlewarePhase::Fetch]
            }

            /// The text handler and the stream processor of one document
            /// share what the handler captures.
            fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
                let captured = Arc::new(Mutex::new(Captured::default()));
                MiddlewareAction {
                    text_handlers: vec![Box::new(Scripts {
                        captured: Arc::clone(&captured),
                        held: String::new(),
                    })],
                    stream: Some(Box::new(Processor {
                        captured,
                        origin_host: context.origin_host.to_owned(),
                        request_host: context.request_host.to_owned(),
                        held: Vec::new(),
                    })),
                    ..MiddlewareAction::pass()
                }
            }
        }

        struct Processor {
            captured: Arc<Mutex<Captured>>,
            origin_host: String,
            request_host: String,
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
                let captured = self.captured.lock().unwrap_or_else(PoisonError::into_inner);
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
                                    .replace(&self.origin_host, &self.request_host)
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

        /// Has `settings` select the stand-in and run its middleware, and no
        /// other, on every page.
        pub(crate) fn select_and_place(settings: &mut Settings) {
            settings.select_module("testing", MODULE);
            crate::html_processor::test_support::place_on_every_page(
                settings,
                MiddlewarePhase::Fetch,
                &[MODULE],
            );
        }

        fn register(
            settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            if settings.module_config::<FixtureSettings>(MODULE)?.is_none() {
                return Ok(None);
            }
            Ok(Some(
                IntegrationRegistration::builder(ID)
                    .with_middleware(Arc::new(Payloads))
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
    use super::test_support::middleware_fixture as fixture;
    use super::test_support::{
        PROBE_JS, PROBE_JS_SHA256, carried_probe_registration, probe_registration, validate_nothing,
    };
    use super::*;
    use crate::constants::COOKIE_EC;
    use crate::middleware::{HTML_MEDIA_TYPE, MiddlewareAction, MiddlewareContext, PhaseEntry};
    use crate::permissions::{Permission, PermissionSet, PermissionState};
    use crate::platform::test_support::noop_services;
    use http::{HeaderValue, StatusCode, header};

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
            _call: crate::module_context::ModuleCall<'_>,
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
            _call: crate::module_context::ModuleCall<'_>,
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
            _call: crate::module_context::ModuleCall<'_>,
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

    /// A route that answers with what its module call handed it.
    struct StateRoute {
        declared: PermissionSet,
    }

    impl StateRoute {
        fn report(
            &self,
            _req: Request<EdgeBody>,
            request: crate::module_context::ModuleRequest<'_>,
            permissions: &PermissionState,
            edge_cookie: Option<crate::module_context::EdgeCookie<'_>>,
        ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
            let body = format!(
                "{} storage={} id={}",
                request.host(),
                permissions.is_set(Permission::StoreOnDevice),
                edge_cookie.map_or("none", |id| id.as_str()),
            );
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from(body))
                .expect("should build the response"))
        }
    }

    #[async_trait(?Send)]
    impl IntegrationProxy for StateRoute {
        fn integration_name(&self) -> &'static str {
            "state"
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            vec![IntegrationEndpoint::get("/integrations/test/state")]
        }

        fn required_permissions(&self) -> PermissionSet {
            self.declared
        }

        async fn handle(
            &self,
            call: ModuleCall<'_>,
            req: Request<EdgeBody>,
        ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
            call.inject_with(self, req, Self::report)?
        }
    }

    /// A route is handed the permissions resolved for its request, and the
    /// request's Edge Cookie identifier when it declares the permission the
    /// identifier's use needs.
    #[test]
    fn a_route_is_handed_its_requests_permissions_and_identity() {
        let settings = create_test_settings();
        let storage = PermissionSet::none().with(Permission::StoreOnDevice);
        let answer = |declared: PermissionSet| {
            let registry = IntegrationRegistry::from_routes(vec![(
                Method::GET,
                "/integrations/test/state",
                (
                    Arc::new(StateRoute { declared }) as Arc<dyn IntegrationProxy>,
                    "state",
                ),
            )]);
            let req = Request::builder()
                .method(Method::GET)
                .uri("https://test-publisher.com/integrations/test/state")
                .header(header::HOST, "test-publisher.com")
                .body(EdgeBody::empty())
                .expect("should build request");
            let mut ec_context = EcContext::new_for_test(
                Some("an-id".to_owned()),
                crate::consent::ConsentContext::default(),
            )
            .with_module_for_test(Arc::new(crate::ec::module::HmacModule::new(
                crate::redacted::Redacted::new("test-secret-key-32-bytes-minimum".to_owned()),
            )));
            let services = noop_services();
            let response = futures::executor::block_on(registry.handle_proxy(ProxyDispatchInput {
                method: &Method::GET,
                path: "/integrations/test/state",
                settings: &settings,
                kv: None,
                ec_context: &mut ec_context,
                services: &services,
                req,
            }))
            .expect("should find the route")
            .expect("the route should answer");
            String::from_utf8(
                response
                    .into_body()
                    .into_bytes()
                    .unwrap_or_default()
                    .to_vec(),
            )
            .expect("the answer is text")
        };

        assert_eq!(
            answer(storage),
            "test-publisher.com storage=true id=an-id",
            "a route declaring storage is handed the identifier"
        );
        assert_eq!(
            answer(PermissionSet::none()),
            "test-publisher.com storage=true id=none",
            "and one declaring nothing still sees the permissions but not the identifier"
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
                "ts-{COOKIE_EC}={}",
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
    fn js_module_ids_defer_a_deferred_module_and_include_core_js_only_modules() {
        let mut settings = crate::test_support::tests::create_test_settings();
        enable_deferred_module(&mut settings);

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

        let all = registry.js_module_ids();
        let immediate = registry.js_module_ids_immediate();
        let deferred = registry.js_module_ids_deferred();
        let module = test_support::deferred_fixture::ID;

        assert!(
            all.contains(&module),
            "should include the deferred module in the module IDs"
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
            !immediate.contains(&module),
            "should not include the deferred module in immediate IDs"
        );
        assert!(
            deferred.contains(&module),
            "should serve the module as a deferred module"
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
    fn js_module_ids_split_is_exhaustive() {
        let mut settings = crate::test_support::tests::create_test_settings();
        enable_deferred_module(&mut settings);

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");

        let all = registry.js_module_ids();
        assert!(
            !registry.js_module_ids_deferred().is_empty(),
            "should have a deferred module to divide from the bundle"
        );
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

    const SHARED_VENDOR: &str = "example";

    fn shared_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("shared_probe").build(),
        ))
    }

    /// The `shared_probe` builder, run by `[shared.example]`.
    fn shared_probe_builder() -> crate::integrations::IntegrationBuilder {
        crate::integrations::IntegrationBuilder::new(
            "shared_probe",
            "seam-probe",
            shared_probe_registration,
            validate_nothing,
        )
        .with_shared_settings(SHARED_VENDOR)
    }

    fn with_shared_table(mut settings: Settings) -> Settings {
        settings.shared =
            serde_json::from_value(serde_json::json!({ SHARED_VENDOR: { "key": "value" } }))
                .expect("should read the shared tables");
        settings
    }

    #[test]
    fn a_builder_its_shared_table_runs_is_built_only_when_the_table_is_present() {
        let extra = [shared_probe_builder()];

        let without = IntegrationRegistry::with_registrations(
            &crate::test_support::tests::create_test_settings(),
            &extra,
        )
        .expect("nothing is built, which is not an error");
        assert!(!without.integration_runs("shared_probe"));

        let with = IntegrationRegistry::with_registrations(
            &with_shared_table(crate::test_support::tests::create_test_settings()),
            &extra,
        )
        .expect("should build the builder its table runs");
        assert!(with.integration_runs("shared_probe"));
    }

    #[test]
    fn a_shared_table_no_builder_reads_is_refused() {
        let refused = IntegrationRegistry::with_registrations(
            &with_shared_table(crate::test_support::tests::create_test_settings()),
            &[],
        )
        .err()
        .expect("nothing reads the table")
        .to_string();

        assert!(
            refused.contains("[shared.example] is read by no module"),
            "{refused}"
        );
    }

    #[test]
    fn a_builder_its_shared_table_runs_answers_to_no_selection() {
        let refused = IntegrationRegistry::with_registrations(
            &with_shared_table(settings_naming("shared_probe")),
            &[shared_probe_builder()],
        )
        .err()
        .expect("the table runs the builder, so no section can select it")
        .to_string();

        assert!(refused.contains("selects `shared_probe`"), "{refused}");
    }

    #[test]
    fn a_selection_nothing_supplies_names_the_shared_table_that_is_absent() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.geo.module = Some("example".to_owned());

        let refused = IntegrationRegistry::with_registrations(&settings, &[shared_probe_builder()])
            .err()
            .expect("no geo module is supplied")
            .to_string();

        assert!(
            refused.contains("There is no [shared.example]"),
            "the message has to name the table to add: {refused}"
        );
    }

    #[test]
    fn a_list_selection_nothing_supplies_names_the_shared_table_that_is_absent() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.select_module("identity", "example.id");

        let refused = IntegrationRegistry::with_registrations(&settings, &[shared_probe_builder()])
            .err()
            .expect("no identity module is supplied")
            .to_string();

        assert!(
            refused.contains("[identity] selects `example.id`")
                && refused.contains("There is no [shared.example]"),
            "the message has to name the table to add: {refused}"
        );
    }

    #[test]
    fn a_builder_its_shared_table_runs_is_refused_a_module_name() {
        let mut settings = with_shared_table(crate::test_support::tests::create_test_settings());
        settings.geo.module = Some("example".to_owned());

        let refused = IntegrationRegistry::with_registrations(
            &settings,
            &[shared_probe_builder().with_module_name("geo.example")],
        )
        .err()
        .expect("a section could select what the table runs")
        .to_string();

        assert!(
            refused.contains("is run by [shared.example] and also names the module `geo.example`"),
            "{refused}"
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

    /// A builder that registers from the auction plan holds its id like any
    /// other, so a second builder claiming it is refused, naming both sources,
    /// whether or not the plan-backed integration runs.
    #[test]
    fn with_registrations_rejects_a_second_builder_claiming_a_plan_backed_id() {
        fn register_nothing(
            _settings: &Settings,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            Ok(None)
        }
        fn nothing_from_the_plan(
            _settings: &Settings,
            _plan: &crate::auction::plan::AuctionPlan,
        ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
            Ok(None)
        }

        let settings = crate::test_support::tests::create_test_settings();
        let id = "plan_backed";
        let extra = [
            crate::integrations::IntegrationBuilder::new(
                id,
                "plan-backed-crate",
                register_nothing,
                validate_nothing,
            )
            .with_plan_registration(nothing_from_the_plan),
            crate::integrations::IntegrationBuilder::new(
                id,
                "seam-probe",
                register_nothing,
                validate_nothing,
            ),
        ];

        let Err(error) = IntegrationRegistry::with_registrations(&settings, &extra) else {
            panic!("should refuse a second builder claiming `{id}`");
        };

        let message = error.to_string();
        assert!(
            message.contains(id)
                && message.contains("plan-backed-crate")
                && message.contains("seam-probe"),
            "error should name `{id}` and both sources: {message}"
        );
    }

    /// A path outside `/integrations/` that the route clash test declares.
    const OWN_ENDPOINT_PATH: &str = "/own-endpoint";

    /// A proxy that declares the routes it holds and answers each with an
    /// empty body.
    struct DeclaredRoutes {
        name: &'static str,
        routes: Vec<IntegrationEndpoint>,
    }

    #[async_trait(?Send)]
    impl IntegrationProxy for DeclaredRoutes {
        fn integration_name(&self) -> &'static str {
            self.name
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            self.routes.clone()
        }

        async fn handle(
            &self,
            _call: crate::module_context::ModuleCall<'_>,
            _req: Request<EdgeBody>,
        ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
            Ok(Response::new(EdgeBody::empty()))
        }
    }

    /// A registration under `id` whose proxy declares `methods` on
    /// [`OWN_ENDPOINT_PATH`], one route for each.
    fn declaring(id: &'static str, methods: &[Method]) -> Option<IntegrationRegistration> {
        let routes = methods
            .iter()
            .map(|method| IntegrationEndpoint::new(method.clone(), OWN_ENDPOINT_PATH))
            .collect();
        Some(
            IntegrationRegistration::builder(id)
                .with_proxy(Arc::new(DeclaredRoutes { name: id, routes }))
                .build(),
        )
    }

    /// A test module, as its integration id, its module name and its build
    /// function.
    type RouteModule = (
        &'static str,
        &'static str,
        crate::integrations::IntegrationBuilderFn,
    );

    const GETS: RouteModule = ("gets", "testing.gets", |_| {
        Ok(declaring("gets", &[Method::GET]))
    });
    const ALSO_GETS: RouteModule = ("also-gets", "testing.also-gets", |_| {
        Ok(declaring("also-gets", &[Method::GET]))
    });
    const GETS_TWICE: RouteModule = ("gets-twice", "testing.gets-twice", |_| {
        Ok(declaring("gets-twice", &[Method::GET, Method::GET]))
    });
    const POSTS: RouteModule = ("posts", "testing.posts", |_| {
        Ok(declaring("posts", &[Method::POST]))
    });

    /// Builds a registry from `modules`, with every one of them selected.
    fn registry_selecting(
        modules: &[RouteModule],
    ) -> Result<IntegrationRegistry, Report<TrustedServerError>> {
        let mut settings = crate::test_support::tests::create_test_settings();
        let mut builders = Vec::new();
        for &(id, module, register) in modules {
            settings.select_module("testing", module);
            builders.push(
                crate::integrations::IntegrationBuilder::new(
                    id,
                    "route-clash-test",
                    register,
                    validate_nothing,
                )
                .with_module_name(module),
            );
        }
        IntegrationRegistry::with_registrations(&settings, &builders)
    }

    #[test]
    fn two_routes_on_one_method_and_path_are_refused_when_the_registry_is_built() {
        let expected = format!("Integration route registration failed for GET {OWN_ENDPOINT_PATH}");
        // Two modules declaring one route each, and one module declaring the
        // route twice.
        for modules in [&[GETS, ALSO_GETS][..], &[GETS_TWICE]] {
            let Err(error) = registry_selecting(modules) else {
                panic!("should refuse a second GET route on {OWN_ENDPOINT_PATH}");
            };
            let message = error.to_string();
            assert!(
                message.contains(&expected),
                "should name the method and the path: {message}"
            );
        }

        let registry =
            registry_selecting(&[GETS, POSTS]).expect("should accept two methods on one path");
        let routed_to = |method: &Method| {
            registry
                .find_route(method, OWN_ENDPOINT_PATH)
                .map(|(_, integration)| *integration)
        };
        assert_eq!(
            (routed_to(&Method::GET), routed_to(&Method::POST)),
            (Some("gets"), Some("posts")),
            "should route each method to the module that declares it"
        );
    }

    /// Selects core's stand-in, which registers a deferred browser module.
    fn enable_deferred_module(settings: &mut Settings) {
        settings.select_module("testing", test_support::deferred_fixture::MODULE);
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
        // none registers without a browser script.
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
        enable_deferred_module(&mut settings);
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
            test_support::deferred_fixture::ID,
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

    // Each probe supplies a middleware that changes nothing, so a test can
    // read the order registrations were made in from the names the registry
    // lists.
    const SECTION_MIDDLEWARE: &str = "testing.probe-section";
    const PLAN_MIDDLEWARE: &str = "testing.probe-plan";

    fn section_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("probe-section")
                .with_middleware(Arc::new(Named {
                    id: SECTION_MIDDLEWARE,
                    phases: FETCH_ONLY,
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
                .with_middleware(Arc::new(Named {
                    id: PLAN_MIDDLEWARE,
                    phases: FETCH_ONLY,
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
        let probe_middleware = |registry: &IntegrationRegistry| {
            registry
                .middleware_ids()
                .into_iter()
                .filter(|name| *name == SECTION_MIDDLEWARE || *name == PLAN_MIDDLEWARE)
                .collect::<Vec<_>>()
        };

        settings.auction.enabled = false;
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");
        assert!(
            !registry.integration_runs("probe-plan"),
            "should register nothing where the function finds nothing in the plan"
        );
        assert_eq!(probe_middleware(&registry), vec![SECTION_MIDDLEWARE]);

        settings.auction.enabled = true;
        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build registry");
        assert!(
            registry.integration_runs("probe-plan"),
            "should register from the plan a module no section selects"
        );
        assert_eq!(
            probe_middleware(&registry),
            vec![PLAN_MIDDLEWARE, SECTION_MIDDLEWARE],
            "should register what the plan supplies ahead of a section's module"
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
            _call: crate::module_context::ModuleCall<'_>,
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

    /// The names the two prefix test modules run under.
    const PREFIX_PROBE_MODULE: &str = "testing.prefix-probe";
    const PREFIX_READER_MODULE: &str = "testing.prefix-reader";

    /// A prefix module giving `_ex` and `ex.<domain>`, so a test can tell it
    /// apart from core's own.
    #[derive(Debug)]
    struct ExamplePrefix;

    impl ExamplePrefix {
        fn server_key(&self, settings: &Settings) -> crate::platform::ServerKeyPrefix {
            crate::platform::ServerKeyPrefix::new("ex", &settings.publisher.domain)
        }
    }

    impl PrefixModule for ExamplePrefix {
        fn public_prefix(
            &self,
            _call: ModuleCall<'_>,
        ) -> Result<crate::prefix::PublicPrefix, crate::module_context::Withheld> {
            Ok(crate::prefix::PublicPrefix::new(["_ex"]).expect("should accept the prefix"))
        }

        fn server_key_prefix(
            &self,
            call: ModuleCall<'_>,
        ) -> Result<crate::platform::ServerKeyPrefix, crate::module_context::Withheld> {
            call.inject(self, Self::server_key)
        }
    }

    /// A prefix module naming the reader's evidence, which its call never
    /// carries.
    #[derive(Debug)]
    struct EvidenceReadingPrefix;

    impl EvidenceReadingPrefix {
        fn public(
            &self,
            evidence: &dyn crate::evidence::RequestInfo,
        ) -> crate::prefix::PublicPrefix {
            crate::prefix::PublicPrefix::new([evidence.user_agent()])
                .expect("should accept the prefix")
        }
    }

    impl PrefixModule for EvidenceReadingPrefix {
        fn public_prefix(
            &self,
            call: ModuleCall<'_>,
        ) -> Result<crate::prefix::PublicPrefix, crate::module_context::Withheld> {
            call.inject(self, Self::public)
        }

        fn server_key_prefix(
            &self,
            _call: ModuleCall<'_>,
        ) -> Result<crate::platform::ServerKeyPrefix, crate::module_context::Withheld> {
            Ok(crate::platform::ServerKeyPrefix::new("ev", "example.com"))
        }
    }

    fn prefix_probe_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("prefix-probe")
                .with_prefix_module(PREFIX_PROBE_MODULE, Arc::new(ExamplePrefix))
                .build(),
        ))
    }

    fn prefix_reader_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("prefix-reader")
                .with_prefix_module(PREFIX_READER_MODULE, Arc::new(EvidenceReadingPrefix))
                .build(),
        ))
    }

    fn prefix_probe_builders() -> [crate::integrations::IntegrationBuilder; 2] {
        [
            crate::integrations::IntegrationBuilder::new(
                "prefix-probe",
                "seam-probe",
                prefix_probe_registration,
                validate_nothing,
            )
            .with_module_name(PREFIX_PROBE_MODULE),
            crate::integrations::IntegrationBuilder::new(
                "prefix-reader",
                "seam-probe",
                prefix_reader_registration,
                validate_nothing,
            )
            .with_module_name(PREFIX_READER_MODULE),
        ]
    }

    /// Settings running both probe modules on `www.example.com`, with
    /// `[prefix] module` naming `selected` when given.
    fn settings_selecting_prefix_module(selected: Option<&str>) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.publisher.domain = "www.example.com".to_owned();
        for id in ["prefix-probe", "prefix-reader"] {
            settings
                .insert_module_config("testing", &format!("testing.{id}"), &serde_json::json!({}))
                .expect("should select the test module");
        }
        settings.prefix.module = selected.map(str::to_owned);
        settings
    }

    #[test]
    fn with_no_prefix_module_selected_the_registry_gives_core_ts() {
        let settings = settings_selecting_prefix_module(None);
        let registry = IntegrationRegistry::with_registrations(&settings, &prefix_probe_builders())
            .expect("should build the registry");

        let prefixes =
            registry.prefixes(&settings, crate::module_context::test_support::request("/"));

        assert_eq!(registry.prefix_module().name(), "ts");
        assert_eq!(prefixes.public().as_str(), "ts");
        assert_eq!(prefixes.public().cookie_name("ec"), "ts-ec");
        assert_eq!(prefixes.server_key().as_str(), "ts.example.com");
    }

    #[test]
    fn a_prefix_selector_naming_ts_selects_cores_own() {
        let settings = settings_selecting_prefix_module(Some("ts"));
        let registry = IntegrationRegistry::with_registrations(&settings, &prefix_probe_builders())
            .expect("should accept the name the registry reports core's module by");

        let prefixes =
            registry.prefixes(&settings, crate::module_context::test_support::request("/"));

        assert_eq!(registry.prefix_module().name(), "ts");
        assert_eq!(prefixes.public().cookie_name("ec"), "ts-ec");
        assert_eq!(prefixes.server_key().as_str(), "ts.example.com");
    }

    #[test]
    fn the_selected_prefix_module_gives_the_prefixes() {
        let settings = settings_selecting_prefix_module(Some(PREFIX_PROBE_MODULE));
        let registry = IntegrationRegistry::with_registrations(&settings, &prefix_probe_builders())
            .expect("should build the registry");

        let prefixes =
            registry.prefixes(&settings, crate::module_context::test_support::request("/"));

        assert_eq!(registry.prefix_module().name(), PREFIX_PROBE_MODULE);
        assert_eq!(prefixes.public().cookie_name("ec"), "_ex-ec");
        assert_eq!(prefixes.server_key().as_str(), "ex.example.com");
    }

    #[test]
    fn a_prefix_module_nothing_supplies_is_refused() {
        let settings = settings_selecting_prefix_module(Some("missing"));

        let error = IntegrationRegistry::with_registrations(&settings, &prefix_probe_builders())
            .err()
            .expect("a prefix module nothing supplies should be refused");

        assert!(
            error.to_string().contains("[prefix] module") && error.to_string().contains("missing"),
            "{error}"
        );
    }

    #[test]
    fn a_prefix_module_naming_what_its_call_never_carries_is_refused() {
        let settings = settings_selecting_prefix_module(Some(PREFIX_READER_MODULE));

        let error = IntegrationRegistry::with_registrations(&settings, &prefix_probe_builders())
            .err()
            .expect("a prefix module that cannot answer should be refused at startup");

        assert!(
            error.to_string().contains(PREFIX_READER_MODULE)
                && error.to_string().contains("request evidence"),
            "{error}"
        );
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
            ("auction", "fixture"),
            ("auction", "ad-server.fixture"),
            ("auction", "plain-fixture"),
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
            _call: crate::module_context::ModuleCall<'_>,
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
            _call: crate::module_context::ModuleCall<'_>,
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

    fn html_entry(path: Option<&str>, names: &[&str]) -> PhaseEntry {
        PhaseEntry {
            media_type: HTML_MEDIA_TYPE.to_owned(),
            path: path.map(str::to_owned),
            middleware: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// Settings that select core's middleware stand-in and hold `entries`.
    fn fixture_settings_with_entries(entries: Vec<PhaseEntry>) -> Settings {
        let mut settings = settings_naming(fixture::MODULE);
        settings.fetch = PhaseEntries::new(entries);
        settings
    }

    #[test]
    fn a_selected_module_s_middleware_run_where_an_entry_names_them() {
        let settings = fixture_settings_with_entries(vec![
            html_entry(Some("/news/"), &[fixture::LINKS, fixture::HEAD]),
            html_entry(None, &[fixture::HEAD]),
        ]);

        let registry = IntegrationRegistry::new(&settings)
            .expect("should build a registry whose entries name registered middleware");

        assert_eq!(
            registry.middleware_ids(),
            [
                fixture::HEAD,
                fixture::LINKS,
                fixture::BROKEN,
                fixture::READER,
                fixture::BROKEN_READER,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING,
                fixture::TEXT_HEADER,
                crate::service_version::MODULE,
            ],
            "should list the middleware the selected module supplies, and core's own after them"
        );
        assert_eq!(
            registry.middleware_in(MiddlewarePhase::Fetch),
            [
                fixture::HEAD,
                fixture::LINKS,
                fixture::BROKEN,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING
            ],
            "should list the middleware that run in the fetch phase"
        );
        assert_eq!(
            registry.middleware_in(MiddlewarePhase::Serve),
            [
                fixture::READER,
                fixture::BROKEN_READER,
                fixture::PREPARING,
                fixture::TEXT_HEADER,
                crate::service_version::MODULE
            ],
            "should list the middleware that run in the serve phase, core's own last"
        );
        let chain_for = |media_type: &str, path: &str| {
            registry
                .middleware_chain(&settings.fetch, MiddlewarePhase::Fetch, media_type, path)
                .ids()
        };
        assert_eq!(
            chain_for(HTML_MEDIA_TYPE, "/news/today"),
            [fixture::LINKS, fixture::HEAD],
            "should run the first covering entry's middleware in the order it names them"
        );
        assert_eq!(chain_for(HTML_MEDIA_TYPE, "/"), [fixture::HEAD]);
        assert!(
            chain_for("text/css", "/news/site.css").is_empty(),
            "should run nothing on a media type no entry covers"
        );
    }

    /// An entry for `text/plain` naming the middleware that works on it.
    fn text_entry(names: &[&str]) -> PhaseEntry {
        PhaseEntry {
            media_type: fixture::TEXT_MEDIA_TYPE.to_owned(),
            path: None,
            middleware: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn an_entry_runs_a_middleware_on_the_media_type_it_handles() {
        let settings = fixture_settings_with_entries(vec![
            html_entry(None, &[fixture::HEAD]),
            text_entry(&[fixture::TEXT]),
        ]);

        let registry = IntegrationRegistry::new(&settings).expect("should build a registry");

        let chain_for = |media_type: &str| {
            registry
                .middleware_chain(
                    &settings.fetch,
                    MiddlewarePhase::Fetch,
                    media_type,
                    "/a.txt",
                )
                .ids()
        };
        assert_eq!(
            chain_for(fixture::TEXT_MEDIA_TYPE),
            [fixture::TEXT],
            "should run the text middleware on a text response"
        );
        assert_eq!(
            chain_for(HTML_MEDIA_TYPE),
            [fixture::HEAD],
            "should run the page middleware on a page and not the text one"
        );
    }

    #[test]
    fn an_entry_naming_a_middleware_under_a_media_type_it_does_not_handle_is_refused() {
        for (media_type, name) in [
            (fixture::TEXT_MEDIA_TYPE, fixture::HEAD),
            (HTML_MEDIA_TYPE, fixture::TEXT),
        ] {
            let settings = fixture_settings_with_entries(vec![PhaseEntry {
                media_type: media_type.to_owned(),
                path: None,
                middleware: vec![name.to_owned()],
            }]);

            let error = IntegrationRegistry::new(&settings)
                .err()
                .expect("should refuse a middleware under a media type it does not handle");

            let message = error.to_string();
            assert!(
                message.contains(&format!(
                    "[[fetch]] entry 1 covers `{media_type}` and names `{name}`, which does not \
                     work on that media type"
                )),
                "should name the entry, the media type and the middleware: {message}"
            );
        }
    }

    #[test]
    fn a_module_no_section_selects_supplies_no_middleware() {
        let settings = crate::test_support::tests::create_test_settings();

        let registry = IntegrationRegistry::new(&settings).expect("should build a registry");

        assert!(
            !registry.middleware_ids().contains(&fixture::HEAD),
            "should register nothing of a module that does not run"
        );
    }

    #[test]
    fn an_entry_naming_a_middleware_nothing_supplies_is_refused_with_what_is_supplied() {
        let settings = fixture_settings_with_entries(vec![
            html_entry(Some("/news/"), &[fixture::HEAD]),
            html_entry(None, &[fixture::HEAD, "testing.nothing"]),
        ]);

        let error = IntegrationRegistry::new(&settings)
            .err()
            .expect("should refuse an entry naming a middleware nothing supplies");

        let message = error.to_string();
        assert!(
            message.contains(
                "[[fetch]] entry 2 names `testing.nothing`, which no module that runs supplies"
            ) && message.contains(&format!(
                "[{}, {}, {}, {}, {}, {}, {}, {}, {}, {}]",
                fixture::HEAD,
                fixture::LINKS,
                fixture::BROKEN,
                fixture::READER,
                fixture::BROKEN_READER,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING,
                fixture::TEXT_HEADER,
                crate::service_version::MODULE
            )),
            "should name the entry and list what could be named: {message}"
        );
        assert!(
            !message.contains("no section selects"),
            "should not blame a selection when the name is no module's: {message}"
        );
    }

    #[test]
    fn an_entry_naming_the_middleware_of_a_module_no_section_selects_says_so() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.fetch = PhaseEntries::new(vec![html_entry(None, &[fixture::LINKS])]);

        let error = IntegrationRegistry::new(&settings)
            .err()
            .expect("should refuse an entry naming the middleware of a module that does not run");

        let message = error.to_string();
        assert!(
            message.contains(&format!("[[fetch]] entry 1 names `{}`", fixture::LINKS))
                && message.contains(&format!(
                    "`{}` is a module no section selects, so nothing it supplies is running",
                    fixture::MODULE
                )),
            "should say the module is not selected: {message}"
        );
    }

    #[test]
    fn a_middleware_no_entry_names_is_listed_for_the_startup_warning() {
        let unnamed = |entries: Vec<PhaseEntry>| -> Vec<&'static str> {
            let settings = fixture_settings_with_entries(entries);
            let registry = IntegrationRegistry::new(&settings).expect("should build a registry");
            unnamed_middleware(&settings, &registry.inner)
                .into_iter()
                .map(|(integration, middleware)| {
                    assert_eq!(integration, fixture::ID, "should name the supplier");
                    middleware.middleware_id()
                })
                .collect()
        };

        assert_eq!(
            unnamed(Vec::new()),
            [
                fixture::HEAD,
                fixture::LINKS,
                fixture::BROKEN,
                fixture::READER,
                fixture::BROKEN_READER,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING,
                fixture::TEXT_HEADER,
            ],
            "should list every middleware when there are no entries"
        );
        assert_eq!(
            unnamed(vec![
                html_entry(Some("/news/"), &[fixture::LINKS]),
                html_entry(None, &[fixture::HEAD]),
            ]),
            [
                fixture::BROKEN,
                fixture::READER,
                fixture::BROKEN_READER,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING,
                fixture::TEXT_HEADER
            ],
            "should leave out a middleware any entry names"
        );
    }

    #[test]
    fn an_entry_naming_a_middleware_in_a_phase_it_does_not_run_in_is_refused() {
        let mut settings = settings_naming(fixture::MODULE);
        settings.serve = PhaseEntries::new(vec![html_entry(None, &[fixture::HEAD])]);
        let error = IntegrationRegistry::new(&settings)
            .err()
            .expect("should refuse a fetch middleware named in a serve entry");
        let message = error.to_string();
        assert!(
            message.contains(&format!("[[serve]] entry 1 names `{}`", fixture::HEAD))
                && message.contains("does not run in that phase. It runs in [[fetch]]"),
            "should say which phase the middleware runs in: {message}"
        );

        let mut settings = settings_naming(fixture::MODULE);
        settings.fetch = PhaseEntries::new(vec![html_entry(None, &[fixture::READER])]);
        let error = IntegrationRegistry::new(&settings)
            .err()
            .expect("should refuse a serve middleware named in a fetch entry");
        let message = error.to_string();
        assert!(
            message.contains(&format!("[[fetch]] entry 1 names `{}`", fixture::READER))
                && message.contains("does not run in that phase. It runs in [[serve]]"),
            "should say which phase the middleware runs in: {message}"
        );
    }

    #[test]
    fn each_phase_has_entries_of_its_own() {
        let mut settings = settings_naming(fixture::MODULE);
        settings.fetch = PhaseEntries::new(vec![html_entry(None, &[fixture::HEAD])]);
        settings.serve = PhaseEntries::new(vec![html_entry(Some("/news/"), &[fixture::READER])]);
        let registry = IntegrationRegistry::new(&settings)
            .expect("should build a registry with an entry in each phase");

        let chain_for = |phase: MiddlewarePhase, path: &str| {
            registry
                .middleware_chain(settings.phase_entries(phase), phase, HTML_MEDIA_TYPE, path)
                .ids()
        };
        assert_eq!(
            chain_for(MiddlewarePhase::Fetch, "/news/today"),
            [fixture::HEAD]
        );
        assert_eq!(
            chain_for(MiddlewarePhase::Serve, "/news/today"),
            [fixture::READER]
        );
        assert!(
            chain_for(MiddlewarePhase::Serve, "/sport/today").is_empty(),
            "should run no serve middleware on a path only the fetch entries cover"
        );
        let unnamed: Vec<&str> = unnamed_middleware(&settings, &registry.inner)
            .into_iter()
            .map(|(_, middleware)| middleware.middleware_id())
            .collect();
        assert_eq!(
            unnamed,
            [
                fixture::LINKS,
                fixture::BROKEN,
                fixture::BROKEN_READER,
                fixture::TEXT,
                fixture::PREPARING,
                fixture::REFUSING,
                fixture::TEXT_HEADER
            ],
            "should count a middleware as named only by an entry of a phase it runs in"
        );
    }

    /// A middleware under a name of the test's choosing, running in the
    /// phases given.
    struct Named {
        id: &'static str,
        phases: &'static [MiddlewarePhase],
    }

    impl Middleware for Named {
        fn middleware_id(&self) -> &'static str {
            self.id
        }

        fn phases(&self) -> &[MiddlewarePhase] {
            self.phases
        }

        fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
            MiddlewareAction::pass()
        }
    }

    const FETCH_ONLY: &[MiddlewarePhase] = &[MiddlewarePhase::Fetch];

    fn first_supplier(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("first_supplier")
                .without_js()
                .with_middleware(Arc::new(Named {
                    id: "testing.shared-name",
                    phases: FETCH_ONLY,
                }))
                .build(),
        ))
    }

    fn second_supplier(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("second_supplier")
                .without_js()
                .with_middleware(Arc::new(Named {
                    id: "testing.shared-name",
                    phases: FETCH_ONLY,
                }))
                .build(),
        ))
    }

    fn misnamed_supplier(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("misnamed_supplier")
                .without_js()
                .with_middleware(Arc::new(Named {
                    id: "Not A Name",
                    phases: FETCH_ONLY,
                }))
                .build(),
        ))
    }

    fn phaseless_supplier(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("phaseless_supplier")
                .without_js()
                .with_middleware(Arc::new(Named {
                    id: "testing.phaseless",
                    phases: &[],
                }))
                .build(),
        ))
    }

    /// A builder for `register`, selected in `settings` under a module name
    /// made from its id.
    fn selected_supplier(
        settings: &mut Settings,
        id: &'static str,
        module: &'static str,
        register: crate::integrations::IntegrationBuilderFn,
    ) -> crate::integrations::IntegrationBuilder {
        settings.select_module("testing", module);
        crate::integrations::IntegrationBuilder::new(
            id,
            "a-vendor-crate",
            register,
            validate_nothing,
        )
        .with_module_name(module)
    }

    #[test]
    fn two_modules_supplying_one_middleware_name_are_refused() {
        let mut settings = crate::test_support::tests::create_test_settings();
        let extra = [
            selected_supplier(
                &mut settings,
                "first_supplier",
                "testing.first-supplier",
                first_supplier,
            ),
            selected_supplier(
                &mut settings,
                "second_supplier",
                "testing.second-supplier",
                second_supplier,
            ),
        ];

        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse one middleware name supplied twice");

        let message = error.to_string();
        assert!(
            message.contains("`first_supplier` and `second_supplier` both supply a middleware")
                && message.contains("`testing.shared-name`"),
            "should name both suppliers and the name: {message}"
        );
    }

    fn marking_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("marking")
                .without_js()
                .with_bundle_tag_attribute("data-example-mode", "first")
                .with_bundle_tag_attribute("data-example-flag", "true")
                .build(),
        ))
    }

    fn disagreeing_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("disagreeing")
                .without_js()
                .with_bundle_tag_attribute("data-example-mode", "second")
                .with_bundle_tag_attribute("data-example-other", "kept")
                .build(),
        ))
    }

    fn unsafe_name_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("unsafe_name")
                .without_js()
                .with_bundle_tag_attribute("onload=alert(1) data-x", "true")
                .build(),
        ))
    }

    fn unsafe_value_registration(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder("unsafe_value")
                .without_js()
                .with_bundle_tag_attribute("data-example-mode", "\"><script>")
                .build(),
        ))
    }

    #[test]
    fn a_registration_s_attributes_reach_the_bundle_s_tag_in_registration_order() {
        let mut settings = crate::test_support::tests::create_test_settings();
        let extra = [
            selected_supplier(
                &mut settings,
                "marking",
                "testing.marking",
                marking_registration,
            ),
            selected_supplier(
                &mut settings,
                "disagreeing",
                "testing.disagreeing",
                disagreeing_registration,
            ),
        ];

        let registry = IntegrationRegistry::with_registrations(&settings, &extra)
            .expect("should build a registry");

        assert_eq!(
            registry.tsjs_script_tag_attributes(),
            vec![
                ("data-example-mode", "first"),
                ("data-example-flag", "true"),
                ("data-example-other", "kept"),
            ],
            "should keep the first value a name is given and the order the registrations \
             gave them in"
        );
    }

    #[test]
    fn an_attribute_that_could_end_the_bundle_s_tag_is_refused() {
        for (id, module, register, shown) in [
            (
                "unsafe_name",
                "testing.unsafe-name",
                unsafe_name_registration as crate::integrations::IntegrationBuilderFn,
                "`onload=alert(1) data-x`",
            ),
            (
                "unsafe_value",
                "testing.unsafe-value",
                unsafe_value_registration,
                "`\"><script>`",
            ),
        ] {
            let mut settings = crate::test_support::tests::create_test_settings();
            let extra = [selected_supplier(&mut settings, id, module, register)];

            let error = IntegrationRegistry::with_registrations(&settings, &extra)
                .err()
                .expect("should refuse an attribute that is not plain");

            let message = error.to_string();
            assert!(
                message.contains(&format!("integration `{id}` puts the attribute"))
                    && message.contains(shown),
                "should name the integration and show what it asked for: {message}"
            );
        }
    }

    #[test]
    fn a_middleware_no_entry_could_name_is_refused() {
        let mut settings = crate::test_support::tests::create_test_settings();
        let extra = [selected_supplier(
            &mut settings,
            "misnamed_supplier",
            "testing.misnamed-supplier",
            misnamed_supplier,
        )];
        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse a middleware whose name no entry can write");
        let message = error.to_string();
        assert!(
            message.contains("`misnamed_supplier` supplies a middleware named `Not A Name`")
                && message.contains("not a name an entry can write"),
            "should name the supplier and the name: {message}"
        );

        let mut settings = crate::test_support::tests::create_test_settings();
        let extra = [selected_supplier(
            &mut settings,
            "phaseless_supplier",
            "testing.phaseless-supplier",
            phaseless_supplier,
        )];
        let error = IntegrationRegistry::with_registrations(&settings, &extra)
            .err()
            .expect("should refuse a middleware that runs in no phase");
        let message = error.to_string();
        assert!(
            message.contains("`phaseless_supplier` supplies the middleware `testing.phaseless`")
                && message.contains("runs in no phase"),
            "should name the supplier and the middleware: {message}"
        );
    }
}
