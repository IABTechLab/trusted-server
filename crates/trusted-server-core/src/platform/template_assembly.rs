//! Platform boundary for assembling a shared template with reader-specific state.
//!
//! Core owns the cache-safety ordering and the portable byte-seam fallback. An adapter
//! may provide a richer assembler for the cold response after the reader-neutral
//! template has been stored.

use core::fmt;

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
}
