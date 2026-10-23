//! The sole-owner publication run for one tag.

use std::collections::BTreeSet;

use error_stack::Report;

use super::api::{ApiFailure, ObservedRelease, ReleaseApi, ReleaseId, ReleaseStatement};
use super::incident::{
    CleanupOutcome, Incident, IncidentKind, PublicationFailure, PublishedRelease,
};
use super::model::IntendedRelease;
use super::role::PublicationRole;
use super::{rejected, sha256_hex};
use crate::error::ReleaseError;

/// Publishes `release` as its tag's sole immutable-release owner.
///
/// The git tag must be absent or already point at the intended commit, both
/// when the draft is located and again immediately before publishing. The
/// draft is re-read immediately before publishing and must still equal the
/// validated asset set (asset IDs, names, sizes and digests).
///
/// # Errors
///
/// Returns a [`PublicationFailure`] whenever publication is not completed and
/// verified. Failures after a publish attempt carry an [`Incident`].
pub(crate) fn publish<A: ReleaseApi>(
    api: &mut A,
    role: &PublicationRole,
    release: &IntendedRelease,
) -> Result<PublishedRelease, PublicationFailure> {
    let mut owner = Owner { api, role, release };
    let id = owner.locate_draft().map_err(PublicationFailure::refused)?;
    owner
        .upload_absent(id)
        .and_then(|()| owner.verify_exact_set(id))
        .and_then(|validated| owner.recheck_before_publish(&validated))
        .map_err(PublicationFailure::refused)?;
    owner.publish_draft(id)?;
    owner.final_check(id)
}

fn api_error(action: &str, failure: ApiFailure) -> Report<ReleaseError> {
    match failure {
        ApiFailure::Rejected { status } => rejected(format!(
            "releases API rejected {action} with status {status}"
        )),
        ApiFailure::Indeterminate => {
            rejected(format!("releases API outcome of {action} is unknown"))
        }
    }
}

/// The single in-flight publication run for one tag.
struct Owner<'run, A> {
    api: &'run mut A,
    role: &'run PublicationRole,
    release: &'run IntendedRelease,
}

impl<A: ReleaseApi> Owner<'_, A> {
    fn incident(
        &self,
        id: ReleaseId,
        kind: IncidentKind,
        cleanup: CleanupOutcome,
        failure: Option<ApiFailure>,
        message: &str,
    ) -> PublicationFailure {
        PublicationFailure {
            report: rejected(format!(
                "release `{}` publication failed: {message}; cleanup outcome {cleanup:?}; API failure {failure:?}",
                self.release.tag()
            )),
            incident: Some(Incident {
                tag: self.release.tag().as_str().to_owned(),
                release: Some(id),
                kind,
                cleanup,
                api_failure: failure,
            }),
        }
    }

    fn list_for_tag(&mut self) -> Result<Vec<ObservedRelease>, Report<ReleaseError>> {
        self.api
            .releases_for_tag(self.role.contents(), self.release.tag())
            .map_err(|failure| api_error("listing releases for the tag", failure))
    }

    /// Requires the git tag to be absent or to point at the intended commit.
    fn check_tag_ref(&mut self) -> Result<(), Report<ReleaseError>> {
        let target = self
            .api
            .tag_ref(self.role.contents(), self.release.tag())
            .map_err(|failure| api_error("reading the tag reference", failure))?;
        match target {
            Some(commit) if commit != *self.release.commit() => Err(rejected(format!(
                "tag `{}` already points at `{commit}`, not the intended `{}`; tags are never retagged",
                self.release.tag(),
                self.release.commit()
            ))),
            Some(_) | None => Ok(()),
        }
    }

    fn check_draft_source(&self, draft: &ObservedRelease) -> Result<(), Report<ReleaseError>> {
        if draft.tag != self.release.tag().as_str()
            || draft.target_commit != self.release.commit().as_str()
        {
            return Err(rejected(format!(
                "conflicting draft {:?} targets `{}` at `{}`; explicit recovery is required",
                draft.id, draft.tag, draft.target_commit
            )));
        }
        if let Some(extra) = draft
            .assets
            .iter()
            .find(|asset| !self.release.assets().contains_key(&asset.name))
        {
            return Err(rejected(format!(
                "draft asset `{}` is outside the intended set; explicit recovery is required",
                extra.name
            )));
        }
        Ok(())
    }

    fn select_existing(
        &self,
        releases: &[ObservedRelease],
    ) -> Result<Option<ReleaseId>, Report<ReleaseError>> {
        if releases.iter().any(|release| !release.draft) {
            return Err(rejected(format!(
                "tag `{}` already has a published release; published releases and tags are never overwritten",
                self.release.tag()
            )));
        }
        match releases {
            [] => Ok(None),
            [draft] => {
                self.check_draft_source(draft)?;
                Ok(Some(draft.id))
            }
            _ => Err(rejected(format!(
                "tag `{}` has multiple drafts; explicit recovery is required",
                self.release.tag()
            ))),
        }
    }

    fn locate_draft(&mut self) -> Result<ReleaseId, Report<ReleaseError>> {
        self.check_tag_ref()?;
        let releases = self.list_for_tag()?;
        if let Some(id) = self.select_existing(&releases)? {
            return Ok(id);
        }
        match self.api.create_draft(
            self.role.contents(),
            self.release.tag(),
            self.release.commit(),
        ) {
            Ok(id) => Ok(id),
            Err(ApiFailure::Indeterminate | ApiFailure::Rejected { status: 409 | 422 }) => {
                // A lost response or a racing run: reconcile read-only before any mutation.
                let releases = self.list_for_tag()?;
                self.select_existing(&releases)?.ok_or_else(|| {
                    rejected(
                        "draft creation outcome is unknown and reconciliation found no draft; rerun publication",
                    )
                })
            }
            Err(failure) => Err(api_error("draft creation", failure)),
        }
    }

    fn read_draft(&mut self, id: ReleaseId) -> Result<ObservedRelease, Report<ReleaseError>> {
        let observed = self
            .api
            .release(self.role.contents(), id)
            .map_err(|failure| api_error("reading the draft", failure))?
            .ok_or_else(|| rejected(format!("draft {id:?} disappeared during publication")))?;
        if !observed.draft {
            return Err(rejected(format!(
                "release {id:?} was published outside this owner before validation"
            )));
        }
        self.check_draft_source(&observed)?;
        Ok(observed)
    }

    fn check_downloaded(&mut self, id: ReleaseId, name: &str) -> Result<(), Report<ReleaseError>> {
        let release = self.release;
        let intended = release
            .assets()
            .get(name)
            .ok_or_else(|| rejected(format!("asset `{name}` is outside the intended set")))?;
        let bytes = self
            .api
            .download_asset(self.role.contents(), id, name, intended.size())
            .map_err(|failure| api_error("asset re-download", failure))?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > intended.size()) {
            return Err(rejected(format!(
                "downloaded asset `{name}` exceeds its intended size of {} bytes",
                intended.size()
            )));
        }
        if bytes != intended.bytes() || sha256_hex(&bytes) != intended.sha256() {
            return Err(rejected(format!(
                "draft asset `{name}` bytes differ from the intended asset; explicit recovery is required"
            )));
        }
        Ok(())
    }

    fn upload_absent(&mut self, id: ReleaseId) -> Result<(), Report<ReleaseError>> {
        let observed = self.read_draft(id)?;
        let present: BTreeSet<String> = observed
            .assets
            .into_iter()
            .map(|asset| asset.name)
            .collect();
        for name in &present {
            self.check_downloaded(id, name)?;
        }
        let release = self.release;
        for asset in release.assets().values() {
            if present.contains(asset.name()) {
                continue;
            }
            match self
                .api
                .upload_asset(self.role.contents(), id, asset.name(), asset.bytes())
            {
                Ok(()) => {}
                Err(ApiFailure::Indeterminate) => self.reconcile_upload(id, asset.name())?,
                Err(failure) => return Err(api_error("asset upload", failure)),
            }
        }
        Ok(())
    }

    fn reconcile_upload(&mut self, id: ReleaseId, name: &str) -> Result<(), Report<ReleaseError>> {
        let observed = self.read_draft(id)?;
        if !observed.assets.iter().any(|asset| asset.name == name) {
            return Err(rejected(format!(
                "upload outcome of `{name}` is unknown and the asset is absent after reconciliation; rerun to resume the draft"
            )));
        }
        self.check_downloaded(id, name)
    }

    /// Validates the exact asset set and returns the validated draft snapshot.
    fn verify_exact_set(&mut self, id: ReleaseId) -> Result<ObservedRelease, Report<ReleaseError>> {
        let observed = self.read_draft(id)?;
        let release = self.release;
        let names: BTreeSet<&str> = observed
            .assets
            .iter()
            .map(|asset| asset.name.as_str())
            .collect();
        if names.len() != observed.assets.len() {
            return Err(rejected("draft lists a duplicated asset name"));
        }
        if let Some(missing) = release
            .assets()
            .keys()
            .find(|name| !names.contains(name.as_str()))
        {
            return Err(rejected(format!(
                "draft asset `{missing}` is missing after upload"
            )));
        }
        if !self.assets_match(&observed) {
            return Err(rejected(
                "draft asset size or digest differs from the intended asset; explicit recovery is required",
            ));
        }
        for name in release.assets().keys() {
            self.check_downloaded(id, name)?;
        }
        Ok(observed)
    }

    /// Re-checks the tag and the draft read-only immediately before publishing.
    fn recheck_before_publish(
        &mut self,
        validated: &ObservedRelease,
    ) -> Result<(), Report<ReleaseError>> {
        self.check_tag_ref()?;
        let current = self.read_draft(validated.id)?;
        let mut expected = validated.assets.clone();
        let mut observed = current.assets;
        expected.sort_by_key(|asset| asset.id);
        observed.sort_by_key(|asset| asset.id);
        if observed != expected {
            return Err(rejected(format!(
                "draft {:?} assets changed after validation; publication is refused and the draft kept",
                validated.id
            )));
        }
        Ok(())
    }

    fn publish_draft(&mut self, id: ReleaseId) -> Result<(), PublicationFailure> {
        match self.api.publish_draft(self.role.contents(), id) {
            Ok(()) => Ok(()),
            Err(failure @ ApiFailure::Rejected { .. }) => Err(PublicationFailure::refused(
                api_error("publishing the draft", failure),
            )),
            // A lost publish response: reconcile read-only before any other mutation.
            Err(ApiFailure::Indeterminate) => match self.api.release(self.role.contents(), id) {
                Ok(Some(observed)) if !observed.draft => Ok(()),
                Ok(Some(_)) => Err(PublicationFailure::refused(rejected(
                    "publish response was lost and reconciliation shows the draft still unpublished; rerun publication",
                ))),
                Ok(None) => Err(self.incident(
                    id,
                    IncidentKind::UnknownState,
                    CleanupOutcome::NotAttempted,
                    Some(ApiFailure::Indeterminate),
                    "publish response was lost and reconciliation finds no release",
                )),
                Err(failure) => Err(self.incident(
                    id,
                    IncidentKind::UnknownState,
                    CleanupOutcome::NotAttempted,
                    Some(failure),
                    "publish response was lost and reconciliation cannot determine the release state",
                )),
            },
        }
    }

    fn final_check(&mut self, id: ReleaseId) -> Result<PublishedRelease, PublicationFailure> {
        let observed = match self.api.release(self.role.contents(), id) {
            Ok(Some(observed)) if !observed.draft => observed,
            Ok(Some(_)) => {
                return Err(self.incident(
                    id,
                    IncidentKind::UnknownState,
                    CleanupOutcome::NotAttempted,
                    None,
                    "release is still reported as a draft after publish; reconciliation is read-only",
                ));
            }
            Ok(None) => {
                return Err(self.incident(
                    id,
                    IncidentKind::UnknownState,
                    CleanupOutcome::NotAttempted,
                    None,
                    "published release is missing; reconciliation is read-only",
                ));
            }
            Err(failure) => {
                return Err(self.incident(
                    id,
                    IncidentKind::UnknownState,
                    CleanupOutcome::NotAttempted,
                    Some(failure),
                    "published release state is unreadable; reconciliation is read-only",
                ));
            }
        };
        match observed.immutable {
            Some(true) => self.verify_membership(&observed),
            Some(false) => {
                let (cleanup, failure) = self.cleanup_mutable(id);
                Err(self.incident(
                    id,
                    IncidentKind::MutablePublication,
                    cleanup,
                    failure,
                    "published release is confirmed mutable",
                ))
            }
            None => Err(self.incident(
                id,
                IncidentKind::UnknownState,
                CleanupOutcome::NotAttempted,
                None,
                "published release immutability is unknown; reconciliation is read-only",
            )),
        }
    }

    /// Removes a confirmed still-mutable release when the role authorizes it.
    ///
    /// The release is re-read immediately before deletion. The remaining
    /// window between that read and the delete is benign: GitHub applies
    /// immutability only at publication, so a published mutable release
    /// cannot later become immutable, and the API itself refuses to delete an
    /// immutable release.
    ///
    /// Deleting a release leaves its git tag in place. A rerun is not blocked
    /// by that tag: [`Owner::check_tag_ref`] accepts a tag that already points
    /// at the intended commit, and no published release remains to refuse.
    /// A tag at any other commit still requires explicit recovery.
    fn cleanup_mutable(&mut self, id: ReleaseId) -> (CleanupOutcome, Option<ApiFailure>) {
        let role = self.role;
        let Some(cleanup) = role.cleanup() else {
            return (CleanupOutcome::NotAuthorized, None);
        };
        let authority = role.contents();
        match self.api.release(authority, id) {
            Ok(Some(observed)) if observed.immutable == Some(false) => {}
            Ok(Some(observed)) if observed.immutable == Some(true) => {
                return (CleanupOutcome::RetainedImmutable, None);
            }
            Ok(None) => return (CleanupOutcome::Removed, None),
            Ok(Some(_)) => return (CleanupOutcome::RetainedStateUnknown, None),
            Err(failure) => return (CleanupOutcome::RetainedStateUnknown, Some(failure)),
        }
        match self.api.delete_mutable_release(authority, cleanup, id) {
            Ok(()) => (CleanupOutcome::Removed, None),
            Err(failure @ ApiFailure::Rejected { status }) => {
                (CleanupOutcome::Denied { status }, Some(failure))
            }
            Err(ApiFailure::Indeterminate) => match self.api.release(authority, id) {
                Ok(None) => (CleanupOutcome::Removed, Some(ApiFailure::Indeterminate)),
                Ok(Some(_)) => (
                    CleanupOutcome::Indeterminate,
                    Some(ApiFailure::Indeterminate),
                ),
                Err(failure) => (CleanupOutcome::Indeterminate, Some(failure)),
            },
        }
    }

    fn statement_matches(&self, statement: &ReleaseStatement) -> bool {
        let intended: BTreeSet<(&str, &str)> = self
            .release
            .assets()
            .values()
            .map(|asset| (asset.name(), asset.sha256()))
            .collect();
        let signed: BTreeSet<(&str, &str)> = statement
            .subjects
            .iter()
            .map(|subject| (subject.name.as_str(), subject.sha256.as_str()))
            .collect();
        statement.tag == self.release.tag().as_str()
            && statement.commit == self.release.commit().as_str()
            && signed.len() == statement.subjects.len()
            && signed == intended
    }

    fn assets_match(&self, observed: &ObservedRelease) -> bool {
        observed.assets.len() == self.release.assets().len()
            && observed.assets.iter().all(|asset| {
                self.release
                    .assets()
                    .get(&asset.name)
                    .is_some_and(|intended| {
                        asset.size == intended.size()
                            && asset
                                .sha256
                                .as_ref()
                                .is_none_or(|digest| digest == intended.sha256())
                    })
            })
    }

    fn release_identity_matches(&self, observed: &ObservedRelease) -> bool {
        !observed.draft
            && observed.tag == self.release.tag().as_str()
            && observed.target_commit == self.release.commit().as_str()
    }

    fn verify_membership(
        &mut self,
        observed: &ObservedRelease,
    ) -> Result<PublishedRelease, PublicationFailure> {
        let id = observed.id;
        let mismatch = |owner: &Self| {
            owner.incident(
                id,
                IncidentKind::MembershipMismatch,
                CleanupOutcome::RetainedImmutable,
                None,
                "immutable release identity, statement or assets differ from the intended set; an advisory and new version are required",
            )
        };
        if !(self.release_identity_matches(observed) && self.assets_match(observed)) {
            return Err(mismatch(self));
        }
        let verified = match self
            .api
            .release_statement(self.role.contents(), self.release.tag())
        {
            Ok(statement) => statement,
            Err(failure) => {
                return Err(self.incident(
                    id,
                    IncidentKind::UnverifiedStatement,
                    CleanupOutcome::RetainedImmutable,
                    Some(failure),
                    "immutable release statement could not be verified",
                ));
            }
        };
        if !self.statement_matches(&verified) {
            return Err(mismatch(self));
        }
        Ok(PublishedRelease {
            id,
            tag: self.release.tag().as_str().to_owned(),
            approval: self.release.approval().clone(),
        })
    }
}
