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

use esi::{CacheConfig, Configuration, DcaMode, PendingFragmentContent, Processor};
use fastly::Response;
use fastly::http::StatusCode;
use std::io::Cursor;
use trusted_server_core::platform::{PlatformTemplateAssembler, TemplateAssemblyError};

/// The processor configuration, with every safety-relevant field stated.
///
/// Not `Configuration::default()`. Two of these settings fail **open**, and this is a
/// pre-1.0 crate whose defaults can move in a patch release — a comment saying "the
/// default is already what we want" would be an assumption rechecked by nobody.
///
/// The two that matter:
///
/// - **`is_includes_cacheable` defaults to `true`.** Fragments here carry one visitor's
///   bids. Letting the ESI layer cache them is precisely the per-user leak this whole
///   design exists to prevent, and it would happen silently on a cache hit.
/// - **`default_dca` / `inherit_parent_dca`** decide whether fragment bytes are
///   re-parsed as ESI. Our fragment is a `<script>` built from auction data; treating
///   it as ESI would let bid content be interpreted as markup instructions.
///
/// `is_rendered_cacheable` and `rendered_cache_control` are also pinned off. The
/// publisher path sets `private, no-store` itself, before any body byte is written, and
/// a `Cache-Control` computed from include TTLs would contradict it.
fn assembly_configuration() -> Configuration {
    Configuration::default()
        // Fragments are markup already; escaping would render them as visible text.
        .with_escaped(false)
        // Fragment bytes are data, not instructions.
        .with_default_dca(DcaMode::None)
        .with_inherit_parent_dca(false)
        // One include, no nesting. A template asking for more is not one this arm built.
        .with_max_include_depth(1)
        // `edge_control` lets the document influence downstream cache behaviour; the
        // publisher path owns those headers.
        .with_edge_control(false)
        .with_caching(CacheConfig {
            // The one that fails open. See above.
            is_includes_cacheable: false,
            includes_default_ttl: None,
            // Would cache *everything*, ignoring `private`, `no-store` and `Set-Cookie`.
            includes_force_ttl: None,
            is_rendered_cacheable: false,
            rendered_cache_control: false,
            rendered_ttl: None,
        })
}

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
    let mut processor = Processor::new(None, assembly_configuration());

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

/// Fastly's implementation of the core assembler seam.
pub struct FastlyTemplateAssembler;

impl PlatformTemplateAssembler for FastlyTemplateAssembler {
    fn assemble(&self, template: &str, fragment: &str) -> Result<String, TemplateAssemblyError> {
        assemble(template, fragment).map_err(|e| TemplateAssemblyError::Failed {
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written include, no longer the seam's marker.
    ///
    /// It used to be `trusted_server_core::publisher::ESI_BIDS_INCLUDE`, on the
    /// reasoning that a test writing its own marker would keep passing after the
    /// seam's shape changed. That reasoning expired with the seam: the marker is now
    /// an inert HTML comment (`SEAM_BIDS_MARKER`), which this crate cannot resolve and
    /// is not meant to. What is left under test here is the `esi` crate's own
    /// behaviour over an ESI document — including
    /// [`the_crate_truncates_a_script_larger_than_its_chunk_size`], the defect that
    /// took the crate out of the render path in the first place.
    const ESI_BIDS_INCLUDE: &str = "<esi:include src=\"/_ts/page-bids?format=fragment\"/>";

    const FRAGMENT: &str = "<script>window.tsjs={bids:{}};</script>";

    fn template_with_include() -> String {
        format!("<html><body><div id=\"slot\"></div>{ESI_BIDS_INCLUDE}</body></html>")
    }

    #[test]
    fn the_seams_own_marker_is_resolved() {
        // The processor must resolve a well-formed include. This is the crate's
        // contract, not the seam's — the seam no longer emits ESI at all.
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
    fn fragment_caching_is_off_because_its_default_leaks() {
        // `is_includes_cacheable` defaults to `true`. A fragment here carries one
        // visitor's bids, so caching it serves those bids to the next visitor. This is
        // the single most consequential line in the module and the default is wrong,
        // which is why it is asserted rather than trusted.
        let config = assembly_configuration();

        assert!(
            !config.cache.is_includes_cacheable,
            "per-user fragments must never be cached by the ESI layer"
        );
        assert!(
            config.cache.includes_force_ttl.is_none(),
            "force_ttl caches everything, ignoring private/no-store/Set-Cookie"
        );
    }

    #[test]
    fn fragment_bytes_are_never_reparsed_as_esi() {
        // The fragment is a script built from auction data. Re-parsing it as ESI would
        // let bid content act as markup instructions.
        let config = assembly_configuration();

        assert_eq!(config.default_dca, DcaMode::None);
        assert!(!config.inherit_parent_dca);
    }

    #[test]
    fn the_processor_emits_no_cache_headers_of_its_own() {
        // The publisher path sets `private, no-store` before any body byte is written,
        // and on this adapter headers cannot change once streaming starts. A
        // Cache-Control derived from include TTLs would contradict it, and the
        // contradiction would favour caching.
        let config = assembly_configuration();

        assert!(!config.cache.is_rendered_cacheable);
        assert!(!config.cache.rendered_cache_control);
        assert!(!config.enable_edge_control);
    }

    #[test]
    fn a_nested_include_inside_a_fragment_is_not_followed() {
        // Depth is capped at 1, and the fragment is not parsed as ESI, so a fragment
        // that happened to contain an include tag must be spliced as text rather than
        // dispatched. Otherwise auction data could drive fragment requests.
        let fragment = "<script>/*</script><esi:include src=\"/evil\"/>";
        let assembled = assemble(&template_with_include(), fragment).expect("should assemble");

        assert!(
            assembled.contains("/evil"),
            "the inner tag must survive as text rather than being resolved: {assembled}"
        );
    }

    #[test]
    fn react_suspense_markers_and_inline_scripts_survive() {
        // The document this runs over is the publisher's entire page, not an ESI
        // template. React marks Suspense boundaries with HTML comments — `<!--$-->`,
        // `<!--/$-->`, `<!--$?-->`, `<!--$!-->` — and hydration fails if they are altered
        // or dropped. Next.js also embeds inline scripts full of escaped JSON.
        //
        // Nothing previously checked that assembly is byte-faithful to any of it. Every
        // test used a five-line fixture.
        let document = format!(
            "<!doctype html><html><head>\
             <script>self.__next_s.push([0,{{\"children\":\"a\\u003eb &amp; c\"}}]);</script>\
             </head><body>\
             <div hidden=\"\"><!--$--><!--/$--></div>\
             <div hidden=\"\"><!--$?--><!--$!--></div>\
             <p>copy</p>{ESI_BIDS_INCLUDE}</body></html>"
        );

        let assembled = assemble(&document, FRAGMENT).expect("should assemble");

        for marker in ["<!--$-->", "<!--/$-->", "<!--$?-->", "<!--$!-->"] {
            assert!(
                assembled.contains(marker),
                "React Suspense marker {marker} must survive assembly, or hydration \
                 fails: {assembled}"
            );
        }
        assert!(
            assembled.contains("a\\u003eb &amp; c"),
            "inline script content must be byte-faithful: {assembled}"
        );
        // Everything except the marker substitution must be untouched.
        assert_eq!(
            assembled,
            document.replace(ESI_BIDS_INCLUDE, FRAGMENT),
            "assembly must change nothing but the seam"
        );
    }

    #[test]
    fn a_large_realistic_document_survives_byte_identically() {
        // The real document is ~690KB of publisher HTML. Every other test here uses a
        // handful of lines, and `Configuration` has a `chunk_size` — so a parser that is
        // faithful on a small input can still corrupt one that spans many chunks.
        //
        // Observed on a real deployment: the cache-miss path, which runs this parser over
        // the whole document, produced a broken page, while the cache-hit path, which
        // does a plain byte split, produced a working one. This is that difference under
        // test.
        let mut document = String::from("<!doctype html><html><head><title>t</title></head><body>");
        for i in 0..6000 {
            document.push_str(&format!(
                "<div class=\"c{i}\" data-x=\"a&amp;b\"><!--$--><p>copy {i} &lt;tag&gt;</p>\
                 <!--/$--></div><script>self.__next_f.push([1,\"{i}:a\\u003eb\"]);</script>"
            ));
        }
        document.push_str(&format!("{ESI_BIDS_INCLUDE}</body></html>"));
        assert!(
            document.len() > 600_000,
            "fixture must be realistically large, got {}",
            document.len()
        );

        let assembled = assemble(&document, FRAGMENT).expect("should assemble");

        assert_eq!(
            assembled.len(),
            document.len() - ESI_BIDS_INCLUDE.len() + FRAGMENT.len(),
            "assembled length must differ from the source by exactly the seam swap"
        );
        assert_eq!(
            assembled,
            document.replace(ESI_BIDS_INCLUDE, FRAGMENT),
            "a large document must survive byte-identically apart from the seam"
        );
    }

    #[test]
    fn a_document_larger_than_the_real_page_is_not_truncated() {
        // The real page is ~1.4MB decoded. A deployment served 691704 bytes of it and
        // the browser showed an error boundary — the document was cut roughly in half.
        // The cache-hit path, which does a plain byte split, served the same page
        // correctly, so the loss is in this parser and only shows up above some size the
        // 600KB test does not reach.
        let mut document = String::from("<!doctype html><html><head><title>t</title></head><body>");
        for i in 0..14000 {
            document.push_str(&format!(
                "<div class=\"c{i}\" data-x=\"a&amp;b\"><!--$--><p>copy {i} &lt;tag&gt;</p>\
                 <!--/$--></div><script>self.__next_f.push([1,\"{i}:a\\u003eb\"]);</script>"
            ));
        }
        document.push_str(&format!("{ESI_BIDS_INCLUDE}</body></html>"));
        assert!(
            document.len() > 1_400_000,
            "fixture must exceed the real page size, got {}",
            document.len()
        );

        let assembled = assemble(&document, FRAGMENT).expect("should assemble");
        let expected = document.replace(ESI_BIDS_INCLUDE, FRAGMENT);

        assert_eq!(
            assembled.len(),
            expected.len(),
            "assembly dropped {} bytes of a {}-byte document",
            expected.len() as i64 - assembled.len() as i64,
            document.len()
        );
        assert!(
            assembled.ends_with("</body></html>"),
            "the document must not be cut short; it ends with: {:?}",
            &assembled[assembled.len().saturating_sub(80)..]
        );
    }

    #[test]
    fn dollar_signs_in_the_document_are_not_treated_as_esi_variables() {
        // ESI interpolates `$(VAR)`, and a React Server Components payload is full of
        // dollar signs: `[\"$\",\"$L1b\",null,…]` is how RSC encodes element references.
        //
        // A real page lost ~750KB through this parser while the byte-split path served it
        // intact, and the two diverged exactly at `self.__next_f.push([1,"15:[\"$\",…`.
        // Every fixture here until now was dollar-free.
        let payload = r#"<script>self.__next_f.push([1,"15:["$","$L1b",null,{"a":1}]"])</script>"#;
        let document = format!(
            "<!doctype html><html><body><p>before</p>{payload}<p>after</p>{ESI_BIDS_INCLUDE}</body></html>"
        );

        let assembled = assemble(&document, FRAGMENT).expect("should assemble");

        assert!(
            assembled.contains("after"),
            "content after a dollar sign must survive: {assembled}"
        );
        assert_eq!(
            assembled,
            document.replace(ESI_BIDS_INCLUDE, FRAGMENT),
            "a document containing `$` must survive byte-identically apart from the seam"
        );
    }

    #[test]
    fn the_crate_truncates_a_script_larger_than_its_chunk_size() {
        // Asserts the *defect*, deliberately. This is why the render path no longer uses
        // this crate: `esi` 0.7.1 empties its buffer before parsing and never restores
        // those bytes when the parser returns `Incomplete`, so any element larger than
        // its 16KB `chunk_size` loses content. Next.js streams its RSC payload as a few
        // enormous `self.__next_f.push(...)` scripts, so a real 1.4MB page came back at
        // 697KB and the browser showed an error boundary.
        //
        // Total size is not the trigger — a 1.4MB document of small scripts is fine, and
        // that is why every fixture here passed for weeks. The size of one element is.
        // Raising `chunk_size` moves the threshold rather than removing it.
        //
        // If this ever starts failing, the crate has been fixed and edge assembly could
        // be reconsidered on its merits rather than ruled out on this one.
        let payload = "x".repeat(120_000);
        let document = format!(
            "<!doctype html><html><body><p>before</p>\
             <script>self.__next_f.push([1,\"{payload}\"])</script>\
             <p id=\"after-the-big-script\">after</p>{ESI_BIDS_INCLUDE}</body></html>"
        );

        let assembled = assemble(&document, FRAGMENT).expect("should assemble");
        let expected = document.replace(ESI_BIDS_INCLUDE, FRAGMENT);

        assert!(
            assembled.len() < expected.len(),
            "the crate is expected to drop bytes here; if it no longer does, this test \
             and the decision it justifies both need revisiting"
        );
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
