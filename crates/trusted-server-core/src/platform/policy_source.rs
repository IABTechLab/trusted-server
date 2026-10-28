//! Where a domain's permission policy file is read from.
//!
//! On most adapters the push writes the file into the domain's blob, so the
//! runtime loader takes it from there. An adapter that keeps policy files
//! somewhere of its own reads them through a [`PlatformPolicySource`].

use error_stack::Report;

use super::PlatformError;
use crate::permissions::PolicyReference;

/// Reads the permission policy file a domain's settings name.
pub trait PlatformPolicySource: Send + Sync {
    /// The text of the file `reference` names.
    ///
    /// The runtime loader checks the text against the digest the reference
    /// pins, so a source returns the file as it finds it.
    ///
    /// # Errors
    ///
    /// [`PlatformError::PolicySource`] when the file cannot be read.
    fn policy_text(&self, reference: &PolicyReference) -> Result<String, Report<PlatformError>>;
}

/// Where the runtime loader takes a domain's policy file from.
#[derive(Clone, Copy)]
pub enum PolicyOrigin<'a> {
    /// The domain's blob, as `[permissions] text`.
    Blob,
    /// A source of the adapter's own. Any text the blob carries is not used.
    Source(&'a dyn PlatformPolicySource),
}
