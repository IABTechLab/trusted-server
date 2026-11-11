//! Fictional terminal HTTP consumer, available only with `test-utils`.
//!
//! It also answers [`FRAMING_TRAILER_PATH`] with a `Trailer` field, which
//! `EdgeZero`'s response framing rejects, so the runtime checks can reach a
//! framing fallback that Trusted Server itself never produces.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::error::EdgeError;
use edgezero_core::outbound::{OutboundCachePolicy, ResponseDecoding};
use error_stack::{Report, ResultExt as _};
use futures::future::LocalBoxFuture;
use http::{HeaderValue, Request, Response, header};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationEndpoint, IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory,
    IntegrationProxy, IntegrationRegistration,
};
use crate::outbound::{buffered, outbound_request};
use crate::platform::RuntimeServices;
use crate::settings::{IntegrationConfig, Settings};
use crate::streaming_processor::StreamProcessor;
use crate::streaming_processor::finish::{StreamFinish, StreamFinishError, StreamFinishInput};

pub(crate) const ID: &str = "html_finalization_fixture";
/// Path the fixture answers with a `Trailer` field.
pub(crate) const FRAMING_TRAILER_PATH: &str =
    "/integrations/html_finalization_fixture/framing-trailer";
const EOF_SENTINEL: &[u8] = b"<!--example-document-eof-->";
const ORIGINAL: &[u8] = b"EXAMPLE_UNSIGNED";
const REPLACEMENT: &[u8] = b"EXAMPLE_FINALIZED";

/// Largest lookup reply the fixture reads.
const MAX_LOOKUP_RESPONSE_BYTES: u64 = 64;

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
            .with_proxy(Arc::new(FramingTrailerProxy))
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
            // Fastly cannot interrupt a blocking host call, so its request also
            // carries the remaining chain budget as a deadline. The constant
            // timeout keeps the backend identity stable; the reported timeout is
            // the bound the request actually runs under.
            let transport_timeout =
                Duration::from_millis(u64::from(self.config.transport_timeout_ms));
            let cooperative = input.deadline.has_cooperative_timer();
            let reported_timeout = if cooperative {
                transport_timeout
            } else {
                input.deadline.remaining().min(transport_timeout)
            };
            let payload = serde_json::to_vec(&json!({
                "complete": true,
                "sha256": hex::encode(self.digest.clone().finalize()),
                "bytes": self.bytes,
                "transport_timeout_ms": reported_timeout.as_millis(),
            }))
            .change_context(StreamFinishError::Rejected)?;
            let mut request = buffered(
                outbound_request(http::Method::POST, &self.config.lookup_url)
                    .map_err(|error| fixture_error(error, StreamFinishError::Rejected))?,
                transport_timeout,
                MAX_LOOKUP_RESPONSE_BYTES,
                ResponseDecoding::Decode,
            )
            .cache_policy(OutboundCachePolicy::Bypass)
            .header(http::header::CONTENT_TYPE, "application/json")
            .map_err(|error| fixture_error(error, StreamFinishError::Rejected))?
            .body(payload);
            if !cooperative {
                request = request.deadline(
                    input
                        .services
                        .clock()
                        .deadline_after(input.deadline.remaining()),
                );
            }
            let response = input
                .services
                .outbound_client()
                .send(request)
                .await
                .map_err(|error| fixture_error(error, StreamFinishError::Unavailable))?;
            if !response.is_success() {
                return Err(Report::new(StreamFinishError::Unavailable));
            }
            let reply = response
                .into_bytes_bounded(MAX_LOOKUP_RESPONSE_BYTES)
                .await
                .map_err(|error| fixture_error(error, StreamFinishError::Rejected))?;
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

/// Answers [`FRAMING_TRAILER_PATH`] with a `Trailer` field.
///
/// `EdgeZero` rejects any `Trailer` before commit and delivers its framing
/// fallback 500 in place of this response.
struct FramingTrailerProxy;

#[async_trait(?Send)]
impl IntegrationProxy for FramingTrailerProxy {
    fn integration_name(&self) -> &'static str {
        ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![IntegrationEndpoint::get(FRAMING_TRAILER_PATH)]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        _services: &RuntimeServices,
        _req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let mut response = Response::new(EdgeBody::from("example body with a trailer"));
        response
            .headers_mut()
            .insert(header::TRAILER, HeaderValue::from_static("x-example"));
        Ok(response)
    }
}

/// Reports an outbound failure as `context`, carrying the `EdgeError` for diagnosis.
fn fixture_error(error: EdgeError, context: StreamFinishError) -> Report<StreamFinishError> {
    Report::new(context).attach_opaque(error)
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
            assert_eq!(
                registry.has_route(&http::Method::GET, FRAMING_TRAILER_PATH),
                enabled,
                "should route the framing fixture only when enabled"
            );
        }
    }

    #[test]
    fn framing_route_answers_with_a_trailer_field() {
        let response = futures::executor::block_on(FramingTrailerProxy.handle(
            &create_test_settings(),
            &crate::platform::test_support::noop_services(),
            Request::new(EdgeBody::empty()),
        ))
        .expect("should answer the framing route");

        assert_eq!(
            response.headers().get(header::TRAILER),
            Some(&HeaderValue::from_static("x-example")),
            "should carry the Trailer field EdgeZero rejects"
        );
    }
}
