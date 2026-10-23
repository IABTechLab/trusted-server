//! The abstract releases API the publication owner drives.

use super::model::ReleaseTag;
use super::role::{CleanupAuthority, ContentsWrite};
use crate::source::CommitId;

/// A GitHub release numeric ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ReleaseId(pub(crate) u64);

/// One asset as reported by the releases API (not trusted for bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedAsset {
    /// Asset numeric ID; a replaced asset gets a new ID.
    pub(crate) id: u64,
    /// Asset name.
    pub(crate) name: String,
    /// Reported size in bytes.
    pub(crate) size: u64,
    /// Reported lowercase hex SHA-256, when the API provides one.
    pub(crate) sha256: Option<String>,
}

/// A release as reported by the releases API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedRelease {
    /// Release ID.
    pub(crate) id: ReleaseId,
    /// Release tag name.
    pub(crate) tag: String,
    /// Target commit.
    pub(crate) target_commit: String,
    /// Whether the release is still a draft.
    pub(crate) draft: bool,
    /// Reported immutability; `None` when absent or unreadable.
    pub(crate) immutable: Option<bool>,
    /// Attached assets.
    pub(crate) assets: Vec<ObservedAsset>,
}

/// One signed subject of the GitHub release statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatementSubject {
    /// Asset name.
    pub(crate) name: String,
    /// Lowercase hex SHA-256.
    pub(crate) sha256: String,
}

/// The verified GitHub release statement for a tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseStatement {
    /// Signed tag.
    pub(crate) tag: String,
    /// Signed target commit.
    pub(crate) commit: String,
    /// Signed asset membership.
    pub(crate) subjects: Vec<StatementSubject>,
}

/// A releases API failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApiFailure {
    /// The API answered with a definite rejection status.
    Rejected {
        /// HTTP status.
        status: u16,
    },
    /// The outcome is unknown (lost response, timeout or unreadable state).
    Indeterminate,
}

/// The releases API operations available to the publication owner.
///
/// Every call requires [`ContentsWrite`]; deletion additionally requires
/// [`CleanupAuthority`]. No real adapter exists while publication is closed.
pub(crate) trait ReleaseApi {
    /// Lists every release (draft or published) for `tag`.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the listing is rejected or unknown.
    fn releases_for_tag(
        &mut self,
        authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<Vec<ObservedRelease>, ApiFailure>;

    /// Reads one release; `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the read is rejected or unknown.
    fn release(
        &mut self,
        authority: &ContentsWrite,
        id: ReleaseId,
    ) -> Result<Option<ObservedRelease>, ApiFailure>;

    /// Reads the commit the git tag `tag` points at; `None` when no tag exists.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the read is rejected or unknown.
    fn tag_ref(
        &mut self,
        authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<Option<CommitId>, ApiFailure>;

    /// Creates a draft for `tag` targeting `commit`.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when creation is rejected or unknown.
    fn create_draft(
        &mut self,
        authority: &ContentsWrite,
        tag: &ReleaseTag,
        commit: &CommitId,
    ) -> Result<ReleaseId, ApiFailure>;

    /// Uploads a new asset to a draft; never replaces an existing asset.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the upload is rejected or unknown.
    fn upload_asset(
        &mut self,
        authority: &ContentsWrite,
        id: ReleaseId,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), ApiFailure>;

    /// Downloads at most `limit + 1` bytes of an asset.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the download is rejected or unknown.
    fn download_asset(
        &mut self,
        authority: &ContentsWrite,
        id: ReleaseId,
        name: &str,
        limit: u64,
    ) -> Result<Vec<u8>, ApiFailure>;

    /// Publishes a draft.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when publication is rejected or unknown.
    fn publish_draft(&mut self, authority: &ContentsWrite, id: ReleaseId)
    -> Result<(), ApiFailure>;

    /// Reads and verifies the GitHub release statement for `tag`.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when the statement is absent or unverifiable.
    fn release_statement(
        &mut self,
        authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<ReleaseStatement, ApiFailure>;

    /// Deletes a release confirmed still mutable.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiFailure`] when deletion is rejected or unknown.
    fn delete_mutable_release(
        &mut self,
        authority: &ContentsWrite,
        cleanup: &CleanupAuthority,
        id: ReleaseId,
    ) -> Result<(), ApiFailure>;
}
