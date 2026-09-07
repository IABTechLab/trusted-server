use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::integrations::IntegrationDocumentState;

use super::NEXTJS_INTEGRATION_ID;
use super::rsc::{TChunkScan, scan_tchunks};

pub(super) const RSC_PAYLOAD_PLACEHOLDER_PREFIX: &str = "__ts_rsc_";
pub(super) const RSC_PAYLOAD_PLACEHOLDER_SUFFIX: &str = "__";

#[derive(Debug, Default)]
pub(super) enum FragmentState {
    #[default]
    Idle,
    Buffering(String),
    BypassUntilLast,
}

#[derive(Debug)]
pub(super) struct CapturedPayload {
    pub(super) placeholder: String,
    pub(super) original: String,
}

#[derive(Debug)]
pub(super) struct NextJsDocumentState {
    pub(super) namespace: String,
    pub(super) next_data: FragmentState,
    pub(super) rsc_script: FragmentState,
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
}
