//! Edge assembly: splice the per-user fragment into the shared template (arm A3).
//!
//! This is the step that distinguishes A3 from A2. Both serve the same C2 template;
//! A2 has the browser fetch the fragment, A3 resolves it here, before the bytes leave
//! the edge. Whether that difference is worth a Fastly-only rendering path is the
//! question the spike exists to answer.
//!
//! **The fragment is resolved before this runs, not by it.** `esi`'s dispatcher is
//! synchronous, and this codebase's fragment producer is `async`; calling it from
//! inside the dispatcher would mean a nested executor, which panics. The way out is
//! [`PendingFragmentContent::CompletedRequest`], which lets the dispatcher hand back a
//! response that was already built. So the caller runs the auction in the normal async
//! flow and passes the bytes in, and the dispatcher never performs I/O at all — no
//! subrequest, no backend, no self-call.
//!
//! Spike-only. Remove with the spike.

// Not yet reachable from the request path: resolving the fragment means running the
// auction, which is the next step (the spike plan's Task 5). The tests below do
// exercise it, so `expect` is wrong here — it would be unfulfilled under `cfg(test)`
// and satisfied in the binary, and no single attribute can be both.
#![allow(dead_code)]

use esi::{Configuration, PendingFragmentContent, Processor};
use fastly::Response;
use fastly::http::StatusCode;
use std::io::Cursor;

/// Why assembly could not produce a document.
#[derive(Debug, derive_more::Display)]
pub enum EsiAssemblyError {
    /// The template is not valid ESI, or processing failed part-way.
    #[display("ESI processing failed: {message}")]
    Processing { message: String },
    /// The assembled bytes are not UTF-8. Only reachable if the template was not.
    #[display("assembled output is not valid UTF-8")]
    NotUtf8,
}

impl core::error::Error for EsiAssemblyError {}

/// Splices `fragment` into `template` wherever an `esi:include` appears.
///
/// Every include resolves to the same `fragment`. That is not a simplification — the
/// template carries exactly one marker, emitted as a constant by the `</body>` seam,
/// and it is constant precisely because it lives in bytes shared between visitors.
/// Resolving every include identically therefore matches what the seam can produce,
/// and a template containing some *other* include is a template this arm did not build.
///
/// # Errors
///
/// Returns [`EsiAssemblyError::Processing`] if the template is not valid ESI, and
/// [`EsiAssemblyError::NotUtf8`] if the result is not UTF-8.
///
/// # Performance
///
/// Buffers the whole document. The template is already materialized by the time it
/// reaches here — C2 stores complete bytes — so this adds a copy rather than a new
/// buffering point.
pub fn assemble(template: &str, fragment: &str) -> Result<String, EsiAssemblyError> {
    let mut processor = Processor::new(None, Configuration::default());

    // Synthetic, not fetched. See the module docs: this is what keeps a synchronous
    // dispatcher compatible with an asynchronous fragment producer.
    let fragment = fragment.to_string();
    let dispatcher = move |_request, _index| {
        Ok(PendingFragmentContent::CompletedRequest(Box::new(
            Response::from_status(StatusCode::OK)
                .with_header(
                    fastly::http::header::CONTENT_TYPE,
                    "text/html; charset=utf-8",
                )
                .with_body(fragment.clone()),
        )))
    };

    let mut output = Vec::new();
    processor
        .process_stream(
            Cursor::new(template.as_bytes()),
            &mut output,
            Some(&dispatcher),
            None,
        )
        .map_err(|e| EsiAssemblyError::Processing {
            message: format!("{e:?}"),
        })?;

    String::from_utf8(output).map_err(|_| EsiAssemblyError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::publisher::ESI_BIDS_INCLUDE;

    const FRAGMENT: &str = "<script>window.tsjs={bids:{}};</script>";

    fn template_with_include() -> String {
        format!("<html><body><div id=\"slot\"></div>{ESI_BIDS_INCLUDE}</body></html>")
    }

    #[test]
    fn the_seams_own_marker_is_resolved() {
        // Deliberately built from `ESI_BIDS_INCLUDE` rather than a hand-written
        // include. The two live in different crates, and a test that wrote its own
        // marker would keep passing after the seam's changed shape stopped parsing.
        let assembled = assemble(&template_with_include(), FRAGMENT).expect("should assemble");

        assert!(
            assembled.contains(FRAGMENT),
            "the fragment must reach the document: {assembled}"
        );
        assert!(
            !assembled.contains("esi:include"),
            "no unresolved include may survive: {assembled}"
        );
    }

    #[test]
    fn the_fragment_lands_where_the_marker_was() {
        // Position matters: the script reads slots defined earlier in the document, so
        // an assembler that appended instead of substituting would produce a page that
        // parses and does nothing.
        let assembled = assemble(&template_with_include(), FRAGMENT).expect("should assemble");

        let slot = assembled.find("id=\"slot\"").expect("slot should survive");
        let script = assembled
            .find(FRAGMENT)
            .expect("fragment should be present");
        let body_close = assembled
            .find("</body>")
            .expect("body close should survive");

        assert!(slot < script, "the fragment must follow the slot markup");
        assert!(script < body_close, "the fragment must precede `</body>`");
    }

    #[test]
    fn a_document_without_an_include_is_returned_unchanged() {
        // Inline mode's documents pass through this path only if something is
        // misrouted, and a mangled document would be a far worse failure than a no-op.
        let plain = "<html><body><p>no includes here</p></body></html>";

        assert_eq!(
            assemble(plain, FRAGMENT).expect("should assemble"),
            plain,
            "a template with nothing to splice must be byte-identical"
        );
    }

    #[test]
    fn an_empty_fragment_still_removes_the_marker() {
        // The empty-bids case is normal, not exceptional: an auction that returned
        // nothing still has to produce a document with no `esi:include` left in it, or
        // the browser renders the raw tag as text.
        let assembled = assemble(&template_with_include(), "").expect("should assemble");

        assert!(
            !assembled.contains("esi:include"),
            "an empty fragment must still consume the marker: {assembled}"
        );
        assert!(assembled.contains("</body>"), "the document must survive");
    }

    #[test]
    fn script_bearing_fragments_are_spliced_verbatim() {
        // ESI substitutes bytes without escaping, which is exactly why the fragment
        // endpoint must return markup rather than JSON. This pins that behaviour, since
        // an `esi` release that started escaping would silently turn every fragment
        // into visible text.
        let fragment = "<script>var x=\"</not-really>\";</script>";
        let assembled = assemble(&template_with_include(), fragment).expect("should assemble");

        assert!(
            assembled.contains(fragment),
            "the fragment must be spliced verbatim: {assembled}"
        );
    }
}
