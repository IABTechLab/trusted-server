use std::borrow::Cow;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use crate::integrations::{
    IntegrationDocumentState, IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory,
};
use crate::streaming_processor::StreamProcessor;

use super::rsc::{
    DEFAULT_MAX_COMBINED_PAYLOAD_BYTES, TChunkScan, rewrite_rsc_scripts_combined_with_limit,
    scan_tchunks,
};
use super::shared::RscUrlRewriter;
use super::{NEXTJS_INTEGRATION_ID, NextJsIntegrationConfig};

pub(super) const RSC_PAYLOAD_PLACEHOLDER_PREFIX: &str = "__ts_rsc_";
pub(super) const RSC_PAYLOAD_PLACEHOLDER_SUFFIX: &str = "__";
const MAX_UNRESOLVED_RSC_PAYLOADS: usize = 256;

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
        // Classification scans the logical group. Bound its segment count as
        // well as its bytes so adversarial tiny scripts cannot amplify that
        // work quadratically; the hydration-safe fallback restores originals.
        if self.group.len() > MAX_UNRESOLVED_RSC_PAYLOADS {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .bypass_rsc = true;
            return self.release_group(None).map(Some);
        }
        let payloads: Vec<&str> = self
            .group
            .iter()
            .map(|payload| payload.original.as_str())
            .collect();
        match classify_rsc_group(&payloads, self.limit) {
            RscGroupStatus::NeedMore => Ok(None),
            RscGroupStatus::CompleteRewritable => {
                let rewritten = rewrite_rsc_scripts_combined_with_limit(
                    &payloads,
                    &self.rewriter,
                    &self.origin_host,
                    &self.request_host,
                    &self.request_scheme,
                    self.limit,
                );
                if rewritten.len() != self.group.len() {
                    return Err(io::Error::other(
                        "Next.js RSC rewrite returned a mismatched payload count",
                    ));
                }
                self.release_group(Some(&rewritten)).map(Some)
            }
            RscGroupStatus::CompleteUnrewritable => self.release_group(None).map(Some),
            RscGroupStatus::Invalid => {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .bypass_rsc = true;
                self.release_group(None).map(Some)
            }
        }
    }

    fn release_bypass(&mut self, current: &[u8]) -> io::Result<Vec<u8>> {
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
                output.extend(self.release_group(None)?);
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

pub(super) fn classify_rsc_group(
    payloads: &[&str],
    max_combined_payload_bytes: usize,
) -> RscGroupStatus {
    let Some(total_size) = payloads
        .iter()
        .try_fold(0usize, |total, payload| total.checked_add(payload.len()))
    else {
        return RscGroupStatus::Invalid;
    };
    if total_size > max_combined_payload_bytes {
        return RscGroupStatus::Invalid;
    }

    let mut boundaries = Vec::with_capacity(payloads.len().saturating_sub(1));
    let combined = if let [payload] = payloads {
        Cow::Borrowed(*payload)
    } else {
        let mut combined = String::with_capacity(total_size);
        for (index, payload) in payloads.iter().enumerate() {
            combined.push_str(payload);
            if index + 1 < payloads.len() {
                boundaries.push(combined.len());
            }
        }
        Cow::Owned(combined)
    };

    let chunks = match scan_tchunks(combined.as_ref()) {
        TChunkScan::Complete(chunks) => chunks,
        TChunkScan::NeedMore => return RscGroupStatus::NeedMore,
        TChunkScan::Invalid => return RscGroupStatus::Invalid,
    };

    let mut segment_start = 0;
    for chunk in &chunks {
        if inspect_non_chunk_segment(&combined[segment_start..chunk.match_start], false)
            == HeaderSuffixStatus::Invalid
        {
            return RscGroupStatus::Invalid;
        }
        segment_start = chunk.content_end;
    }

    match inspect_non_chunk_segment(&combined[segment_start..], true) {
        HeaderSuffixStatus::Complete => {}
        HeaderSuffixStatus::NeedMore => return RscGroupStatus::NeedMore,
        HeaderSuffixStatus::Invalid => return RscGroupStatus::Invalid,
    }

    if chunks.iter().any(|chunk| {
        boundaries
            .iter()
            .any(|boundary| chunk.match_start < *boundary && *boundary < chunk.header_end)
    }) {
        RscGroupStatus::CompleteUnrewritable
    } else {
        RscGroupStatus::CompleteRewritable
    }
}

fn inspect_non_chunk_segment(segment: &str, terminal: bool) -> HeaderSuffixStatus {
    let bytes = segment.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if !bytes[index].is_ascii_hexdigit() || index > 0 && bytes[index - 1].is_ascii_hexdigit() {
            index += 1;
            continue;
        }

        let mut cursor = index;
        while cursor < bytes.len() && bytes[cursor].is_ascii_hexdigit() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            return if terminal {
                HeaderSuffixStatus::NeedMore
            } else {
                HeaderSuffixStatus::Complete
            };
        }
        if bytes.get(cursor..cursor + 2) != Some(b":T") {
            index = cursor + 1;
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

        index = cursor + 1;
    }

    HeaderSuffixStatus::Complete
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
}
