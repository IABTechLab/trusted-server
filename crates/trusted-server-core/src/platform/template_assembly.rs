//! Platform boundary for assembling a shared template with reader-specific state.
//!
//! Core owns the cache-safety ordering and the portable byte-seam fallback. An adapter
//! may provide a richer assembler for the cold response after the reader-neutral
//! template has been stored.

use core::fmt;

/// Whether publisher bytes contain an ESI directive form understood by the parser.
///
/// Both ordinary `<esi:...>` elements and `<!--esi ...-->` comment blocks are active
/// parser input. The conservative byte scan also rejects these sequences inside scripts:
/// bypassing shared processing is safer than treating publisher data as edge instructions.
#[must_use]
pub fn contains_publisher_esi_directive(bytes: &[u8]) -> bool {
    [b"<esi:".as_slice(), b"<!--esi".as_slice()]
        .into_iter()
        .any(|prefix| {
            bytes
                .windows(prefix.len())
                .any(|window| window.eq_ignore_ascii_case(prefix))
        })
}

/// Why a platform assembler could not produce a document.
#[derive(Debug, derive_more::Display)]
pub enum TemplateAssemblyError {
    /// The adapter has no template assembler.
    #[display("this adapter cannot assemble shared templates")]
    Unsupported,
    /// The platform assembler rejected or could not process the document.
    #[display("template assembly failed: {message}")]
    Failed {
        /// Human-readable failure context.
        message: String,
    },
}

impl core::error::Error for TemplateAssemblyError {}

/// Assembles reader-specific state into a shared HTML template.
pub trait PlatformTemplateAssembler: Send + Sync {
    /// Produce the complete document served to this reader.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateAssemblyError`] when the adapter cannot assemble the template.
    fn assemble(&self, template: &[u8], fragment: &[u8]) -> Result<Vec<u8>, TemplateAssemblyError>;
}

impl fmt::Debug for dyn PlatformTemplateAssembler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformTemplateAssembler")
    }
}

/// Default assembler used by adapters that do not provide platform assembly.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableTemplateAssembler;

impl PlatformTemplateAssembler for UnavailableTemplateAssembler {
    fn assemble(
        &self,
        _template: &[u8],
        _fragment: &[u8],
    ) -> Result<Vec<u8>, TemplateAssemblyError> {
        Err(TemplateAssemblyError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_assembler_refuses_the_document() {
        let error = UnavailableTemplateAssembler
            .assemble(b"<html><!--ts-ad-seam--></html>", b"<script></script>")
            .expect_err("should refuse when platform assembly is unavailable");

        assert!(matches!(error, TemplateAssemblyError::Unsupported));
    }

    #[test]
    fn assembler_contract_is_object_safe() {
        let assembler: Box<dyn PlatformTemplateAssembler> = Box::new(UnavailableTemplateAssembler);

        assert!(matches!(
            assembler.assemble(b"template", b"fragment"),
            Err(TemplateAssemblyError::Unsupported)
        ));
    }

    #[test]
    fn publisher_esi_detection_covers_elements_and_comment_blocks() {
        for directive in [
            b"<esi:include src=\"/fragment\"/>".as_slice(),
            b"<ESI:REMOVE>secret</ESI:REMOVE>".as_slice(),
            b"<!--esi anything-->".as_slice(),
            b"<!--ESI anything-->".as_slice(),
        ] {
            assert!(
                contains_publisher_esi_directive(directive),
                "should detect publisher ESI bytes: {directive:?}"
            );
        }
        assert!(
            !contains_publisher_esi_directive(b"<!--ts-ad-seam-->"),
            "should not classify the inert TS seam as publisher ESI"
        );
    }
}
