//! Pure ordinary upload-output/API joins; transport authentication is external.

use std::collections::BTreeSet;

use error_stack::Report;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{ASSET, digest, require};
use crate::documents::{self, Role};
use crate::error::ReleaseError;

const MAX_ID: u64 = 9_007_199_254_740_991;
const ARCHIVE_LIMIT: u64 = 768 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    artifact_id: u64,
    artifact_digest: String,
    observation_id: u64,
    observation_digest: String,
}

/// Invocation context supplied by the workflow, never a build observation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    pub(super) repository: String,
    pub(super) repository_id: u64,
    pub(super) head_repository_id: u64,
    pub(super) source_commit: String,
    pub(super) run_id: u64,
    pub(super) run_attempt: u64,
    canonical: Upload,
    reproduction: Upload,
}

/// Service-correlated archive identity; its content hash still needs checking.
#[derive(Debug)]
pub(super) struct Archive {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) sha256: String,
    pub(super) size: u64,
}

/// One current-attempt successful producing job and its exact two uploads.
#[derive(Debug)]
pub(super) struct Build {
    pub(super) job_id: u64,
    pub(super) started: OffsetDateTime,
    pub(super) completed: OffsetDateTime,
    pub(super) artifact: Archive,
    pub(super) observation: Archive,
}

/// Validated current-run ordinary bindings, without collection qualification.
#[derive(Debug)]
pub(super) struct Bindings {
    context: Context,
    canonical: Build,
    reproduction: Build,
}

impl Bindings {
    /// The distinct canonical and reproduction producing jobs.
    #[cfg(test)]
    pub(super) fn job_ids(&self) -> [u64; 2] {
        [self.canonical.job_id, self.reproduction.job_id]
    }

    /// The immutable workflow context used by every joined record.
    pub(super) fn context(&self) -> &Context {
        &self.context
    }

    /// The canonical and reproduction job bindings, in that order.
    pub(super) fn builds(&self) -> [&Build; 2] {
        [&self.canonical, &self.reproduction]
    }
}

fn json(bytes: &[u8]) -> Result<Value, Report<ReleaseError>> {
    documents::parse_json(bytes, Role::Metadata).map_err(|_| {
        Report::new(ReleaseError::Document).attach("ordinary service response JSON rejected")
    })
}

fn positive(id: u64) -> bool {
    (1..=MAX_ID).contains(&id)
}

fn repository(repository: &str) -> bool {
    repository.len() <= 200
        && repository.split('/').count() == 2
        && repository.split('/').all(|part| {
            part.as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
}

fn page<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], Report<ReleaseError>> {
    let entries = value[key].as_array().ok_or_else(|| {
        Report::new(ReleaseError::Document).attach("ordinary service response has no record list")
    })?;
    require(
        entries.len() <= 100 && value["total_count"].as_u64() == Some(entries.len() as u64),
        "ordinary service response is truncated or exceeds the single-page bound",
    )?;
    let mut ids = BTreeSet::new();
    for entry in entries {
        require(
            entry["id"]
                .as_u64()
                .is_some_and(|id| positive(id) && ids.insert(id)),
            "ordinary service record ID is missing, invalid or duplicated",
        )?;
    }
    Ok(entries)
}

fn timestamp(value: &Value) -> Result<OffsetDateTime, Report<ReleaseError>> {
    let text = value
        .as_str()
        .filter(|text| text.len() <= 40)
        .ok_or_else(|| {
            Report::new(ReleaseError::Document).attach("ordinary service timestamp is missing")
        })?;
    OffsetDateTime::parse(text, &Rfc3339).map_err(|_| {
        Report::new(ReleaseError::Document).attach("ordinary service timestamp is invalid")
    })
}

fn uploaded(
    context: &Context,
    artifacts: &[Value],
    id: u64,
    sha256: &str,
    name: String,
    interval: (OffsetDateTime, OffsetDateTime),
) -> Result<Archive, Report<ReleaseError>> {
    let matching: Vec<_> = artifacts
        .iter()
        .filter(|artifact| artifact["id"].as_u64() == Some(id))
        .collect();
    require(
        matching.len() == 1,
        "ordinary upload ID is absent or duplicated",
    )?;
    let artifact = matching[0];
    require(
        artifacts
            .iter()
            .filter(|artifact| artifact["name"].as_str() == Some(&name))
            .count()
            == 1,
        "ordinary upload name is absent or duplicated",
    )?;
    let size = artifact["size_in_bytes"].as_u64().unwrap_or(0);
    let run = &artifact["workflow_run"];
    require(
        artifact["name"].as_str() == Some(&name)
            && artifact["digest"].as_str() == Some(format!("sha256:{sha256}").as_str())
            && artifact["expired"].as_bool() == Some(false)
            && (1..=ARCHIVE_LIMIT).contains(&size)
            && run["id"].as_u64() == Some(context.run_id)
            && run["repository_id"].as_u64() == Some(context.repository_id)
            && run["head_repository_id"].as_u64() == Some(context.head_repository_id)
            && run["head_sha"].as_str() == Some(&context.source_commit),
        "ordinary upload service identity does not match its direct output",
    )?;
    let created = timestamp(&artifact["created_at"])?;
    require(
        interval.0 <= created && created <= interval.1,
        "ordinary upload is outside its producing job interval",
    )?;
    Ok(Archive {
        id,
        name,
        sha256: sha256.to_owned(),
        size,
    })
}

fn build(
    context: &Context,
    jobs: &[Value],
    artifacts: &[Value],
    upload: &Upload,
    role: &str,
) -> Result<Build, Report<ReleaseError>> {
    let name = match role {
        "canonical" => format!("build / Build {ASSET}"),
        _ => format!("build / Reproduce {ASSET}"),
    };
    let matching: Vec<_> = jobs
        .iter()
        .filter(|job| job["name"].as_str() == Some(&name))
        .collect();
    require(
        matching.len() == 1,
        "ordinary producing job is absent or duplicated",
    )?;
    let job = matching[0];
    let job_id = job["id"].as_u64().unwrap_or(0);
    require(
        positive(job_id)
            && job["run_id"].as_u64() == Some(context.run_id)
            && job["run_attempt"].as_u64() == Some(context.run_attempt)
            && job["head_sha"].as_str() == Some(&context.source_commit)
            && job["status"].as_str() == Some("completed")
            && job["conclusion"].as_str() == Some("success"),
        "ordinary producing job is unsuccessful, stale or from another source",
    )?;
    let steps = job["steps"]
        .as_array()
        .filter(|steps| steps.len() <= 100)
        .ok_or_else(|| {
            Report::new(ReleaseError::Document).attach("ordinary upload steps are missing")
        })?;
    for name in [
        format!("Upload {role} release artifact"),
        format!("Upload {role} observation"),
    ] {
        let steps: Vec<_> = steps
            .iter()
            .filter(|step| step["name"].as_str() == Some(&name))
            .collect();
        require(
            steps.len() == 1
                && steps[0]["status"].as_str() == Some("completed")
                && steps[0]["conclusion"].as_str() == Some("success"),
            "ordinary upload step did not succeed exactly once",
        )?;
    }
    let started = timestamp(&job["started_at"])?;
    let completed = timestamp(&job["completed_at"])?;
    require(
        started.unix_timestamp() > 0 && started <= completed,
        "ordinary job interval rejected",
    )?;
    let artifact_name = if role == "canonical" {
        format!("release-{ASSET}")
    } else {
        format!("reproduction-{ASSET}")
    };
    Ok(Build {
        job_id,
        started,
        completed,
        artifact: uploaded(
            context,
            artifacts,
            upload.artifact_id,
            &upload.artifact_digest,
            artifact_name,
            (started, completed),
        )?,
        observation: uploaded(
            context,
            artifacts,
            upload.observation_id,
            &upload.observation_digest,
            format!("fastly-observation-{role}"),
            (started, completed),
        )?,
    })
}

/// Joins trusted invocation/upload outputs with authenticated API responses.
pub(super) fn bind(
    request: &[u8],
    jobs: &[u8],
    artifacts: &[u8],
) -> Result<Bindings, Report<ReleaseError>> {
    let context: Context = serde_json::from_value(json(request)?).map_err(|_| {
        Report::new(ReleaseError::Document).attach("ordinary workflow context fields rejected")
    })?;
    require(
        repository(&context.repository)
            && [
                context.repository_id,
                context.head_repository_id,
                context.run_id,
                context.run_attempt,
            ]
            .into_iter()
            .all(positive)
            && digest(&context.source_commit, 40),
        "ordinary workflow context identity rejected",
    )?;
    let mut ids = BTreeSet::new();
    for upload in [&context.canonical, &context.reproduction] {
        require(
            positive(upload.artifact_id)
                && positive(upload.observation_id)
                && ids.insert(upload.artifact_id)
                && ids.insert(upload.observation_id)
                && digest(&upload.artifact_digest, 64)
                && digest(&upload.observation_digest, 64),
            "ordinary direct upload outputs are invalid or reused",
        )?;
    }
    let jobs = json(jobs)?;
    let artifacts = json(artifacts)?;
    let jobs = page(&jobs, "jobs")?;
    let artifacts = page(&artifacts, "artifacts")?;
    let canonical = build(&context, jobs, artifacts, &context.canonical, "canonical")?;
    let reproduction = build(
        &context,
        jobs,
        artifacts,
        &context.reproduction,
        "reproduction",
    )?;
    require(
        canonical.job_id != reproduction.job_id,
        "ordinary producing job ID reused",
    )?;
    Ok(Bindings {
        context,
        canonical,
        reproduction,
    })
}

#[cfg(test)]
pub(super) mod tests;
