//! Ordered HTML processor completion and optional bounded output retention.

use std::io;

use futures::FutureExt as _;
use futures::future::{Fuse, LocalBoxFuture};
use http::StatusCode;
use web_time::Instant;

use super::HtmlWithStreamingProcessors;
use crate::platform::RuntimeServices;
use crate::streaming_processor::StreamProcessor;
use crate::streaming_processor::finish::{
    StreamFinish, StreamFinishDeadline, StreamFinishError, StreamFinishInput, StreamOutputWriter,
};

pub(crate) struct HtmlProcessorStage {
    integration_id: &'static str,
    processor: Box<dyn StreamProcessor>,
    async_capable: bool,
    retained: Option<StreamOutputWriter>,
    skip_async: bool,
    buffer_output: bool,
    limit: usize,
    previous_suffix: Vec<u8>,
}

impl HtmlProcessorStage {
    pub(super) fn new(
        integration_id: &'static str,
        processor: Box<dyn StreamProcessor>,
        async_capable: bool,
        buffered: bool,
        limit: usize,
    ) -> Self {
        Self {
            integration_id,
            processor,
            async_capable,
            retained: buffered.then(|| StreamOutputWriter::new(limit)),
            skip_async: false,
            buffer_output: buffered,
            limit,
            previous_suffix: Vec::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.processor.reset();
        self.retained = self
            .buffer_output
            .then(|| StreamOutputWriter::new(self.limit));
        self.skip_async = false;
        self.previous_suffix.clear();
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if self.async_capable != self.processor.requires_async_finish()
            || (self.retained.is_some() && !self.async_capable)
        {
            return Err(io::Error::other(format!(
                "Integration {} declares inconsistent asynchronous EOF capabilities",
                self.integration_id,
            )));
        }
        Ok(())
    }

    pub(super) fn requires_async_finish(&self) -> bool {
        self.async_capable || self.processor.requires_async_finish()
    }

    fn output(&mut self, chunk: &[u8], last: bool) -> io::Result<Vec<u8>> {
        let output = self.processor.process_chunk(chunk, last)?;
        let Some(retained) = self.retained.as_mut() else {
            return Ok(output);
        };
        if retained.append(&output).is_ok() {
            return if last {
                Ok(self
                    .retained
                    .take()
                    .expect("should own retained stage output")
                    .into_bytes())
            } else {
                Ok(Vec::new())
            };
        }
        self.skip_async = true;
        let mut ready = self
            .retained
            .take()
            .expect("should own overflow prefix")
            .into_bytes();
        ready.extend_from_slice(&output);
        log::warn!(
            "HTML EOF processor {} bypasses optional I/O after output collection overflow",
            self.integration_id
        );
        Ok(ready)
    }

    fn remember_suffix(&mut self, output: &[u8], marker_length: usize) {
        let keep = marker_length.saturating_sub(1);
        if keep == 0 || output.is_empty() {
            return;
        }
        if output.len() >= keep {
            self.previous_suffix.clear();
            self.previous_suffix
                .extend_from_slice(&output[output.len() - keep..]);
        } else {
            self.previous_suffix.extend_from_slice(output);
            if self.previous_suffix.len() > keep {
                self.previous_suffix
                    .drain(..self.previous_suffix.len() - keep);
            }
        }
    }

    pub(super) fn process(
        &mut self,
        bytes: &[u8],
        last: bool,
        marker_length: usize,
    ) -> io::Result<Vec<u8>> {
        let output = self.output(bytes, last)?;
        self.remember_suffix(&output, marker_length);
        Ok(output)
    }
}

pub(crate) struct HtmlFinishContext<'a> {
    pub response_status: StatusCode,
    pub services: &'a RuntimeServices,
    pub deadline: &'a StreamFinishDeadline,
    pub timer: &'a mut Option<Fuse<LocalBoxFuture<'static, ()>>>,
    pub limit: usize,
}

fn marker_count(prefix: &[u8], bytes: &[u8], marker: &[u8]) -> usize {
    if marker.is_empty() {
        return 0;
    }
    let ordinary = bytes
        .windows(marker.len())
        .filter(|window| *window == marker)
        .count();
    let mut boundary = prefix.to_vec();
    boundary.extend_from_slice(&bytes[..bytes.len().min(marker.len().saturating_sub(1))]);
    ordinary
        + boundary
            .windows(marker.len())
            .enumerate()
            .filter(|(start, window)| *start < prefix.len() && *window == marker)
            .count()
}

async fn finish_output(
    stage: &mut HtmlProcessorStage,
    fallback: Vec<u8>,
    context: &mut HtmlFinishContext<'_>,
    marker: Option<&[u8]>,
) -> Vec<u8> {
    if stage.skip_async || !stage.async_capable {
        return fallback;
    }
    if fallback.len() > context.limit {
        log::warn!(
            "HTML EOF processor {} skips optional I/O for {} bytes exceeding the {}-byte limit",
            stage.integration_id,
            fallback.len(),
            context.limit
        );
        return fallback;
    }
    if context.deadline.is_expired_at(Instant::now()) {
        log::debug!(
            "HTML EOF processor {} skips optional I/O after deadline expiry",
            stage.integration_id
        );
        return fallback;
    }
    let mut candidate = StreamOutputWriter::new(context.limit);
    let result = {
        let Some(work) = stage.processor.async_finish(StreamFinishInput {
            response_status: context.response_status,
            output: &fallback,
            replacement: &mut candidate,
            services: context.services,
            deadline: context.deadline,
        }) else {
            return fallback;
        };
        let work = work.fuse();
        futures::pin_mut!(work);
        if context.deadline.is_expired_at(Instant::now()) {
            None
        } else {
            if context.timer.is_none()
                && let Some(timer) = context.services.timer()
            {
                *context.timer = Some(timer.delay(context.deadline.remaining()).fuse());
            }
            if let Some(mut timer) = context.timer.as_mut() {
                futures::select_biased! {
                    _ = timer => None,
                    result = work => Some(result),
                }
            } else {
                Some(work.await)
            }
        }
    };
    if result.is_none() {
        context.deadline.mark_expired();
    }
    if let Some(Err(error)) = &result {
        let outcome = match error.current_context() {
            StreamFinishError::Unavailable => "unavailable",
            StreamFinishError::Rejected => "rejected",
            StreamFinishError::OutputLimitExceeded { .. } => "output_limit",
        };
        log::warn!(
            "HTML EOF processor {} retains input: {outcome}",
            stage.integration_id
        );
    }
    let replacement =
        if matches!(result, Some(Ok(StreamFinish::Replace))) && !candidate.is_poisoned() {
            let replacement = candidate.into_bytes();
            if marker.is_some_and(|marker| {
                marker_count(&stage.previous_suffix, &fallback, marker)
                    != marker_count(&stage.previous_suffix, &replacement, marker)
            }) {
                log::warn!(
                    "HTML EOF processor {} retains input after invalid assembly markers",
                    stage.integration_id
                );
                None
            } else {
                Some(replacement)
            }
        } else {
            None
        };
    if result.is_none() || context.deadline.is_expired_at(Instant::now()) {
        log::warn!(
            "HTML EOF processor {} times out after {} ms; retains {} bytes",
            stage.integration_id,
            context.deadline.elapsed().as_millis(),
            fallback.len()
        );
        return fallback;
    }
    replacement.unwrap_or(fallback)
}

impl HtmlWithStreamingProcessors {
    pub(crate) fn from_stages(
        inner: Box<dyn StreamProcessor>,
        stages: Vec<HtmlProcessorStage>,
    ) -> Self {
        Self {
            inner,
            stages,
            finish_index: None,
            finish_marker: None,
        }
    }

    pub(crate) fn validate_capabilities(&self) -> io::Result<()> {
        for stage in &self.stages {
            stage.validate()?;
        }
        Ok(())
    }

    pub(crate) fn set_finish_marker(&mut self, marker: Option<&[u8]>) {
        self.finish_marker = marker.map(<[u8]>::to_vec);
    }

    pub(crate) fn next_finish_requires_async(&self) -> bool {
        self.finish_index
            .and_then(|index| self.stages.get(index))
            .is_some_and(|stage| stage.async_capable && !stage.skip_async)
    }

    pub(crate) fn begin_finish(&mut self) -> io::Result<Vec<u8>> {
        self.validate_capabilities()?;
        if self.finish_index.is_some() {
            return Err(io::Error::other("HTML parser already receives EOF"));
        }
        let mut output = self.inner.process_chunk(&[], true)?;
        for stage in &mut self.stages {
            output = stage.process(
                &output,
                false,
                self.finish_marker.as_ref().map_or(0, Vec::len),
            )?;
        }
        self.finish_index = Some(0);
        Ok(output)
    }

    pub(crate) async fn finish_next(
        &mut self,
        mut context: HtmlFinishContext<'_>,
    ) -> io::Result<Option<Vec<u8>>> {
        let index = self
            .finish_index
            .ok_or_else(|| io::Error::other("HTML parser must finish before processor EOF"))?;
        let Some(stage) = self.stages.get_mut(index) else {
            return Ok(None);
        };
        let fallback = stage.output(&[], true)?;
        let mut output =
            finish_output(stage, fallback, &mut context, self.finish_marker.as_deref()).await;
        stage.remember_suffix(&output, self.finish_marker.as_ref().map_or(0, Vec::len));
        for downstream in &mut self.stages[index + 1..] {
            output = downstream.process(
                &output,
                false,
                self.finish_marker.as_ref().map_or(0, Vec::len),
            )?;
        }
        self.finish_index = Some(index + 1);
        Ok(Some(output))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io;
    use std::rc::Rc;
    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use error_stack::Report;
    use futures::executor::block_on;
    use futures::future::{LocalBoxFuture, poll_fn};
    use http::StatusCode;
    use std::task::Poll;

    use super::*;
    use crate::html_processor::HtmlWithStreamingProcessors;
    use crate::platform::test_support::noop_services;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::platform::test_support::{
        NoopBackend, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore,
    };
    #[cfg(not(target_arch = "wasm32"))]
    use crate::platform::{PlatformTimer, RuntimeServices};
    use crate::streaming_processor::StreamProcessor;
    use crate::streaming_processor::finish::{
        StreamFinish, StreamFinishDeadline, StreamFinishError, StreamFinishInput,
    };

    struct Stage {
        name: &'static str,
        tail: &'static [u8],
        replacement: Option<&'static [u8]>,
        events: Rc<RefCell<Vec<String>>>,
        calls: Rc<Cell<usize>>,
    }

    impl StreamProcessor for Stage {
        fn process_chunk(&mut self, chunk: &[u8], last: bool) -> io::Result<Vec<u8>> {
            if last {
                self.events.borrow_mut().push(format!("{}:eof", self.name));
                Ok(self.tail.to_vec())
            } else {
                if !chunk.is_empty() {
                    self.events.borrow_mut().push(format!(
                        "{}:chunk:{}",
                        self.name,
                        String::from_utf8_lossy(chunk)
                    ));
                }
                if self.name == "C" {
                    Ok(chunk.to_ascii_uppercase())
                } else {
                    Ok(chunk.to_vec())
                }
            }
        }

        fn requires_async_finish(&self) -> bool {
            self.replacement.is_some()
        }

        fn async_finish<'a>(
            &'a mut self,
            input: StreamFinishInput<'a>,
        ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
            let replacement = self.replacement?;
            Some(Box::pin(async move {
                self.calls.set(self.calls.get() + 1);
                self.events
                    .borrow_mut()
                    .push(format!("{}:async", self.name));
                input.replacement.append(replacement)?;
                Ok(StreamFinish::Replace)
            }))
        }
    }

    fn stage(
        name: &'static str,
        tail: &'static [u8],
        replacement: Option<&'static [u8]>,
        events: &Rc<RefCell<Vec<String>>>,
        calls: &Rc<Cell<usize>>,
    ) -> Box<dyn StreamProcessor> {
        Box::new(Stage {
            name,
            tail,
            replacement,
            events: Rc::clone(events),
            calls: Rc::clone(calls),
        })
    }

    #[test]
    fn async_eof_direct_composite_eof_rejects_before_parser_effects() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"parser-tail", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"tail", Some(b"late"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        chain
            .process_chunk(b"page", true)
            .expect_err("should reject synchronous EOF for an async-capable chain");
        assert!(
            events.borrow().is_empty(),
            "should reject before parser or stage effects"
        );
        assert_eq!(calls.get(), 0, "should not start optional work");
    }

    #[test]
    fn async_eof_retention_without_async_capability_rejects_before_parser_effects() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"parser-tail", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "invalid-retention",
                stage("B", b"tail", None, &events, &calls),
                false,
                true,
                64,
            )],
        );
        chain
            .process_chunk(b"page", false)
            .expect_err("should reject retention without asynchronous EOF capability");
        assert!(
            events.borrow().is_empty(),
            "should validate retention before parser or stage effects"
        );
        assert_eq!(calls.get(), 0, "should not start optional work");
    }

    #[test]
    fn async_eof_output_reaches_downstream_before_its_eof() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"parser-tail", None, &events, &calls),
            vec![
                HtmlProcessorStage::new(
                    "B",
                    stage("B", b"old", Some(b"late"), &events, &calls),
                    true,
                    false,
                    64,
                ),
                HtmlProcessorStage::new(
                    "C",
                    stage("C", b"end", None, &events, &calls),
                    false,
                    false,
                    64,
                ),
            ],
        );
        assert_eq!(
            chain
                .process_chunk(b"page", false)
                .expect("should stream ordinary bytes"),
            b"PAGE"
        );
        assert_eq!(
            chain.begin_finish().expect("should release parser tail"),
            b"PARSER-TAIL"
        );
        assert_eq!(
            calls.get(),
            0,
            "should expose ready prefix before optional I/O"
        );
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), false);
        let mut timer = None;
        let first = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should finish upstream stage")
        .expect("should produce a stage result");
        assert_eq!(first, b"LATE", "should transform async output downstream");
        assert!(
            !events.borrow().iter().any(|event| event == "C:eof"),
            "should not close downstream early"
        );
        let second = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should finish downstream stage")
        .expect("should return its tail");
        assert_eq!(second, b"end");
        for name in ["parser", "B", "C"] {
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|event| **event == format!("{name}:eof"))
                    .count(),
                1,
                "should finish every stage once"
            );
        }
    }

    #[test]
    fn async_eof_buffer_overflow_replays_once_and_skips_io() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"end", Some(b"wrong"), &events, &calls),
                true,
                true,
                4,
            )],
        );
        assert!(
            chain
                .process_chunk(b"page", false)
                .expect("should hold under cap")
                .is_empty()
        );
        assert_eq!(
            chain
                .process_chunk(b"!", false)
                .expect("should replay cap crossing"),
            b"page!"
        );
        assert_eq!(
            chain
                .process_chunk(b"next", false)
                .expect("should continue delivery"),
            b"next"
        );
        assert!(
            chain
                .begin_finish()
                .expect("should finish parser")
                .is_empty()
        );
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), false);
        let mut timer = None;
        let tail = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 4,
        }))
        .expect("should finish bypassed stage")
        .expect("should emit sync tail");
        assert_eq!(tail, b"end");
        assert_eq!(
            calls.get(),
            0,
            "should skip optional I/O after collection overflow"
        );
    }

    #[test]
    fn async_eof_rejects_marker_removal_across_streamed_boundary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"EN", Some(b"wrong"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        assert_eq!(
            chain
                .process_chunk(b"TOK", false)
                .expect("should stream prefix"),
            b"TOK"
        );
        chain.begin_finish().expect("should finish parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), false);
        let mut timer = None;
        let tail = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should retain marker fallback")
        .expect("should return EOF output");
        assert_eq!(
            tail, b"EN",
            "should retain a marker completed across the earlier prefix"
        );
    }

    fn finish_marker_stage(chain: &mut HtmlWithStreamingProcessors, limit: usize) -> Vec<u8> {
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), false);
        let mut timer = None;
        block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit,
        }))
        .expect("should finish the marker-aware stage")
        .expect("should return stage EOF output")
    }

    #[test]
    fn async_eof_rejects_marker_creation_across_streamed_boundary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"ordinary", Some(b"EN"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        assert_eq!(
            chain
                .process_chunk(b"TOK", false)
                .expect("should stream the prefix"),
            b"TOK",
            "should release ordinary output before EOF"
        );
        chain.begin_finish().expect("should finish the parser");
        assert_eq!(
            finish_marker_stage(&mut chain, 64),
            b"ordinary",
            "should reject a candidate that creates a token across emitted output"
        );
        assert_eq!(calls.get(), 1, "should evaluate the candidate once");
    }

    #[test]
    fn async_eof_retained_output_preserves_a_marker_completed_at_eof() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"EN", Some(b"wrong"), &events, &calls),
                true,
                true,
                64,
            )],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        assert!(
            chain
                .process_chunk(b"TOK", false)
                .expect("should retain the prefix")
                .is_empty(),
            "should withhold a declaring stage's output"
        );
        chain.begin_finish().expect("should finish the parser");
        assert_eq!(
            finish_marker_stage(&mut chain, 64),
            b"TOKEN",
            "should retain the complete collected fallback when replacement removes its marker"
        );
        assert_eq!(calls.get(), 1, "should evaluate retained output once");
    }

    #[test]
    fn async_eof_overflow_replays_a_split_marker_and_skips_invalid_replacement() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"!", Some(b"wrong"), &events, &calls),
                true,
                true,
                4,
            )],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        assert!(
            chain
                .process_chunk(b"TOK", false)
                .expect("should retain the prefix")
                .is_empty()
        );
        assert_eq!(
            chain
                .process_chunk(b"EN", false)
                .expect("should replay cap crossing"),
            b"TOKEN",
            "should replay the split marker exactly once"
        );
        chain.begin_finish().expect("should finish the parser");
        assert_eq!(
            finish_marker_stage(&mut chain, 4),
            b"!",
            "should emit synchronous EOF after overflow"
        );
        assert_eq!(
            calls.get(),
            0,
            "should never start the invalid optional replacement after overflow"
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| event.as_str() == "B:eof")
                .count(),
            1,
            "should finish the bypassed stage exactly once"
        );
    }

    #[test]
    fn async_eof_upstream_replacement_updates_downstream_marker_boundary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![
                HtmlProcessorStage::new(
                    "A",
                    stage("A", b"old", Some(b"TOK"), &events, &calls),
                    true,
                    false,
                    64,
                ),
                HtmlProcessorStage::new(
                    "B",
                    stage("B", b"EN", Some(b"wrong"), &events, &calls),
                    true,
                    false,
                    64,
                ),
            ],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        chain.begin_finish().expect("should finish the parser");
        assert_eq!(
            finish_marker_stage(&mut chain, 64),
            b"TOK",
            "should accept upstream ordinary output"
        );
        assert_eq!(
            finish_marker_stage(&mut chain, 64),
            b"EN",
            "should preserve the downstream token completed from accepted upstream output"
        );
        assert_eq!(calls.get(), 2, "should evaluate both stage candidates once");
    }

    struct ResettableStage(usize);
    impl StreamProcessor for ResettableStage {
        fn process_chunk(&mut self, _: &[u8], _: bool) -> io::Result<Vec<u8>> {
            self.0 += 1;
            Ok(vec![
                u8::try_from(self.0).expect("should keep test counts in a byte"),
            ])
        }
        fn reset(&mut self) {
            self.0 = 0;
        }
    }

    #[test]
    fn async_eof_revision_preserves_synchronous_reset_forwarding() {
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            Box::new(ResettableStage(0)),
            vec![HtmlProcessorStage::new(
                "reset",
                Box::new(ResettableStage(0)),
                false,
                false,
                8,
            )],
        );
        assert_eq!(
            chain
                .process_chunk(b"page", false)
                .expect("should process the first document"),
            vec![1],
            "should start with fresh state"
        );
        chain.reset();
        assert_eq!(
            chain
                .process_chunk(b"next", false)
                .expect("should process after reset"),
            vec![1],
            "should reset parser and processor state"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    struct RecordingTimer(Arc<Mutex<Vec<Duration>>>);
    #[cfg(not(target_arch = "wasm32"))]
    impl PlatformTimer for RecordingTimer {
        fn delay(&self, duration: Duration) -> LocalBoxFuture<'static, ()> {
            self.0
                .lock()
                .expect("should lock recorded timer requests")
                .push(duration);
            Box::pin(futures::future::pending())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_eof_timer_uses_remaining_budget_when_first_callback_starts() {
        let requested = Arc::new(Mutex::new(Vec::new()));
        let services = RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .client_info(crate::platform::ClientInfo::default())
            .timer(Arc::new(RecordingTimer(Arc::clone(&requested))))
            .build();
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"ready", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"tail", Some(b"late"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        let deadline = StreamFinishDeadline::new(Duration::from_secs(5), true);
        chain
            .begin_finish()
            .expect("should make parser output ready");
        std::thread::sleep(Duration::from_millis(20));
        let mut timer = None;
        block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should finish a callback after ready-prefix delivery");
        let requested = requested
            .lock()
            .expect("should read recorded timer request");
        assert_eq!(
            requested.len(),
            1,
            "should initialize one cooperative timer at first callback"
        );
        assert!(
            requested[0] <= Duration::from_millis(4980),
            "should subtract time already spent before callback polling"
        );
    }

    struct PendingFinish {
        tail: Vec<u8>,
        dropped: Rc<Cell<bool>>,
        polls: Rc<Cell<usize>>,
        fallback_pointer: Rc<Cell<usize>>,
    }
    struct BorrowProbe<'a> {
        _bytes: &'a [u8],
        dropped: Rc<Cell<bool>>,
    }
    impl Drop for BorrowProbe<'_> {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }
    impl StreamProcessor for PendingFinish {
        fn process_chunk(&mut self, chunk: &[u8], last: bool) -> io::Result<Vec<u8>> {
            if last {
                Ok(std::mem::take(&mut self.tail))
            } else {
                Ok(chunk.to_vec())
            }
        }
        fn requires_async_finish(&self) -> bool {
            true
        }
        fn async_finish<'a>(
            &'a mut self,
            input: StreamFinishInput<'a>,
        ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
            Some(Box::pin(async move {
                let _probe = BorrowProbe {
                    _bytes: input.output,
                    dropped: Rc::clone(&self.dropped),
                };
                self.fallback_pointer.set(input.output.as_ptr() as usize);
                self.polls.set(self.polls.get() + 1);
                input.replacement.append(b"partial")?;
                futures::future::pending().await
            }))
        }
    }

    fn timer_after_callback_poll() -> Option<Fuse<LocalBoxFuture<'static, ()>>> {
        let mut polled = false;
        Some(
            Box::pin(poll_fn(move |context| {
                if polled {
                    Poll::Ready(())
                } else {
                    polled = true;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            }))
            .boxed_local()
            .fuse(),
        )
    }

    #[test]
    fn async_eof_timeout_drops_borrow_and_preserves_fallback_allocation() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let dropped = Rc::new(Cell::new(false));
        let pointer = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "pending",
                Box::new(PendingFinish {
                    tail: b"held".to_vec(),
                    dropped: Rc::clone(&dropped),
                    polls: Rc::clone(&calls),
                    fallback_pointer: Rc::clone(&pointer),
                }),
                true,
                false,
                64,
            )],
        );
        chain.begin_finish().expect("should finish parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), true);
        let mut timer = timer_after_callback_poll();
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should retain output on timeout")
        .expect("should return held output");
        assert_eq!(output, b"held", "should discard partial candidate");
        assert_eq!(
            output.as_ptr() as usize,
            pointer.get(),
            "should keep the original allocation outside the dropped future"
        );
        assert!(
            dropped.get(),
            "should drop callback borrows before selecting fallback"
        );
        assert_eq!(calls.get(), 1, "should poll the pending callback");
    }

    #[test]
    fn async_eof_timer_expiry_skips_later_io_but_completes_downstream_eof() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![
                HtmlProcessorStage::new(
                    "pending",
                    Box::new(PendingFinish {
                        tail: b"held".to_vec(),
                        dropped: Rc::new(Cell::new(false)),
                        polls: Rc::clone(&calls),
                        fallback_pointer: Rc::new(Cell::new(0)),
                    }),
                    true,
                    false,
                    64,
                ),
                HtmlProcessorStage::new(
                    "C",
                    stage("C", b"ctail", Some(b"wrong"), &events, &calls),
                    true,
                    false,
                    64,
                ),
            ],
        );
        chain.begin_finish().expect("should finish parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(1), true);
        let mut timer = timer_after_callback_poll();
        let first = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should finish timed-out upstream")
        .expect("should return transformed fallback");
        assert_eq!(
            first, b"HELD",
            "should send fallback downstream as normal input"
        );
        let second = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should complete downstream EOF")
        .expect("should return its synchronous tail");
        assert_eq!(
            second, b"ctail",
            "should skip later optional I/O after timer expiry"
        );
        assert_eq!(calls.get(), 1, "should not poll a later optional callback");
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| event.as_str() == "C:eof")
                .count(),
            1,
            "should complete downstream synchronous EOF once"
        );
    }

    #[derive(Clone, Copy)]
    enum LeafOutcome {
        Error,
        PoisonedReplacement,
        LateReplacement,
        #[cfg(not(target_arch = "wasm32"))]
        ElapsedReplacement,
        EchoReplacement,
        EmptyReplacement,
        Replacement,
    }

    #[derive(Clone, Default)]
    struct LeafObservation {
        calls: Rc<Cell<usize>>,
        eof_calls: Rc<Cell<usize>>,
        fallback_pointer: Rc<Cell<usize>>,
        dropped: Rc<Cell<bool>>,
    }

    struct LeafProbe {
        tail: Vec<u8>,
        outcome: LeafOutcome,
        observation: LeafObservation,
    }

    impl StreamProcessor for LeafProbe {
        fn process_chunk(&mut self, chunk: &[u8], last: bool) -> io::Result<Vec<u8>> {
            if last {
                self.observation
                    .eof_calls
                    .set(self.observation.eof_calls.get() + 1);
                Ok(std::mem::take(&mut self.tail))
            } else {
                Ok(chunk.to_vec())
            }
        }

        fn requires_async_finish(&self) -> bool {
            true
        }

        fn async_finish<'a>(
            &'a mut self,
            input: StreamFinishInput<'a>,
        ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
            Some(Box::pin(async move {
                let _probe = BorrowProbe {
                    _bytes: input.output,
                    dropped: Rc::clone(&self.observation.dropped),
                };
                self.observation.calls.set(self.observation.calls.get() + 1);
                self.observation
                    .fallback_pointer
                    .set(input.output.as_ptr() as usize);
                match self.outcome {
                    LeafOutcome::Error => {
                        input.replacement.append(b"partial")?;
                        Err(Report::new(StreamFinishError::Unavailable))
                    }
                    LeafOutcome::PoisonedReplacement => {
                        input.replacement.append(b"partial")?;
                        let _ = input
                            .replacement
                            .append(&[b'x'; 65])
                            .expect_err("should poison output beyond the 64-byte cap");
                        Ok(StreamFinish::Replace)
                    }
                    LeafOutcome::LateReplacement => {
                        input.replacement.append(b"candidate")?;
                        input.deadline.mark_expired();
                        Ok(StreamFinish::Replace)
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    LeafOutcome::ElapsedReplacement => {
                        std::thread::sleep(input.deadline.remaining() + Duration::from_millis(5));
                        input.replacement.append(b"candidate")?;
                        Ok(StreamFinish::Replace)
                    }
                    LeafOutcome::EchoReplacement => {
                        input.replacement.append(input.output)?;
                        Ok(StreamFinish::Replace)
                    }
                    LeafOutcome::EmptyReplacement => Ok(StreamFinish::Replace),
                    LeafOutcome::Replacement => {
                        input.replacement.append(b"candidate")?;
                        Ok(StreamFinish::Replace)
                    }
                }
            }))
        }
    }

    fn complete_leaf_probe(
        outcome: LeafOutcome,
        mut timer: Option<Fuse<LocalBoxFuture<'static, ()>>>,
    ) -> (Vec<u8>, LeafObservation, usize) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let observation = LeafObservation::default();
        let fallback = b"fallback".to_vec();
        let fallback_pointer = fallback.as_ptr() as usize;
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "probe",
                Box::new(LeafProbe {
                    tail: fallback,
                    outcome,
                    observation: observation.clone(),
                }),
                true,
                false,
                64,
            )],
        );
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(30), timer.is_some());
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should complete synchronous EOF despite optional failure")
        .expect("should return the selected stage output");
        assert_eq!(
            observation.eof_calls.get(),
            1,
            "should complete the processor EOF exactly once"
        );
        (output, observation, fallback_pointer)
    }

    #[test]
    fn async_eof_callback_error_discards_partial_candidate_without_copying_fallback() {
        let (output, observation, fallback_pointer) = complete_leaf_probe(LeafOutcome::Error, None);
        assert_eq!(output, b"fallback", "should discard the partial candidate");
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should retain the driver-owned fallback allocation"
        );
        assert_eq!(
            observation.fallback_pointer.get(),
            fallback_pointer,
            "should lend the original fallback to optional work"
        );
        assert!(
            observation.dropped.get(),
            "should drop callback borrows before returning fallback"
        );
        assert_eq!(observation.calls.get(), 1, "should run optional work once");
    }

    #[test]
    fn async_eof_poisoned_candidate_cannot_commit_even_when_callback_returns_replace() {
        let (output, observation, fallback_pointer) =
            complete_leaf_probe(LeafOutcome::PoisonedReplacement, None);
        assert_eq!(
            output, b"fallback",
            "should reject partial output after an ignored append error"
        );
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should retain the original fallback allocation after poisoning"
        );
        assert!(observation.dropped.get(), "should release callback borrows");
        assert_eq!(observation.calls.get(), 1, "should run optional work once");
    }

    #[test]
    fn async_eof_timer_wins_when_timer_and_replacement_are_both_ready() {
        let timer = Some(futures::future::ready(()).boxed_local().fuse());
        let (output, observation, fallback_pointer) =
            complete_leaf_probe(LeafOutcome::Replacement, timer);
        assert_eq!(
            output, b"fallback",
            "should prefer expiry over ready output"
        );
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should preserve fallback allocation when the timer wins"
        );
        assert_eq!(
            observation.calls.get(),
            0,
            "should not poll callback work when the timer is already ready"
        );
    }

    #[test]
    fn async_eof_replacement_completed_after_expiry_retains_original_fallback() {
        let (output, observation, fallback_pointer) =
            complete_leaf_probe(LeafOutcome::LateReplacement, None);
        assert_eq!(
            output, b"fallback",
            "should discard a late successful result"
        );
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should preserve the original fallback after late completion"
        );
        assert!(
            observation.dropped.get(),
            "should drop the completed callback before selecting fallback"
        );
        assert_eq!(
            observation.calls.get(),
            1,
            "should exercise expiry during callback execution"
        );
    }

    #[test]
    fn async_eof_empty_replace_intentionally_removes_nonempty_fallback() {
        let (output, observation, _) = complete_leaf_probe(LeafOutcome::EmptyReplacement, None);
        assert!(
            output.is_empty(),
            "should accept an empty replacement instead of retaining fallback"
        );
        assert_eq!(observation.calls.get(), 1, "should run optional work once");
        assert!(observation.dropped.get(), "should release callback borrows");
    }

    #[test]
    fn async_eof_failed_later_stage_preserves_accepted_upstream_replacement() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let upstream_calls = Rc::new(Cell::new(0));
        let observation = LeafObservation::default();
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &upstream_calls),
            vec![
                HtmlProcessorStage::new(
                    "upstream",
                    stage("A", b"old", Some(b"accepted"), &events, &upstream_calls),
                    true,
                    false,
                    64,
                ),
                HtmlProcessorStage::new(
                    "later",
                    Box::new(LeafProbe {
                        tail: b"tail".to_vec(),
                        outcome: LeafOutcome::Error,
                        observation: observation.clone(),
                    }),
                    true,
                    true,
                    64,
                ),
            ],
        );
        assert!(
            chain
                .process_chunk(b"page", false)
                .expect("should process ordinary input")
                .is_empty(),
            "should retain only the explicitly buffered downstream stage"
        );
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(30), false);
        let mut timer = None;
        let first = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should commit the upstream replacement")
        .expect("should return the downstream stage's ready output");
        assert!(
            first.is_empty(),
            "should deliver accepted upstream bytes into downstream retention"
        );
        assert_eq!(upstream_calls.get(), 1, "should finish upstream I/O once");
        assert_eq!(
            observation.eof_calls.get(),
            0,
            "should preserve downstream EOF ordering"
        );
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should retain downstream fallback on optional failure")
        .expect("should release downstream retained output");
        assert_eq!(
            output, b"pageacceptedtail",
            "should retain accepted upstream output while discarding the later candidate"
        );
        assert_eq!(
            output.as_ptr() as usize,
            observation.fallback_pointer.get(),
            "should move the retained fallback without a rollback copy"
        );
        assert_eq!(observation.calls.get(), 1, "should run later I/O once");
        assert_eq!(
            observation.eof_calls.get(),
            1,
            "should finish later EOF once"
        );
        assert!(observation.dropped.get(), "should release callback borrows");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_eof_timerless_callback_elapsed_past_deadline_preserves_fallback() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let observation = LeafObservation::default();
        let fallback = b"fallback".to_vec();
        let fallback_pointer = fallback.as_ptr() as usize;
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "elapsed",
                Box::new(LeafProbe {
                    tail: fallback,
                    outcome: LeafOutcome::ElapsedReplacement,
                    observation: observation.clone(),
                }),
                true,
                false,
                64,
            )],
        );
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_millis(100), false);
        let mut timer = None;
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should finish timerless work that completes late")
        .expect("should retain synchronous output");
        assert_eq!(
            observation.calls.get(),
            1,
            "should exercise callback execution rather than pre-poll expiry"
        );
        assert!(
            deadline.elapsed() >= Duration::from_millis(100),
            "should cross the actual wall-clock deadline"
        );
        assert_eq!(output, b"fallback", "should reject the late replacement");
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should preserve the original fallback allocation"
        );
        assert!(observation.dropped.get(), "should release callback borrows");
    }

    #[test]
    fn async_eof_oversized_streaming_fallback_skips_io_without_truncation_or_copy() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let observation = LeafObservation::default();
        let fallback = vec![b'x'; 65];
        let fallback_pointer = fallback.as_ptr() as usize;
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "oversized",
                Box::new(LeafProbe {
                    tail: fallback,
                    outcome: LeafOutcome::Replacement,
                    observation: observation.clone(),
                }),
                true,
                false,
                64,
            )],
        );
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(30), false);
        let mut timer = None;
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should deliver an oversized synchronous tail")
        .expect("should return the complete tail");
        assert_eq!(
            output.as_slice(),
            &[b'x'; 65],
            "should not truncate fallback"
        );
        assert_eq!(
            output.as_ptr() as usize,
            fallback_pointer,
            "should deliver the original allocation"
        );
        assert_eq!(observation.calls.get(), 0, "should skip optional I/O");
        assert_eq!(
            observation.eof_calls.get(),
            1,
            "should finish synchronous EOF once"
        );
    }

    #[test]
    fn async_eof_small_chunks_keep_a_bounded_marker_suffix() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"EN", Some(b"wrong"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        chain.set_finish_marker(Some(b"TOKEN"));
        for chunk in [b"xxxx".as_slice(), b"TO", b"K"] {
            assert_eq!(
                chain
                    .process_chunk(chunk, false)
                    .expect("should stream each small chunk"),
                chunk,
                "should preserve ordinary output"
            );
            assert!(
                chain.stages[0].previous_suffix.len() <= 4,
                "should bound marker history after every chunk"
            );
        }
        assert_eq!(
            chain.stages[0].previous_suffix, b"xTOK",
            "should retain the trailing bytes across small chunks"
        );
        chain.begin_finish().expect("should finish the parser");
        assert_eq!(
            finish_marker_stage(&mut chain, 64),
            b"EN",
            "should reject removal of a marker completed after trimmed history"
        );
        assert_eq!(calls.get(), 1, "should evaluate the candidate once");
    }

    #[test]
    fn async_eof_single_byte_marker_rejects_creation_and_removal() {
        for (tail, replacement) in [
            (b"|".as_slice(), b"wrong".as_slice()),
            (b"tail".as_slice(), b"|".as_slice()),
        ] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let calls = Rc::new(Cell::new(0));
            let mut chain = HtmlWithStreamingProcessors::from_stages(
                stage("parser", b"", None, &events, &calls),
                vec![HtmlProcessorStage::new(
                    "marker",
                    stage("B", tail, Some(replacement), &events, &calls),
                    true,
                    false,
                    64,
                )],
            );
            chain.set_finish_marker(Some(b"|"));
            assert_eq!(
                chain
                    .process_chunk(b"page|", false)
                    .expect("should release ordinary output"),
                b"page|",
                "should preserve the already emitted marker"
            );
            chain.begin_finish().expect("should finish the parser");
            let services = noop_services();
            let deadline = StreamFinishDeadline::new(Duration::from_secs(30), false);
            let mut timer = None;
            let output = block_on(chain.finish_next(HtmlFinishContext {
                response_status: StatusCode::OK,
                services: &services,
                deadline: &deadline,
                timer: &mut timer,
                limit: 64,
            }))
            .expect("should retain output after invalid marker replacement")
            .expect("should return the fallback tail");
            assert_eq!(
                output, tail,
                "should reject changing single-byte marker count"
            );
            assert_eq!(calls.get(), 1, "should evaluate optional output once");
        }
    }

    #[test]
    fn async_eof_duplicate_parser_finish_rejects_before_repeating_effects() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"parser-tail", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "B",
                stage("B", b"tail", Some(b"candidate"), &events, &calls),
                true,
                false,
                64,
            )],
        );
        assert_eq!(
            chain.begin_finish().expect("should finish the parser once"),
            b"parser-tail",
            "should release its tail once"
        );
        let original_events = events.borrow().clone();
        chain
            .begin_finish()
            .expect_err("should reject a repeated parser EOF");
        assert_eq!(
            *events.borrow(),
            original_events,
            "should not repeat parser or downstream chunk effects"
        );
        assert_eq!(
            calls.get(),
            0,
            "should not start optional work at parser EOF"
        );
    }

    #[test]
    fn async_eof_exact_retention_and_replacement_cap_succeed_at_eof() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let observation = LeafObservation::default();
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "exact",
                Box::new(LeafProbe {
                    tail: b"tail".to_vec(),
                    outcome: LeafOutcome::EchoReplacement,
                    observation: observation.clone(),
                }),
                true,
                true,
                12,
            )],
        );
        for chunk in [b"page", b"more"] {
            assert!(
                chain
                    .process_chunk(chunk, false)
                    .expect("should retain ordinary output")
                    .is_empty(),
                "should withhold output until EOF"
            );
        }
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_secs(30), false);
        let mut timer = None;
        let output = block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 12,
        }))
        .expect("should accept exact-cap EOF completion")
        .expect("should return retained output through its replacement");
        assert_eq!(
            output, b"pagemoretail",
            "should include the whole retained output"
        );
        assert_eq!(
            observation.calls.get(),
            1,
            "should not bypass exact-cap I/O"
        );
        assert_ne!(
            output.as_ptr() as usize,
            observation.fallback_pointer.get(),
            "should commit the exact-cap replacement instead of retaining fallback"
        );
        assert_eq!(
            observation.eof_calls.get(),
            1,
            "should finish synchronous EOF once"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    struct SlowConstructor(Rc<Cell<usize>>);
    #[cfg(not(target_arch = "wasm32"))]
    impl StreamProcessor for SlowConstructor {
        fn process_chunk(&mut self, chunk: &[u8], _: bool) -> io::Result<Vec<u8>> {
            Ok(chunk.to_vec())
        }
        fn requires_async_finish(&self) -> bool {
            true
        }
        fn async_finish<'a>(
            &'a mut self,
            _: StreamFinishInput<'a>,
        ) -> Option<LocalBoxFuture<'a, Result<StreamFinish, Report<StreamFinishError>>>> {
            std::thread::sleep(Duration::from_millis(5));
            Some(Box::pin(async move {
                self.0.set(self.0.get() + 1);
                Ok(StreamFinish::Unchanged)
            }))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_eof_does_not_poll_work_after_constructor_consumes_budget() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(Cell::new(0));
        let mut chain = HtmlWithStreamingProcessors::from_stages(
            stage("parser", b"", None, &events, &calls),
            vec![HtmlProcessorStage::new(
                "slow",
                Box::new(SlowConstructor(Rc::clone(&calls))),
                true,
                false,
                64,
            )],
        );
        chain.begin_finish().expect("should finish the parser");
        let services = noop_services();
        let deadline = StreamFinishDeadline::new(Duration::from_millis(1), false);
        let mut timer = None;
        block_on(chain.finish_next(HtmlFinishContext {
            response_status: StatusCode::OK,
            services: &services,
            deadline: &deadline,
            timer: &mut timer,
            limit: 64,
        }))
        .expect("should retain fallback after constructor consumes budget");
        assert_eq!(
            calls.get(),
            0,
            "should check deadline again before polling optional work"
        );
    }
}
