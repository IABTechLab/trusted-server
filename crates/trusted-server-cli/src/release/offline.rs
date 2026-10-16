//! Private build-evidence-only verification of securely transferred inputs.
//!
//! The caller supplies independently approved source, artifact and root pins.
//! No publication proof, report or deployment authorization is produced.

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::Instant;

use error_stack::Report;
use serde_json::Value;

use super::artifact::{AssetRole, RetainedAsset, RetainedDownload};
use super::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use super::documents::{ApprovedRelease, lower_hex, safe_name};
use super::environment::CallerEnvironment;
use super::error::{ReleaseError, report};
use super::evidence::BuildEvidenceVerified;
use super::gh::{GhLocation, GhRunner};
use super::online::{Transport, build_with};
use super::source_policy::{ApprovedSourcePolicy, ApprovedSourceRoot, read_approved_snapshot};

/// Independent trust inputs, transferred separately from untrusted evidence.
pub(super) struct OfflineApproval<'a> {
    pub(super) release: &'a ApprovedRelease,
    pub(super) source: &'a ApprovedSourceRoot,
    pub(super) asset: &'a str,
    pub(super) artifact_sha256: &'a str,
    pub(super) roots: &'a Path,
    pub(super) roots_sha256: &'a str,
    pub(super) gh: &'a Path,
}

/// Offline evidence deliberately hides its build proof from publication APIs.
pub(super) struct OfflineBuildEvidence {
    _build: BuildEvidenceVerified,
    _asset: RetainedAsset,
}

struct LocalTransport<'a> {
    directory: &'a Path,
    source: &'a ApprovedSourceRoot,
    cancellation: &'a Cancellation,
}

fn regular_file(path: &Path) -> Result<File, Report<ReleaseError>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| report(ReleaseError::Asset, error))?;
    if !file
        .metadata()
        .map_err(|error| report(ReleaseError::Asset, error))?
        .is_file()
    {
        return Err(report(
            ReleaseError::Asset,
            "offline input must be a regular file",
        ));
    }
    Ok(file)
}

impl Transport for LocalTransport<'_> {
    async fn policy(
        &self,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        read_approved_snapshot(self.source)
    }

    async fn asset(
        &self,
        name: &str,
        role: AssetRole,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<RetainedAsset, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        if !safe_name(name) {
            return Err(report(ReleaseError::Asset, "unsafe offline input name"));
        }
        let mut file = regular_file(&self.directory.join(name))?;
        let mut retained = RetainedDownload::start(name, role)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            if self.cancellation.is_cancelled() {
                return Err(report(ReleaseError::Cancelled, "offline input cancelled"));
            }
            if Instant::now() >= deadline.instant() {
                return Err(report(
                    ReleaseError::Timeout,
                    "offline input deadline elapsed",
                ));
            }
            let read = file
                .read(&mut buffer)
                .map_err(|error| report(ReleaseError::Asset, error))?;
            if read == 0 {
                return retained.finish();
            }
            retained.push(&buffer[..read])?;
        }
    }

    async fn publication(
        &self,
        _tag_ref: bool,
        deadline: AttemptDeadline,
        cancellation: &Cancellation,
    ) -> Result<Value, Report<ReleaseError>> {
        super::artifact::check_attempt(deadline, cancellation)?;
        Err(report(
            ReleaseError::Publication,
            "offline verification cannot acquire publication evidence",
        ))
    }
}

/// Verifies local build evidence with independently pinned source, roots and bytes.
///
/// The caller must enforce OS network denial during this operation. The gh
/// invocation uses local bundles and custom roots, with no token or cache.
///
/// # Errors
///
/// Rejects any changed trust input, artifact pin or evidence graph edge.
pub(super) async fn verify_build(
    approval: &OfflineApproval<'_>,
    evidence_directory: &Path,
    caller: &CallerEnvironment,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<OfflineBuildEvidence, Report<ReleaseError>> {
    if !lower_hex(approval.artifact_sha256, 64)
        || approval.source.commit() != approval.release.commit()
    {
        return Err(report(
            ReleaseError::Policy,
            "offline source and artifact pins must be independently approved",
        ));
    }
    if super::policy::embedded_policy()?
        .artifact(approval.asset)
        .is_none()
    {
        return Err(report(
            ReleaseError::Policy,
            "offline asset is outside the embedded cohort",
        ));
    }
    let runner = GhRunner::resolve_offline(
        GhLocation::Explicit(approval.gh),
        caller,
        ProcessBounds::production(),
        deadline,
        cancellation,
        approval.roots,
        approval.roots_sha256,
    )?;
    let transport = LocalTransport {
        directory: evidence_directory,
        source: approval.source,
        cancellation,
    };
    let built = build_with(
        &transport,
        &runner,
        approval.release,
        approval.asset,
        deadline,
        cancellation,
    )
    .await?;
    if built.executable.measurement().sha256() != approval.artifact_sha256 {
        return Err(report(
            ReleaseError::Evidence,
            "offline executable differs from independently approved digest",
        ));
    }
    super::artifact::check_attempt(deadline, cancellation)?;
    Ok(OfflineBuildEvidence {
        _build: built.build,
        _asset: built.executable,
    })
}
