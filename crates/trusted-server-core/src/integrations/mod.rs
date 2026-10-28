//! Integration module registry and sample implementations.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use http::{Request, Response};
use url::Url;

use crate::auction::AuctionPlan;
use crate::auction::demand::{AdServerImplementation, DemandImplementation};
use crate::error::TrustedServerError;
use crate::platform::{DEFAULT_FIRST_BYTE_TIMEOUT, PlatformBackendSpec, RuntimeServices};
use crate::settings::Settings;

pub mod js_asset_proxy;
mod registry;

#[cfg(test)]
pub(crate) use registry::test_support as registry_test_support;
pub use registry::{
    AttributeRewriteAction, CarriedJsModule, HeaderMutation, HeaderMutationMode,
    IntegrationDocumentState, IntegrationEndpoint, IntegrationMetadata, IntegrationProxy,
    IntegrationRegistration, IntegrationRegistrationBuilder, IntegrationRegistry,
    IntegrationRequestFilter, IntegrationRequestState, ProxyDispatchInput, RequestFilterDecision,
    RequestFilterEffects, RequestFilterInput, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome, ScriptRewriteAction,
};

/// Registers or retrieves a platform backend for the given URL.
///
/// Parses `url`, builds a [`PlatformBackendSpec`] with TLS enabled and a
/// 15-second first-byte timeout, and delegates to
/// [`crate::platform::PlatformBackend::ensure`].
///
/// Public for the same reason as [`ensure_integration_backend_with_timeout`].
///
/// # Errors
///
/// Returns an error when `url` cannot be parsed, is missing a host, or the
/// backend registration fails.
pub fn ensure_integration_backend(
    services: &RuntimeServices,
    url: &str,
    integration: &'static str,
    first_byte_timeout: Option<Duration>,
) -> Result<String, Report<TrustedServerError>> {
    services
        .backend()
        .ensure(&integration_backend_spec(
            url,
            integration,
            true,
            first_byte_timeout.unwrap_or(DEFAULT_FIRST_BYTE_TIMEOUT),
        )?)
        .change_context(TrustedServerError::Integration {
            integration: integration.to_string(),
            message: "Failed to register backend".to_string(),
        })
}

/// Registers or retrieves a platform backend for the given URL with a custom
/// first-byte timeout.
///
/// Parses `url`, builds a [`PlatformBackendSpec`] with TLS enabled and the
/// given `first_byte_timeout`, and delegates to
/// [`crate::platform::PlatformBackend::ensure`].
///
/// Public because an integration crate outside this one cannot reach an
/// upstream without registering a backend first, and a crate rolling its own
/// registration would produce a different backend name for the same URL.
///
/// # Errors
///
/// Returns an error when `url` cannot be parsed, is missing a host, or the
/// backend registration fails.
pub fn ensure_integration_backend_with_timeout(
    services: &RuntimeServices,
    url: &str,
    integration: &'static str,
    first_byte_timeout: Duration,
) -> Result<String, Report<TrustedServerError>> {
    services
        .backend()
        .ensure(&integration_backend_spec(
            url,
            integration,
            true,
            first_byte_timeout,
        )?)
        .change_context(TrustedServerError::Integration {
            integration: integration.to_string(),
            message: "Failed to register backend".to_string(),
        })
}

/// Compute the deterministic platform backend name for a URL without registering it.
///
/// Parses `url`, builds a [`PlatformBackendSpec`], and delegates to
/// [`crate::platform::PlatformBackend::predict_name`].
///
/// Public for the same reason as [`ensure_integration_backend_with_timeout`],
/// and the two have to agree, so a crate outside this one uses this function
/// and does not derive a name of its own.
///
/// # Errors
///
/// Returns an error when the URL cannot be parsed, is missing a host, or the
/// platform backend cannot predict a name for the spec.
pub fn predict_integration_backend_name(
    services: &RuntimeServices,
    url: &str,
    integration: &'static str,
    first_byte_timeout: Duration,
) -> Result<String, Report<TrustedServerError>> {
    services
        .backend()
        .predict_name(&integration_backend_spec(
            url,
            integration,
            true,
            first_byte_timeout,
        )?)
        .change_context(TrustedServerError::Integration {
            integration: integration.to_string(),
            message: "Failed to predict backend name".to_string(),
        })
}

fn integration_backend_spec(
    url: &str,
    integration: &'static str,
    certificate_check: bool,
    first_byte_timeout: Duration,
) -> Result<PlatformBackendSpec, Report<TrustedServerError>> {
    let parsed = Url::parse(url).change_context(TrustedServerError::Integration {
        integration: integration.to_string(),
        message: format!("Invalid upstream URL: {url}"),
    })?;
    Ok(PlatformBackendSpec {
        scheme: parsed.scheme().to_string(),
        host: parsed
            .host_str()
            .ok_or_else(|| {
                Report::new(TrustedServerError::Integration {
                    integration: integration.to_string(),
                    message: "Upstream URL missing host".to_string(),
                })
            })?
            .to_string(),
        port: parsed.port(),
        host_header_override: None,
        certificate_check,
        first_byte_timeout,
        between_bytes_timeout: first_byte_timeout,
        // Distinguish this integration's backend from any other provider that
        // targets the same origin, so auction response correlation by backend
        // name cannot cross providers.
        discriminator: Some(integration.to_string()),
    })
}

/// Maximum body size accepted by integration proxy endpoints (256 KiB).
///
/// Public so every integration crate bounds a request body at one size.
pub const INTEGRATION_MAX_BODY_BYTES: usize = 256 * 1024;

/// Maximum response body size from RTB providers (prebid, aps, ad server).
///
/// Public so an integration crate outside this one bounds an upstream
/// response at the size the built-in ones do.
pub const UPSTREAM_RTB_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum response body size from SDK/proxy integrations.
///
/// Public so every integration crate bounds an upstream script or proxied
/// response at one size.
pub const UPSTREAM_SDK_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Drains an [`EdgeBody`] into a byte vector, rejecting bodies larger than
/// `max_bytes` with [`TrustedServerError::RequestTooLarge`].
///
/// Public because an integration crate outside this one reads request bodies
/// too, and an unbounded read of a client's body is not a fault each crate
/// should solve again.
///
/// # Errors
///
/// Returns an error when:
/// - The body exceeds `max_bytes`.
/// - A streaming body chunk cannot be read (mapped to an `Integration` error).
pub async fn collect_body_bounded(
    body: EdgeBody,
    max_bytes: usize,
    integration: &'static str,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    match body {
        EdgeBody::Once(bytes) => {
            if bytes.len() > max_bytes {
                return Err(Report::new(TrustedServerError::RequestTooLarge {
                    message: format!(
                        "{integration}: request body ({} bytes) exceeds the {max_bytes} byte limit",
                        bytes.len(),
                    ),
                }));
            }
            Ok(bytes.to_vec())
        }
        EdgeBody::Stream(mut stream) => {
            let mut body_bytes = Vec::new();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(|error| {
                    Report::new(TrustedServerError::Integration {
                        integration: integration.to_string(),
                        message: format!("Failed to read request body: {error}"),
                    })
                })?;
                if body_bytes.len() + chunk.len() > max_bytes {
                    return Err(Report::new(TrustedServerError::RequestTooLarge {
                        message: format!(
                            "{integration}: request body exceeds the {max_bytes} byte limit",
                        ),
                    }));
                }
                // Size check runs after chunk is materialized — effective bound is
                // ≤ max_bytes + one_chunk (Fastly H2/H3 chunks are ≤ 16 KiB in practice).
                body_bytes.extend_from_slice(&chunk);
            }
            Ok(body_bytes)
        }
    }
}

/// Drains an upstream [`EdgeBody`] response into a byte vector, rejecting
/// bodies larger than `max_bytes` with [`TrustedServerError::Integration`].
///
/// Use this for upstream (provider/integration) response bodies to bound
/// memory usage when a third-party server misbehaves. Unlike
/// `collect_body_bounded`, oversized bodies are classified as
/// [`TrustedServerError::Integration`] (502 `BAD_GATEWAY`) rather than
/// [`TrustedServerError::RequestTooLarge`] (413).
///
/// Note: the effective bound for streaming bodies is ≤ `max_bytes` + `one_chunk`
/// because the size check runs after each chunk is materialized. Fastly
/// H2/H3 chunks are ≤ 16 KiB in practice, making the overshoot negligible.
///
/// Public because an integration crate outside this one reads upstream
/// responses too, and an unbounded read of a misbehaving upstream is not a
/// fault each crate should solve again.
///
/// # Errors
///
/// Returns an error when:
/// - The body exceeds `max_bytes` (mapped to [`TrustedServerError::Integration`]).
/// - A streaming body chunk cannot be read (same error type).
pub async fn collect_response_bounded(
    body: EdgeBody,
    max_bytes: usize,
    integration: &'static str,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    match body {
        EdgeBody::Once(bytes) => {
            if bytes.len() > max_bytes {
                return Err(Report::new(TrustedServerError::Integration {
                    integration: integration.to_string(),
                    message: format!(
                        "response body ({} bytes) exceeds the {max_bytes} byte limit",
                        bytes.len(),
                    ),
                }));
            }
            Ok(bytes.to_vec())
        }
        EdgeBody::Stream(mut stream) => {
            let mut body_bytes = Vec::new();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(|error| {
                    Report::new(TrustedServerError::Integration {
                        integration: integration.to_string(),
                        message: format!("Failed to read response body: {error}"),
                    })
                })?;
                // Size check runs after chunk is materialized — effective bound is
                // ≤ max_bytes + one_chunk (Fastly H2/H3 chunks are ≤ 16 KiB in practice).
                if body_bytes.len() + chunk.len() > max_bytes {
                    return Err(Report::new(TrustedServerError::Integration {
                        integration: integration.to_string(),
                        message: format!("response body exceeds the {max_bytes} byte limit",),
                    }));
                }
                body_bytes.extend_from_slice(&chunk);
            }
            Ok(body_bytes)
        }
    }
}

/// Builds an integration's registration from settings, or `None` when the
/// settings give it nothing to register.
///
/// The registry calls this only for an integration a section selects, so an integration runs exactly when an operator names it.
pub type IntegrationBuilderFn =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

/// Validates an integration's configuration for deployment and reports
/// whether a section selects it.
///
/// Runs for every builder, named or not, so one builder's rules cannot be
/// skipped by the order the builders happen to be in. At deploy time secret
/// fields hold secret-store key names rather than values, so a validator must
/// not depend on a resolved secret.
pub type IntegrationValidateFn = fn(&Settings) -> Result<bool, Report<TrustedServerError>>;

/// Prepares a request before routing, for every routed request except the
/// health check.
///
/// Runs whether or not a section selects the integration, so one
/// can strip its own reserved query or cookie in a deployment that does not
/// run it.
pub type IntegrationPrepareRequestFn =
    fn(&Settings, &mut Request<EdgeBody>) -> Result<(), Report<TrustedServerError>>;

/// Finishes the response the page path returns for one request, from what
/// the module's request hooks left for it in the request's
/// [`IntegrationRequestState`].
///
/// Runs whether or not a section selects the integration, as the preparer
/// does, and has nothing to do for a request its module left nothing on.
pub type IntegrationFinalizeResponseFn = fn(&IntegrationRequestState, &mut Response<EdgeBody>);

/// Builds a module's registration from the settings and the compiled auction
/// plan, or `None` when the two give it nothing to register.
///
/// Runs for every builder that has one, whether or not a section selects the
/// module, because a module registered this way follows what the plan
/// selects and decides for itself whether it runs.
pub type IntegrationPlanRegistrationFn =
    fn(
        &Settings,
        &AuctionPlan,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

/// Checks a module's configuration against the compiled auction plan, for a
/// rule that depends on what the plan selects.
///
/// Runs for every builder that has one, when a deployment is validated and
/// as the settings load.
pub type IntegrationPlanValidateFn =
    fn(&Settings, &AuctionPlan) -> Result<(), Report<TrustedServerError>>;

/// A setting in a module's own table that holds the name of a secret.
///
/// The name is looked up in the default secret store as the settings load,
/// and the setting holds the secret from then on.
#[derive(Clone, Copy, Debug)]
pub struct ModuleSecretSetting {
    /// Where the setting sits inside the module's table, one name for each
    /// level.
    pub path: &'static [&'static str],
    /// Whether the module's table, as written, puts the setting to use. A
    /// setting not in use is cleared as the settings load and is not looked
    /// up, and one in use has to name a key before a deployment is accepted.
    pub in_use: fn(&serde_json::Map<String, serde_json::Value>) -> bool,
}

/// Source label for the built-in integrations.
pub const CORE_SOURCE: &str = "trusted-server-core";

/// A named factory for one integration, the unit an adapter or a vendor crate
/// hands to [`IntegrationRegistry::with_plan_and_registrations`].
///
/// # Examples
///
/// ```
/// use error_stack::Report;
/// use trusted_server_core::error::TrustedServerError;
/// use std::sync::Arc;
///
/// use trusted_server_core::auction::compile_auction_plan;
/// use trusted_server_core::integrations::{
///     IntegrationBuilder, IntegrationRegistration, IntegrationRegistry,
/// };
/// use trusted_server_core::settings::Settings;
///
/// fn build(
///     _settings: &Settings,
/// ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
///     Ok(Some(IntegrationRegistration::builder("example").build()))
/// }
///
/// fn validate(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
///     Ok(true)
/// }
///
/// # fn demo(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
/// let builder = IntegrationBuilder::new("example", "example-crate", build, validate)
///     .with_module_name("testing.example");
/// // The registry builds a module a section selects, so a deployment that
/// // wants this one writes `[testing] modules = ["example"]`.
/// let mut settings = settings.clone();
/// settings.select_module("testing", "testing.example");
/// let plan = Arc::new(compile_auction_plan(&settings)?);
/// let registry = IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder])?;
/// assert!(registry.integration_runs("example"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct IntegrationBuilder {
    id: &'static str,
    source: &'static str,
    build: IntegrationBuilderFn,
    validate: IntegrationValidateFn,
    prepare_request: Option<IntegrationPrepareRequestFn>,
    finalize_response: Option<IntegrationFinalizeResponseFn>,
    plan_registration: Option<IntegrationPlanRegistrationFn>,
    plan_validator: Option<IntegrationPlanValidateFn>,
    reads_auction_token: bool,
    secret_settings: &'static [ModuleSecretSetting],
    supplies_integration: bool,
    demand: Option<&'static DemandImplementation>,
    adserver: Option<&'static AdServerImplementation>,
    module: Option<&'static str>,
    section: Option<&'static str>,
    loading_cache: Option<LoadingCacheFactory>,
    shared: Option<&'static str>,
    cache_features: &'static [&'static str],
}

/// Gives the loading cache a module supplies, see
/// [`IntegrationBuilder::with_loading_cache`].
pub type LoadingCacheFactory = fn() -> Arc<dyn crate::platform::PlatformLoadingCache>;

/// The build function of a builder that supplies no page integration.
fn no_registration(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(None)
}

/// The validate function of a builder that supplies no page integration.
fn nothing_to_validate(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(false)
}

impl IntegrationBuilder {
    /// Creates a builder for the integration `id`, attributed to `source`
    /// (a crate or package name used in duplicate-id errors).
    #[must_use]
    pub const fn new(
        id: &'static str,
        source: &'static str,
        build: IntegrationBuilderFn,
        validate: IntegrationValidateFn,
    ) -> Self {
        Self {
            id,
            source,
            build,
            validate,
            prepare_request: None,
            finalize_response: None,
            plan_registration: None,
            plan_validator: None,
            reads_auction_token: false,
            secret_settings: &[],
            supplies_integration: true,
            demand: None,
            adserver: None,
            module: None,
            section: None,
            loading_cache: None,
            shared: None,
            cache_features: &[],
        }
    }

    /// Creates a builder that supplies only implementations, such as a demand
    /// or ad server implementation, and no page integration.
    ///
    /// Its `id` still claims a place among builder ids, so two crates cannot
    /// register under one id, but it is not an integration a deployment can
    /// name.
    #[must_use]
    pub const fn implementations(id: &'static str, source: &'static str) -> Self {
        Self {
            id,
            source,
            build: no_registration,
            validate: nothing_to_validate,
            prepare_request: None,
            finalize_response: None,
            plan_registration: None,
            plan_validator: None,
            reads_auction_token: false,
            secret_settings: &[],
            supplies_integration: false,
            demand: None,
            adserver: None,
            module: None,
            section: None,
            loading_cache: None,
            shared: None,
            cache_features: &[],
        }
    }

    /// Registers a demand implementation, which `[demand] modules` or an
    /// `implementation` line can name.
    #[must_use]
    pub const fn with_demand(mut self, demand: &'static DemandImplementation) -> Self {
        self.demand = Some(demand);
        self
    }

    /// Registers an ad server implementation, which `[ad-server] module` or
    /// an `implementation` line can name.
    #[must_use]
    pub const fn with_adserver(mut self, adserver: &'static AdServerImplementation) -> Self {
        self.adserver = Some(adserver);
        self
    }

    /// Whether this builder supplies a page integration, as opposed to only
    /// implementations.
    #[must_use]
    pub const fn supplies_integration(&self) -> bool {
        self.supplies_integration
    }

    /// The demand implementation this builder registers, when it registers
    /// one.
    #[must_use]
    pub const fn demand(&self) -> Option<&'static DemandImplementation> {
        self.demand
    }

    /// The ad server implementation this builder registers, when it registers
    /// one.
    #[must_use]
    pub const fn adserver(&self) -> Option<&'static AdServerImplementation> {
        self.adserver
    }

    /// Attaches a request preparation function that runs before routing on
    /// every request, whether or not the integration runs.
    #[must_use]
    pub const fn with_request_preparer(mut self, prepare: IntegrationPrepareRequestFn) -> Self {
        self.prepare_request = Some(prepare);
        self
    }

    /// Attaches a function that finishes the response the page path returns,
    /// from what the module's request hooks left for the request.
    #[must_use]
    pub const fn with_response_finalizer(
        mut self,
        finalize: IntegrationFinalizeResponseFn,
    ) -> Self {
        self.finalize_response = Some(finalize);
        self
    }

    /// Registers the module from the compiled auction plan as well as the
    /// settings, for a module whose page support follows what the plan
    /// selects.
    ///
    /// The function runs whether or not a section selects the module, and
    /// what it registers has its hooks run ahead of the modules the sections
    /// select.
    #[must_use]
    pub const fn with_plan_registration(mut self, register: IntegrationPlanRegistrationFn) -> Self {
        self.plan_registration = Some(register);
        self
    }

    /// Attaches a check of the module's configuration against the compiled
    /// auction plan.
    #[must_use]
    pub const fn with_plan_validator(mut self, validate: IntegrationPlanValidateFn) -> Self {
        self.plan_validator = Some(validate);
        self
    }

    /// Declares that the module's browser script reads the token an auction
    /// publishes with its winning bids.
    ///
    /// A token is made for each auction when a section selects such a
    /// module, and none is made in a deployment where nothing reads it.
    #[must_use]
    pub const fn with_auction_token(mut self) -> Self {
        self.reads_auction_token = true;
        self
    }

    /// Whether the module's browser script reads the auction token.
    #[must_use]
    pub const fn reads_auction_token(&self) -> bool {
        self.reads_auction_token
    }

    /// Declares the settings in the module's own table that hold the name of
    /// a secret, so each is looked up as the settings load and checked when
    /// a deployment is validated.
    #[must_use]
    pub const fn with_secret_settings(mut self, secrets: &'static [ModuleSecretSetting]) -> Self {
        self.secret_settings = secrets;
        self
    }

    /// The settings in the module's own table that hold the name of a
    /// secret.
    #[must_use]
    pub const fn secret_settings(&self) -> &'static [ModuleSecretSetting] {
        self.secret_settings
    }

    /// Runs this builder when a section selects the module `name`, which is a
    /// crate's path under `crates` with `.` between the parts, such as
    /// `cmp.example`. An integration still in core holds the name its crate
    /// will have as a constant.
    #[must_use]
    pub const fn with_module_name(mut self, name: &'static str) -> Self {
        self.module = Some(name);
        self
    }

    /// The name a section selects this builder's module by, when it has one.
    #[must_use]
    pub const fn module_name(&self) -> Option<&'static str> {
        self.module
    }

    /// Offers the loading cache this crate supplies, see
    /// [`crate::platform::PlatformLoadingCache`].
    ///
    /// The offer is not a selection. A cache is selected by naming the module
    /// in its own `[cache.<name>]` table, and an adapter resolves every table's
    /// selection with [`loading_caches_from`] when it builds its state. A table
    /// naming no module takes the one loading cache module the build links.
    #[must_use]
    pub const fn with_loading_cache(mut self, factory: LoadingCacheFactory) -> Self {
        self.loading_cache = Some(factory);
        self
    }

    /// Runs this builder when `[shared.<vendor>]` is present, for a vendor
    /// whose modules all read that one table and cannot be built without it.
    /// A builder the table runs is selected by no section, so the registry
    /// refuses one that also has a module name.
    #[must_use]
    pub const fn with_shared_settings(mut self, vendor: &'static str) -> Self {
        self.shared = Some(vendor);
        self
    }

    /// The vendor whose `[shared.<vendor>]` table runs this builder, when one
    /// does.
    #[must_use]
    pub const fn shared_settings(&self) -> Option<&'static str> {
        self.shared
    }

    /// Declares the caches this builder's modules load through, by name,
    /// each selected in its own `[cache.<name>]` table the way core's own
    /// are. A name is the module's own, as `example-answers` is for a
    /// module from `crates/geo/example`. Declaring core's `robots-txt` or a
    /// name another builder declares is refused, and so is a table naming a
    /// cache no builder declares, when a deployment is validated and when
    /// [`loading_caches_from`] makes its caches.
    #[must_use]
    pub const fn with_cache_features(mut self, features: &'static [&'static str]) -> Self {
        self.cache_features = features;
        self
    }

    /// The caches this builder's modules load through, by name.
    #[must_use]
    pub const fn cache_features(&self) -> &'static [&'static str] {
        self.cache_features
    }

    /// The loading cache this builder offers, when it offers one.
    #[must_use]
    pub const fn loading_cache(&self) -> Option<LoadingCacheFactory> {
        self.loading_cache
    }

    /// Names the section that selects this module, for one selected outside
    /// the section of its type, such as a module of core's own in a section
    /// of core's.
    #[must_use]
    pub const fn selected_in(mut self, section: &'static str) -> Self {
        self.section = Some(section);
        self
    }

    /// The section that selects this builder's module: the one
    /// [`selected_in`](Self::selected_in) named, or else its type's.
    #[must_use]
    pub fn section(&self) -> Option<&'static str> {
        self.section.or_else(|| {
            self.module
                .and_then(|name| name.split_once('.').map(|(folder, _)| folder))
        })
    }

    /// The integration id this builder produces.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// The source label used in diagnostics.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        self.source
    }

    /// Builds the registration, or `None` when the settings give the
    /// integration nothing to register.
    ///
    /// # Errors
    ///
    /// Returns an error when the integration runs with invalid configuration.
    pub(crate) fn build(
        &self,
        settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        (self.build)(settings)
    }

    /// Validates the integration's configuration for deployment and reports
    /// whether a section selects the integration's module.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be parsed or fails
    /// validation.
    pub(crate) fn validate(&self, settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        (self.validate)(settings)
    }

    /// The request preparation function, when one is attached.
    pub(crate) fn prepare_request(&self) -> Option<IntegrationPrepareRequestFn> {
        self.prepare_request
    }

    /// The response finalizer, when one is attached.
    pub(crate) fn finalize_response(&self) -> Option<IntegrationFinalizeResponseFn> {
        self.finalize_response
    }

    /// The registration from the auction plan, when one is attached.
    pub(crate) fn plan_registration(&self) -> Option<IntegrationPlanRegistrationFn> {
        self.plan_registration
    }

    /// The check against the auction plan, when one is attached.
    pub(crate) fn plan_validator(&self) -> Option<IntegrationPlanValidateFn> {
        self.plan_validator
    }
}

/// The built-in integrations, in the order `trusted-server-modules` lists them.
const BUILT_IN_BUILDERS: &[IntegrationBuilder] = &[
    // This stays the first module a section selects, so its middleware is
    // the first of theirs in the order the registry lists them.
    js_asset_proxy::BUILDER,
    // A stand-in for an integration that streams, which core's own tests
    // select where they need one.
    #[cfg(test)]
    registry_test_support::payload_fixture::BUILDER,
    // A stand-in for an integration that tags a page, for the same tests.
    #[cfg(test)]
    registry_test_support::tag_fixture::BUILDER,
    // A stand-in for an integration that acts on one request, for the same
    // tests.
    #[cfg(test)]
    registry_test_support::request_fixture::BUILDER,
    // A stand-in for an integration whose browser module loads deferred, for
    // the same tests.
    #[cfg(test)]
    registry_test_support::deferred_fixture::BUILDER,
    // A stand-in for a module that changes a page through middleware, for
    // the same tests.
    #[cfg(test)]
    registry_test_support::middleware_fixture::BUILDER,
    // A stand-in for the plainest demand implementation there can be, which
    // core's own tests name where they need a source.
    #[cfg(test)]
    IntegrationBuilder::implementations(
        crate::auction::test_support::plain_fixture::MODULE,
        CORE_SOURCE,
    )
    .with_demand(&crate::auction::test_support::plain_fixture::DEMAND),
    // A stand-in demand implementation, which core's own tests name where
    // they need one that departs from the plain one.
    #[cfg(test)]
    IntegrationBuilder::implementations(
        crate::auction::test_support::demand_fixture::MODULE,
        CORE_SOURCE,
    )
    .with_demand(&crate::auction::test_support::demand_fixture::DEMAND),
    // A stand-in ad server, which core's own tests select where they need
    // one.
    #[cfg(test)]
    IntegrationBuilder::implementations(
        crate::auction::test_support::adserver_fixture::MODULE,
        CORE_SOURCE,
    )
    .with_adserver(&crate::auction::test_support::adserver_fixture::ADSERVER),
];

/// The built-in integration builders, in the order `trusted-server-modules`
/// lists them.
pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    BUILT_IN_BUILDERS
}

/// Every builder the registry will consider: the built-in set followed by
/// `extra`, in that order, so the built-ins keep their order.
pub(crate) fn all_builders(
    extra: &[IntegrationBuilder],
) -> impl Iterator<Item = IntegrationBuilder> + '_ {
    builders().iter().copied().chain(extra.iter().copied())
}

/// The caches a deployment may hold answers in, by name: core's own first,
/// then the ones `builders` declare, in builder order.
///
/// # Errors
///
/// When a builder declares core's own `robots-txt`, or two builders declare
/// one name, naming the builders, since the two would otherwise share one
/// cache and its keys without a word.
pub(crate) fn declared_caches(
    builders: &[IntegrationBuilder],
) -> Result<Vec<&'static str>, Report<TrustedServerError>> {
    let mut declared: Vec<(&'static str, &'static str)> =
        vec![(crate::settings::ROBOTS_TXT_CACHE, "core")];
    for builder in builders {
        for name in builder.cache_features() {
            if let Some((_, by)) = declared.iter().find(|(declared, _)| declared == name) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: if *by == "core" {
                        format!(
                            "`{}` declares the cache `{name}`, which is core's own. A module \
                             names its caches under itself, as modules are named",
                            builder.id()
                        )
                    } else {
                        format!(
                            "`{}` and `{by}` both declare the cache `{name}`, which would hold \
                             both their answers under one set of keys. A module names its \
                             caches under itself, as modules are named",
                            builder.id()
                        )
                    },
                }));
            }
            declared.push((name, builder.id()));
        }
    }
    Ok(declared.into_iter().map(|(name, _)| name).collect())
}

/// Refuses a `[cache.<name>]` table naming a cache no linked module declares.
/// Run when a deployment's settings are validated and again when its caches
/// are made, so a misspelt table fails the deploy check and not the first
/// request.
///
/// # Errors
///
/// When a table names an undeclared cache, naming the table and the caches
/// this build's modules declare, or when the declarations themselves are
/// refused, see [`declared_caches`].
pub(crate) fn check_cache_tables(
    settings: &Settings,
    builders: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    let declared = declared_caches(builders)?;
    for (table, _) in settings.cache.loading_caches() {
        if !declared.contains(&table) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[cache.{table}] names a cache no linked module loads through. The caches \
                     this build's modules declare are: {}",
                    declared.join(", ")
                ),
            }));
        }
    }
    Ok(())
}

/// The loading cache each `[cache.<name>]` table selects, from the caches the
/// builders a deployment links offer. A table that names no module takes the
/// one loading cache module the build links, and keeps no copy when the build
/// links none or several. A module's cache is made once however many tables
/// select it.
///
/// # Errors
///
/// When a table names a module no linked builder offers a cache from, naming
/// the table and the modules that do, when a `[cache.<name>]` table names a
/// cache no linked module declares, naming the ones they do, and when a
/// builder declares core's `robots-txt` or a name another builder declares,
/// naming the builders.
pub fn loading_caches_from(
    settings: &Settings,
    builders: &[IntegrationBuilder],
) -> Result<Arc<crate::platform::LoadingCaches>, Report<TrustedServerError>> {
    check_cache_tables(settings, builders)?;
    let offered = offered_caches(builders);
    let declared = declared_caches(builders)?;
    let mut made: Vec<(&'static str, Arc<dyn crate::platform::PlatformLoadingCache>)> = Vec::new();
    let mut caches = crate::platform::LoadingCaches::default();
    for (table, config) in settings.cache.loading_caches() {
        // A table naming no module takes the one loading cache module the
        // build links, so answers are held without a line in the document.
        // With none linked, or several, it holds nothing.
        let written = match config.module.as_deref() {
            Some(written) => written,
            None if offered.len() == 1 => offered[0].0,
            None => continue,
        };
        let (name, factory) = named_cache(table, written, &offered)?;
        let cache = match made.iter().find(|(made_name, _)| *made_name == name) {
            Some((_, cache)) => Arc::clone(cache),
            None => {
                let cache = factory();
                made.push((name, Arc::clone(&cache)));
                cache
            }
        };
        caches.insert(table, cache);
    }
    // A declared cache with no table at all takes the one loading cache
    // module the build links, as a table naming no module does, so a
    // deployment that links one holds a module's answers without a line in
    // the document.
    if let [(_, factory)] = offered.as_slice() {
        let tabled: Vec<&str> = settings
            .cache
            .loading_caches()
            .into_iter()
            .map(|(table, _)| table)
            .collect();
        let mut default = None;
        for name in declared.iter().filter(|name| !tabled.contains(name)) {
            let cache = default
                .get_or_insert_with(|| {
                    made.iter()
                        .find(|(made_name, _)| *made_name == offered[0].0)
                        .map_or_else(factory, |(_, cache)| Arc::clone(cache))
                })
                .clone();
            caches.insert(name, cache);
        }
    }
    Ok(Arc::new(caches))
}

/// Refuses a `[cache.<name>]` table whose `module` no linked builder offers
/// a loading cache from, as [`loading_caches_from`] does. The deploy check
/// runs it, so a misspelt module fails `ts config validate` rather than the
/// service.
///
/// # Errors
///
/// When a table names a module no linked builder offers a cache from, naming
/// the table and the modules that do.
pub fn check_loading_cache_modules(
    settings: &Settings,
    builders: &[IntegrationBuilder],
) -> Result<(), Report<TrustedServerError>> {
    let offered = offered_caches(builders);
    for (table, config) in settings.cache.loading_caches() {
        if let Some(written) = config.module.as_deref() {
            named_cache(table, written, &offered)?;
        }
    }
    Ok(())
}

/// The loading cache each builder offers, by its module name.
fn offered_caches(builders: &[IntegrationBuilder]) -> Vec<(&'static str, LoadingCacheFactory)> {
    builders
        .iter()
        .filter_map(|builder| {
            builder
                .loading_cache()
                .map(|factory| (builder.id(), factory))
        })
        .collect()
}

/// The offered cache that `[cache.<table>] module` names as `written`, by the
/// module's short name or in full.
///
/// # Errors
///
/// When nothing offered has that name, naming the table and the modules that
/// offer a cache.
fn named_cache(
    table: &str,
    written: &str,
    offered: &[(&'static str, LoadingCacheFactory)],
) -> Result<(&'static str, LoadingCacheFactory), Report<TrustedServerError>> {
    offered
        .iter()
        .find(|(name, _)| {
            *name == written || crate::module_name::short_form(LOADING_CACHE_TYPE, name) == written
        })
        .copied()
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[cache.{table}] module names `{written}`, which no linked crate offers \
                     a loading cache from. The loading cache modules this build has are: {}",
                    offered_cache_names(offered)
                ),
            })
        })
}

/// The type folder loading cache modules live under, which their short
/// names leave out.
const LOADING_CACHE_TYPE: &str = "loading-cache";

/// The short names of the modules offering a cache, for a refusal.
fn offered_cache_names(offered: &[(&'static str, LoadingCacheFactory)]) -> String {
    if offered.is_empty() {
        return "none".to_owned();
    }
    offered
        .iter()
        .map(|(name, _)| crate::module_name::short_form(LOADING_CACHE_TYPE, name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod loading_caches_from_tests {
    use bytes::Bytes;

    use super::*;
    use crate::platform::{CacheSpec, LoadError, Loader, PlatformLoadingCache, StoreKey};
    use crate::settings::ROBOTS_TXT_CACHE;

    fn no_build(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(None)
    }

    fn no_validate(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Ok(true)
    }

    /// A cache that says it keeps values, so a test can tell it from the
    /// default, which keeps none.
    struct Keeping;

    #[async_trait::async_trait(?Send)]
    impl PlatformLoadingCache for Keeping {
        async fn get_or_load(
            &self,
            _services: &crate::platform::RuntimeServices,
            _spec: &CacheSpec,
            _key: &StoreKey,
            load: Loader,
        ) -> Result<Bytes, LoadError> {
            load().await.map(|loaded| loaded.bytes)
        }

        async fn remove(
            &self,
            _services: &crate::platform::RuntimeServices,
            _spec: &CacheSpec,
            _key: &StoreKey,
        ) {
        }
    }

    fn keeping() -> Arc<dyn PlatformLoadingCache> {
        Arc::new(Keeping)
    }

    fn nothing() -> Arc<dyn PlatformLoadingCache> {
        Arc::new(crate::platform::NoLoadingCache)
    }

    fn builder(source: &'static str) -> IntegrationBuilder {
        IntegrationBuilder::new("an_id", source, no_build, no_validate)
    }

    /// A module `id` offering a cache that keeps values.
    fn offering(id: &'static str) -> IntegrationBuilder {
        IntegrationBuilder::implementations(id, "a-crate").with_loading_cache(keeping)
    }

    /// A module `id` offering a cache that keeps nothing.
    fn offering_nothing(id: &'static str) -> IntegrationBuilder {
        IntegrationBuilder::implementations(id, "a-crate").with_loading_cache(nothing)
    }

    /// A module `id` declaring a cache of its own, `answers`.
    fn declaring(id: &'static str) -> IntegrationBuilder {
        IntegrationBuilder::implementations(id, "a-crate").with_cache_features(&["answers"])
    }

    fn settings_declaring(table: &str, module: Option<&str>) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.cache.declared.insert(
            table.to_owned(),
            crate::settings::LoadingCacheConfig {
                module: module.map(str::to_owned),
                ..Default::default()
            },
        );
        settings
    }

    #[test]
    fn a_cache_a_module_declares_selects_a_module_as_core_s_own_does() {
        let caches = loading_caches_from(
            &settings_declaring("answers", Some("one")),
            &[declaring("a_vendor"), offering("loading-cache.one")],
        )
        .expect("a declared cache selects a module");

        assert!(caches.for_feature("answers").keeps_values());
        assert!(
            caches.for_feature(ROBOTS_TXT_CACHE).keeps_values(),
            "core's own table, naming no module, takes the one module offered"
        );
    }

    #[test]
    fn a_declared_cache_with_no_table_takes_the_one_module_offered() {
        let caches = loading_caches_from(
            &settings_selecting(None),
            &[declaring("a_vendor"), offering("loading-cache.one")],
        )
        .expect("no table is not an error");

        assert!(caches.for_feature("answers").keeps_values());
        assert!(caches.for_feature(ROBOTS_TXT_CACHE).keeps_values());
    }

    #[test]
    fn a_declared_cache_with_no_table_keeps_nothing_when_two_are_offered() {
        let caches = loading_caches_from(
            &settings_selecting(None),
            &[
                declaring("a_vendor"),
                offering("loading-cache.one"),
                offering("loading-cache.two"),
            ],
        )
        .expect("no table is not an error");

        assert!(!caches.for_feature("answers").keeps_values());
        assert!(caches.is_empty());
    }

    #[test]
    fn a_cache_no_module_declares_is_refused_naming_the_declared_ones() {
        let refused = loading_caches_from(
            &settings_declaring("answres", Some("one")),
            &[declaring("a_vendor"), offering("loading-cache.one")],
        )
        .expect_err("a misspelt cache name is refused");

        let message = format!("{refused:?}");
        assert!(message.contains("[cache.answres]"), "{message}");
        assert!(message.contains("robots-txt, answers"), "{message}");
    }

    #[test]
    fn the_deploy_check_refuses_the_table_the_cache_build_refuses() {
        let settings = settings_declaring("answres", Some("one"));
        let builders = [declaring("a_vendor"), offering("loading-cache.one")];

        let refused = check_cache_tables(&settings, &builders)
            .expect_err("the check refuses a misspelt cache name without making a cache");
        let built = loading_caches_from(&settings, &builders).expect_err("as the build does");

        // The message alone: the debug form carries a backtrace where the
        // runner asks for one, and the two are raised on different stacks.
        assert_eq!(
            format!("{refused}"),
            format!("{built}"),
            "the deploy check should refuse what the cache build refuses"
        );
        check_cache_tables(&settings_declaring("answers", Some("one")), &builders)
            .expect("a declared cache passes the check");
    }

    #[test]
    fn two_builders_declaring_one_cache_are_refused_naming_both() {
        let refused = loading_caches_from(
            &settings_selecting(None),
            &[
                declaring("a_vendor"),
                declaring("another_vendor"),
                offering("loading-cache.one"),
            ],
        )
        .expect_err("two builders sharing a cache name are refused");

        let message = format!("{refused:?}");
        assert!(
            message.contains("`another_vendor` and `a_vendor`"),
            "{message}"
        );
        assert!(message.contains("`answers`"), "{message}");
    }

    #[test]
    fn a_builder_declaring_core_s_own_cache_is_refused() {
        let refused = loading_caches_from(
            &settings_selecting(None),
            &[
                IntegrationBuilder::implementations("a_vendor", "a-crate")
                    .with_cache_features(&[ROBOTS_TXT_CACHE]),
                offering("loading-cache.one"),
            ],
        )
        .expect_err("a builder declaring robots-txt is refused");

        let message = format!("{refused:?}");
        assert!(
            message.contains("`a_vendor` declares the cache `robots-txt`"),
            "{message}"
        );
        assert!(message.contains("core's own"), "{message}");
    }

    fn settings_selecting(module: Option<&str>) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.cache.robots_txt.module = module.map(str::to_owned);
        settings
    }

    #[test]
    fn a_feature_selecting_nothing_takes_the_one_module_offered() {
        let caches = loading_caches_from(
            &settings_selecting(None),
            &[builder("first"), offering("loading-cache.one")],
        )
        .expect("no selection is not an error");

        assert!(caches.for_feature(ROBOTS_TXT_CACHE).keeps_values());
        assert!(!caches.is_empty());
    }

    #[test]
    fn a_feature_selecting_nothing_keeps_nothing_when_two_are_offered() {
        let caches = loading_caches_from(
            &settings_selecting(None),
            &[offering("loading-cache.one"), offering("loading-cache.two")],
        )
        .expect("no selection is not an error");

        assert!(!caches.for_feature(ROBOTS_TXT_CACHE).keeps_values());
        assert!(caches.is_empty());
    }

    #[test]
    fn a_feature_selecting_nothing_keeps_nothing_when_none_is_offered() {
        let caches = loading_caches_from(&settings_selecting(None), &[builder("first")])
            .expect("no selection is not an error");

        assert!(!caches.for_feature(ROBOTS_TXT_CACHE).keeps_values());
        assert!(caches.is_empty());
    }

    #[test]
    fn a_feature_selects_a_module_by_its_short_name_or_in_full() {
        for written in ["one", "loading-cache.one"] {
            let caches = loading_caches_from(
                &settings_selecting(Some(written)),
                &[
                    offering_nothing("loading-cache.other"),
                    offering("loading-cache.one"),
                ],
            )
            .expect("a selected module should be found");

            assert!(
                caches.for_feature(ROBOTS_TXT_CACHE).keeps_values(),
                "{written} should select the cache that keeps values, not the first offered"
            );
            assert!(!caches.is_empty());
        }
    }

    #[test]
    fn a_feature_naming_a_module_nothing_offers_is_refused_naming_what_is_offered() {
        let error = loading_caches_from(
            &settings_selecting(Some("none")),
            &[offering("loading-cache.one"), offering("loading-cache.two")],
        )
        .expect_err("a module nothing offers should be refused");

        let message = format!("{error:?}");
        assert!(
            message.contains(
                "[cache.robots-txt] module names `none`, which no linked crate offers a \
                 loading cache from. The loading cache modules this build has are: one, two"
            ),
            "{message}"
        );
    }

    #[test]
    fn with_nothing_offered_the_refusal_says_so() {
        let error = loading_caches_from(&settings_selecting(Some("one")), &[builder("first")])
            .expect_err("a selection with nothing offered should be refused");

        let message = format!("{error:?}");
        assert!(
            message.contains("The loading cache modules this build has are: none"),
            "{message}"
        );
    }
}
