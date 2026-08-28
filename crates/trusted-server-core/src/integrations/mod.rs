//! Integration module registry and sample implementations.

use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use http::Request;
use url::Url;

use crate::error::TrustedServerError;
use crate::platform::{DEFAULT_FIRST_BYTE_TIMEOUT, PlatformBackendSpec, RuntimeServices};
use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod datadome;
pub mod didomi;
pub mod google_tag_manager;
pub mod gpt;
pub mod gpt_diagnostics;
pub mod lockr;
pub mod nextjs;
pub mod osano;
pub mod permutive;
pub mod prebid;
mod registry;
pub mod sourcepoint;
pub mod testlight;

#[cfg(test)]
pub(crate) use registry::test_support as registry_test_support;
pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, CarriedJsModule, HeaderMutation,
    HeaderMutationMode, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationDocumentState, IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext,
    IntegrationHtmlPostProcessor, IntegrationMetadata, IntegrationProxy, IntegrationRegistration,
    IntegrationRegistrationBuilder, IntegrationRegistry, IntegrationRequestFilter,
    IntegrationScriptContext, IntegrationScriptRewriter, ProxyDispatchInput, RequestFilterDecision,
    RequestFilterEffects, RequestFilterInput, RequestFilterRegistryInput,
    RequestFilterRegistryOutcome, ScriptRewriteAction,
};

/// Registers or retrieves a platform backend for the given URL.
///
/// Parses `url`, builds a [`PlatformBackendSpec`] with TLS enabled and a
/// 15-second first-byte timeout, and delegates to
/// [`crate::platform::PlatformBackend::ensure`].
///
/// # Errors
///
/// Returns an error when `url` cannot be parsed, is missing a host, or the
/// backend registration fails.
pub(crate) fn ensure_integration_backend(
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
/// # Errors
///
/// Returns an error when `url` cannot be parsed, is missing a host, or the
/// backend registration fails.
pub(crate) fn ensure_integration_backend_with_timeout(
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
/// # Errors
///
/// Returns an error when the URL cannot be parsed, is missing a host, or the
/// platform backend cannot predict a name for the spec.
pub(crate) fn predict_integration_backend_name(
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
pub(crate) const INTEGRATION_MAX_BODY_BYTES: usize = 256 * 1024;

/// Maximum response body size from RTB providers (prebid, aps, mediator).
pub(crate) const UPSTREAM_RTB_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum response body size from SDK/proxy integrations.
pub(crate) const UPSTREAM_SDK_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Drains an [`EdgeBody`] into a byte vector, rejecting bodies larger than
/// `max_bytes` with [`TrustedServerError::RequestTooLarge`].
///
/// # Errors
///
/// Returns an error when:
/// - The body exceeds `max_bytes`.
/// - A streaming body chunk cannot be read (mapped to an `Integration` error).
pub(crate) async fn collect_body_bounded(
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
/// [`collect_body_bounded`], oversized bodies are classified as
/// [`TrustedServerError::Integration`] (502 `BAD_GATEWAY`) rather than
/// [`TrustedServerError::RequestTooLarge`] (413).
///
/// Note: the effective bound for streaming bodies is ≤ `max_bytes` + `one_chunk`
/// because the size check runs after each chunk is materialized. Fastly
/// H2/H3 chunks are ≤ 16 KiB in practice, making the overshoot negligible.
///
/// # Errors
///
/// Returns an error when:
/// - The body exceeds `max_bytes` (mapped to [`TrustedServerError::Integration`]).
/// - A streaming body chunk cannot be read (same error type).
pub(crate) async fn collect_response_bounded(
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
/// integration is not enabled.
pub type IntegrationBuilderFn =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

/// Validates an integration's configuration for deployment and reports
/// whether the integration is enabled.
///
/// Runs for every builder, enabled or not, so a typo in a disabled block is
/// still caught.
pub type IntegrationValidateFn = fn(&Settings) -> Result<bool, Report<TrustedServerError>>;

/// Prepares a request before routing, for every request the adapter sees.
///
/// Runs whether or not the integration is enabled, so an integration can
/// strip its reserved query or cookie even when it is switched off.
pub type IntegrationPrepareRequestFn =
    fn(&Settings, &mut Request<EdgeBody>) -> Result<(), Report<TrustedServerError>>;

/// Source label for the built-in integrations.
pub const CORE_SOURCE: &str = "trusted-server-core";

/// A named factory for one integration, the unit an adapter or a vendor crate
/// hands to [`IntegrationRegistry::with_registrations`].
///
/// # Examples
///
/// ```
/// use error_stack::Report;
/// use trusted_server_core::error::TrustedServerError;
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
/// let builder = IntegrationBuilder::new("example", "example-crate", build, validate);
/// let registry = IntegrationRegistry::with_registrations(settings, &[builder])?;
/// assert!(registry.integration_enabled("example"));
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
        }
    }

    /// Attaches a request preparation function that runs before routing on
    /// every request, enabled or not.
    #[must_use]
    pub const fn with_request_preparer(mut self, prepare: IntegrationPrepareRequestFn) -> Self {
        self.prepare_request = Some(prepare);
        self
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

    /// Builds the registration, or `None` when the integration is not enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the integration is enabled with invalid
    /// configuration.
    pub(crate) fn build(
        &self,
        settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        (self.build)(settings)
    }

    /// Validates the integration's configuration for deployment and reports
    /// whether the integration is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be parsed or fails
    /// validation.
    pub(crate) fn validate(&self, settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        (self.validate)(settings)
    }

    /// The request preparation function, when one is attached.
    // The adapters do not yet run request preparers, so nothing in the crate
    // calls this until that is wired up.
    #[allow(dead_code)]
    pub(crate) fn prepare_request(&self) -> Option<IntegrationPrepareRequestFn> {
        self.prepare_request
    }
}

/// The built-in integrations, in hook order.
const BUILT_IN_BUILDERS: &[IntegrationBuilder] = &[
    IntegrationBuilder::new("aps", CORE_SOURCE, aps::register, aps::validate),
    IntegrationBuilder::new("prebid", CORE_SOURCE, prebid::register, prebid::validate),
    IntegrationBuilder::new(
        "testlight",
        CORE_SOURCE,
        testlight::register,
        testlight::validate,
    ),
    IntegrationBuilder::new("nextjs", CORE_SOURCE, nextjs::register, nextjs::validate),
    IntegrationBuilder::new(
        "permutive",
        CORE_SOURCE,
        permutive::register,
        permutive::validate,
    ),
    IntegrationBuilder::new("lockr", CORE_SOURCE, lockr::register, lockr::validate),
    IntegrationBuilder::new("didomi", CORE_SOURCE, didomi::register, didomi::validate),
    IntegrationBuilder::new(
        "sourcepoint",
        CORE_SOURCE,
        sourcepoint::register,
        sourcepoint::validate,
    ),
    IntegrationBuilder::new("osano", CORE_SOURCE, osano::register, osano::validate),
    IntegrationBuilder::new(
        "google_tag_manager",
        CORE_SOURCE,
        google_tag_manager::register,
        google_tag_manager::validate,
    ),
    IntegrationBuilder::new(
        "datadome",
        CORE_SOURCE,
        datadome::register,
        datadome::validate,
    ),
    IntegrationBuilder::new("gpt", CORE_SOURCE, gpt::register, gpt::validate),
    IntegrationBuilder::new(
        "gpt_diagnostics",
        CORE_SOURCE,
        gpt_diagnostics::register,
        gpt_diagnostics::validate,
    ),
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
