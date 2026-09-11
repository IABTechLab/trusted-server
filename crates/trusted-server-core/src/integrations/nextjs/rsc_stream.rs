use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use crate::integrations::{
    IntegrationDocumentState, IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory,
};
use crate::streaming_processor::StreamProcessor;

use super::rsc::{
    DEFAULT_MAX_COMBINED_PAYLOAD_BYTES, PendingTChunk, TChunkStep, next_tchunk,
    rewrite_rsc_scripts_combined_with_limit,
};
use super::shared::RscUrlRewriter;
use super::{NEXTJS_INTEGRATION_ID, NextJsIntegrationConfig};

pub(super) const RSC_PAYLOAD_PLACEHOLDER_PREFIX: &str = "__ts_rsc_";
pub(super) const RSC_PAYLOAD_PLACEHOLDER_SUFFIX: &str = "__";
pub(super) const MAX_UNRESOLVED_RSC_PAYLOADS: usize = 256;

#[derive(Debug, Default)]
pub(super) enum FragmentState {
    #[default]
    Idle,
    Buffering(String),
    BypassUntilLast,
}

#[derive(Debug, Clone)]
pub(super) struct CapturedPayload {
    pub(super) placeholder: String,
    pub(super) original: String,
}

#[derive(Debug)]
pub(super) struct NextJsDocumentState {
    pub(super) namespace: String,
    pub(super) next_data: FragmentState,
    pub(super) rsc_script: FragmentState,
    pub(super) rsc_probe: String,
    /// Tail of script text already released for the current text node, kept so a
    /// `self.`/`window.` receiver that streamed before its `__next_f` identifier
    /// was recognized can still be verified.
    pub(super) rsc_receiver_context: String,
    /// The active claim begins at a bare `__next_f` whose receiver was verified
    /// from [`NextJsDocumentState::rsc_receiver_context`].
    pub(super) rsc_receiver_trimmed: bool,
    pub(super) captured_payloads: VecDeque<CapturedPayload>,
    pub(super) captured_payload_bytes: usize,
    pub(super) next_placeholder_index: usize,
    pub(super) bypass_rsc: bool,
}

impl Default for NextJsDocumentState {
    fn default() -> Self {
        Self {
            namespace: uuid::Uuid::new_v4().simple().to_string(),
            next_data: FragmentState::Idle,
            rsc_script: FragmentState::Idle,
            rsc_probe: String::new(),
            rsc_receiver_context: String::new(),
            rsc_receiver_trimmed: false,
            captured_payloads: VecDeque::new(),
            captured_payload_bytes: 0,
            next_placeholder_index: 0,
            bypass_rsc: false,
        }
    }
}

pub(super) fn document_state(state: &IntegrationDocumentState) -> Arc<Mutex<NextJsDocumentState>> {
    state.get_or_insert_with(NEXTJS_INTEGRATION_ID, || {
        Mutex::new(NextJsDocumentState::default())
    })
}

pub(super) fn rsc_payload_placeholder(namespace: &str, index: usize) -> String {
    format!("{RSC_PAYLOAD_PLACEHOLDER_PREFIX}{namespace}_{index}{RSC_PAYLOAD_PLACEHOLDER_SUFFIX}")
}

pub(super) enum FragmentCapture<'a> {
    CompleteBorrowed(&'a str),
    CompleteOwned(String),
    Suppress,
    Restore(String),
    PassThrough,
}

pub(super) fn capture_fragment<'a>(
    state: &mut FragmentState,
    content: &'a str,
    is_last: bool,
    limit: usize,
) -> FragmentCapture<'a> {
    match state {
        FragmentState::Idle if is_last => {
            if content.len() > limit {
                FragmentCapture::PassThrough
            } else {
                FragmentCapture::CompleteBorrowed(content)
            }
        }
        FragmentState::Idle => {
            if content.len() > limit {
                *state = FragmentState::BypassUntilLast;
                FragmentCapture::PassThrough
            } else {
                *state = FragmentState::Buffering(content.to_owned());
                FragmentCapture::Suppress
            }
        }
        FragmentState::Buffering(buffer) => {
            let exceeds_limit = buffer
                .len()
                .checked_add(content.len())
                .is_none_or(|combined| combined > limit);
            if exceeds_limit {
                let mut restored = std::mem::take(buffer);
                restored.push_str(content);
                *state = if is_last {
                    FragmentState::Idle
                } else {
                    FragmentState::BypassUntilLast
                };
                FragmentCapture::Restore(restored)
            } else {
                buffer.push_str(content);
                if is_last {
                    let complete = std::mem::take(buffer);
                    *state = FragmentState::Idle;
                    FragmentCapture::CompleteOwned(complete)
                } else {
                    FragmentCapture::Suppress
                }
            }
        }
        FragmentState::BypassUntilLast => {
            if is_last {
                *state = FragmentState::Idle;
            }
            FragmentCapture::PassThrough
        }
    }
}

pub(super) struct NextJsRscStreamProcessorFactory {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsRscStreamProcessorFactory {
    pub(super) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationHtmlStreamProcessorFactory for NextJsRscStreamProcessorFactory {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
        let limit = if self.config.max_combined_payload_bytes == 0 {
            DEFAULT_MAX_COMBINED_PAYLOAD_BYTES
        } else {
            self.config.max_combined_payload_bytes
        };
        Box::new(NextJsRscStreamProcessor::new(
            document_state(&context.document_state),
            context.origin_host,
            context.request_host,
            context.request_scheme,
            limit,
        ))
    }
}

pub(super) struct NextJsRscStreamProcessor {
    state: Arc<Mutex<NextJsDocumentState>>,
    origin_host: String,
    request_host: String,
    request_scheme: String,
    limit: usize,
    pending_candidate: Vec<u8>,
    held_output: Vec<u8>,
    group: Vec<CapturedPayload>,
    classifier: RscGroupClassifier,
    rewriter: RscUrlRewriter,
}

impl NextJsRscStreamProcessor {
    fn new(
        state: Arc<Mutex<NextJsDocumentState>>,
        origin_host: String,
        request_host: String,
        request_scheme: String,
        limit: usize,
    ) -> Self {
        Self {
            state,
            origin_host,
            request_host,
            request_scheme,
            limit,
            pending_candidate: Vec::new(),
            held_output: Vec::new(),
            group: Vec::new(),
            classifier: RscGroupClassifier::new(limit),
            rewriter: RscUrlRewriter::new(),
        }
    }

    fn namespace_prefix(&self) -> Vec<u8> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        format!("{RSC_PAYLOAD_PLACEHOLDER_PREFIX}{}_", state.namespace).into_bytes()
    }

    fn next_captured(&self) -> Option<CapturedPayload> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .captured_payloads
            .front()
            .cloned()
    }

    fn pop_captured(&self, placeholder: &str) -> io::Result<CapturedPayload> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(payload) = state.captured_payloads.pop_front() else {
            return Err(io::Error::other(
                "Next.js RSC placeholder has no captured payload",
            ));
        };
        if payload.placeholder != placeholder {
            state.captured_payloads.push_front(payload);
            return Err(io::Error::other(
                "Next.js RSC placeholders are out of document order",
            ));
        }
        Ok(payload)
    }

    fn append_held(&mut self, bytes: &[u8]) -> bool {
        if self
            .held_output
            .len()
            .checked_add(bytes.len())
            .is_none_or(|combined| combined > self.limit)
        {
            false
        } else {
            self.held_output.extend_from_slice(bytes);
            true
        }
    }

    fn release_group(&mut self, rewritten: Option<&[String]>) -> io::Result<Vec<u8>> {
        let released_payload_bytes = self
            .group
            .iter()
            .map(|payload| payload.original.len())
            .sum::<usize>();
        let replacements: Vec<&str> = match &rewritten {
            Some(rewritten) => rewritten.iter().map(String::as_str).collect(),
            None => self
                .group
                .iter()
                .map(|payload| payload.original.as_str())
                .collect(),
        };
        let held_output = std::mem::take(&mut self.held_output);
        let output = substitute_payloads(
            &held_output,
            &self.group,
            &replacements,
            &self.namespace_prefix(),
        )?;
        self.group.clear();
        self.classifier.reset();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.captured_payload_bytes = state
            .captured_payload_bytes
            .saturating_sub(released_payload_bytes);
        Ok(output)
    }

    fn resolve_group(&mut self) -> io::Result<Option<Vec<u8>>> {
        // Classification is incremental, but each completed chunk still costs a
        // segment inspection and a boundary check, so bound the segment count;
        // the hydration-safe fallback restores originals.
        if self.group.len() > MAX_UNRESOLVED_RSC_PAYLOADS {
            log::warn!(
                "Next.js RSC fallback: segment limit, {} payloads",
                self.group.len()
            );
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .bypass_rsc = true;
            return self.release_group(None).map(Some);
        }
        let payload = self
            .group
            .last()
            .expect("should resolve a group only after a payload joined it");
        let status = self.classifier.push(&payload.original);
        self.resolve_status(status)
    }

    /// Act on a group status: release the group, or hold for more payloads.
    fn resolve_status(&mut self, status: RscGroupStatus) -> io::Result<Option<Vec<u8>>> {
        match status {
            RscGroupStatus::NeedMore => Ok(None),
            RscGroupStatus::CompleteRewritable => {
                let payloads: Vec<&str> = self
                    .group
                    .iter()
                    .map(|payload| payload.original.as_str())
                    .collect();
                let rewritten = rewrite_rsc_scripts_combined_with_limit(
                    &payloads,
                    &self.rewriter,
                    &self.origin_host,
                    &self.request_host,
                    &self.request_scheme,
                    self.limit,
                );
                if rewritten.len() != self.group.len() {
                    log::warn!(
                        "Next.js RSC fallback: rewrite count mismatch, {} payloads",
                        self.group.len()
                    );
                    return self.release_group(None).map(Some);
                }
                log::debug!("Next.js RSC group completes: {} payloads", self.group.len());
                self.release_group(Some(&rewritten)).map(Some)
            }
            RscGroupStatus::CompleteUnrewritable => {
                log::warn!(
                    "Next.js RSC fallback: split header, {} payloads",
                    self.group.len()
                );
                self.release_group(None).map(Some)
            }
            RscGroupStatus::Invalid => {
                log::warn!(
                    "Next.js RSC fallback: invalid group, {} payloads",
                    self.group.len()
                );
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .bypass_rsc = true;
                self.release_group(None).map(Some)
            }
        }
    }

    /// Restore every captured payload and hand back the bytes unchanged.
    ///
    /// Draining the whole queue is safe because capture and placeholder emission
    /// are atomic: `NextJsRscPlaceholderRewriter::rewrite_complete` pushes a
    /// payload and returns the script carrying its placeholder in the same call,
    /// so a queued payload's placeholder is always already in the held output or
    /// in `current`. A queued payload whose placeholder had not yet been emitted
    /// would fail substitution rather than degrade to unchanged bytes.
    fn release_bypass(&mut self, current: &[u8]) -> io::Result<Vec<u8>> {
        if !self.group.is_empty() || self.next_captured().is_some() {
            log::warn!(
                "Next.js RSC fallback: capture or output limit, {} held payloads",
                self.group.len()
            );
        }
        let mut output = self.release_group(None)?;
        output.extend_from_slice(&self.pending_candidate);
        self.pending_candidate.clear();
        let captured = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.bypass_rsc = true;
            state.captured_payload_bytes = 0;
            state.captured_payloads.drain(..).collect::<Vec<_>>()
        };
        let replacements: Vec<&str> = captured
            .iter()
            .map(|payload| payload.original.as_str())
            .collect();
        output.extend(substitute_payloads(
            current,
            &captured,
            &replacements,
            &self.namespace_prefix(),
        )?);
        Ok(output)
    }
}

impl StreamProcessor for NextJsRscStreamProcessor {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> io::Result<Vec<u8>> {
        let mut current = std::mem::take(&mut self.pending_candidate);
        current.extend_from_slice(chunk);

        let bypass = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bypass_rsc;
        if bypass {
            return self.release_bypass(&current);
        }

        let namespace_prefix = self.namespace_prefix();
        let mut output = Vec::new();
        let mut cursor = 0;
        loop {
            let Some(expected) = self.next_captured() else {
                let remainder = &current[cursor..];
                if self.group.is_empty() {
                    output.extend_from_slice(remainder);
                } else if !self.append_held(remainder) {
                    output.extend(self.release_bypass(remainder)?);
                }
                break;
            };

            let remainder = &current[cursor..];
            let Some(relative_start) = find_bytes(remainder, &namespace_prefix) else {
                let retained = longest_suffix_prefix(remainder, expected.placeholder.as_bytes());
                let ready_end = remainder.len() - retained;
                let ready = &remainder[..ready_end];
                if self.group.is_empty() {
                    output.extend_from_slice(ready);
                } else if !self.append_held(ready) {
                    output.extend(self.release_bypass(remainder)?);
                    break;
                }
                self.pending_candidate
                    .extend_from_slice(&remainder[ready_end..]);
                break;
            };

            let placeholder_start = cursor + relative_start;
            let before = &current[cursor..placeholder_start];
            if self.group.is_empty() {
                output.extend_from_slice(before);
            } else if !self.append_held(before) {
                output.extend(self.release_bypass(&current[cursor..])?);
                break;
            }

            let placeholder = expected.placeholder.as_bytes();
            let available = &current[placeholder_start..];
            if available.len() < placeholder.len() && placeholder.starts_with(available) {
                self.pending_candidate.extend_from_slice(available);
                break;
            }
            if !available.starts_with(placeholder) {
                return Err(io::Error::other(
                    "Next.js RSC output contains an unknown generated placeholder",
                ));
            }
            if !self.append_held(placeholder) {
                output.extend(self.release_bypass(&current[placeholder_start..])?);
                break;
            }
            let captured = self.pop_captured(&expected.placeholder)?;
            self.group.push(captured);
            cursor = placeholder_start + placeholder.len();

            if let Some(released) = self.resolve_group()? {
                output.extend(released);
            }
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .bypass_rsc
            {
                output.extend(self.release_bypass(&current[cursor..])?);
                break;
            }
        }

        if is_last {
            if !self.pending_candidate.is_empty() {
                if self.group.is_empty() {
                    output.append(&mut self.pending_candidate);
                } else {
                    let pending = std::mem::take(&mut self.pending_candidate);
                    if !self.append_held(&pending) {
                        output.extend(self.release_bypass(&pending)?);
                    }
                }
            }
            if !self.group.is_empty() {
                // No further payload can arrive, so reclassify with nothing
                // held back before giving up on the group.
                let status = self.classifier.finalize();
                if matches!(status, RscGroupStatus::CompleteRewritable) {
                    if let Some(released) = self.resolve_status(status)? {
                        output.extend(released);
                    }
                } else {
                    log::warn!(
                        "Next.js RSC fallback: incomplete group at EOF, {} payloads",
                        self.group.len()
                    );
                    output.extend(self.release_group(None)?);
                }
            }
            if self.next_captured().is_some() {
                return Err(io::Error::other(
                    "Next.js RSC captured payload was not present in parser output",
                ));
            }
        }

        Ok(output)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn longest_suffix_prefix(bytes: &[u8], pattern: &[u8]) -> usize {
    let maximum = bytes.len().min(pattern.len().saturating_sub(1));
    (1..=maximum)
        .rev()
        .find(|length| bytes.ends_with(&pattern[..*length]))
        .unwrap_or(0)
}

fn substitute_payloads(
    input: &[u8],
    payloads: &[CapturedPayload],
    replacements: &[&str],
    namespace_prefix: &[u8],
) -> io::Result<Vec<u8>> {
    if payloads.len() != replacements.len() {
        return Err(io::Error::other(
            "Next.js RSC substitution received mismatched payloads",
        ));
    }
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    for (payload, replacement) in payloads.iter().zip(replacements) {
        let placeholder = payload.placeholder.as_bytes();
        let Some(relative_position) = find_bytes(&input[cursor..], placeholder) else {
            return Err(io::Error::other(
                "Next.js RSC captured placeholder is missing from held output",
            ));
        };
        let position = cursor + relative_position;
        output.extend_from_slice(&input[cursor..position]);
        output.extend_from_slice(replacement.as_bytes());
        cursor = position + placeholder.len();
    }
    output.extend_from_slice(&input[cursor..]);
    if find_bytes(&output, namespace_prefix).is_some() {
        return Err(io::Error::other(
            "Next.js RSC generated placeholder remained after substitution",
        ));
    }
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RscGroupStatus {
    CompleteRewritable,
    CompleteUnrewritable,
    NeedMore,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderSuffixStatus {
    Complete,
    NeedMore,
    Invalid,
}

/// Incremental classifier for one logical RSC group.
///
/// Payloads are appended once and scanned once: a chunk whose content has not
/// fully arrived is resumed where consumption stopped, and the trailing
/// non-chunk segment is inspected from where inspection stopped. Re-deriving
/// the whole group on every payload made a large but permitted response cost
/// CPU quadratic in its segment count.
pub(super) struct RscGroupClassifier {
    max_combined_payload_bytes: usize,
    combined: String,
    /// Offsets in [`Self::combined`] where one payload meets the next.
    boundaries: Vec<usize>,
    /// A completed chunk's header straddled a payload boundary, so the group
    /// cannot be rewritten even once it completes.
    header_split: bool,
    /// Where the next chunk-header search begins.
    scan_from: usize,
    /// A chunk whose declared content is still incomplete.
    pending: Option<PendingTChunk>,
    /// Start of the trailing non-chunk segment.
    segment_start: usize,
    inspector: SegmentInspector,
    invalid: bool,
}

impl RscGroupClassifier {
    pub(super) fn new(max_combined_payload_bytes: usize) -> Self {
        Self {
            max_combined_payload_bytes,
            combined: String::new(),
            boundaries: Vec::new(),
            header_split: false,
            scan_from: 0,
            pending: None,
            segment_start: 0,
            inspector: SegmentInspector::default(),
            invalid: false,
        }
    }

    /// Append the next payload of the group and reclassify.
    pub(super) fn push(&mut self, payload: &str) -> RscGroupStatus {
        if self.invalid {
            return RscGroupStatus::Invalid;
        }
        let exceeds_limit = self
            .combined
            .len()
            .checked_add(payload.len())
            .is_none_or(|total| total > self.max_combined_payload_bytes);
        if exceeds_limit {
            self.invalid = true;
            return RscGroupStatus::Invalid;
        }
        if !self.combined.is_empty() {
            self.boundaries.push(self.combined.len());
        }
        self.combined.push_str(payload);
        self.advance(false)
    }

    /// Reclassify knowing no further payload can arrive, so no trailing escape
    /// needs to be held back.
    pub(super) fn finalize(&mut self) -> RscGroupStatus {
        self.advance(true)
    }

    /// Drop all group state, ready for the next group.
    pub(super) fn reset(&mut self) {
        self.combined.clear();
        self.boundaries.clear();
        self.header_split = false;
        self.scan_from = 0;
        self.pending = None;
        self.segment_start = 0;
        self.inspector = SegmentInspector::default();
        self.invalid = false;
    }

    fn advance(&mut self, finalize: bool) -> RscGroupStatus {
        if self.invalid {
            return RscGroupStatus::Invalid;
        }
        loop {
            let step = next_tchunk(
                &self.combined,
                self.scan_from,
                self.pending.take(),
                None,
                !finalize,
            );
            match step {
                TChunkStep::Found(chunk) => {
                    // Text between chunks is final once the chunk after it
                    // completes, so it is inspected exactly once.
                    let mut settled = SegmentInspector::default();
                    if settled.inspect(&self.combined[self.segment_start..chunk.match_start], false)
                        == HeaderSuffixStatus::Invalid
                    {
                        self.invalid = true;
                        return RscGroupStatus::Invalid;
                    }
                    if self.boundaries.iter().any(|boundary| {
                        chunk.match_start < *boundary && *boundary < chunk.header_end
                    }) {
                        self.header_split = true;
                    }
                    self.segment_start = chunk.content_end;
                    self.scan_from = chunk.content_end;
                    self.inspector = SegmentInspector::default();
                }
                TChunkStep::Pending(chunk) => {
                    self.pending = Some(chunk);
                    return RscGroupStatus::NeedMore;
                }
                TChunkStep::Exhausted => break,
                TChunkStep::Invalid => {
                    self.invalid = true;
                    return RscGroupStatus::Invalid;
                }
            }
        }

        match self
            .inspector
            .inspect(&self.combined[self.segment_start..], true)
        {
            HeaderSuffixStatus::Complete => {
                if self.header_split {
                    RscGroupStatus::CompleteUnrewritable
                } else {
                    RscGroupStatus::CompleteRewritable
                }
            }
            HeaderSuffixStatus::NeedMore => {
                // A header can only start inside the pending candidate, so the
                // next search resumes there rather than at the scanned end.
                //
                // This is the one step still proportional to accumulated bytes
                // rather than to the new payload: a group that is one long hex
                // run keeps the candidate at its start, so the header search
                // re-scans it. That search is a literal prefilter bounded by
                // `max_combined_payload_bytes` (~13ms over 4MiB in 256
                // payloads), unlike the escape walk this classifier replaced.
                self.scan_from = self.segment_start + self.inspector.partial_header_start();
                RscGroupStatus::NeedMore
            }
            HeaderSuffixStatus::Invalid => {
                self.invalid = true;
                RscGroupStatus::Invalid
            }
        }
    }
}

/// Classify a complete group in one call.
pub(super) fn classify_rsc_group(
    payloads: &[&str],
    max_combined_payload_bytes: usize,
) -> RscGroupStatus {
    let mut classifier = RscGroupClassifier::new(max_combined_payload_bytes);
    for payload in payloads {
        classifier.push(payload);
    }
    classifier.finalize()
}

/// Resumable inspection of text outside T-chunk content.
///
/// Retains how far the text has been proven free of an incomplete
/// `id:Tlength,` header so growing text is not re-inspected from the start.
#[derive(Debug, Clone, Copy, Default)]
struct SegmentInspector {
    /// Where the next inspection resumes.
    index: usize,
    /// How far the hex run starting at [`Self::index`] has been verified.
    run_cursor: usize,
}

impl SegmentInspector {
    /// Offset at which a partial header could still begin.
    fn partial_header_start(&self) -> usize {
        self.index
    }

    fn inspect(&mut self, segment: &str, terminal: bool) -> HeaderSuffixStatus {
        let bytes = segment.as_bytes();

        while self.index < bytes.len() {
            if !bytes[self.index].is_ascii_hexdigit()
                || self.index > 0 && bytes[self.index - 1].is_ascii_hexdigit()
            {
                self.advance_to(self.index + 1);
                continue;
            }

            let mut cursor = self.run_cursor.max(self.index);
            while cursor < bytes.len() && bytes[cursor].is_ascii_hexdigit() {
                cursor += 1;
            }
            if cursor == bytes.len() {
                // The run may still grow, so keep its verified extent.
                self.run_cursor = cursor;
                return if terminal {
                    HeaderSuffixStatus::NeedMore
                } else {
                    HeaderSuffixStatus::Complete
                };
            }
            if terminal && &bytes[cursor..] == b":" {
                return HeaderSuffixStatus::NeedMore;
            }
            if bytes.get(cursor..cursor + 2) != Some(b":T") {
                self.advance_to(cursor + 1);
                continue;
            }

            cursor += 2;
            if cursor == bytes.len() {
                return if terminal {
                    HeaderSuffixStatus::NeedMore
                } else {
                    HeaderSuffixStatus::Invalid
                };
            }
            if !bytes[cursor].is_ascii_hexdigit() {
                return HeaderSuffixStatus::Invalid;
            }
            while cursor < bytes.len() && bytes[cursor].is_ascii_hexdigit() {
                cursor += 1;
            }
            if cursor == bytes.len() {
                return if terminal {
                    HeaderSuffixStatus::NeedMore
                } else {
                    HeaderSuffixStatus::Invalid
                };
            }
            if bytes[cursor] != b',' {
                return HeaderSuffixStatus::Invalid;
            }

            self.advance_to(cursor + 1);
        }

        HeaderSuffixStatus::Complete
    }

    fn advance_to(&mut self, index: usize) {
        self.index = index;
        self.run_cursor = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_complete_header_with_cross_payload_content_as_rewritable() {
        let payloads = ["1a:T3,ab", "c\n"];

        assert_eq!(
            classify_rsc_group(&payloads, usize::MAX),
            RscGroupStatus::CompleteRewritable,
            "should rewrite a complete header whose content crosses payloads",
        );
    }

    #[test]
    fn classifies_header_split_across_payloads_as_complete_unrewritable() {
        let payloads = ["1a:T", "3,abc\n"];

        assert_eq!(
            classify_rsc_group(&payloads, usize::MAX),
            RscGroupStatus::CompleteUnrewritable,
            "should restore a physically split header unchanged",
        );
    }

    #[test]
    fn classifies_every_header_split_as_unrewritable_after_completion() {
        let header = "1a:T3e,";
        for split in 1..header.len() {
            let first = &header[..split];
            let second = format!("{}{}", &header[split..], "x".repeat(0x3e));
            let payloads = [first, second.as_str()];

            assert_eq!(
                classify_rsc_group(&payloads, usize::MAX),
                RscGroupStatus::CompleteUnrewritable,
                "should restore a header split at byte {split}",
            );
        }
    }

    #[test]
    fn classifies_incomplete_content_and_header_candidates_as_needing_more() {
        for payloads in [vec!["1a:T3,ab"], vec!["prefix1a:T"], vec!["prefix1a"]] {
            assert_eq!(
                classify_rsc_group(&payloads, usize::MAX),
                RscGroupStatus::NeedMore,
                "should retain an incomplete T-chunk candidate",
            );
        }
    }

    #[test]
    fn retains_every_incomplete_header_prefix() {
        let header = "1a:T3e,";
        for split in 1..header.len() {
            assert_eq!(
                classify_rsc_group(&[&header[..split]], usize::MAX),
                RscGroupStatus::NeedMore,
                "should retain the header prefix split at byte {split}",
            );
        }
    }

    #[test]
    fn classifies_disproved_hex_suffix_as_complete() {
        let payloads = ["ordinary1a", "-suffix"];

        assert_eq!(
            classify_rsc_group(&payloads, usize::MAX),
            RscGroupStatus::CompleteRewritable,
            "should release a trailing hexadecimal run once disproved",
        );
    }

    #[test]
    fn classifies_malformed_and_unreasonable_lengths_as_invalid() {
        for payload in ["1a:Tzz,value", "1a:T6400001,value"] {
            assert_eq!(
                classify_rsc_group(&[payload], usize::MAX),
                RscGroupStatus::Invalid,
                "should reject malformed or unreasonable T-chunk lengths",
            );
        }
    }

    #[test]
    fn counts_javascript_escapes_and_unicode_bytes() {
        for payload in [r#"1:T3,a\n\""#, r#"1:T4,\ud83d\ude00"#, "1:T3,€"] {
            assert_eq!(
                classify_rsc_group(&[payload], usize::MAX),
                RscGroupStatus::CompleteRewritable,
                "should count decoded JavaScript string bytes",
            );
        }
    }

    #[test]
    fn classifies_multiple_complete_tchunks() {
        let payloads = ["1:T1,a2:T2,bc"];

        assert_eq!(
            classify_rsc_group(&payloads, usize::MAX),
            RscGroupStatus::CompleteRewritable,
            "should accept multiple complete T-chunks",
        );
    }

    #[test]
    fn rejects_payloads_over_the_group_bound_before_combining() {
        let payloads = ["1:T1,a", "tail"];

        assert_eq!(
            classify_rsc_group(&payloads, 4),
            RscGroupStatus::Invalid,
            "should reject a group larger than its configured bound",
        );
    }

    fn processor_with_payloads(
        payloads: &[&str],
        limit: usize,
    ) -> (NextJsRscStreamProcessor, Vec<String>) {
        let integration_state = IntegrationDocumentState::default();
        let shared = document_state(&integration_state);
        let mut placeholders = Vec::new();
        {
            let mut state = shared.lock().expect("should lock document state");
            for payload in payloads {
                let placeholder =
                    rsc_payload_placeholder(&state.namespace, state.next_placeholder_index);
                state.next_placeholder_index += 1;
                state.captured_payload_bytes += payload.len();
                state.captured_payloads.push_back(CapturedPayload {
                    placeholder: placeholder.clone(),
                    original: (*payload).to_owned(),
                });
                placeholders.push(placeholder);
            }
        }
        (
            NextJsRscStreamProcessor::new(
                shared,
                "origin.example.com".to_owned(),
                "proxy.example.com".to_owned(),
                "https".to_owned(),
                limit,
            ),
            placeholders,
        )
    }

    #[test]
    fn stream_processor_emits_ordinary_html_before_eof() {
        let (mut processor, _) = processor_with_payloads(&[], 1024);

        assert_eq!(
            processor
                .process_chunk(b"<html><body>ordinary", false)
                .expect("should process ordinary HTML"),
            b"<html><body>ordinary",
            "should not wait for EOF without an unresolved RSC group",
        );
    }

    #[test]
    fn stream_processor_restores_header_split_after_colon() {
        let payloads = ["1:", "T25,https://origin.example.com/longer-path!"];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 1024);
        let first = processor
            .process_chunk(placeholders[0].as_bytes(), false)
            .expect("should retain partial header");
        assert!(first.is_empty(), "should wait for the rest of the header");

        let second = processor
            .process_chunk(placeholders[1].as_bytes(), true)
            .expect("should restore a physically split header");
        assert_eq!(
            second,
            payloads.concat().as_bytes(),
            "should preserve URL and declared length together",
        );
    }

    #[test]
    fn stream_processor_restores_payloads_on_rewrite_count_mismatch() {
        let payloads = ["1:T3,ab", "c\n\0SPLIT\0https://origin.example.com/path/"];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 1024);
        let output = processor
            .process_chunk(placeholders.concat().as_bytes(), true)
            .expect("should restore originals when the rewrite count differs");
        assert_eq!(
            output,
            payloads.concat().as_bytes(),
            "should preserve all original bytes"
        );
    }

    #[test]
    fn stream_processor_rewrites_and_releases_a_complete_payload_in_one_call() {
        let payload = r#"1:T29,{"url":"https://origin.example.com/path"}"#;
        let (mut processor, placeholders) = processor_with_payloads(&[payload], 1024);
        let input = format!("before{}after", placeholders[0]);

        let output = processor
            .process_chunk(input.as_bytes(), false)
            .expect("should process complete RSC payload");
        let output = String::from_utf8(output).expect("should emit UTF-8 HTML");

        assert!(output.starts_with("before"));
        assert!(output.ends_with("after"));
        assert!(output.contains("proxy.example.com/path"));
        assert!(!output.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX));
    }

    #[test]
    fn stream_processor_holds_only_until_cross_payload_content_completes() {
        let payloads = ["1:T3,ab", "c"];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 1024);

        let first = processor
            .process_chunk(format!("head{}middle", placeholders[0]).as_bytes(), false)
            .expect("should process incomplete group");
        assert_eq!(first, b"head", "should hold from the first placeholder");

        let second = processor
            .process_chunk(format!("{}tail", placeholders[1]).as_bytes(), false)
            .expect("should complete group");
        assert_eq!(
            second,
            format!("{}middle{}tail", payloads[0], payloads[1]).as_bytes(),
            "should release the complete group and interstitial output in order",
        );
    }

    #[test]
    fn held_output_overflow_restores_interstitial_bytes_before_the_next_payload() {
        let payloads = ["1:T3,ab", "c"];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 80);

        assert!(
            processor
                .process_chunk(placeholders[0].as_bytes(), false)
                .expect("should hold incomplete group")
                .is_empty()
        );
        let interstitial = "x".repeat(50);
        let output = processor
            .process_chunk(
                format!("{interstitial}{}tail", placeholders[1]).as_bytes(),
                false,
            )
            .expect("should restore over-limit group");
        assert_eq!(
            output,
            format!("{}{interstitial}{}tail", payloads[0], payloads[1]).as_bytes(),
            "overflow fallback must preserve bytes between payload scripts"
        );
    }

    #[test]
    fn invalid_group_restores_later_payloads_in_the_same_output_chunk() {
        let payloads = [
            "1:Tzz,invalid",
            r#"1:T29,{"url":"https://origin.example.com/path"}"#,
        ];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 1024);
        let input = format!("{}middle{}tail", placeholders[0], placeholders[1]);

        let output = processor
            .process_chunk(input.as_bytes(), false)
            .expect("should restore invalid group and later payload");
        assert_eq!(
            output,
            format!("{}middle{}tail", payloads[0], payloads[1]).as_bytes(),
            "document-wide bypass must take effect within the current output chunk"
        );
    }

    #[test]
    fn stream_processor_matches_a_placeholder_split_across_output_chunks() {
        let (mut processor, placeholders) = processor_with_payloads(&["plain"], 1024);
        let placeholder = &placeholders[0];
        let split = placeholder.len() / 2;

        let first = processor
            .process_chunk(&placeholder.as_bytes()[..split], false)
            .expect("should retain a partial placeholder");
        assert!(first.is_empty(), "should retain only the candidate suffix");
        let second = processor
            .process_chunk(&placeholder.as_bytes()[split..], false)
            .expect("should finish the placeholder");
        assert_eq!(second, b"plain", "should restore the captured payload");
    }

    #[test]
    fn unresolved_group_bytes_remain_charged_until_release() {
        let payloads = ["1:T3,ab", "c"];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, 1024);
        let state = Arc::clone(&processor.state);

        assert!(
            processor
                .process_chunk(placeholders[0].as_bytes(), false)
                .expect("should hold incomplete group")
                .is_empty()
        );
        assert_eq!(
            state
                .lock()
                .expect("should lock document state")
                .captured_payload_bytes,
            payloads.iter().map(|payload| payload.len()).sum::<usize>(),
            "held payloads must remain in the request-scoped capture budget"
        );

        processor
            .process_chunk(placeholders[1].as_bytes(), false)
            .expect("should release complete group");
        assert_eq!(
            state
                .lock()
                .expect("should lock document state")
                .captured_payload_bytes,
            0,
            "released payloads should return their request-scoped budget"
        );
    }

    #[test]
    fn excessive_unresolved_payload_count_falls_back_unchanged() {
        let payloads = vec!["1:Tffff,x"; MAX_UNRESOLVED_RSC_PAYLOADS + 1];
        let (mut processor, placeholders) = processor_with_payloads(&payloads, usize::MAX);
        let input = placeholders.join("");

        let output = processor
            .process_chunk(input.as_bytes(), false)
            .expect("should restore excessive unresolved group");
        assert_eq!(output, payloads.join("").as_bytes());
        assert!(
            processor
                .state
                .lock()
                .expect("should lock document state")
                .bypass_rsc,
            "excessive segment count should enable document-wide fallback"
        );
    }

    /// A T-chunk spread over many payloads must classify the same whether it is
    /// fed incrementally or all at once. Feeding it incrementally is what keeps
    /// the cost linear in the group's bytes instead of bytes times segments.
    #[test]
    fn incremental_classification_matches_whole_group_classification() {
        // Each repetition unescapes to `ab` + newline + `cd` + `A` = 6 bytes.
        let repetitions = 64;
        let content = r"ab\ncd\x41".repeat(repetitions);
        let declared = repetitions * 6;
        let document = format!("1:T{declared:x},{content}\n");

        for segments in [2usize, 7, 64] {
            let per = document.len().div_ceil(segments);
            let payloads: Vec<&str> = document
                .as_bytes()
                .chunks(per)
                .map(|chunk| std::str::from_utf8(chunk).expect("fixture should be ASCII"))
                .collect();

            let mut classifier = RscGroupClassifier::new(usize::MAX);
            for payload in &payloads {
                classifier.push(payload);
            }
            let incremental = classifier.finalize();

            assert_eq!(
                incremental,
                classify_rsc_group(&payloads, usize::MAX),
                "incremental classification of {segments} segments should match the whole group"
            );
            assert_eq!(
                incremental,
                RscGroupStatus::CompleteRewritable,
                "a complete T-chunk split into {segments} segments should stay rewritable"
            );
        }
    }

    /// Classification must not depend on how a group was split, except for the
    /// one rule that is defined in terms of splits: a header straddling a
    /// payload boundary is complete but unrewritable.
    #[test]
    fn classification_is_independent_of_payload_split() {
        // Each fixture lists the byte span of every `id:Tlength,` header in it.
        for (document, headers) in [
            (r"1:T6,ab\ncdx", &[(0usize, 5usize)][..]),
            (r"1:T6,ab\ncdx\nplain text", &[(0, 5)][..]),
            (r"1:T6,ab\ncdx\ndead", &[(0, 5)][..]),
            (r"1:T6,ab\ncdx2:T2,zz", &[(0, 5), (12, 17)][..]),
            ("plain text with no chunk", &[][..]),
            ("trailing hex dead", &[][..]),
            ("5:T", &[][..]),
        ] {
            let whole = classify_rsc_group(&[document], usize::MAX);

            for split in 1..document.len() {
                let parts = [&document[..split], &document[split..]];
                let actual = classify_rsc_group(&parts, usize::MAX);
                // A straddling header only downgrades a group that is otherwise
                // rewritable; an incomplete group stays incomplete.
                let straddles = headers
                    .iter()
                    .any(|(start, end)| *start < split && split < *end);
                let expected = match whole {
                    RscGroupStatus::CompleteRewritable if straddles => {
                        RscGroupStatus::CompleteUnrewritable
                    }
                    other => other,
                };
                assert_eq!(
                    actual, expected,
                    "`{document}` split at byte {split} should classify as {expected:?}"
                );
            }
        }
    }

    /// The byte limit must trip at the same accumulation point whether payloads
    /// are fed one at a time or classified as a whole group.
    #[test]
    fn incremental_classification_honors_the_byte_limit() {
        let first = "1:T6,ab";
        let second = r"\ncdx";
        // One byte short of holding both payloads.
        let limit = first.len() + second.len() - 1;

        let mut classifier = RscGroupClassifier::new(limit);
        assert_eq!(
            classifier.push(first),
            RscGroupStatus::NeedMore,
            "a payload within the limit should await the rest of its chunk"
        );
        assert_eq!(
            classifier.push(second),
            RscGroupStatus::Invalid,
            "the payload that crosses the limit should invalidate the group"
        );
        assert_eq!(
            classify_rsc_group(&[first, second], limit),
            RscGroupStatus::Invalid,
            "whole-group classification should reach the same verdict"
        );
    }
}
