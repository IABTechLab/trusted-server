//! Integration module registry and sample implementations.

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
            first_byte_timeout: std::time::Duration::from_secs(15),
        })
        .change_context(TrustedServerError::Integration {
            integration: integration.to_string(),
            message: "Failed to register backend".to_string(),
        })
}

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
