//! EC-specific consent gating.
//!
//! This module provides the public consent-check API for the EC subsystem.
//! The underlying logic lives in [`crate::consent::allows_ec_creation`]; this
//! wrapper exists so that EC callers can import from `ec::consent` and the
//! eventual migration path (renaming, adding EC-specific conditions) is
//! contained here.

use crate::consent::ConsentContext;

/// Determines whether Edge Cookie creation is permitted based on the
/// user's consent and detected jurisdiction.
///
/// This is the canonical entry point for EC consent checks. It delegates
/// to [`crate::consent::allows_ec_creation`] today but may diverge as
/// EC-specific consent rules evolve.
///
/// See [`crate::consent::allows_ec_creation`] for the full decision matrix.
#[must_use]
pub fn ec_consent_granted(consent_context: &ConsentContext) -> bool {
    crate::consent::allows_ec_creation(consent_context)
}
