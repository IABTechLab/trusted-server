//! EC-specific consent gating.
//!
//! This module provides the public consent-check API for the EC subsystem.
//! The underlying logic lives in [`crate::consent::allows_ec_creation`]; this
//! wrapper exists so that EC callers can import from `ec::consent` and the
//! eventual migration path (renaming, adding EC-specific conditions) is
//! contained here.

use crate::consent::{ConsentContext, EcConsentDecision};

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
    ec_consent_decision(consent_context).allowed
}

/// Determines whether Edge Cookie creation is permitted and why.
#[must_use]
pub fn ec_consent_decision(consent_context: &ConsentContext) -> EcConsentDecision {
    crate::consent::ec_creation_decision(consent_context)
}

/// Returns `true` when the request carries an explicit EC withdrawal signal.
///
/// This is intentionally stricter than [`ec_consent_granted`]. A fail-closed
/// result such as unknown jurisdiction or missing consent data must not be
/// treated as an authoritative withdrawal of an already-issued EC.
#[must_use]
pub fn ec_consent_withdrawn(consent_context: &ConsentContext) -> bool {
    crate::consent::has_explicit_ec_withdrawal(consent_context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;

    #[test]
    fn ec_consent_granted_allows_non_regulated_requests() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };

        assert!(
            ec_consent_granted(&ctx),
            "non-regulated requests should be allowed"
        );
    }

    #[test]
    fn ec_consent_granted_blocks_unknown_jurisdiction() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Unknown,
            ..ConsentContext::default()
        };

        assert!(
            !ec_consent_granted(&ctx),
            "unknown jurisdiction should fail closed"
        );
    }

    #[test]
    fn ec_consent_withdrawn_does_not_treat_unknown_jurisdiction_as_revocation() {
        let ctx = ConsentContext {
            jurisdiction: Jurisdiction::Unknown,
            ..ConsentContext::default()
        };

        assert!(
            !ec_consent_withdrawn(&ctx),
            "unknown jurisdiction should block creation without revoking existing EC"
        );
    }
}
