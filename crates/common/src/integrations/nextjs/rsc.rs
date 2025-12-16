use std::borrow::Cow;

use once_cell::sync::Lazy;
use regex::{escape, Regex};

/// T-chunk header pattern: hex_id:Thex_length,
static TCHUNK_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"([0-9a-fA-F]+):T([0-9a-fA-F]+),").expect("valid T-chunk regex"));

/// Marker used to track script boundaries when combining RSC content.
pub(crate) const RSC_MARKER: &str = "\x00SPLIT\x00";

/// Maximum combined payload size for cross-script processing (10 MB).
/// Payloads exceeding this limit are processed individually without cross-script T-chunk handling.
const MAX_COMBINED_PAYLOAD_SIZE: usize = 10 * 1024 * 1024;

// =============================================================================
// Escape Sequence Parsing
// =============================================================================
//
// JS escape sequences are parsed by a shared iterator to avoid code duplication.
// The iterator yields (source_len, unescaped_byte_count) for each logical unit.

/// A single parsed element from a JS string.
#[derive(Clone, Copy)]
struct EscapeElement {
    /// Number of unescaped bytes this represents.
    byte_count: usize,
}

/// Iterator over escape sequences in a JS string.
/// Yields the unescaped byte count for each element.
struct EscapeSequenceIter<'a> {
    bytes: &'a [u8],
    str_ref: &'a str,
    pos: usize,
    skip_marker: Option<&'a [u8]>,
}

impl<'a> EscapeSequenceIter<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            str_ref: s,
            pos: 0,
            skip_marker: None,
        }
    }

    fn with_marker(s: &'a str, marker: &'a [u8]) -> Self {
        Self {
            bytes: s.as_bytes(),
            str_ref: s,
            pos: 0,
            skip_marker: Some(marker),
        }
    }

    fn from_position(s: &'a str, start: usize) -> Self {
        Self {
            bytes: s.as_bytes(),
            str_ref: s,
            pos: start,
            skip_marker: None,
        }
    }

    fn from_position_with_marker(s: &'a str, start: usize, marker: &'a [u8]) -> Self {
        Self {
            bytes: s.as_bytes(),
            str_ref: s,
            pos: start,
            skip_marker: Some(marker),
        }
    }

    /// Current position in the source string.
    fn position(&self) -> usize {
        self.pos
    }
}

impl Iterator for EscapeSequenceIter<'_> {
    type Item = EscapeElement;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.bytes.len() {
            return None;
        }

        if let Some(marker) = self.skip_marker {
            if self.pos + marker.len() <= self.bytes.len()
                && &self.bytes[self.pos..self.pos + marker.len()] == marker
            {
                self.pos += marker.len();
                return Some(EscapeElement { byte_count: 0 });
            }
        }

        if self.bytes[self.pos] == b'\\' && self.pos + 1 < self.bytes.len() {
            let esc = self.bytes[self.pos + 1];

            if matches!(
                esc,
                b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/'
            ) {
                self.pos += 2;
                return Some(EscapeElement { byte_count: 1 });
            }

            if esc == b'x' && self.pos + 3 < self.bytes.len() {
                self.pos += 4;
                return Some(EscapeElement { byte_count: 1 });
            }

            if esc == b'u' && self.pos + 5 < self.bytes.len() {
                let hex = &self.str_ref[self.pos + 2..self.pos + 6];
                if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                        if (0xD800..=0xDBFF).contains(&code_unit)
                            && self.pos + 11 < self.bytes.len()
                            && self.bytes[self.pos + 6] == b'\\'
                            && self.bytes[self.pos + 7] == b'u'
                        {
                            let hex2 = &self.str_ref[self.pos + 8..self.pos + 12];
                            if hex2.chars().all(|c| c.is_ascii_hexdigit()) {
                                if let Ok(code_unit2) = u16::from_str_radix(hex2, 16) {
                                    if (0xDC00..=0xDFFF).contains(&code_unit2) {
                                        self.pos += 12;
                                        return Some(EscapeElement { byte_count: 4 });
                                    }
                                }
                            }
                        }

                        let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                        self.pos += 6;
                        return Some(EscapeElement {
                            byte_count: c.len_utf8(),
                        });
                    }
                }
            }
        }

        if self.bytes[self.pos] < 0x80 {
            self.pos += 1;
            Some(EscapeElement { byte_count: 1 })
        } else {
            let c = self.str_ref[self.pos..]
                .chars()
                .next()
                .unwrap_or('\u{FFFD}');
            let len = c.len_utf8();
            self.pos += len;
            Some(EscapeElement { byte_count: len })
        }
    }
}

/// Calculate the unescaped byte length of a JS string with escape sequences.
fn calculate_unescaped_byte_length(s: &str) -> usize {
    EscapeSequenceIter::new(s).map(|e| e.byte_count).sum()
}

/// Consume a specified number of unescaped bytes from a JS string, returning the end position.
fn consume_unescaped_bytes(s: &str, start_pos: usize, byte_count: usize) -> (usize, usize) {
    let mut iter = EscapeSequenceIter::from_position(s, start_pos);
    let mut consumed = 0;

    while consumed < byte_count {
        match iter.next() {
            Some(elem) => consumed += elem.byte_count,
            None => break,
        }
    }

    (iter.position(), consumed)
}

// =============================================================================
// T-chunk discovery
// =============================================================================

/// Information about a T-chunk found in the combined RSC content.
struct TChunkInfo {
    /// The chunk ID (hex string like "1a", "443").
    id: String,
    /// Position where the T-chunk header starts (e.g., position of "1a:T...").
    match_start: usize,
    /// Position right after the comma (where content begins).
    header_end: usize,
    /// Position where the content ends.
    content_end: usize,
}

/// Find all T-chunks in content, optionally skipping markers.
fn find_tchunks_impl(content: &str, skip_markers: bool) -> Vec<TChunkInfo> {
    let mut chunks = Vec::new();
    let mut search_pos = 0;
    let marker = if skip_markers {
        Some(RSC_MARKER.as_bytes())
    } else {
        None
    };

    while search_pos < content.len() {
        if let Some(cap) = TCHUNK_PATTERN.captures(&content[search_pos..]) {
            let m = cap.get(0).expect("T-chunk match should exist");
            let match_start = search_pos + m.start();
            let header_end = search_pos + m.end();

            let id = cap
                .get(1)
                .expect("T-chunk id should exist")
                .as_str()
                .to_string();
            let length_hex = cap.get(2).expect("T-chunk length should exist").as_str();
            let declared_length = usize::from_str_radix(length_hex, 16).unwrap_or(0);

            let content_end = if let Some(marker_bytes) = marker {
                let mut iter = EscapeSequenceIter::from_position_with_marker(
                    content,
                    header_end,
                    marker_bytes,
                );
                let mut consumed = 0;
                while consumed < declared_length {
                    match iter.next() {
                        Some(elem) => consumed += elem.byte_count,
                        None => break,
                    }
                }
                iter.position()
            } else {
                let (pos, _) = consume_unescaped_bytes(content, header_end, declared_length);
                pos
            };

            chunks.push(TChunkInfo {
                id,
                match_start,
                header_end,
                content_end,
            });

            search_pos = content_end;
        } else {
            break;
        }
    }

    chunks
}

fn find_tchunks(content: &str) -> Vec<TChunkInfo> {
    find_tchunks_impl(content, false)
}

fn find_tchunks_with_markers(content: &str) -> Vec<TChunkInfo> {
    find_tchunks_impl(content, true)
}

// =============================================================================
// URL rewriting (cached per call)
// =============================================================================

/// Rewriter for RSC payload URL patterns.
///
/// This is constructed per document / payload rewrite so that the origin-host-dependent regex is
/// compiled once, then reused across multiple calls.
pub(crate) struct RscUrlRewriter {
    origin_host: String,
    request_host: String,
    request_scheme: String,
    pattern: Regex,
}

impl RscUrlRewriter {
    pub(crate) fn new(origin_host: &str, request_host: &str, request_scheme: &str) -> Self {
        let escaped_origin = escape(origin_host);

        // Match:
        // - https://origin_host or http://origin_host
        // - //origin_host (protocol-relative)
        // - escaped variants inside JSON-in-JS strings (e.g., \/\/origin_host)
        let pattern = Regex::new(&format!(
            r#"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//){}"#,
            escaped_origin
        ))
        .expect("valid RSC URL rewrite regex");

        Self {
            origin_host: origin_host.to_string(),
            request_host: request_host.to_string(),
            request_scheme: request_scheme.to_string(),
            pattern,
        }
    }

    pub(crate) fn rewrite<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if !input.contains(&self.origin_host) {
            return Cow::Borrowed(input);
        }

        // Phase 1: Regex-based URL pattern rewriting (handles escaped slashes, schemes, etc.)
        let replaced = self
            .pattern
            .replace_all(input, |caps: &regex::Captures<'_>| {
                let slashes = caps.get(3).map_or("//", |m| m.as_str());
                if caps.get(1).is_some() {
                    format!("{}:{}{}", self.request_scheme, slashes, self.request_host)
                } else {
                    format!("{}{}", slashes, self.request_host)
                }
            });

        // Phase 2: Handle bare host occurrences not matched by the URL regex
        // (e.g., `siteProductionDomain`). Only check if regex made no changes,
        // because if it did, we already know origin_host was present.
        let text = match &replaced {
            Cow::Borrowed(s) => *s,
            Cow::Owned(s) => s.as_str(),
        };

        if !text.contains(&self.origin_host) {
            return replaced;
        }

        // Bare host replacement needed
        Cow::Owned(text.replace(&self.origin_host, &self.request_host))
    }

    pub(crate) fn rewrite_to_string(&self, input: &str) -> String {
        self.rewrite(input).into_owned()
    }
}

// =============================================================================
// Single-script T-chunk processing
// =============================================================================

pub(crate) fn rewrite_rsc_tchunks_with_rewriter(
    content: &str,
    rewriter: &RscUrlRewriter,
) -> String {
    let chunks = find_tchunks(content);

    if chunks.is_empty() {
        return rewriter.rewrite_to_string(content);
    }

    let mut result = String::with_capacity(content.len());
    let mut last_end = 0;

    for chunk in &chunks {
        let before = &content[last_end..chunk.match_start];
        result.push_str(rewriter.rewrite(before).as_ref());

        let chunk_content = &content[chunk.header_end..chunk.content_end];
        let rewritten_content = rewriter.rewrite_to_string(chunk_content);

        let new_length = calculate_unescaped_byte_length(&rewritten_content);
        let new_length_hex = format!("{new_length:x}");

        result.push_str(&chunk.id);
        result.push_str(":T");
        result.push_str(&new_length_hex);
        result.push(',');
        result.push_str(&rewritten_content);

        last_end = chunk.content_end;
    }

    let remaining = &content[last_end..];
    result.push_str(rewriter.rewrite(remaining).as_ref());

    result
}

// =============================================================================
// Cross-script RSC processing
// =============================================================================

fn calculate_unescaped_byte_length_skip_markers(s: &str) -> usize {
    EscapeSequenceIter::with_marker(s, RSC_MARKER.as_bytes())
        .map(|e| e.byte_count)
        .sum()
}

/// Process multiple RSC script payloads together, handling cross-script T-chunks.
pub fn rewrite_rsc_scripts_combined(
    payloads: &[&str],
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> Vec<String> {
    if payloads.is_empty() {
        return Vec::new();
    }

    let rewriter = RscUrlRewriter::new(origin_host, request_host, request_scheme);

    if payloads.len() == 1 {
        return vec![rewrite_rsc_tchunks_with_rewriter(payloads[0], &rewriter)];
    }

    // Check total size before allocating combined buffer
    let total_size: usize =
        payloads.iter().map(|p| p.len()).sum::<usize>() + (payloads.len() - 1) * RSC_MARKER.len();

    if total_size > MAX_COMBINED_PAYLOAD_SIZE {
        // Fall back to individual processing if combined size is too large.
        // This sacrifices cross-script T-chunk correctness for memory safety.
        log::warn!(
            "RSC combined payload size {} exceeds limit {}, processing individually",
            total_size,
            MAX_COMBINED_PAYLOAD_SIZE
        );
        return payloads
            .iter()
            .map(|p| rewrite_rsc_tchunks_with_rewriter(p, &rewriter))
            .collect();
    }

    let mut combined = String::with_capacity(total_size);
    combined.push_str(payloads[0]);
    for payload in &payloads[1..] {
        combined.push_str(RSC_MARKER);
        combined.push_str(payload);
    }

    let chunks = find_tchunks_with_markers(&combined);
    if chunks.is_empty() {
        return payloads
            .iter()
            .map(|p| rewriter.rewrite_to_string(p))
            .collect();
    }

    let mut result = String::with_capacity(combined.len());
    let mut last_end = 0;

    for chunk in &chunks {
        let before = &combined[last_end..chunk.match_start];
        result.push_str(rewriter.rewrite(before).as_ref());

        let chunk_content = &combined[chunk.header_end..chunk.content_end];
        let rewritten_content = rewriter.rewrite_to_string(chunk_content);

        let new_length = calculate_unescaped_byte_length_skip_markers(&rewritten_content);
        let new_length_hex = format!("{new_length:x}");

        result.push_str(&chunk.id);
        result.push_str(":T");
        result.push_str(&new_length_hex);
        result.push(',');
        result.push_str(&rewritten_content);

        last_end = chunk.content_end;
    }

    let remaining = &combined[last_end..];
    result.push_str(rewriter.rewrite(remaining).as_ref());

    result.split(RSC_MARKER).map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tchunk_length_recalculation() {
        let content = r#"1a:T29,{"url":"https://origin.example.com/path"}"#;
        let rewriter = RscUrlRewriter::new("origin.example.com", "test.example.com", "https");
        let result = rewrite_rsc_tchunks_with_rewriter(content, &rewriter);

        assert!(
            result.contains("test.example.com"),
            "URL should be rewritten"
        );
        assert!(
            result.starts_with("1a:T27,"),
            "T-chunk length should be updated from 29 (41) to 27 (39). Got: {}",
            result
        );
    }

    #[test]
    fn tchunk_length_recalculation_with_length_increase() {
        let content = r#"1a:T1c,{"url":"https://short.io/x"}"#;
        let rewriter = RscUrlRewriter::new("short.io", "test.example.com", "https");
        let result = rewrite_rsc_tchunks_with_rewriter(content, &rewriter);

        assert!(
            result.contains("test.example.com"),
            "URL should be rewritten"
        );
        assert!(
            result.starts_with("1a:T24,"),
            "T-chunk length should be updated from 1c (28) to 24 (36). Got: {}",
            result
        );
    }

    #[test]
    fn calculate_unescaped_byte_length_handles_common_escapes() {
        assert_eq!(calculate_unescaped_byte_length("hello"), 5);
        assert_eq!(calculate_unescaped_byte_length(r#"\n"#), 1);
        assert_eq!(calculate_unescaped_byte_length(r#"\r\n"#), 2);
        assert_eq!(calculate_unescaped_byte_length(r#"\""#), 1);
        assert_eq!(calculate_unescaped_byte_length(r#"\\"#), 1);
        assert_eq!(calculate_unescaped_byte_length(r#"\x41"#), 1);
        assert_eq!(calculate_unescaped_byte_length(r#"\u0041"#), 1);
        assert_eq!(calculate_unescaped_byte_length(r#"\u00e9"#), 2);
    }

    #[test]
    fn multiple_tchunks() {
        let content = r#"1a:T1c,{"url":"https://short.io/x"}\n1b:T1c,{"url":"https://short.io/y"}"#;
        let rewriter = RscUrlRewriter::new("short.io", "test.example.com", "https");
        let result = rewrite_rsc_tchunks_with_rewriter(content, &rewriter);

        assert!(
            result.contains("test.example.com"),
            "URLs should be rewritten"
        );
        let count = result.matches(":T24,").count();
        assert_eq!(count, 2, "Both T-chunks should have updated lengths");
    }

    #[test]
    fn cross_script_tchunk_rewriting() {
        let script0 = r#"other:data\n1a:T40,partial content"#;
        let script1 = r#" with https://origin.example.com/page goes here"#;

        let combined_content = "partial content with https://origin.example.com/page goes here";
        let combined_len = calculate_unescaped_byte_length(combined_content);
        println!(
            "Combined T-chunk content length: {} bytes = 0x{:x}",
            combined_len, combined_len
        );

        let payloads: Vec<&str> = vec![script0, script1];
        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        assert_eq!(results.len(), 2, "Should return same number of scripts");
        assert!(
            results[1].contains("test.example.com"),
            "URL in script 1 should be rewritten. Got: {}",
            results[1]
        );

        let rewritten_content = "partial content with https://test.example.com/page goes here";
        let rewritten_len = calculate_unescaped_byte_length(rewritten_content);
        let expected_header = format!(":T{:x},", rewritten_len);
        assert!(
            results[0].contains(&expected_header),
            "T-chunk length in script 0 should be updated to {}. Got: {}",
            expected_header,
            results[0]
        );
    }

    #[test]
    fn cross_script_preserves_non_tchunk_content() {
        let script0 = r#"{"url":"https://origin.example.com/first"}\n1a:T40,partial"#;
        let script1 = r#" content with https://origin.example.com/page end"#;

        let payloads: Vec<&str> = vec![script0, script1];
        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        assert!(
            results[0].contains("test.example.com/first"),
            "URL outside T-chunk should be rewritten. Got: {}",
            results[0]
        );

        assert!(
            results[1].contains("test.example.com/page"),
            "URL inside cross-script T-chunk should be rewritten. Got: {}",
            results[1]
        );
    }

    #[test]
    fn preserves_protocol_relative_urls() {
        let input = r#"{"url":"//origin.example.com/path"}"#;
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let rewritten = rewriter.rewrite_to_string(input);

        assert!(
            rewritten.contains(r#""url":"//proxy.example.com/path""#),
            "Protocol-relative URL should remain protocol-relative. Got: {rewritten}",
        );
    }

    #[test]
    fn rewrites_bare_host_occurrences() {
        let input = r#"{"siteProductionDomain":"origin.example.com"}"#;
        let rewriter = RscUrlRewriter::new("origin.example.com", "proxy.example.com", "https");
        let rewritten = rewriter.rewrite_to_string(input);

        assert!(
            rewritten.contains(r#""siteProductionDomain":"proxy.example.com""#),
            "Bare host should be rewritten inside RSC payload. Got: {rewritten}"
        );
    }

    #[test]
    fn single_payload_bypasses_combining() {
        // When there's only one payload, we should process it directly without combining
        // Content: {"url":"https://origin.example.com/x"} = 37 bytes = 0x25 hex
        let payload = r#"1a:T25,{"url":"https://origin.example.com/x"}"#;
        let payloads: Vec<&str> = vec![payload];

        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        assert_eq!(results.len(), 1);
        assert!(
            results[0].contains("test.example.com"),
            "Single payload should be rewritten. Got: {}",
            results[0]
        );
        // The length should be updated for the rewritten URL
        // {"url":"https://test.example.com/x"} = 35 bytes = 0x23 hex
        assert!(
            results[0].contains(":T23,"),
            "T-chunk length should be updated. Got: {}",
            results[0]
        );
    }

    #[test]
    fn empty_payloads_returns_empty() {
        let payloads: Vec<&str> = vec![];
        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );
        assert!(results.is_empty());
    }

    #[test]
    fn no_origin_in_payloads_returns_unchanged() {
        let payloads: Vec<&str> = vec![r#"1a:T10,{"key":"value"}"#, r#"1b:T10,{"foo":"bar"}"#];

        let results = rewrite_rsc_scripts_combined(
            &payloads,
            "origin.example.com",
            "test.example.com",
            "https",
        );

        assert_eq!(results.len(), 2);
        // Content should be identical - note that T-chunk lengths may be recalculated
        // even if content is unchanged (due to how the algorithm works)
        assert!(
            !results[0].contains("origin.example.com") && !results[0].contains("test.example.com"),
            "No host should be present in payload without URLs"
        );
        assert!(
            !results[1].contains("origin.example.com") && !results[1].contains("test.example.com"),
            "No host should be present in payload without URLs"
        );
        // The content after T-chunk header should be preserved
        assert!(
            results[0].contains(r#"{"key":"value"}"#),
            "Content should be preserved. Got: {}",
            results[0]
        );
        assert!(
            results[1].contains(r#"{"foo":"bar"}"#),
            "Content should be preserved. Got: {}",
            results[1]
        );
    }
}
