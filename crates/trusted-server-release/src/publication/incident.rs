//! Publication results: success, refusal and release incidents.

use error_stack::Report;

use super::api::{ApiFailure, ReleaseId};
use super::model::SourceApproval;
use crate::error::ReleaseError;

/// The kind of release incident raised after a publish attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncidentKind {
    /// The published release was confirmed mutable.
    MutablePublication,
    /// The post-publish state could not be determined; reconciliation is read-only.
    UnknownState,
    /// The immutable release's statement or assets differ from the intended set.
    MembershipMismatch,
    /// The release statement could not be verified.
    UnverifiedStatement,
}

/// The outcome of still-mutable release cleanup recorded in an incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// No cleanup applies to this incident kind.
    NotAttempted,
    /// The release-write role does not authorize cleanup.
    NotAuthorized,
    /// The still-mutable release was removed (or confirmed absent).
    Removed,
    /// Deletion was rejected with this status.
    Denied {
        /// HTTP status.
        status: u16,
    },
    /// The deletion outcome could not be determined.
    Indeterminate,
    /// The release is immutable and is never deleted.
    RetainedImmutable,
    /// The state could not be reconfirmed before deletion, so it was retained.
    RetainedStateUnknown,
}

/// A release incident record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Incident {
    pub(super) tag: String,
    pub(super) release: Option<ReleaseId>,
    pub(super) kind: IncidentKind,
    pub(super) cleanup: CleanupOutcome,
    pub(super) api_failure: Option<ApiFailure>,
}

impl Incident {
    /// The release tag.
    pub(crate) fn tag(&self) -> &str {
        &self.tag
    }

    /// The release concerned, when known.
    pub(crate) fn release(&self) -> Option<ReleaseId> {
        self.release
    }

    /// What went wrong.
    pub(crate) fn kind(&self) -> IncidentKind {
        self.kind
    }

    /// The cleanup outcome.
    pub(crate) fn cleanup(&self) -> CleanupOutcome {
        self.cleanup
    }

    /// The releases API failure behind this incident, when one was observed.
    pub(crate) fn api_failure(&self) -> Option<ApiFailure> {
        self.api_failure
    }
}

/// A failed publication with its optional incident.
#[derive(Debug)]
pub(crate) struct PublicationFailure {
    pub(super) report: Report<ReleaseError>,
    pub(super) incident: Option<Incident>,
}

impl PublicationFailure {
    pub(super) fn refused(report: Report<ReleaseError>) -> Self {
        Self {
            report,
            incident: None,
        }
    }

    /// The failure report.
    pub(crate) fn report(&self) -> &Report<ReleaseError> {
        &self.report
    }

    /// The release incident, when the failure follows a publish attempt.
    pub(crate) fn incident(&self) -> Option<&Incident> {
        self.incident.as_ref()
    }
}

/// A release published, confirmed immutable and membership-verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishedRelease {
    pub(super) id: ReleaseId,
    pub(super) tag: String,
    pub(super) approval: SourceApproval,
}

impl PublishedRelease {
    /// The release ID.
    pub(crate) fn id(&self) -> ReleaseId {
        self.id
    }

    /// The release tag.
    pub(crate) fn tag(&self) -> &str {
        &self.tag
    }

    /// The source approval the release was published under.
    pub(crate) fn approval(&self) -> &SourceApproval {
        &self.approval
    }
}
