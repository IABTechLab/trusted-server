use super::rsc::{TChunkScan, scan_tchunks};

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

    let mut combined = String::with_capacity(total_size);
    let mut boundaries = Vec::with_capacity(payloads.len().saturating_sub(1));
    for (index, payload) in payloads.iter().enumerate() {
        combined.push_str(payload);
        if index + 1 < payloads.len() {
            boundaries.push(combined.len());
        }
    }

    let chunks = match scan_tchunks(&combined) {
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
