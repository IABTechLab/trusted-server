//! Integration module registry and sample implementations.

use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use url::Url;

use crate::error::TrustedServerError;
use crate::platform::{PlatformBackendSpec, RuntimeServices};
use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod datadome;
pub mod didomi;
pub mod google_tag_manager;
pub mod gpt;
pub mod lockr;
pub mod nextjs;
pub mod permutive;
pub mod prebid;
mod registry;
pub mod sourcepoint;
pub mod testlight;

pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, HeaderMutation, HeaderMutationMode,
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationDocumentState,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext,
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
    let parsed = Url::parse(url).change_context(TrustedServerError::Integration {
        integration: integration.to_string(),
        message: "Invalid upstream URL".to_string(),
    })?;

    services
        .backend()
        .ensure(&PlatformBackendSpec {
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
            certificate_check: true,
            first_byte_timeout: first_byte_timeout.unwrap_or_else(|| Duration::from_secs(15)),
        })
        .change_context(TrustedServerError::Integration {
            integration: integration.to_string(),
            message: "Failed to register backend".to_string(),
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

type IntegrationBuilder =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[
        prebid::register,
        testlight::register,
        nextjs::register,
        permutive::register,
        lockr::register,
        didomi::register,
        sourcepoint::register,
        google_tag_manager::register,
        datadome::register,
        gpt::register,
    ]
}
