//! The contents-write-only publication role and its credential markers.

use std::collections::BTreeSet;

use error_stack::Report;

use super::rejected;
use crate::error::ReleaseError;

/// A workflow job permission or credential that might reach publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum JobPermission {
    /// `contents: read`.
    ContentsRead,
    /// `contents: write`.
    ContentsWrite,
    /// `actions: read`.
    ActionsRead,
    /// `checks: read`.
    ChecksRead,
    /// `id-token: write` (OIDC).
    IdTokenWrite,
    /// `attestations: write`.
    AttestationsWrite,
    /// Any deployment-provider credential.
    ProviderCredential,
    /// Any repository-settings token.
    RepositorySettings,
}

/// Whether the workflow's release-write role authorizes mutable cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupGrant {
    /// Removing a confirmed still-mutable release is authorized.
    Authorized,
    /// No cleanup is authorized; a mutable release is retained for the incident.
    NotAuthorized,
}

/// Proof that the caller holds `contents: write` and nothing broader.
///
/// Only [`PublicationRole::from_job`] constructs it, and every
/// [`super::api::ReleaseApi`] call requires it. No OIDC, attestation, provider or
/// settings credential type exists in the publication API surface.
#[derive(Debug)]
pub(crate) struct ContentsWrite(());

/// Proof that the release-write role authorizes still-mutable cleanup.
///
/// Only [`PublicationRole::from_job`] constructs it.
#[derive(Debug)]
pub(crate) struct CleanupAuthority(());

/// The contents-write-only publication role.
#[derive(Debug)]
pub(crate) struct PublicationRole {
    contents: ContentsWrite,
    cleanup: Option<CleanupAuthority>,
}

impl PublicationRole {
    /// Validates the job's granted permissions for publication.
    ///
    /// The same role applies to approved `main` and reviewed RC commits.
    ///
    /// `granted` is caller-attested: this pure model cannot observe the job.
    /// A real adapter MUST derive it from the parsed, reviewed workflow
    /// `permissions` block and the job's actual credential inputs, never from
    /// an operator-supplied list.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Publication`] unless `granted` is exactly
    /// `contents: write`, optionally with the implied `contents: read`.
    pub(crate) fn from_job(
        granted: &[JobPermission],
        cleanup: CleanupGrant,
    ) -> Result<Self, Report<ReleaseError>> {
        let granted: BTreeSet<JobPermission> = granted.iter().copied().collect();
        let allowed = BTreeSet::from([JobPermission::ContentsRead, JobPermission::ContentsWrite]);
        if !granted.contains(&JobPermission::ContentsWrite) {
            return Err(rejected("publication requires contents: write"));
        }
        if let Some(extra) = granted.difference(&allowed).next() {
            return Err(rejected(format!(
                "publication role must hold contents: write only; {extra:?} is forbidden"
            )));
        }
        Ok(Self {
            contents: ContentsWrite(()),
            cleanup: match cleanup {
                CleanupGrant::Authorized => Some(CleanupAuthority(())),
                CleanupGrant::NotAuthorized => None,
            },
        })
    }

    /// The contents-write marker every API call requires.
    pub(crate) fn contents(&self) -> &ContentsWrite {
        &self.contents
    }

    /// The cleanup authority, when the release-write role grants it.
    pub(crate) fn cleanup(&self) -> Option<&CleanupAuthority> {
        self.cleanup.as_ref()
    }
}
