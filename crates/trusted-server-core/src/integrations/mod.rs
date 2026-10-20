//! Integration module registry and sample implementations.

use std::time::Duration;

use edgezero_core::body::{Body as EdgeBody, BodyContent};
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
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
pub mod js_asset_proxy;
pub mod lockr;
pub mod nextjs;
pub mod osano;
pub mod permutive;
pub mod prebid;
mod registry;
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
    ScriptTextAccumulator,
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
    match body.into_content() {
        BodyContent::Once(bytes) => {
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
        BodyContent::Stream(mut stream) => {
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
    match body.into_content() {
        BodyContent::Once(bytes) => {
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
        BodyContent::Stream(mut stream) => {
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
        IntegrationBuilder {
            id: "testlight",
            build: testlight::register,
        },
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use bytes::Bytes;

    use super::*;

    fn managed_body(streaming: bool) -> EdgeBody {
        let body = if streaming {
            EdgeBody::stream(futures::stream::iter([
                Bytes::from_static(b"ab"),
                Bytes::from_static(b"cd"),
            ]))
        } else {
            EdgeBody::from("abcd")
        };
        body.with_buffered_json_policy(NonZeroUsize::new(1).expect("should be positive"), true)
    }

    #[test]
    fn managed_collectors_use_explicit_limits_and_preserve_error_contexts() {
        futures::executor::block_on(async {
            for streaming in [false, true] {
                assert_eq!(
                    collect_body_bounded(managed_body(streaming), 4, "test")
                        .await
                        .expect("should collect request at limit"),
                    b"abcd",
                    "should collect managed request content with the caller's limit"
                );
                assert_eq!(
                    collect_response_bounded(managed_body(streaming), 4, "test")
                        .await
                        .expect("should collect response at limit"),
                    b"abcd",
                    "should collect managed response content with the caller's limit"
                );
                let request_error = collect_body_bounded(managed_body(streaming), 3, "test")
                    .await
                    .expect_err("should reject oversized request");
                assert!(
                    matches!(
                        request_error.current_context(),
                        TrustedServerError::RequestTooLarge { .. }
                    ),
                    "should retain request-too-large classification"
                );
                let response_error = collect_response_bounded(managed_body(streaming), 3, "test")
                    .await
                    .expect_err("should reject oversized response");
                assert!(
                    matches!(
                        response_error.current_context(),
                        TrustedServerError::Integration { .. }
                    ),
                    "should retain upstream integration error classification"
                );
            }
        });
    }

    #[test]
    fn managed_collectors_report_external_stream_errors() {
        futures::executor::block_on(async {
            for response in [false, true] {
                let body = EdgeBody::from_external_stream(futures::stream::iter([
                    Ok(Bytes::from_static(b"ab")),
                    Err(std::io::Error::other("fixture stream failure")),
                ]))
                .with_buffered_json_policy(NonZeroUsize::new(1).expect("should be positive"), true);
                let result = if response {
                    collect_response_bounded(body, 4, "test").await
                } else {
                    collect_body_bounded(body, 4, "test").await
                };
                let error = result.expect_err("should report external stream error");
                assert!(
                    matches!(
                        error.current_context(),
                        TrustedServerError::Integration { .. }
                    ),
                    "should retain integration error classification"
                );
                let expected = if response {
                    "Failed to read response body"
                } else {
                    "Failed to read request body"
                };
                assert!(
                    format!("{error:?}").contains(expected),
                    "should retain request or response read context"
                );
            }
        });
    }
}
