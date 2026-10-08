//! Integration module registry and sample implementations.

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

pub mod aps;
pub mod js_asset_proxy;
pub mod openrtb;
pub mod prebid;
pub mod prebid_server;
mod registry;

#[cfg(test)]
pub(crate) use registry::test_support as registry_test_support;
pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, CarriedJsModule, HeaderMutation,
    HeaderMutationMode, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationDocumentState, IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext,
    IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory, IntegrationMetadata,
    IntegrationProxy, IntegrationRegistration, IntegrationRegistrationBuilder, IntegrationRegistry,
    IntegrationRequestFilter, IntegrationRequestState, IntegrationScriptContext,
    IntegrationScriptRewriter, ProxyDispatchInput, RequestFilterDecision, RequestFilterEffects,
    RequestFilterInput, RequestFilterRegistryInput, RequestFilterRegistryOutcome,
    ScriptRewriteAction, ScriptTextAccumulator,
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
}

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

/// The built-in integrations, in hook order.
const BUILT_IN_BUILDERS: &[IntegrationBuilder] = &[
    // Prebid is registered from the auction plan, which puts its hooks ahead
    // of every section's module whatever its place here.
    prebid::BUILDER,
    // This must remain the first module a section selects: attribute
    // rewriters chain replacements and short-circuit removals.
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
    // Implementations `[demand]` and `[ad-server]` can name. None of them is
    // a module a section selects.
    IntegrationBuilder::implementations(openrtb::MODULE, CORE_SOURCE).with_demand(&openrtb::DEMAND),
    IntegrationBuilder::implementations(prebid_server::MODULE, CORE_SOURCE)
        .with_demand(&prebid_server::DEMAND),
    IntegrationBuilder::implementations(aps::APS_INTEGRATION_ID, CORE_SOURCE)
        .with_demand(&aps::DEMAND)
        .with_plan_registration(aps::register_for_plan),
    // A stand-in ad server, which core's own tests select where they need
    // one.
    #[cfg(test)]
    IntegrationBuilder::implementations(
        crate::auction::test_support::adserver_fixture::MODULE,
        CORE_SOURCE,
    )
    .with_adserver(&crate::auction::test_support::adserver_fixture::ADSERVER),
];

/// The built-in integration builders, in hook order.
pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    BUILT_IN_BUILDERS
}

/// Every builder the registry will consider: the built-in set followed by
/// `extra`, in that order, so hook order for the built-ins never changes.
pub(crate) fn all_builders(
    extra: &[IntegrationBuilder],
) -> impl Iterator<Item = IntegrationBuilder> + '_ {
    builders().iter().copied().chain(extra.iter().copied())
}
