//! Integration module registry and sample implementations.

use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use url::Url;

use crate::error::TrustedServerError;
use crate::platform::{PlatformBackendSpec, RuntimeServices, DEFAULT_FIRST_BYTE_TIMEOUT};
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
pub mod testlight;

pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationDocumentState, IntegrationEndpoint,
    IntegrationHeadInjector, IntegrationHtmlContext, IntegrationHtmlPostProcessor,
    IntegrationMetadata, IntegrationProxy, IntegrationRegistration, IntegrationRegistrationBuilder,
    IntegrationRegistry, IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
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
) -> Result<String, Report<TrustedServerError>> {
    services
        .backend()
        .ensure(&integration_backend_spec(
            url,
            integration,
            true,
            DEFAULT_FIRST_BYTE_TIMEOUT,
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
    certificate_check: bool,
    first_byte_timeout: Duration,
) -> Result<String, Report<TrustedServerError>> {
    services
        .backend()
        .predict_name(&integration_backend_spec(
            url,
            integration,
            certificate_check,
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
        certificate_check,
        first_byte_timeout,
    })
}

/// Maximum body size accepted by integration proxy endpoints (256 KiB).
pub(crate) const INTEGRATION_MAX_BODY_BYTES: usize = 256 * 1024;

/// Drains an [`EdgeBody`] into a byte vector.
///
/// # Errors
///
/// Returns an error when a streaming body chunk cannot be read.
pub(crate) async fn collect_body(
    body: EdgeBody,
    integration: &'static str,
) -> Result<Vec<u8>, Report<TrustedServerError>> {
    match body {
        EdgeBody::Once(bytes) => Ok(bytes.to_vec()),
        EdgeBody::Stream(mut stream) => {
            let mut body_bytes = Vec::new();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(|error| {
                    Report::new(TrustedServerError::Integration {
                        integration: integration.to_string(),
                        message: format!("Failed to read response body: {error}"),
                    })
                })?;
                body_bytes.extend_from_slice(&chunk);
            }
            Ok(body_bytes)
        }
    }
}

/// Drains an [`EdgeBody`] into a byte vector, rejecting bodies larger than
/// `max_bytes` with [`TrustedServerError::RequestTooLarge`].
///
/// Use this instead of [`collect_body`] for inbound request bodies where an
/// unbounded read would allow clients to exhaust memory.
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
        google_tag_manager::register,
        datadome::register,
        gpt::register,
    ]
}
