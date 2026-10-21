//! Sole-owner immutable release publication state machine.
//!
//! Publication remains closed. This module has no HTTP adapter, workflow,
//! token acquisition or command: it drives an abstract [`api::ReleaseApi`] so
//! every draft, upload, publish, reconciliation and incident path is exercised
//! against an in-process fake.
//!
//! The owner is tag scoped and never cancelled
//! ([`model::ConcurrencyPolicy`]). It creates or resumes exactly one matching
//! draft, uploads only absent assets, re-downloads and validates the exact
//! asset set, publishes, then requires `immutable == true` and a release
//! statement whose membership equals the intended set. Published releases and
//! tags are never overwritten. A lost mutation response is followed by a
//! read-only reconciliation before any other mutation. A confirmed mutable
//! release fails publication and is removed only under
//! [`role::CleanupAuthority`]; an immutable or unknown-state release is never
//! deleted.
//!
//! # Explicit recovery gap
//!
//! The owner never repairs a draft. A lost upload response can leave a
//! partial or altered asset; reconciliation then refuses with "explicit
//! recovery is required", as it does for a conflicting draft, extra asset,
//! multiple drafts or a tag at another commit. No automated recovery exists:
//! the release runbook must cover inspecting the unpublished draft, deleting
//! the offending draft asset or draft by hand under maintainer authority, and
//! rerunning publication.

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;

pub(crate) mod api;
pub(crate) mod incident;
pub(crate) mod model;
mod owner;
pub(crate) mod role;
#[cfg(test)]
mod tests;

#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "publication remains closed: only the fake-API tests call it"
    )
)]
pub(crate) use owner::publish;

fn rejected(message: impl Into<String>) -> Report<ReleaseError> {
    Report::new(ReleaseError::Publication).attach(message.into())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
