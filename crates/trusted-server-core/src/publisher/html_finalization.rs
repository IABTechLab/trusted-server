//! Optional terminal HTML work; current bytes stay owned outside callback futures.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use brotli::Decompressor;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use flate2::{Decompress, FlushDecompress, Status};
use futures::FutureExt as _;
use web_time::Instant;

use super::{
    AuctionCollectDeps, AuctionHoldState, AuctionTelemetryCarry, BodyChunkSource,
    DispatchedAuctionGuard, HoldStepSegments, HtmlStreamProcessorParams, InlineBodyCloseSeam,
    OwnedProcessResponseParams, abandon_hold_auction, body_as_reader,
    create_html_stream_processor_with_document_state, deferred_inline_seam_token,
    encode_processed_html, hold_collect_close_tail, hold_finish_tail_segments,
};
use crate::error::TrustedServerError;
use crate::integrations::{
    HtmlFinalization, HtmlFinalizationDeadline, HtmlFinalizationError, HtmlFinalizationInput,
    HtmlReplacementWriter, IntegrationDocumentState, IntegrationHtmlFinalizer,
    IntegrationHtmlStreamContext, IntegrationRegistry,
};
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use crate::streaming_processor::{
    BodyStreamDecoder, BodyStreamEncoder, Compression, GzipDecodeReader, STREAM_CHUNK_SIZE,
    StreamProcessor,
};

struct HtmlFinalizerSession {
    integration_id: &'static str,
    finalizer: Box<dyn IntegrationHtmlFinalizer>,
}

fn log_timeout(integration_id: &str, started_at: Instant, bytes: usize) {
    log::warn!(
        "HTML finalizer {integration_id} times out after {} ms; retains {bytes} bytes",
        started_at.elapsed().as_millis()
    );
}

async fn finalize_document(
    mut current: Vec<u8>,
    sessions: &mut [HtmlFinalizerSession],
    marker: Option<&[u8]>,
    services: &RuntimeServices,
    limit: usize,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = HtmlFinalizationDeadline::new(timeout, services.timer().is_some());
    let started_at = Instant::now();
    let mut timer = services.timer().map(|timer| timer.delay(timeout).fuse());
    for session in sessions {
        if deadline.is_expired_at(Instant::now()) {
            break;
        }
        let mut replacement = HtmlReplacementWriter::new(limit);
        let result = {
            let work = session
                .finalizer
                .finalize(HtmlFinalizationInput {
                    html: &current,
                    replacement: &mut replacement,
                    services,
                    deadline: &deadline,
                })
                .fuse();
            futures::pin_mut!(work);
            if let Some(mut timer) = timer.as_mut() {
                futures::select_biased! {
                    _ = timer => None,
                    result = work => Some(result),
                }
            } else {
                Some(work.await)
            }
        };
        // The callback future has dropped before either borrowed buffer is used.
        let expired = result.is_none() || deadline.is_expired_at(Instant::now());
        if expired {
            log_timeout(session.integration_id, started_at, current.len());
            break;
        }
        let result = result.expect("should have a result before the deadline");
        let outcome = match result {
            Ok(HtmlFinalization::Unchanged) => "unchanged",
            Ok(HtmlFinalization::Replace) if !replacement.is_poisoned() => {
                let candidate = replacement.into_bytes();
                if marker.is_none_or(|token| {
                    marker_count(&candidate, token) == marker_count(&current, token)
                }) {
                    if deadline.is_expired_at(Instant::now()) {
                        log_timeout(session.integration_id, started_at, current.len());
                        break;
                    }
                    current = candidate;
                    "replaced"
                } else {
                    "invalid_marker"
                }
            }
            Ok(HtmlFinalization::Replace) => "output_limit",
            Err(error) => match error.current_context() {
                HtmlFinalizationError::Unavailable => "unavailable",
                HtmlFinalizationError::Rejected => "rejected",
                HtmlFinalizationError::OutputLimitExceeded { .. } => "output_limit",
            },
        };
        if matches!(outcome, "unchanged" | "replaced") {
            log::debug!(
                "HTML finalizer {} completes: {outcome}, {} bytes, {} ms",
                session.integration_id,
                current.len(),
                started_at.elapsed().as_millis()
            );
        } else {
            log::warn!(
                "HTML finalizer {} retains input: {outcome}, {} bytes, {} ms",
                session.integration_id,
                current.len(),
                started_at.elapsed().as_millis()
            );
        }
    }
    current
}

fn marker_count(bytes: &[u8], token: &[u8]) -> usize {
    if token.is_empty() {
        return 0;
    }
    bytes
        .windows(token.len())
        .filter(|window| *window == token)
        .count()
}

struct HtmlCollector {
    retained: Option<HtmlReplacementWriter>,
}

impl HtmlCollector {
    fn new(limit: usize) -> Self {
        Self {
            retained: Some(HtmlReplacementWriter::new(limit)),
        }
    }

    fn push(&mut self, output: Vec<u8>) -> Vec<bytes::Bytes> {
        let Some(retained) = self.retained.as_mut() else {
            return vec![bytes::Bytes::from(output)];
        };
        if retained.append(&output).is_ok() {
            return Vec::new();
        }
        let retained = self
            .retained
            .take()
            .expect("should own the collected prefix");
        log::warn!("HTML finalization bypasses document: collection exceeds its byte limit");
        vec![
            bytes::Bytes::from(retained.into_bytes()),
            bytes::Bytes::from(output),
        ]
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        self.retained.take().map(HtmlReplacementWriter::into_bytes)
    }
}

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

pub(super) struct HtmlFinalizationDriver {
    input: TerminalInput,
    encoder: BodyStreamEncoder,
    processor: Box<dyn StreamProcessor>,
    collector: HtmlCollector,
    replay: VecDeque<Bytes>,
    sessions: Vec<HtmlFinalizerSession>,
    marker: Option<Vec<u8>>,
    hold: Option<AuctionHoldState>,
    limit: usize,
    timeout: Duration,
    source_finished: bool,
    finished: bool,
    seam_flushed: bool,
    failure_reason: &'static str,
}

impl HtmlFinalizationDriver {
    pub(super) fn new(
        body: EdgeBody,
        params: &mut OwnedProcessResponseParams,
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> Result<Self, Report<TrustedServerError>> {
        let document_state = IntegrationDocumentState::default();
        let marker = deferred_inline_seam_token(
            settings,
            false,
            params.ad_slots_script.is_some(),
            params.dispatched_auction.is_some(),
        );
        let processor = create_html_stream_processor_with_document_state(
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
        let context = IntegrationHtmlStreamContext {
            request_host: params.request_host.clone(),
            request_scheme: params.request_scheme.clone(),
            origin_host: params.origin_host.clone(),
            document_state,
        };
        let sessions = registry
            .html_finalizer_factories()
            .iter()
            .map(|factory| HtmlFinalizerSession {
                integration_id: factory.integration_id(),
                finalizer: factory.create(context.clone()),
            })
            .collect();
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
        let compression = Compression::from_content_encoding(&params.content_encoding);
        let limit = settings.publisher.max_buffered_body_bytes;
        Ok(Self {
            input: TerminalInput::new(body, compression, limit)?,
            encoder: BodyStreamEncoder::new(compression),
            processor: Box::new(processor),
            collector: HtmlCollector::new(limit),
            replay: VecDeque::new(),
            sessions,
            marker,
            hold,
            limit,
            timeout: Duration::from_millis(u64::from(
                settings.publisher.html_finalization_timeout_ms,
            )),
            source_finished: false,
            finished: false,
            seam_flushed: false,
            failure_reason: "stream_process_error",
        })
    }

    pub(super) async fn next_step(
        &mut self,
        deps: &AuctionCollectDeps<'_>,
    ) -> Result<Option<HoldStepSegments>, Report<TrustedServerError>> {
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
                self.failure_reason = "stream_encode_error";
                return encode_processed_html(
                    &mut self.encoder,
                    chunk.to_vec(),
                    self.hold.as_mut(),
                )
                .map(Some);
            }
            if self.finished {
                return Ok(None);
            }
            if self.source_finished && !self.seam_flushed {
                self.seam_flushed = true;
                self.failure_reason = "stream_encode_error";
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
            if self.source_finished {
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
                self.accept(&[], true)?;
                self.source_finished = true;
                if let Some(current) = self.collector.finish() {
                    let current = finalize_document(
                        current,
                        &mut self.sessions,
                        self.marker.as_deref(),
                        deps.services,
                        self.limit,
                        self.timeout,
                    )
                    .await;
                    self.replay.push_back(Bytes::from(current));
                }
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
        self.replay.extend(self.collector.push(output));
        Ok(())
    }

    pub(super) fn input_finished(&self) -> bool {
        self.source_finished
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
    #[test]
    fn collection_overflow_replays_prefix_and_crossing_chunk_once() {
        let mut collector = HtmlCollector::new(4);
        assert!(
            collector.push(b"page".to_vec()).is_empty(),
            "should retain input at the cap"
        );
        let mut emitted = collector.push(b"!".to_vec()).concat();
        emitted.extend(collector.push(b"tail".to_vec()).concat());
        assert_eq!(
            emitted, b"page!tail",
            "should release overflow without duplication"
        );
        assert!(
            collector.finish().is_none(),
            "should skip finalizer I/O after bypass"
        );
    }

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    use error_stack::Report;
    use futures::executor::block_on;
    use futures::future::{LocalBoxFuture, poll_fn};
    use std::task::Poll;

    use crate::integrations::{
        HtmlFinalization, HtmlFinalizationError, HtmlFinalizationInput, IntegrationHtmlFinalizer,
    };
    use crate::platform::test_support::{
        NoopBackend, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore, noop_services,
    };
    use crate::platform::{PlatformTimer, RuntimeServices};

    use super::*;

    struct AppendFinalizer {
        suffix: &'static [u8],
        inputs: Rc<RefCell<Vec<Vec<u8>>>>,
        fail: bool,
    }

    impl IntegrationHtmlFinalizer for AppendFinalizer {
        fn finalize<'a>(
            &'a mut self,
            input: HtmlFinalizationInput<'a>,
        ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>> {
            Box::pin(async move {
                let mut yielded = false;
                poll_fn(|context| {
                    if yielded {
                        Poll::Ready(())
                    } else {
                        yielded = true;
                        context.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                self.inputs.borrow_mut().push(input.html.to_vec());
                input.replacement.append(input.html)?;
                input.replacement.append(self.suffix)?;
                if self.fail {
                    Err(Report::new(HtmlFinalizationError::Rejected))
                } else {
                    Ok(HtmlFinalization::Replace)
                }
            })
        }
    }

    #[test]
    fn yielding_finalizers_commit_in_order_and_discard_failed_candidates() {
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let mut sessions = [
            HtmlFinalizerSession {
                integration_id: "example_a",
                finalizer: Box::new(AppendFinalizer {
                    suffix: b"a",
                    inputs: Rc::clone(&inputs),
                    fail: false,
                }),
            },
            HtmlFinalizerSession {
                integration_id: "example_b",
                finalizer: Box::new(AppendFinalizer {
                    suffix: b"bad",
                    inputs: Rc::clone(&inputs),
                    fail: true,
                }),
            },
            HtmlFinalizerSession {
                integration_id: "example_c",
                finalizer: Box::new(AppendFinalizer {
                    suffix: b"c",
                    inputs: Rc::clone(&inputs),
                    fail: false,
                }),
            },
        ];
        let services = noop_services();
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &services,
            32,
            Duration::from_secs(1),
        ));
        assert_eq!(output, b"pageac", "should retain only accepted candidates");
        assert_eq!(
            *inputs.borrow(),
            [b"page".to_vec(), b"pagea".to_vec(), b"pagea".to_vec()],
            "should propagate committed input in order"
        );
    }

    #[test]
    fn poisoned_replacement_preserves_the_original_allocation() {
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(AppendFinalizer {
                suffix: b"oversize",
                inputs,
                fail: false,
            }),
        }];
        let input = b"page".to_vec();
        let allocation = input.as_ptr();
        let output = block_on(finalize_document(
            input,
            &mut sessions,
            None,
            &noop_services(),
            4,
            Duration::from_secs(1),
        ));
        assert_eq!(output, b"page", "should discard oversized output");
        assert_eq!(
            output.as_ptr(),
            allocation,
            "should move fallback without copying it"
        );
    }
    struct ControlledTimer {
        pending_polls: usize,
    }

    impl PlatformTimer for ControlledTimer {
        fn delay(&self, _: Duration) -> LocalBoxFuture<'static, ()> {
            let mut remaining = self.pending_polls;
            Box::pin(poll_fn(move |context| {
                if remaining == 0 {
                    Poll::Ready(())
                } else {
                    remaining -= 1;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            }))
        }
    }

    struct DropProbe(Rc<Cell<bool>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    struct HungFinalizer {
        dropped: Rc<Cell<bool>>,
        started: Rc<Cell<bool>>,
    }

    impl IntegrationHtmlFinalizer for HungFinalizer {
        fn finalize<'a>(
            &'a mut self,
            input: HtmlFinalizationInput<'a>,
        ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>> {
            let probe = DropProbe(Rc::clone(&self.dropped));
            Box::pin(async move {
                let _probe = probe;
                self.started.set(true);
                input.replacement.append(b"partial")?;
                futures::future::pending().await
            })
        }
    }

    fn timer_services(pending_polls: usize) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .client_info(crate::platform::ClientInfo::default())
            .timer(Arc::new(ControlledTimer { pending_polls }))
            .build()
    }

    #[test]
    fn timeout_drops_borrowing_future_preserves_allocation_and_skips_later_work() {
        let dropped = Rc::new(Cell::new(false));
        let started = Rc::new(Cell::new(false));
        let later_inputs = Rc::new(RefCell::new(Vec::new()));
        let mut sessions = [
            HtmlFinalizerSession {
                integration_id: "example_hung",
                finalizer: Box::new(HungFinalizer {
                    dropped: Rc::clone(&dropped),
                    started: Rc::clone(&started),
                }),
            },
            HtmlFinalizerSession {
                integration_id: "example_later",
                finalizer: Box::new(AppendFinalizer {
                    suffix: b"later",
                    inputs: Rc::clone(&later_inputs),
                    fail: false,
                }),
            },
        ];
        let input = b"page".to_vec();
        let allocation = input.as_ptr();
        let output = block_on(finalize_document(
            input,
            &mut sessions,
            None,
            &timer_services(1),
            32,
            Duration::from_secs(1),
        ));
        assert!(
            started.get(),
            "should poll outside work before timer expiry"
        );
        assert!(
            dropped.get(),
            "should drop the borrowing future before returning"
        );
        assert!(
            later_inputs.borrow().is_empty(),
            "should skip remaining finalizers"
        );
        assert_eq!(
            output, b"page",
            "should discard a timed-out partial candidate"
        );
        assert_eq!(
            output.as_ptr(),
            allocation,
            "should retain the original allocation"
        );
    }

    #[test]
    fn expired_timer_wins_before_polling_ready_work() {
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(AppendFinalizer {
                suffix: b"replacement",
                inputs: Rc::clone(&inputs),
                fail: false,
            }),
        }];
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &timer_services(0),
            32,
            Duration::from_secs(1),
        ));
        assert_eq!(output, b"page", "should give timer expiration priority");
        assert!(
            inputs.borrow().is_empty(),
            "should not poll work after expiry"
        );
    }

    #[test]
    fn marker_multiplicity_rejects_added_markers() {
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(AppendFinalizer {
                suffix: b"TOKEN",
                inputs: Rc::new(RefCell::new(Vec::new())),
                fail: false,
            }),
        }];
        let output = block_on(finalize_document(
            b"pageTOKEN".to_vec(),
            &mut sessions,
            Some(b"TOKEN"),
            &noop_services(),
            32,
            Duration::from_secs(1),
        ));
        assert_eq!(
            output, b"pageTOKEN",
            "should reject a duplicated assembly marker"
        );
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            Some(b"TOKEN"),
            &noop_services(),
            32,
            Duration::from_secs(1),
        ));
        assert_eq!(
            output, b"page",
            "should reject adding a marker to a document without one"
        );
    }
    struct ImmediateFinalizer {
        ignore_overflow: bool,
    }

    impl IntegrationHtmlFinalizer for ImmediateFinalizer {
        fn finalize<'a>(
            &'a mut self,
            input: HtmlFinalizationInput<'a>,
        ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>> {
            Box::pin(async move {
                input.replacement.append(input.html)?;
                if self.ignore_overflow {
                    let _ = input.replacement.append(b"oversize");
                } else {
                    input.replacement.append(b"a")?;
                }
                Ok(HtmlFinalization::Replace)
            })
        }
    }

    #[test]
    fn ignored_output_limit_cannot_commit_a_partial_candidate() {
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(ImmediateFinalizer {
                ignore_overflow: true,
            }),
        }];
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &noop_services(),
            4,
            Duration::from_secs(1),
        ));
        assert_eq!(
            output, b"page",
            "should reject Replace after an ignored writer error"
        );
    }

    #[test]
    fn one_timer_covers_the_chain_and_preserves_an_earlier_commit() {
        let dropped = Rc::new(Cell::new(false));
        let mut sessions = [
            HtmlFinalizerSession {
                integration_id: "example_ready",
                finalizer: Box::new(ImmediateFinalizer {
                    ignore_overflow: false,
                }),
            },
            HtmlFinalizerSession {
                integration_id: "example_hung",
                finalizer: Box::new(HungFinalizer {
                    dropped: Rc::clone(&dropped),
                    started: Rc::new(Cell::new(false)),
                }),
            },
        ];
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &timer_services(1),
            32,
            Duration::from_secs(1),
        ));
        assert_eq!(
            output, b"pagea",
            "should retain the earlier commit when the shared timer expires"
        );
        assert!(
            dropped.get(),
            "should drop the second callback at the shared deadline"
        );
    }

    #[test]
    fn cancellation_drops_active_callback_and_its_candidate() {
        let dropped = Rc::new(Cell::new(false));
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(HungFinalizer {
                dropped: Rc::clone(&dropped),
                started: Rc::new(Cell::new(false)),
            }),
        }];
        let services = noop_services();
        let work = finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &services,
            32,
            Duration::from_secs(1),
        );
        let mut work = Box::pin(work);
        let waker = futures::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        assert!(
            std::future::Future::poll(work.as_mut(), &mut context).is_pending(),
            "should yield while outside work is pending"
        );
        drop(work);
        assert!(
            dropped.get(),
            "should cancel the active future with the document"
        );
    }
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
    struct FixedCandidate {
        bytes: &'static [u8],
        decision: HtmlFinalization,
    }
    impl IntegrationHtmlFinalizer for FixedCandidate {
        fn finalize<'a>(
            &'a mut self,
            input: HtmlFinalizationInput<'a>,
        ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>> {
            Box::pin(async move {
                input.replacement.append(self.bytes)?;
                Ok(self.decision)
            })
        }
    }

    #[test]
    fn unchanged_discards_candidate_and_missing_markers_reject_replacement() {
        for (decision, marker) in [
            (HtmlFinalization::Unchanged, None),
            (HtmlFinalization::Replace, Some(&b"TOKEN"[..])),
        ] {
            let mut sessions = [HtmlFinalizerSession {
                integration_id: "example",
                finalizer: Box::new(FixedCandidate {
                    bytes: b"candidate",
                    decision,
                }),
            }];
            let output = block_on(finalize_document(
                b"pageTOKEN".to_vec(),
                &mut sessions,
                marker,
                &noop_services(),
                32,
                Duration::from_secs(1),
            ));
            assert_eq!(
                output, b"pageTOKEN",
                "should retain input when unchanged or a marker is removed"
            );
        }
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(FixedCandidate {
                bytes: b"",
                decision: HtmlFinalization::Replace,
            }),
        }];
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &noop_services(),
            32,
            Duration::from_secs(1),
        ));
        assert!(
            output.is_empty(),
            "should allow an intentional empty replacement without a marker"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn no_timer_rejects_completion_after_the_absolute_deadline() {
        struct LateFinalizer;
        impl IntegrationHtmlFinalizer for LateFinalizer {
            fn finalize<'a>(
                &'a mut self,
                input: HtmlFinalizationInput<'a>,
            ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>
            {
                Box::pin(async move {
                    std::thread::sleep(input.deadline.remaining() + Duration::from_millis(2));
                    input.replacement.append(b"late")?;
                    Ok(HtmlFinalization::Replace)
                })
            }
        }
        let mut sessions = [HtmlFinalizerSession {
            integration_id: "example",
            finalizer: Box::new(LateFinalizer),
        }];
        let output = block_on(finalize_document(
            b"page".to_vec(),
            &mut sessions,
            None,
            &noop_services(),
            32,
            Duration::from_millis(1),
        ));
        assert_eq!(
            output, b"page",
            "should reject late results from a blocking consumer"
        );
    }
}
