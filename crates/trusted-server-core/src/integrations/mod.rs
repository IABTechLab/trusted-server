//! Integration module registry and sample implementations.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use url::Url;

use crate::auction::AuctionPlan;
use crate::error::TrustedServerError;
use crate::platform::{DEFAULT_FIRST_BYTE_TIMEOUT, PlatformBackendSpec, RuntimeServices};
use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod datadome;
mod declaration;
pub mod didomi;
pub mod google_tag_manager;
pub mod gpt;
pub mod gpt_diagnostics;
pub mod js_asset_proxy;
pub mod lockr;
pub mod nextjs;
pub(crate) mod openrtb;
pub mod osano;
pub mod permutive;
pub mod prebid;
pub mod processing;
mod registry;
pub use declaration::{
    IntegrationDeclaration, ReservedIntegrationInput, ReservedRouteClaim, ScriptAssetPolicy,
    ScriptSourceAction, ScriptSourceActionKind, ScriptSourceClaim, ScriptSourceContext,
    ScriptSourceKind, ScriptSourceOutcome, ScriptSourceStep, ScriptSourceTransition,
    evaluate_script_sources, validate_integration_declarations, validate_script_asset_policy,
};
mod script_url;
pub mod sourcepoint;
pub mod testlight;

pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, HeaderMutation, HeaderMutationMode,
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationDocumentState,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext,
    IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory, IntegrationMetadata,
    IntegrationProxy, IntegrationRegistration, IntegrationRegistrationBuilder, IntegrationRegistry,
    IntegrationRequestFilter, IntegrationScriptContext, IntegrationScriptRewriter,
    ProxyDispatchInput, RequestFilterDecision, RequestFilterEffects, RequestFilterInput,
    RequestFilterRegistryInput, RequestFilterRegistryOutcome, ScriptRewriteAction,
};

/// Sole schema-one profile catalog until application composition moves owners.
pub(crate) fn profile_definitions() -> &'static [crate::auction::profile::OpenRtbProfileDefinition]
{
    &[
        openrtb::DEFINITION,
        prebid::profile::DEFINITION,
        aps::profile::DEFINITION,
    ]
}

/// Normalize reserved ingress inputs before integration request preparation.
///
/// Settings-only startup failures retain the same reserved-input handling even
/// when no executable registry is available. The legacy catalog owns concrete
/// normalizer selection until the application composition root moves.
///
/// # Errors
///
/// Returns an integration error when reserved-input normalization or request
/// preparation fails.
///
/// # Examples
///
/// ```
/// use edgezero_core::body::Body;
/// use error_stack::Report;
/// use trusted_server_core::{error::TrustedServerError, settings::Settings,
///     integrations::{prepare_request, processing::RequestProcessingState}};
/// fn prepare(settings: &Settings, request: &mut http::Request<Body>)
///     -> Result<RequestProcessingState, Report<TrustedServerError>> {
///     prepare_request(settings, None, request)
/// }
/// ```
pub fn prepare_request(
    settings: &Settings,
    registry: Option<&IntegrationRegistry>,
    request: &mut http::Request<EdgeBody>,
) -> Result<processing::RequestProcessingState, Report<TrustedServerError>> {
    gpt_diagnostics::normalize_reserved_inputs(request)?;
    if let Some(registry) = registry {
        registry.prepare_request(settings, request)
    } else {
        gpt_diagnostics::prepare_request(settings, request)?;
        Ok(processing::RequestProcessingState::from_request(request))
    }
}

/// Sole schema-one mediator catalog, removed when application composition moves.
pub(crate) fn mediator_definitions() -> &'static [crate::auction::mediator::MediatorDefinition] {
    &[adserver_mock::mediator::DEFINITION]
}

pub(crate) fn compile_legacy_mediator(
    settings: &Settings,
    selected: Option<&str>,
) -> Result<Option<Arc<dyn crate::auction::mediator::CompiledMediator>>, Report<TrustedServerError>>
{
    let Some(id) = selected else {
        return Ok(None);
    };
    let definition = mediator_definitions()
        .iter()
        .find(|definition| definition.id == id)
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!("unknown mediator `{id}`"),
            })
        })?;
    let config = settings.integration_config::<adserver_mock::AdServerMockConfig>(id)?
        .ok_or_else(|| Report::new(TrustedServerError::Configuration { message: format!("auction mediator `{id}` must reference a separately registered enabled integration with the exact same ID") }))?;
    let config =
        serde_json::to_value(config).change_context(TrustedServerError::Configuration {
            message: "cannot compile mediator configuration".to_string(),
        })?;
    (definition.compile)(&config).map(Some)
}

impl IntegrationRegistry {
    /// Build registrations from the existing catalog around a shared plan.
    ///
    /// Runtime adapters compile one plan and reuse its allocation. Catalog
    /// construction stays here while the registry only assembles its inputs.
    ///
    /// # Errors
    ///
    /// Returns an error when catalog construction, route registration or
    /// asynchronous HTML completion validation fails.
    ///
    /// # Panics
    ///
    /// See [`Self::from_registrations`] for its route-validation invariant.
    pub fn with_plan(
        settings: &Settings,
        plan: Arc<AuctionPlan>,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut registrations = Vec::new();
        if let Some(registration) = prebid::register_for_plan(settings, &plan)? {
            registrations.push(registration);
        }
        if let Some(registration) = aps::register_for_plan(settings, &plan)? {
            registrations.push(registration);
        }
        for builder in builders() {
            if let Some(registration) = (builder.build)(settings)? {
                debug_assert_eq!(
                    registration.integration_id, builder.id,
                    "integration builder ID should match registration ID"
                );
                registrations.push(registration);
            }
        }
        Self::from_registrations(settings, plan, registrations, &[]).map(|registry| {
            registry
                .with_reserved_input_normalizers(vec![gpt_diagnostics::normalize_reserved_inputs])
        })
    }
}

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
#[cfg(test)]
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
#[cfg(test)]
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

/// Describe a bounded backend without registering or opening a connection.
///
/// `integration` is the stable owner discriminator used for response
/// correlation. Both transport timers retain the supplied timeout.
///
/// # Errors
///
/// Returns an integration error if the endpoint is not an absolute URL with a host.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use trusted_server_core::integrations::integration_backend_spec;
/// let spec = integration_backend_spec("http://mediator.example/mediate", "example-owner",
///     true, Duration::from_millis(20)).expect("should describe a bounded backend");
/// assert_eq!(spec.discriminator.as_deref(), Some("example-owner"));
/// assert_eq!(spec.first_byte_timeout, Duration::from_millis(20));
/// ```
pub fn integration_backend_spec(
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
pub const UPSTREAM_RTB_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
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
///
/// # Examples
///
/// ```
/// use edgezero_core::body::Body;
/// use error_stack::Report;
/// use trusted_server_core::{error::TrustedServerError,
///     integrations::{collect_response_bounded, UPSTREAM_RTB_MAX_RESPONSE_BYTES}};
/// async fn collect(body: Body) -> Result<Vec<u8>, Report<TrustedServerError>> {
///     collect_response_bounded(body, UPSTREAM_RTB_MAX_RESPONSE_BYTES, "example-owner").await
/// }
/// ```
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

type IntegrationBuilderFn =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

pub(crate) struct IntegrationBuilder {
    id: &'static str,
    build: IntegrationBuilderFn,
}

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[
        // This must remain first: attribute rewriters chain replacements and short-circuit removals.
        IntegrationBuilder {
            id: js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID,
            build: js_asset_proxy::register,
        },
        #[cfg(feature = "test-utils")]
        IntegrationBuilder {
            id: crate::test_support::async_html_finalization::ID,
            build: crate::test_support::async_html_finalization::register,
        },
        IntegrationBuilder {
            id: "testlight",
            build: testlight::register,
        },
        // Next.js must precede GTM: GTM reads the Flight-protection snapshot
        // that the Next.js script stage records for the same fragment.
        IntegrationBuilder {
            id: "nextjs",
            build: nextjs::register,
        },
        IntegrationBuilder {
            id: "permutive",
            build: permutive::register,
        },
        IntegrationBuilder {
            id: "lockr",
            build: lockr::register,
        },
        IntegrationBuilder {
            id: "didomi",
            build: didomi::register,
        },
        IntegrationBuilder {
            id: "sourcepoint",
            build: sourcepoint::register,
        },
        IntegrationBuilder {
            id: "osano",
            build: osano::register,
        },
        IntegrationBuilder {
            id: "google_tag_manager",
            build: google_tag_manager::register,
        },
        IntegrationBuilder {
            id: "datadome",
            build: datadome::register,
        },
        IntegrationBuilder {
            id: "gpt",
            build: gpt::register,
        },
        IntegrationBuilder {
            id: "gpt_diagnostics",
            build: gpt_diagnostics::register,
        },
    ]
}

#[cfg(test)]
pub(crate) fn registered_builder_ids() -> impl Iterator<Item = &'static str> {
    builders().iter().map(|builder| builder.id)
}
