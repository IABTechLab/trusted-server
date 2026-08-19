//! Fastly cold-response assembly backed by the repaired `stackpop/esi` parser.
//!
//! The shared template cache stores an inert marker. This module creates one synthetic
//! ESI include only in a request-private working copy, resolves it from an already-built
//! fragment, and never performs an HTTP request.

use std::io::Cursor;

use esi::{CacheConfig, Configuration, DcaMode, PendingFragmentContent, Processor};
use fastly::http::StatusCode;
use fastly::{Request, Response};
use trusted_server_core::platform::{
    PlatformTemplateAssembler, TemplateAssemblyError, contains_publisher_esi_directive,
};
use trusted_server_core::publisher::AD_ASSEMBLY_SEAM;

const INTERNAL_FRAGMENT_PATH: &str = "/_ts/internal/reader-ad-state";
const SYNTHETIC_ESI_INCLUDE: &[u8] = b"<esi:include src=\"/_ts/internal/reader-ad-state\"/>";

/// Why the Fastly ESI adapter refused or failed to assemble a document.
#[derive(Debug, derive_more::Display)]
enum EsiAssemblyError {
    /// The inert seam marker was missing or repeated.
    #[display("expected exactly one inert seam marker, found {count}")]
    InvalidMarkerCount { count: usize },
    /// Publisher bytes contained ESI instructions outside TS's synthetic seam.
    #[display("publisher-authored ESI directives are not allowed")]
    PublisherEsiDirective,
    /// The parser dispatched a URL other than TS's one synthetic fragment.
    #[display("unexpected fragment request path `{path}` (query present: {has_query})")]
    UnexpectedFragmentRequest { path: String, has_query: bool },
    /// The pinned parser could not process the document.
    #[display("ESI processing failed: {message}")]
    Processing { message: String },
    /// The parser changed bytes outside the one synthetic include.
    #[display("ESI output was not an exact seam substitution")]
    OutputMismatch,
}

impl core::error::Error for EsiAssemblyError {}

/// ESI configuration with every cache- and recursion-sensitive option explicit.
fn assembly_configuration() -> Configuration {
    Configuration::default()
        .with_escaped(false)
        .with_default_dca(DcaMode::None)
        .with_inherit_parent_dca(false)
        .with_max_include_depth(1)
        .with_edge_control(false)
        .with_caching(CacheConfig {
            is_includes_cacheable: false,
            includes_default_ttl: None,
            includes_force_ttl: None,
            is_rendered_cacheable: false,
            rendered_cache_control: false,
            rendered_ttl: None,
        })
}

fn template_with_synthetic_include(template: &[u8]) -> Result<(Vec<u8>, usize), EsiAssemblyError> {
    let marker = AD_ASSEMBLY_SEAM.as_bytes();
    let positions = template
        .windows(marker.len())
        .enumerate()
        .filter_map(|(at, window)| (window == marker).then_some(at))
        .collect::<Vec<_>>();
    if positions.len() != 1 {
        return Err(EsiAssemblyError::InvalidMarkerCount {
            count: positions.len(),
        });
    }
    if contains_publisher_esi_directive(template) {
        return Err(EsiAssemblyError::PublisherEsiDirective);
    }

    let at = positions[0];
    let mut working =
        Vec::with_capacity(template.len() - marker.len() + SYNTHETIC_ESI_INCLUDE.len());
    working.extend_from_slice(&template[..at]);
    working.extend_from_slice(SYNTHETIC_ESI_INCLUDE);
    working.extend_from_slice(&template[at + marker.len()..]);
    Ok((working, at))
}

fn completed_fragment_response(
    request: &Request,
    fragment: &[u8],
) -> Result<PendingFragmentContent, EsiAssemblyError> {
    let path = request.get_path().to_string();
    let has_query = request.get_url().query().is_some();
    if path != INTERNAL_FRAGMENT_PATH || has_query {
        return Err(EsiAssemblyError::UnexpectedFragmentRequest { path, has_query });
    }

    Ok(PendingFragmentContent::CompletedRequest(Box::new(
        Response::from_status(StatusCode::OK)
            .with_header(
                fastly::http::header::CONTENT_TYPE,
                "text/html; charset=utf-8",
            )
            .with_body(fragment.to_vec()),
    )))
}

fn assemble_with_observer<F>(
    template: &[u8],
    fragment: &[u8],
    on_dispatch: F,
) -> Result<Vec<u8>, EsiAssemblyError>
where
    F: Fn() + 'static,
{
    let (working, seam_at) = template_with_synthetic_include(template)?;
    let fragment_len = fragment.len();
    let fragment_response = fragment.to_vec();
    let dispatcher = move |request, _index| {
        on_dispatch();
        completed_fragment_response(&request, &fragment_response)
            .map_err(|error| esi::ESIError::FragmentRequestError(error.to_string()))
    };
    let mut processor = Processor::new(None, assembly_configuration());
    let mut output = Vec::with_capacity(template.len() + fragment_len);
    processor
        .process_stream(Cursor::new(working), &mut output, Some(&dispatcher), None)
        .map_err(|error| EsiAssemblyError::Processing {
            message: error.to_string(),
        })?;
    let expected_len = template.len() - AD_ASSEMBLY_SEAM.len() + fragment_len;
    let output_tail_at = seam_at + fragment_len;
    let template_tail_at = seam_at + AD_ASSEMBLY_SEAM.len();
    if output.len() != expected_len
        || output[..seam_at] != template[..seam_at]
        || &output[seam_at..output_tail_at] != fragment
        || output[output_tail_at..] != template[template_tail_at..]
    {
        return Err(EsiAssemblyError::OutputMismatch);
    }
    Ok(output)
}

fn assemble(template: &[u8], fragment: &[u8]) -> Result<Vec<u8>, EsiAssemblyError> {
    assemble_with_observer(template, fragment, || {})
}

/// Fastly implementation of the core cold-response assembly boundary.
pub struct FastlyTemplateAssembler;

impl PlatformTemplateAssembler for FastlyTemplateAssembler {
    fn assemble(&self, template: &[u8], fragment: &[u8]) -> Result<Vec<u8>, TemplateAssemblyError> {
        assemble(template, fragment).map_err(|error| TemplateAssemblyError::Failed {
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use trusted_server_core::publisher::AD_ASSEMBLY_SEAM;

    const FRAGMENT: &[u8] = b"<script>window.tsjs={adSlots:[{id:'slot'}],bids:{}};</script>";

    fn template(body: &str) -> Vec<u8> {
        format!("<html><head></head><body>{body}{AD_ASSEMBLY_SEAM}</body></html>").into_bytes()
    }

    #[test]
    fn a_script_larger_than_the_parser_chunk_survives_exactly() {
        let script = format!(
            "<script>self.__next_f.push('{}');</script>",
            "x".repeat(120_000)
        );
        let document = template(&script);
        let dispatches = Arc::new(AtomicUsize::new(0));
        let observed_dispatches = Arc::clone(&dispatches);

        let assembled = assemble_with_observer(&document, FRAGMENT, move || {
            observed_dispatches.fetch_add(1, Ordering::Relaxed);
        })
        .expect("should assemble a document with a large script");

        let seam_at = document
            .windows(AD_ASSEMBLY_SEAM.len())
            .position(|window| window == AD_ASSEMBLY_SEAM.as_bytes())
            .expect("should find seam");
        let mut expected = Vec::new();
        expected.extend_from_slice(&document[..seam_at]);
        expected.extend_from_slice(FRAGMENT);
        expected.extend_from_slice(&document[seam_at + AD_ASSEMBLY_SEAM.len()..]);

        assert_eq!(
            assembled, expected,
            "ESI must alter only the synthetic seam"
        );
        assert_eq!(dispatches.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_and_repeated_markers_are_rejected_before_parsing() {
        let missing = assemble(b"<html><body>plain</body></html>", FRAGMENT)
            .expect_err("should reject a missing marker");
        let repeated = assemble(
            format!("{AD_ASSEMBLY_SEAM}{AD_ASSEMBLY_SEAM}").as_bytes(),
            FRAGMENT,
        )
        .expect_err("should reject repeated markers");

        assert!(matches!(
            missing,
            EsiAssemblyError::InvalidMarkerCount { count: 0 }
        ));
        assert!(matches!(
            repeated,
            EsiAssemblyError::InvalidMarkerCount { count: 2 }
        ));
    }

    #[test]
    fn every_publisher_esi_directive_form_is_rejected_case_insensitively() {
        for directive in [
            "<esi:include src=\"/publisher\"/>",
            "<ESI:remove>secret</ESI:remove>",
            "<esi:choose><esi:otherwise>x</esi:otherwise></esi:choose>",
            "<esi:vars>$(HTTP_HOST)</esi:vars>",
            "<esi:text>text</esi:text>",
            "<esi:eval src=\"/publisher\"/>",
            "<!--esi anything-->",
            "<!--ESI anything-->",
        ] {
            let error = assemble(&template(directive), FRAGMENT)
                .expect_err("should reject publisher-authored ESI");

            assert!(matches!(error, EsiAssemblyError::PublisherEsiDirective));
        }
    }

    #[test]
    fn fragment_esi_is_emitted_verbatim_and_never_reparsed() {
        let fragment = b"<script>/* data */</script><esi:include src=\"/never-dispatch\"/>";

        let assembled = assemble(&template("article"), fragment).expect("should assemble");

        assert!(
            assembled
                .windows(fragment.len())
                .any(|window| window == fragment),
            "fragment bytes must remain data"
        );
    }

    #[test]
    fn dispatcher_rejects_every_url_except_the_synthetic_internal_one() {
        let unexpected = fastly::Request::get("https://example.com/not-the-seam");
        let with_query =
            fastly::Request::get("https://example.com/_ts/internal/reader-ad-state?publisher=1");

        assert!(matches!(
            completed_fragment_response(&unexpected, FRAGMENT),
            Err(EsiAssemblyError::UnexpectedFragmentRequest { .. })
        ));
        assert!(matches!(
            completed_fragment_response(&with_query, FRAGMENT),
            Err(EsiAssemblyError::UnexpectedFragmentRequest { .. })
        ));
    }

    #[test]
    fn configuration_cannot_cache_or_reparse_reader_state() {
        let configuration = assembly_configuration();

        assert!(!configuration.cache.is_includes_cacheable);
        assert!(configuration.cache.includes_default_ttl.is_none());
        assert!(configuration.cache.includes_force_ttl.is_none());
        assert!(!configuration.cache.is_rendered_cacheable);
        assert!(!configuration.cache.rendered_cache_control);
        assert!(configuration.cache.rendered_ttl.is_none());
        assert_eq!(configuration.default_dca, DcaMode::None);
        assert!(!configuration.inherit_parent_dca);
        assert_eq!(configuration.max_include_depth, 1);
        assert!(!configuration.enable_edge_control);
    }
}
