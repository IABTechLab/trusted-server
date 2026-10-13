//! Ordered optional HTML EOF work with driver-owned fallback and streaming output.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use brotli::Decompressor;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use flate2::{Decompress, FlushDecompress, Status};
use futures::future::{Fuse, LocalBoxFuture};
use http::StatusCode;

use super::{
    AuctionCollectDeps, AuctionHoldState, AuctionTelemetryCarry, BodyChunkSource,
    DispatchedAuctionGuard, HoldStepSegments, HtmlStreamProcessorParams, InlineBodyCloseSeam,
    OwnedProcessResponseParams, abandon_hold_auction, body_as_reader,
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
    BodyStreamDecoder, BodyStreamEncoder, Compression, GzipDecodeReader, STREAM_CHUNK_SIZE,
    StreamProcessor,
};

// The reader codec must prove StreamEnd before terminal work starts. The
// ordinary flate2 Read wrapper also returns EOF for an incomplete zlib stream.
struct ValidatedZlibReader {
    reader: Box<dyn Read>,
    decoder: Decompress,
    input: [u8; STREAM_CHUNK_SIZE],
    offset: usize,
    available: usize,
    eof: bool,
    complete: bool,
}

impl ValidatedZlibReader {
    fn new(reader: Box<dyn Read>) -> Self {
        Self {
            reader,
            decoder: Decompress::new(true),
            input: [0; STREAM_CHUNK_SIZE],
            offset: 0,
            available: 0,
            eof: false,
            complete: false,
        }
    }
}

impl Read for ValidatedZlibReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.complete {
            return Ok(0);
        }
        loop {
            if self.offset == self.available && !self.eof {
                self.available = self.reader.read(&mut self.input)?;
                self.offset = 0;
                self.eof = self.available == 0;
            }
            let before_in = self.decoder.total_in();
            let before_out = self.decoder.total_out();
            let status = self
                .decoder
                .decompress(
                    &self.input[self.offset..self.available],
                    output,
                    if self.eof {
                        FlushDecompress::Finish
                    } else {
                        FlushDecompress::None
                    },
                )
                .map_err(io::Error::other)?;
            let consumed =
                usize::try_from(self.decoder.total_in() - before_in).map_err(io::Error::other)?;
            let produced =
                usize::try_from(self.decoder.total_out() - before_out).map_err(io::Error::other)?;
            self.offset += consumed;
            self.complete = status == Status::StreamEnd;
            if produced > 0 || self.complete {
                return Ok(produced);
            }
            if self.eof || (consumed == 0 && self.offset < self.available) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete publisher zlib stream",
                ));
            }
        }
    }
}

enum TerminalInput {
    Stream {
        source: BodyChunkSource,
        decoder: Box<BodyStreamDecoder>,
    },
    Materialized(Box<dyn Read>),
}

impl TerminalInput {
    fn new(
        body: EdgeBody,
        compression: Compression,
        limit: usize,
    ) -> Result<Self, Report<TrustedServerError>> {
        if body.is_stream() {
            return Ok(Self::Stream {
                source: BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(limit),
                decoder: Box::new(BodyStreamDecoder::new(compression, limit)),
            });
        }
        // Preserve the existing reader codecs and per-step gzip decode budget.
        let reader = body_as_reader(body)?;
        let reader: Box<dyn Read> = match compression {
            Compression::None => Box::new(reader),
            Compression::Gzip => Box::new(GzipDecodeReader::new(reader, limit)),
            Compression::Deflate => Box::new(ValidatedZlibReader::new(Box::new(reader))),
            Compression::Brotli => Box::new(Decompressor::new(reader, STREAM_CHUNK_SIZE)),
        };
        Ok(Self::Materialized(reader))
    }

    async fn next(
        &mut self,
        failure_reason: &mut &'static str,
    ) -> Result<(Bytes, bool), Report<TrustedServerError>> {
        match self {
            Self::Stream { source, decoder } => {
                *failure_reason = "stream_read_error";
                let raw = source.next_chunk().await?;
                *failure_reason = "stream_decode_error";
                match raw {
                    Some(raw) => Ok((decoder.decode_chunk(raw)?, false)),
                    None => Ok((Bytes::from(decoder.finish()?), true)),
                }
            }
            Self::Materialized(reader) => {
                *failure_reason = "stream_read_error";
                let mut bytes = vec![0; STREAM_CHUNK_SIZE];
                let size = reader
                    .read(&mut bytes)
                    .change_context(TrustedServerError::Proxy {
                        message: "Failed to read decoded HTML terminal input".to_string(),
                    })?;
                bytes.truncate(size);
                Ok((Bytes::from(bytes), size == 0))
            }
        }
    }
}

pub(super) struct HtmlEofDriver {
    response_status: StatusCode,
    input: TerminalInput,
    encoder: BodyStreamEncoder,
    processor: HtmlWithStreamingProcessors,
    replay: VecDeque<Bytes>,
    marker: Option<Vec<u8>>,
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
        let input = TerminalInput::new(body, compression, limit)?;
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
            encoder: BodyStreamEncoder::new(compression),
            processor,
            replay: VecDeque::new(),
            marker,
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
                        marker: self.marker.as_deref(),
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
                self.accept(&decoded, false)?;
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

    fn accept(&mut self, bytes: &[u8], is_last: bool) -> Result<(), Report<TrustedServerError>> {
        self.failure_reason = "stream_process_error";
        let output = self
            .processor
            .process_chunk(bytes, is_last)
            .change_context(TrustedServerError::Proxy {
                message: "Failed to process HTML terminal input".to_string(),
            })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialized_deflate_requires_validated_end_before_terminal_eof() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"<html><body>Example</body></html>")
            .expect("should encode an example page");
        let compressed = encoder.finish().expect("should finish zlib input");
        let mut valid =
            ValidatedZlibReader::new(Box::new(std::io::Cursor::new(compressed.clone())));
        let mut output = Vec::new();
        valid
            .read_to_end(&mut output)
            .expect("should accept a validated zlib trailer");
        for missing in [1, 4, 8] {
            let mut reader = ValidatedZlibReader::new(Box::new(std::io::Cursor::new(
                compressed[..compressed.len() - missing].to_vec(),
            )));
            let _ = reader
                .read_to_end(&mut Vec::new())
                .expect_err("should reject a truncated trailer before terminal EOF");
        }
    }
}
