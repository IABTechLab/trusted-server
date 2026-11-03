//! Ordered optional HTML EOF work with driver-owned fallback and streaming output.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use futures::future::{Fuse, LocalBoxFuture};
use http::StatusCode;

use super::{
    AuctionCollectDeps, AuctionHoldState, AuctionTelemetryCarry, BodyChunkSource,
    DispatchedAuctionGuard, HoldStepSegments, HtmlStreamProcessorParams, InlineBodyCloseSeam,
    OwnedProcessResponseParams, abandon_hold_auction,
    create_html_stream_processor_with_document_state, deferred_inline_seam_token,
    hold_collect_close_tail, hold_finish_tail_segments,
};
use crate::error::TrustedServerError;
use crate::html_processor::HtmlWithStreamingProcessors;
use crate::html_processor::finish::HtmlFinishContext;
use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
use crate::settings::Settings;
use crate::streaming_processor::finish::StreamFinishDeadline;
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, STREAM_CHUNK_SIZE, StreamProcessor,
};

struct TerminalInput {
    source: BodyChunkSource,
    decoder: Box<BodyStreamDecoder>,
}

impl TerminalInput {
    fn new(body: EdgeBody, compression: Compression, limit: usize) -> Self {
        Self {
            source: BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(limit),
            decoder: Box::new(BodyStreamDecoder::new(compression, limit)),
        }
    }

    async fn next(
        &mut self,
        failure_reason: &mut &'static str,
    ) -> Result<(Bytes, bool), Report<TrustedServerError>> {
        *failure_reason = "stream_read_error";
        let raw = self.source.next_chunk().await?;
        *failure_reason = "stream_decode_error";
        match raw {
            Some(raw) => Ok((self.decoder.decode_chunk(raw)?, false)),
            None => Ok((Bytes::from(self.decoder.finish()?), true)),
        }
    }
}

pub(super) struct HtmlEofDriver {
    response_status: StatusCode,
    input: TerminalInput,
    encoder: BodyStreamEncoder,
    processor: HtmlWithStreamingProcessors,
    replay: VecDeque<Bytes>,
    hold: Option<AuctionHoldState>,
    limit: usize,
    timeout: Duration,
    source_finished: bool,
    stages_finished: bool,
    finish_deadline: Option<StreamFinishDeadline>,
    finish_timer: Option<Fuse<LocalBoxFuture<'static, ()>>>,
    had_ready_bytes: bool,
    callback_barrier_issued: bool,
    flush_barrier: bool,
    finished: bool,
    seam_flushed: bool,
    failure_reason: &'static str,
}

impl HtmlEofDriver {
    pub(super) fn new(
        body: EdgeBody,
        params: &mut OwnedProcessResponseParams,
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> Result<Self, Report<TrustedServerError>> {
        registry.ensure_async_html_finish_allowed(settings)?;
        if params.template_cache_key.is_some() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "Async HTML completion cannot use shared-template authorization; request and response registries must match".to_string(),
            }));
        }
        let document_state = IntegrationDocumentState::default();
        let marker = deferred_inline_seam_token(
            settings,
            false,
            params.ad_slots_script.is_some(),
            params.dispatched_auction.is_some(),
        );
        let mut processor = create_html_stream_processor_with_document_state(
            HtmlStreamProcessorParams {
                origin_host: &params.origin_host,
                request_host: &params.request_host,
                request_scheme: &params.request_scheme,
                settings,
                integration_registry: registry,
                ad_slots_script: params.ad_slots_script.clone(),
                ad_bids_state: Arc::clone(params.ad_bids_state.script_cell()),
                suppress_datadome_client_side_tag: params.suppress_datadome_client_side_tag,
                gpt_diagnostics: params.gpt_diagnostics.clone(),
                trace_bootstrap: params.trace_bootstrap.clone(),
                shared_template_authorized: false,
                csp_nonce_observed: None,
                deferred_inline_marker: marker
                    .as_ref()
                    .map(|marker| String::from_utf8_lossy(marker).into_owned()),
            },
            &document_state,
        )?;
        processor
            .validate_capabilities()
            .change_context(TrustedServerError::Configuration {
                message: "HTML processor EOF capabilities are inconsistent".to_string(),
            })?;
        processor.set_finish_marker(marker.as_deref());
        let compression = Compression::from_content_encoding(&params.content_encoding);
        let limit = settings.publisher.max_buffered_body_bytes;
        let input = TerminalInput::new(body, compression, limit);
        let hold = params.dispatched_auction.take().map(|dispatched| {
            AuctionHoldState::new(
                DispatchedAuctionGuard::new(dispatched),
                AuctionTelemetryCarry {
                    observation: params.auction_observation.take(),
                    auction_request: params.auction_request.take(),
                },
                marker.clone(),
            )
        });
        Ok(Self {
            response_status: params.response_status,
            input,
            encoder: BodyStreamEncoder::new(params.transform_compression()),
            processor,
            replay: VecDeque::new(),
            hold,
            limit,
            timeout: Duration::from_millis(u64::from(
                settings.publisher.html_finalization_timeout_ms,
            )),
            source_finished: false,
            stages_finished: false,
            finish_deadline: None,
            finish_timer: None,
            had_ready_bytes: false,
            callback_barrier_issued: false,
            flush_barrier: false,
            finished: false,
            seam_flushed: false,
            failure_reason: "stream_process_error",
        })
    }

    pub(super) async fn next_step(
        &mut self,
        deps: &AuctionCollectDeps<'_>,
    ) -> Result<Option<HoldStepSegments>, Report<TrustedServerError>> {
        self.flush_barrier = false;
        let result = self.next_inner(deps).await;
        if result.is_err() {
            self.abandon(deps, self.failure_reason).await;
        }
        result
    }

    async fn next_inner(
        &mut self,
        deps: &AuctionCollectDeps<'_>,
    ) -> Result<Option<HoldStepSegments>, Report<TrustedServerError>> {
        loop {
            if let Some(bytes) = self.replay.pop_front() {
                if bytes.is_empty() {
                    continue;
                }
                let end = bytes.len().min(STREAM_CHUNK_SIZE);
                let chunk = bytes.slice(..end);
                if end < bytes.len() {
                    self.replay.push_front(bytes.slice(end..));
                }
                self.failure_reason = "auction_suffix_overflow";
                let processed =
                    if let Some(seam) = self.hold.as_mut().and_then(|hold| hold.hold.as_mut()) {
                        seam.push_bounded(&chunk, self.limit).change_context(
                            TrustedServerError::Proxy {
                                message: "HTML auction suffix exceeds its retained-byte limit"
                                    .to_string(),
                            },
                        )?
                    } else {
                        chunk.to_vec()
                    };
                self.failure_reason = "stream_encode_error";
                let encoded = self.encoder.encode_chunk(processed)?;
                self.had_ready_bytes |= !encoded.is_empty();
                return Ok(Some(HoldStepSegments {
                    ready: if encoded.is_empty() {
                        Vec::new()
                    } else {
                        vec![Bytes::from(encoded)]
                    },
                    close_found: false,
                }));
            }
            if self.finished {
                return Ok(None);
            }
            if self.source_finished && !self.stages_finished {
                if self.had_ready_bytes
                    && !self.callback_barrier_issued
                    && self.processor.next_finish_requires_async()
                {
                    self.callback_barrier_issued = true;
                    self.flush_barrier = true;
                    return Ok(Some(HoldStepSegments {
                        ready: Vec::new(),
                        close_found: false,
                    }));
                }
                self.failure_reason = "stream_process_error";
                let output = self
                    .processor
                    .finish_next(HtmlFinishContext {
                        response_status: self.response_status,
                        services: deps.services,
                        deadline: self
                            .finish_deadline
                            .as_ref()
                            .expect("should start EOF deadline after parser completion"),
                        timer: &mut self.finish_timer,
                        limit: self.limit,
                    })
                    .await
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to finish HTML processor".to_string(),
                    })?;
                self.callback_barrier_issued = false;
                if let Some(output) = output {
                    self.replay.push_back(Bytes::from(output));
                } else {
                    self.stages_finished = true;
                }
                continue;
            }
            if self.stages_finished && !self.seam_flushed {
                self.seam_flushed = true;
                self.failure_reason = "stream_encode_error";
                if self
                    .hold
                    .as_ref()
                    .and_then(|hold| hold.hold.as_ref())
                    .is_some_and(InlineBodyCloseSeam::found)
                {
                    return Ok(Some(HoldStepSegments {
                        ready: Vec::new(),
                        close_found: true,
                    }));
                }
                let suffix = self.hold.as_mut().and_then(|hold| {
                    if hold.hold.as_ref().is_some_and(|seam| !seam.found()) {
                        hold.hold.take().map(InlineBodyCloseSeam::finish)
                    } else {
                        None
                    }
                });
                let encoded = self.encoder.encode_chunk(suffix.unwrap_or_default())?;
                return Ok(Some(HoldStepSegments {
                    ready: if encoded.is_empty() {
                        Vec::new()
                    } else {
                        vec![Bytes::from(encoded)]
                    },
                    close_found: false,
                }));
            }
            if self.stages_finished {
                self.finished = true;
                self.failure_reason = "stream_encode_error";
                let ready = if let Some(hold) = self.hold.as_mut() {
                    hold_finish_tail_segments(&mut self.encoder, hold, deps).await?
                } else {
                    let trailer = self.encoder.finish()?;
                    if trailer.is_empty() {
                        Vec::new()
                    } else {
                        vec![Bytes::from(trailer)]
                    }
                };
                return Ok(Some(HoldStepSegments {
                    ready,
                    close_found: false,
                }));
            }
            let (decoded, ended) = self.input.next(&mut self.failure_reason).await?;
            if !decoded.is_empty() {
                self.accept(&decoded)?;
            }
            if ended {
                self.failure_reason = "stream_process_error";
                let output =
                    self.processor
                        .begin_finish()
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to finish HTML parser".to_string(),
                        })?;
                self.source_finished = true;
                self.finish_deadline = Some(StreamFinishDeadline::new(
                    self.timeout,
                    deps.services.timer().is_some(),
                ));

                self.replay.push_back(Bytes::from(output));
            }
        }
    }

    fn accept(&mut self, bytes: &[u8]) -> Result<(), Report<TrustedServerError>> {
        self.failure_reason = "stream_process_error";
        let output = self.processor.process_chunk(bytes, false).change_context(
            TrustedServerError::Proxy {
                message: "Failed to process HTML terminal input".to_string(),
            },
        )?;
        self.replay.push_back(Bytes::from(output));
        Ok(())
    }

    pub(super) fn at_finish_barrier(&self) -> bool {
        self.flush_barrier || (self.stages_finished && self.seam_flushed && !self.finished)
    }

    pub(super) async fn collect_close(
        &mut self,
        deps: &AuctionCollectDeps<'_>,
    ) -> Result<Vec<Bytes>, Report<TrustedServerError>> {
        if let Some(hold) = self.hold.as_mut() {
            hold_collect_close_tail(&mut self.encoder, hold, deps).await
        } else {
            Ok(Vec::new())
        }
    }

    pub(super) async fn abandon(&mut self, deps: &AuctionCollectDeps<'_>, reason: &'static str) {
        if let Some(hold) = self.hold.as_mut() {
            abandon_hold_auction(hold, deps.services, reason).await;
        }
    }
}
