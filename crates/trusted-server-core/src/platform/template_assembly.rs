//! Edge assembly: turn a shared template plus a per-user fragment into a document.
//!
//! Kept behind a trait for the same reason as the template cache: the only
//! implementation that exists uses a Fastly-only crate, and core must stay portable.
//! Adapters without one get [`UnavailableTemplateAssembler`], which refuses rather than
//! guessing.
//!
//! **Ordering this module exists to protect.** The template is stored *before* assembly
//! and assembled *after* — never the reverse. Storing post-assembly would put one
//! visitor's bids in a cache shared with the next, which is the C3 the design forbids.
//! Splitting store from assemble into two call sites is what makes that ordering
//! visible instead of implicit.
//!
//! Spike-only, for the #1009 ESI validation.

use core::fmt;

/// Why assembly could not produce a document.
#[derive(Debug, derive_more::Display)]
pub enum TemplateAssemblyError {
    /// The adapter has no assembler.
    ///
    /// Not a failure to be papered over: reaching here means a shared-template mode is
    /// configured on an adapter that cannot serve one, and the honest response is an
    /// error rather than a page with an unresolved marker in it.
    #[display("this adapter cannot assemble shared templates")]
    Unsupported,
    /// The assembler ran and failed.
    #[display("template assembly failed: {message}")]
    Failed {
        /// What the underlying assembler reported.
        message: String,
    },
}

impl core::error::Error for TemplateAssemblyError {}

/// Splices a per-user fragment into a shared template.
pub trait PlatformTemplateAssembler: Send + Sync {
    /// Produce the document served to this visitor.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateAssemblyError::Unsupported`] when the adapter has no
    /// assembler, or [`TemplateAssemblyError::Failed`] when the template could not be
    /// processed.
    fn assemble(&self, template: &str, fragment: &str) -> Result<String, TemplateAssemblyError>;
}

impl fmt::Debug for dyn PlatformTemplateAssembler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformTemplateAssembler")
    }
}

/// The default: no assembler.
///
/// Refuses rather than returning the template unchanged. Returning it unchanged would
/// serve a page whose ad markup is a literal `esi:include` — a page that looks like it
/// worked, renders no ads, and reports no error.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableTemplateAssembler;

impl PlatformTemplateAssembler for UnavailableTemplateAssembler {
    fn assemble(&self, _template: &str, _fragment: &str) -> Result<String, TemplateAssemblyError> {
        Err(TemplateAssemblyError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_assembler_refuses_rather_than_passing_the_template_through() {
        // Passing it through is the tempting default and the wrong one: the visitor
        // gets a page with a raw `esi:include` in it, no ads, and no error anywhere.
        let error = UnavailableTemplateAssembler
            .assemble(
                "<html><esi:include src=\"/x\"/></html>",
                "<script></script>",
            )
            .expect_err("an adapter with no assembler must refuse");

        assert!(matches!(error, TemplateAssemblyError::Unsupported));
    }
}
