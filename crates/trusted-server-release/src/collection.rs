//! Bounded cross-job transfer diagnostics, without release authorization.
//!
//! REST metadata corroborates a dependency-selected artifact, never its producer
//! job. Only a qualified workflow adapter may eventually mint the dependency
//! capability. Imported documents never reconstruct local recipe, boundary,
//! composition, or runtime proof types. That production adapter remains closed.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::policy::FIXED_ARTIFACTS;

const ARCHIVE_LIMIT: usize = 768 * 1024 * 1024;

mod archive;
pub(crate) mod producer;
mod records;
pub(crate) mod transfer;
mod workflow_jobs;
pub(crate) use records::CollectionDiagnostic;
#[cfg(test)]
pub(crate) use records::{ReceiptFacts, check_composition_receipt};
#[cfg(test)]
mod tests;

/// Qualification of the dependency adapter is not available yet.
#[derive(Clone, Copy)]
pub(crate) enum QualifiedCollection {}

/// Expected invocation identity from the trusted workflow dependency graph.
#[derive(Clone, Debug)]
struct Invocation {
    repository: String,
    repository_id: u64,
    run_id: u64,
    source_commit: String,
    inventory_sha256: String,
    profiles: [records::RoleProfile; 2],
}

/// One upload step output; REST cannot independently prove this job association.
#[derive(Clone, Debug)]
struct Upload {
    asset: String,
    role: String,
    job: String,
    attempt: u64,
    recipe_sha256: String,
    elapsed_seconds: u64,
    deadline_seconds: u64,
    artifact_name: String,
    artifact_id: u64,
    archive_sha256: String,
    members: BTreeMap<String, u64>,
}

/// A terminal job conclusion as the trusted dependency adapter observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum JobConclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
}

/// What the trusted adapter observed for one optional reproduction job.
///
/// `conclusion` is `None` when the job had no terminal conclusion when the
/// collector's bounded wait reached its deadline. The trusted adapter MUST
/// wait for a terminal conclusion or for the full deadline before it
/// constructs a conclusion-less observation; stopping early is not a legal
/// input, and [`NoUpload::outcome`] rejects one whose elapsed wait is short of
/// the deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
struct ObservedJob {
    /// At least one job step executed.
    ///
    /// This is not GitHub's job `started_at`, which a job skipped or cancelled
    /// before any step may still carry; the adapter derives it from executed
    /// steps.
    started: bool,
    conclusion: Option<JobConclusion>,
}

/// An optional reproduction job that produced no upload.
///
/// It has no field for an upload ID, name or digest, so none can be supplied
/// or synthesized for it.
#[derive(Clone, Debug)]
struct NoUpload {
    asset: String,
    job: ObservedJob,
    elapsed_seconds: u64,
    deadline_seconds: u64,
}

impl NoUpload {
    /// Derives the spec outcome, or `None` for contradictory or unbounded input.
    ///
    /// `unavailable` means no reproduction ran: the job was still queued at the
    /// deadline, or the scheduler skipped or cancelled it before any step
    /// started. `failed` means it started and produced no upload, or its own
    /// conclusion states failure or timeout. The latter stays `failed` even
    /// without a started step, so a stated failure is never relabeled as an
    /// absent measurement. A job without a conclusion is accepted only once the
    /// wait reached the deadline, so the collector cannot cut a reproduction
    /// short; deadlines are capped, so it cannot wait indefinitely either.
    fn outcome(&self) -> Option<records::Outcome> {
        if !bounded(self.elapsed_seconds, self.deadline_seconds) {
            return None;
        }
        let at_deadline = self.elapsed_seconds == self.deadline_seconds;
        match (self.job.started, self.job.conclusion) {
            (false, None) if at_deadline => Some(records::Outcome::Unavailable),
            (true, None) if at_deadline => Some(records::Outcome::Failed),
            (_, None) => None,
            (false, Some(JobConclusion::Skipped | JobConclusion::Cancelled)) => {
                Some(records::Outcome::Unavailable)
            }
            (true, Some(JobConclusion::Skipped)) | (false, Some(JobConclusion::Success)) => None,
            (_, Some(JobConclusion::Failure | JobConclusion::TimedOut))
            | (true, Some(JobConclusion::Cancelled | JobConclusion::Success)) => {
                Some(records::Outcome::Failed)
            }
        }
    }
}

/// A bounded optional job result with no invented artifact identity.
#[derive(Clone, Debug)]
enum OptionalBuild {
    /// An upload, valid only from a started job that concluded successfully.
    Uploaded {
        upload: Upload,
        job: ObservedJob,
    },
    NotUploaded(NoUpload),
}

/// Authenticated dependency outputs; no JSON or caller-field constructor exists.
#[derive(Debug)]
pub(crate) struct TrustedDependencies {
    invocation: Invocation,
    required: Vec<Upload>,
    optional: Vec<OptionalBuild>,
}

impl TrustedDependencies {
    /// Keeps production closed until a qualified adapter supplies typed inputs.
    pub(crate) fn from_qualified(qualification: QualifiedCollection) -> Self {
        match qualification {}
    }
}

/// A read-only exact-ID source; the future adapter must enforce transport bounds.
pub(crate) trait ArtifactSource {
    /// Fetches REST metadata for this exact artifact ID.
    fn metadata(
        &mut self,
        repository: &str,
        artifact_id: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>>;
    /// Re-downloads that same artifact's archive and reports any transfer warning.
    fn download(
        &mut self,
        repository: &str,
        artifact_id: u64,
    ) -> Result<Download, Report<ReleaseError>>;
}

/// Downloaded bytes, separate from the final executable digest.
pub(crate) struct Download {
    pub(crate) archive: Vec<u8>,
    pub(crate) transfer_warning: bool,
}

#[derive(Debug)]
struct Transfer {
    upload: Upload,
    members: BTreeMap<String, Vec<u8>>,
}

/// Validated transfer bytes; this is not a qualified build or signing proof.
#[derive(Debug)]
pub(crate) struct ValidatedTransfers {
    invocation: Invocation,
    transfers: Vec<Transfer>,
    absent: Vec<NoUpload>,
}

/// Validates current-run metadata and the complete bounded ZIP before use.
///
/// # Errors
///
/// Rejects mismatched dependency identities, stale REST metadata, transfer
/// warnings, byte changes, or unsafe/undeclared archive members.
pub(crate) fn validate_transfers(
    dependencies: &TrustedDependencies,
    source: &mut impl ArtifactSource,
) -> Result<ValidatedTransfers, Report<ReleaseError>> {
    if dependencies.required.len() > 5 || dependencies.optional.len() > 3 {
        return Err(collection_error(
            "dependency transfer count exceeds cohort bounds",
        ));
    }
    let invocation = &dependencies.invocation;
    if invocation.repository_id == 0
        || invocation.run_id == 0
        || !lower_hex(&invocation.source_commit, 40)
        || !lower_hex(&invocation.inventory_sha256, 64)
        || !safe_repository(&invocation.repository)
    {
        return Err(collection_error("invalid invocation identity"));
    }
    let mut ids = BTreeSet::new();
    let mut roles = BTreeSet::new();
    let mut transfers = Vec::new();
    let mut absent = Vec::new();
    // The mandatory channel carries every canonical build and only the Fastly
    // reproduction; native reproductions are optional.
    if dependencies.required.iter().any(|upload| {
        upload.role != "A" && (upload.role != "B" || upload.asset != FIXED_ARTIFACTS[0].name)
    }) {
        return Err(collection_error(
            "mandatory upload is neither canonical nor the Fastly reproduction",
        ));
    }
    let mut uploads = dependencies.required.iter().collect::<Vec<_>>();
    for optional in &dependencies.optional {
        match optional {
            OptionalBuild::Uploaded { upload, job } => {
                if *job
                    != (ObservedJob {
                        started: true,
                        conclusion: Some(JobConclusion::Success),
                    })
                {
                    return Err(collection_error(
                        "optional B claims an upload identity without a successful job",
                    ));
                }
                // The optional channel carries only native reproductions.
                if upload.role != "B" || upload.asset == FIXED_ARTIFACTS[0].name {
                    return Err(collection_error(
                        "optional upload is not a native reproduction",
                    ));
                }
                uploads.push(upload);
            }
            OptionalBuild::NotUploaded(no_upload) => {
                if no_upload.outcome().is_none()
                    || !roles.insert((no_upload.asset.clone(), "B".to_owned()))
                {
                    return Err(collection_error(
                        "optional B result is not bounded failed/unavailable",
                    ));
                }
                absent.push(no_upload.clone());
            }
        }
    }
    for upload in uploads {
        if !bounded_upload(upload)
            || upload.artifact_id == 0
            || upload.attempt == 0
            || upload.job.is_empty()
            || upload.job.len() > 128
            || upload.job.chars().any(char::is_control)
            || !matches!(upload.role.as_str(), "A" | "B")
            || !lower_hex(&upload.archive_sha256, 64)
            || !lower_hex(&upload.recipe_sha256, 64)
            || !ids.insert(upload.artifact_id)
            || !roles.insert((upload.asset.clone(), upload.role.clone()))
        {
            return Err(collection_error(
                "invalid or reused dependency upload identity",
            ));
        }
        let metadata = parse_json(
            &source.metadata(&invocation.repository, upload.artifact_id)?,
            Role::Metadata,
        )
        .map_err(|error| error.change_context(ReleaseError::Collection))?;
        if metadata["id"].as_u64() != Some(upload.artifact_id)
            || metadata["name"].as_str() != Some(upload.artifact_name.as_str())
            || metadata["expired"].as_bool() != Some(false)
            || metadata["digest"].as_str()
                != Some(format!("sha256:{}", upload.archive_sha256).as_str())
            || metadata["workflow_run"]["id"].as_u64() != Some(invocation.run_id)
            || metadata["workflow_run"]["head_sha"].as_str()
                != Some(invocation.source_commit.as_str())
            || metadata["workflow_run"]["repository_id"].as_u64() != Some(invocation.repository_id)
            || metadata["workflow_run"]["head_repository_id"].as_u64()
                != Some(invocation.repository_id)
        {
            return Err(collection_error(
                "REST artifact metadata disagrees with current invocation",
            ));
        }
        let downloaded = source.download(&invocation.repository, upload.artifact_id)?;
        if downloaded.transfer_warning
            || downloaded.archive.len() > ARCHIVE_LIMIT
            || digest(&downloaded.archive) != upload.archive_sha256
        {
            return Err(collection_error(
                "transfer warning or archive digest mismatch",
            ));
        }
        let members = archive::read(&downloaded.archive, &upload.members)?;
        transfers.push(Transfer {
            upload: upload.clone(),
            members,
        });
    }
    Ok(ValidatedTransfers {
        invocation: invocation.clone(),
        transfers,
        absent,
    })
}

/// Measures validated transfer data without constructing release authorization.
///
/// # Errors
///
/// Rejects missing A, mandatory Fastly B, changed inputs, dishonest observations,
/// failed mandatory gates, or unequal complete final Wasm bytes.
pub(crate) fn diagnose_collection(
    transfers: &ValidatedTransfers,
) -> Result<CollectionDiagnostic, Report<ReleaseError>> {
    records::diagnose(transfers)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn collection_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Collection).attach(detail.to_string())
}

fn bounded_upload(upload: &Upload) -> bool {
    bounded(upload.elapsed_seconds, upload.deadline_seconds)
}

/// Requires a nonzero capped deadline that the observed wait did not exceed.
fn bounded(elapsed_seconds: u64, deadline_seconds: u64) -> bool {
    deadline_seconds > 0 && deadline_seconds <= 5400 && elapsed_seconds <= deadline_seconds
}

fn safe_repository(repository: &str) -> bool {
    let parts = repository.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 100
                && !matches!(*part, "." | "..")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}
