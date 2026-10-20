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

const ARCHIVE_LIMIT: usize = 768 * 1024 * 1024;

mod archive;
pub(crate) mod producer;
mod records;
pub(crate) mod transfer;
pub(crate) use records::CollectionDiagnostic;
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

/// A bounded optional job result with no invented artifact identity.
#[derive(Clone, Debug)]
enum OptionalBuild {
    Uploaded(Upload),
    NotUploaded {
        asset: String,
        outcome: records::Outcome,
        elapsed_seconds: u64,
        deadline_seconds: u64,
    },
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
    absent: Vec<OptionalBuild>,
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
    let mut uploads = dependencies.required.iter().collect::<Vec<_>>();
    for optional in &dependencies.optional {
        match optional {
            OptionalBuild::Uploaded(upload) => uploads.push(upload),
            OptionalBuild::NotUploaded {
                asset,
                outcome,
                elapsed_seconds,
                deadline_seconds,
            } => {
                if !matches!(
                    outcome,
                    records::Outcome::Failed | records::Outcome::Unavailable
                ) || *deadline_seconds == 0
                    || *deadline_seconds > 5400
                    || elapsed_seconds > deadline_seconds
                    || !roles.insert((asset.clone(), "B".to_owned()))
                {
                    return Err(collection_error(
                        "optional B result is not bounded failed/unavailable",
                    ));
                }
                absent.push(optional.clone());
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
    upload.deadline_seconds > 0
        && upload.deadline_seconds <= 5400
        && upload.elapsed_seconds <= upload.deadline_seconds
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
