//! Fictional terminal HTTP consumer, available only with `test-utils`.

use std::io;
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
    IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory, IntegrationRegistration,
};
use crate::platform::{PlatformBackendSpec, PlatformHttpRequest};
use crate::settings::{IntegrationConfig, Settings};
use crate::streaming_processor::StreamProcessor;
use crate::streaming_processor::finish::{StreamFinish, StreamFinishError, StreamFinishInput};

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
    #[serde(default)]
    streaming: bool,
}

impl IntegrationConfig for FixtureConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

struct FixtureFactory(FixtureConfig);
struct FixtureSession {
    config: FixtureConfig,
    digest: Sha256,
    bytes: usize,
    tail: Vec<u8>,
    saw_eof_sentinel: bool,
    synchronous_eof: bool,
}

pub(crate) fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<FixtureConfig>(ID)? else {
        return Ok(None);
    };
    Ok(Some(
        IntegrationRegistration::builder(ID)
            .without_js()
            .with_html_stream_processor(Arc::new(FixtureFactory(config)))
            .build(),
    ))
}

impl IntegrationHtmlStreamProcessorFactory for FixtureFactory {
    fn integration_id(&self) -> &'static str {
        ID
    }

    fn requires_async_finish(&self) -> bool {
        true
    }
    fn buffer_output_until_eof(&self) -> bool {
        !self.0.streaming
    }

    fn create(&self, _: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
        Box::new(FixtureSession {
            config: self.0.clone(),
            digest: Sha256::new(),
            bytes: 0,
            tail: Vec::new(),
            saw_eof_sentinel: false,
            synchronous_eof: false,
        })
    }
}

impl StreamProcessor for FixtureSession {
    fn process_chunk(&mut self, chunk: &[u8], last: bool) -> io::Result<Vec<u8>> {
        self.digest.update(chunk);
        self.bytes = self
            .bytes
            .checked_add(chunk.len())
            .ok_or_else(|| io::Error::other("Fixture byte count exceeds its range"))?;
        let mut observed = std::mem::take(&mut self.tail);
        observed.extend_from_slice(chunk);
        self.saw_eof_sentinel |= observed
            .windows(EOF_SENTINEL.len())
            .any(|bytes| bytes == EOF_SENTINEL);
        let keep = observed.len().min(EOF_SENTINEL.len() - 1);
        self.tail
            .extend_from_slice(&observed[observed.len() - keep..]);
        self.synchronous_eof = last;
        Ok(chunk.to_vec())
    }
    fn requires_async_finish(&self) -> bool {
        true
    }

    fn async_finish<'a>(
        &'a mut self,
        input: StreamFinishInput<'a>,
    ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
        Some(Box::pin(async move {
            if !self.saw_eof_sentinel || !self.synchronous_eof {
                return Err(Report::new(StreamFinishError::Rejected));
            }
            let url =
                Url::parse(&self.config.lookup_url).change_context(StreamFinishError::Rejected)?;
            // Fastly cannot interrupt a blocking host call. Bound its transport
            // by the remaining chain budget; timer adapters deliberately test
            // cancellation against a longer transport timeout.
            let timeout = if input.deadline.has_cooperative_timer() {
                Duration::from_millis(u64::from(self.config.transport_timeout_ms))
            } else {
                input
                    .deadline
                    .remaining()
                    .min(Duration::from_millis(u64::from(
                        self.config.transport_timeout_ms,
                    )))
            };
            let backend = input
                .services
                .backend()
                .ensure(&PlatformBackendSpec {
                    scheme: url.scheme().to_string(),
                    host: url
                        .host_str()
                        .ok_or_else(|| Report::new(StreamFinishError::Rejected))?
                        .to_string(),
                    port: url.port(),
                    host_header_override: None,
                    certificate_check: true,
                    first_byte_timeout: timeout,
                    between_bytes_timeout: timeout,
                    discriminator: Some(ID.to_string()),
                })
                .change_context(StreamFinishError::Unavailable)?;
            let payload = serde_json::to_vec(&json!({
                "complete": true,
                "sha256": hex::encode(self.digest.clone().finalize()),
                "bytes": self.bytes,
                "transport_timeout_ms": timeout.as_millis(),
            }))
            .change_context(StreamFinishError::Rejected)?;
            let request = request_builder()
                .method(http::Method::POST)
                .uri(
                    self.config
                        .lookup_url
                        .parse::<Uri>()
                        .change_context(StreamFinishError::Rejected)?,
                )
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .change_context(StreamFinishError::Rejected)?;
            let response = input
                .services
                .http_client()
                .send(PlatformHttpRequest::new(request, backend).with_cache_bypass())
                .await
                .change_context(StreamFinishError::Unavailable)?;
            if !response.response.status().is_success() {
                return Err(Report::new(StreamFinishError::Unavailable));
            }
            let reply = response
                .response
                .into_body()
                .into_bytes_bounded(64)
                .await
                .change_context(StreamFinishError::Rejected)?;
            if reply.as_ref() != b"EXAMPLE_APPROVED" {
                return Err(Report::new(StreamFinishError::Rejected));
            }
            if self.config.streaming {
                input.replacement.append(input.output)?;
                input
                    .replacement
                    .append(b"<!--example-async-eof-complete-->")?;
                return Ok(StreamFinish::Replace);
            }
            let offset = input
                .output
                .windows(ORIGINAL.len())
                .position(|bytes| bytes == ORIGINAL)
                .ok_or_else(|| Report::new(StreamFinishError::Rejected))?;
            input.replacement.append(&input.output[..offset])?;
            input.replacement.append(REPLACEMENT)?;
            input
                .replacement
                .append(&input.output[offset + ORIGINAL.len()..])?;
            Ok(StreamFinish::Replace)
        }))
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
            settings.publisher.allow_async_html_finish = true;
            settings.integrations.insert_config(ID, &json!({
                "enabled": enabled, "lookup_url": "https://lookup.example.com/approve", "transport_timeout_ms": 1000,
            })).expect("should configure the fictional consumer");
            let registry =
                IntegrationRegistry::new(&settings).expect("should build fixture metadata");
            assert_eq!(
                registry.has_async_html_finish(),
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
