//! Parser-safe SSAT bid placeholder late binding.
//!
//! The publisher HTML rewriter inserts an opaque placeholder only from a
//! parser-confirmed `</body>` handler. This module scans the rewritten,
//! uncompressed HTML stream for that placeholder and lets the caller replace it
//! with the final bids script without scanning raw origin bytes for HTML syntax.

use std::sync::atomic::{AtomicBool, Ordering};

use error_stack::Report;
use uuid::Uuid;

use crate::error::TrustedServerError;

/// Maximum processed-output suffix retained after the bid placeholder is found.
pub(crate) const SSAT_HELD_TAIL_CAP_BYTES: usize = 64 * 1024;

/// Per-request opaque marker inserted before a parser-confirmed `</body>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BidPlaceholder {
    html: String,
}

impl BidPlaceholder {
    /// Generate a new high-entropy HTML comment placeholder for one response.
    #[must_use]
    pub(crate) fn new() -> Self {
        let id = Uuid::new_v4();
        Self {
            html: format!("<!--__TSJS_BIDS_PLACEHOLDER_{id}__-->"),
        }
    }

    /// Return the placeholder as HTML.
    #[must_use]
    pub(crate) fn as_html(&self) -> &str {
        &self.html
    }

    /// Return the placeholder bytes used by the streaming scanner.
    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.html.as_bytes()
    }
}

/// Tracks which parser insertions actually happened for EOF fallback decisions.
#[derive(Debug, Default)]
pub struct HtmlInjectionTracker {
    head_injected: AtomicBool,
    bid_placeholder_inserted: AtomicBool,
}

impl HtmlInjectionTracker {
    /// Mark that the normal `<head>` bootstrap insertion ran.
    pub(crate) fn mark_head_injected(&self) {
        self.head_injected.store(true, Ordering::SeqCst);
    }

    /// Mark that the body end-tag placeholder insertion ran.
    pub(crate) fn mark_bid_placeholder_inserted(&self) {
        self.bid_placeholder_inserted.store(true, Ordering::SeqCst);
    }

    /// Return whether the normal `<head>` bootstrap insertion ran.
    #[must_use]
    pub(crate) fn head_injected(&self) -> bool {
        self.head_injected.load(Ordering::SeqCst)
    }

    /// Return whether the body end-tag placeholder insertion ran.
    #[must_use]
    pub(crate) fn bid_placeholder_inserted(&self) -> bool {
        self.bid_placeholder_inserted.load(Ordering::SeqCst)
    }
}

/// Result of pushing one processed-output chunk through the placeholder scanner.
#[derive(Debug)]
pub(crate) enum PlaceholderScan {
    /// No placeholder was found; emit these bytes immediately.
    Emit(Vec<u8>),
    /// The first placeholder was found. Emit `before`, collect/resolve bids,
    /// then emit the replacement followed by `after`.
    Found { before: Vec<u8>, after: Vec<u8> },
}

/// Streaming scanner for the bid placeholder.
pub(crate) struct PlaceholderLateBinder {
    placeholder: Vec<u8>,
    buffered: Vec<u8>,
    replaced: bool,
    held_tail_cap: usize,
}

impl PlaceholderLateBinder {
    /// Create a scanner for `placeholder` with a bounded post-placeholder hold.
    #[must_use]
    pub(crate) fn new(placeholder: &BidPlaceholder, held_tail_cap: usize) -> Self {
        Self {
            placeholder: placeholder.as_bytes().to_vec(),
            buffered: Vec::new(),
            replaced: false,
            held_tail_cap,
        }
    }

    /// Return true after the first placeholder has been found.
    #[must_use]
    pub(crate) fn replaced(&self) -> bool {
        self.replaced
    }

    /// Push processed uncompressed output through the placeholder scanner.
    ///
    /// # Errors
    ///
    /// Returns a proxy error if the suffix held after the first placeholder
    /// exceeds the configured held-tail cap.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<PlaceholderScan, Report<TrustedServerError>> {
        if self.replaced {
            let emit = self.push_after_replacement(chunk);
            return Ok(PlaceholderScan::Emit(emit));
        }

        self.buffered.extend_from_slice(chunk);
        if let Some(position) = find_bytes(&self.buffered, &self.placeholder) {
            self.replaced = true;
            let before = self.buffered.drain(..position).collect::<Vec<_>>();
            self.buffered.drain(..self.placeholder.len());
            let suffix = std::mem::take(&mut self.buffered);
            if suffix.len() > self.held_tail_cap {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!(
                        "SSAT placeholder held tail {} bytes exceeds {}-byte limit",
                        suffix.len(),
                        self.held_tail_cap
                    ),
                }));
            }
            let after = self.push_after_replacement(&suffix);
            return Ok(PlaceholderScan::Found { before, after });
        }

        Ok(PlaceholderScan::Emit(self.drain_safe_prefix()))
    }

    /// Finish scanning and return all remaining safe bytes.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.replaced {
            remove_all_placeholders(&mut self.buffered, &self.placeholder);
        }
        self.buffered
    }

    fn push_after_replacement(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffered.extend_from_slice(chunk);
        remove_all_placeholders(&mut self.buffered, &self.placeholder);
        self.drain_safe_prefix()
    }

    fn drain_safe_prefix(&mut self) -> Vec<u8> {
        let keep_len = partial_placeholder_prefix_len(&self.buffered, &self.placeholder);
        if self.buffered.len() <= keep_len {
            return Vec::new();
        }

        let split_at = self.buffered.len() - keep_len;
        self.buffered.drain(..split_at).collect()
    }
}

fn partial_placeholder_prefix_len(buffered: &[u8], placeholder: &[u8]) -> usize {
    let max_len = buffered.len().min(placeholder.len().saturating_sub(1));
    (1..=max_len)
        .rev()
        .find(|&len| buffered.ends_with(&placeholder[..len]))
        .unwrap_or(0)
}

fn remove_all_placeholders(buffered: &mut Vec<u8>, placeholder: &[u8]) {
    while let Some(position) = find_bytes(buffered, placeholder) {
        buffered.drain(position..position + placeholder.len());
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bid_placeholder_is_unique_html_comment() {
        let first = BidPlaceholder::new();
        let second = BidPlaceholder::new();

        assert_ne!(first, second, "per-request placeholders should be unique");
        assert!(
            first.as_html().starts_with("<!--__TSJS_BIDS_PLACEHOLDER_"),
            "placeholder should be an HTML comment"
        );
        assert!(
            first.as_html().ends_with("__-->"),
            "placeholder should close the HTML comment"
        );
    }

    #[test]
    fn late_binder_detects_placeholder_split_across_chunks() {
        let placeholder = BidPlaceholder {
            html: "<!--__TSJS_BIDS_PLACEHOLDER_test__-->".to_string(),
        };
        let marker = placeholder.as_html();
        let split = marker.len() / 2;
        let mut binder = PlaceholderLateBinder::new(&placeholder, SSAT_HELD_TAIL_CAP_BYTES);

        let first = binder
            .push(format!("before{}", &marker[..split]).as_bytes())
            .expect("should scan first chunk");
        let second = binder
            .push(format!("{}after", &marker[split..]).as_bytes())
            .expect("should scan second chunk");

        match first {
            PlaceholderScan::Emit(bytes) => assert_eq!(
                String::from_utf8(bytes).expect("should be utf8"),
                "before",
                "safe prefix before the split placeholder should stream"
            ),
            PlaceholderScan::Found { .. } => panic!("should not find split placeholder yet"),
        }
        let mut output = Vec::new();
        match second {
            PlaceholderScan::Found { before, after } => {
                assert!(before.is_empty(), "prefix was already emitted");
                output.extend(after);
            }
            PlaceholderScan::Emit(_) => panic!("should find placeholder in second chunk"),
        }
        output.extend(binder.finish());
        assert_eq!(
            String::from_utf8(output).expect("should be utf8"),
            "after",
            "suffix after placeholder should be returned by the scanner"
        );
    }

    #[test]
    fn late_binder_strips_later_placeholder_occurrences() {
        let placeholder = BidPlaceholder {
            html: "<!--__TSJS_BIDS_PLACEHOLDER_test__-->".to_string(),
        };
        let mut binder = PlaceholderLateBinder::new(&placeholder, SSAT_HELD_TAIL_CAP_BYTES);
        let first = format!("a{}b", placeholder.as_html());
        let second = format!("c{}d", placeholder.as_html());

        let first = binder
            .push(first.as_bytes())
            .expect("should scan first placeholder");
        let second = binder
            .push(second.as_bytes())
            .expect("should scan duplicate placeholder");
        let final_bytes = binder.finish();

        let mut output = Vec::new();
        match first {
            PlaceholderScan::Found { before, after } => {
                output.extend(before);
                output.extend(b"REPLACED");
                output.extend(after);
            }
            PlaceholderScan::Emit(_) => panic!("should find first placeholder"),
        }
        match second {
            PlaceholderScan::Emit(bytes) => output.extend(bytes),
            PlaceholderScan::Found { .. } => panic!("should not find second placeholder"),
        }
        output.extend(final_bytes);

        let output = String::from_utf8(output).expect("should be utf8");
        assert_eq!(output, "aREPLACEDbcd");
        assert!(
            !output.contains(placeholder.as_html()),
            "placeholder should not leak after replacement"
        );
    }

    #[test]
    fn late_binder_rejects_large_held_tail() {
        let placeholder = BidPlaceholder {
            html: "<!--__TSJS_BIDS_PLACEHOLDER_test__-->".to_string(),
        };
        let mut binder = PlaceholderLateBinder::new(&placeholder, 4);
        let input = format!("{}12345", placeholder.as_html());

        let err = binder
            .push(input.as_bytes())
            .expect_err("held tail should exceed cap");

        assert!(
            format!("{err:?}").contains("held tail"),
            "error should mention held tail cap"
        );
    }
}
