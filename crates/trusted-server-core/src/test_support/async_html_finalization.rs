//! Fictional terminal HTTP consumer, available only with `test-utils`.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{Uri, request_builder};
use error_stack::{Report, ResultExt as _};
use futures::future::LocalBoxFuture;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use url::Url;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    HtmlFinalization, HtmlFinalizationError, HtmlFinalizationInput, IntegrationHtmlFinalizer,
    IntegrationHtmlFinalizerFactory, IntegrationHtmlStreamContext, IntegrationRegistration,
};
use crate::platform::{PlatformBackendSpec, PlatformHttpRequest};
use crate::settings::{IntegrationConfig, Settings};

pub(crate) const ID: &str = "html_finalization_fixture";
const EOF_SENTINEL: &[u8] = b"<!--example-document-eof-->";
const ORIGINAL: &[u8] = b"EXAMPLE_UNSIGNED";
const REPLACEMENT: &[u8] = b"EXAMPLE_FINALIZED";

#[derive(Clone, Deserialize, Validate)]
pub(crate) struct FixtureConfig {
    enabled: bool,
    #[validate(url)]
    lookup_url: String,
    #[validate(range(min = 1, max = 60000))]
    transport_timeout_ms: u32,
}

impl IntegrationConfig for FixtureConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

struct FixtureFactory(FixtureConfig);
struct FixtureSession(FixtureConfig);

pub(crate) fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<FixtureConfig>(ID)? else {
        return Ok(None);
    };
    Ok(Some(
        IntegrationRegistration::builder(ID)
            .without_js()
            .with_html_finalizer(Arc::new(FixtureFactory(config)))
            .build(),
    ))
}

impl IntegrationHtmlFinalizerFactory for FixtureFactory {
    fn integration_id(&self) -> &'static str {
        ID
    }

    fn create(&self, _: IntegrationHtmlStreamContext) -> Box<dyn IntegrationHtmlFinalizer> {
        Box::new(FixtureSession(self.0.clone()))
    }
}

impl IntegrationHtmlFinalizer for FixtureSession {
    fn finalize<'a>(
        &'a mut self,
        input: HtmlFinalizationInput<'a>,
    ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>> {
        Box::pin(async move {
            if !input
                .html
                .windows(EOF_SENTINEL.len())
                .any(|bytes| bytes == EOF_SENTINEL)
            {
                return Err(Report::new(HtmlFinalizationError::Rejected));
            }
            let url =
                Url::parse(&self.0.lookup_url).change_context(HtmlFinalizationError::Rejected)?;
            // Fastly cannot interrupt a blocking host call. Bound its transport
            // by the remaining chain budget; timer adapters deliberately test
            // cancellation against a longer transport timeout.
            let timeout = if input.deadline.has_cooperative_timer() {
                Duration::from_millis(u64::from(self.0.transport_timeout_ms))
            } else {
                input
                    .deadline
                    .remaining()
                    .min(Duration::from_millis(u64::from(
                        self.0.transport_timeout_ms,
                    )))
            };
            let backend = input
                .services
                .backend()
                .ensure(&PlatformBackendSpec {
                    scheme: url.scheme().to_string(),
                    host: url
                        .host_str()
                        .ok_or_else(|| Report::new(HtmlFinalizationError::Rejected))?
                        .to_string(),
                    port: url.port(),
                    host_header_override: None,
                    certificate_check: true,
                    first_byte_timeout: timeout,
                    between_bytes_timeout: timeout,
                    discriminator: Some(ID.to_string()),
                })
                .change_context(HtmlFinalizationError::Unavailable)?;
            let payload = serde_json::to_vec(&json!({
                "complete": true,
                "sha256": hex::encode(Sha256::digest(input.html)),
                "bytes": input.html.len(),
                "transport_timeout_ms": timeout.as_millis(),
            }))
            .change_context(HtmlFinalizationError::Rejected)?;
            let request = request_builder()
                .method(http::Method::POST)
                .uri(
                    self.0
                        .lookup_url
                        .parse::<Uri>()
                        .change_context(HtmlFinalizationError::Rejected)?,
                )
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .change_context(HtmlFinalizationError::Rejected)?;
            let response = input
                .services
                .http_client()
                .send(PlatformHttpRequest::new(request, backend).with_cache_bypass())
                .await
                .change_context(HtmlFinalizationError::Unavailable)?;
            if !response.response.status().is_success() {
                return Err(Report::new(HtmlFinalizationError::Unavailable));
            }
            let reply = response
                .response
                .into_body()
                .into_bytes_bounded(64)
                .await
                .change_context(HtmlFinalizationError::Rejected)?;
            if reply.as_ref() != b"EXAMPLE_APPROVED" {
                return Err(Report::new(HtmlFinalizationError::Rejected));
            }
            let offset = input
                .html
                .windows(ORIGINAL.len())
                .position(|bytes| bytes == ORIGINAL)
                .ok_or_else(|| Report::new(HtmlFinalizationError::Rejected))?;
            input.replacement.append(&input.html[..offset])?;
            input.replacement.append(REPLACEMENT)?;
            input
                .replacement
                .append(&input.html[offset + ORIGINAL.len()..])?;
            Ok(HtmlFinalization::Replace)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn fictional_consumer_registers_only_when_enabled_and_adds_no_browser_bundle() {
        for enabled in [false, true] {
            let mut settings = create_test_settings();
            settings.integrations.insert_config(ID, &json!({
                "enabled": enabled, "lookup_url": "https://lookup.example.com/approve", "transport_timeout_ms": 1000,
            })).expect("should configure the fictional consumer");
            let registry =
                IntegrationRegistry::new(&settings).expect("should build fixture metadata");
            assert_eq!(
                registry.has_html_finalizers(),
                enabled,
                "should exclude disabled terminal capabilities"
            );
            assert!(
                !registry.js_module_ids().contains(&ID),
                "should never add a fictional browser bundle"
            );
        }
    }
}
