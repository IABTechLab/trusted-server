//! Pure GitHub Actions jobs adapter for the trusted collector, without authority.
//!
//! It turns one bounded raw "list jobs for a workflow run attempt" REST body,
//! the upload step outputs and the collector's own wait clock into the typed
//! [`Upload`] and [`OptionalBuild`] inputs of [`TrustedDependencies`]. Every
//! expected job name, its asset, role and recipe come from an explicit
//! [`ExpectedTopology`], never from the response. Nothing here performs HTTP,
//! reads workflow files or constructs [`TrustedDependencies`]: that constructor
//! stays closed behind the uninhabited [`QualifiedCollection`].
//!
//! Parsing follows the REST precedent of [`super::transfer`]: the body passes
//! the strict bounded [`parse_json`] (UTF-8, metadata byte limit, no duplicate
//! keys, nesting limit) and only the fields read here are checked; other
//! fields, such as `started_at`, are ignored.
//!
//! [`TrustedDependencies`]: super::TrustedDependencies
//! [`QualifiedCollection`]: super::QualifiedCollection

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde_json::Value;

use super::{
    JobConclusion, NoUpload, ObservedJob, OptionalBuild, Upload, bounded, collection_error,
};
use crate::documents::{self, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::policy::{FIXED_ARTIFACTS, ReleaseTarget};
use crate::recipe::Role;

#[cfg(test)]
mod tests;

/// One REST page; a larger or paginated run is rejected rather than merged.
const JOB_PAGE_LIMIT: usize = 100;
/// GitHub's documented maximum number of steps in one job.
const STEP_LIMIT: usize = 1000;
/// Same bound the collector applies to an upload's job name.
const JOB_NAME_LIMIT: usize = 128;

/// The only job state that may carry an upload identity.
const SUCCEEDED: ObservedJob = ObservedJob {
    started: true,
    conclusion: Some(JobConclusion::Success),
};

type Result<T> = core::result::Result<T, Report<ReleaseError>>;

/// The exact workflow run attempt and source the collector belongs to.
#[derive(Clone, Debug)]
pub(super) struct RunIdentity {
    run_id: u64,
    run_attempt: u64,
    head_sha: String,
}

/// One build job the trusted workflow topology expects.
#[derive(Clone, Debug)]
pub(super) struct ExpectedJob {
    asset: String,
    role: Role,
    name: String,
    recipe_sha256: String,
}

impl ExpectedJob {
    /// Every canonical build and the Fastly reproduction are mandatory.
    ///
    /// The Fastly asset is identified by its compiled
    /// [`ReleaseTarget::Wasm32Wasip1`] target, not by its position in
    /// [`FIXED_ARTIFACTS`]. Native reproductions are optional measurements
    /// (spec R-REPEAT): they record an outcome but never block their asset.
    fn mandatory(&self) -> bool {
        self.role == Role::Canonical
            || FIXED_ARTIFACTS.iter().any(|fixed| {
                fixed.name == self.asset && fixed.target == ReleaseTarget::Wasm32Wasip1
            })
    }
}

/// The complete expected build-job topology for one run attempt.
#[derive(Clone, Debug)]
pub(super) struct ExpectedTopology {
    run: RunIdentity,
    jobs: Vec<ExpectedJob>,
    deadline_seconds: u64,
}

impl ExpectedTopology {
    /// Checks that the topology names each fixed build exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Collection`] for a zero run ID or attempt, a
    /// malformed source commit or recipe digest, an uncapped deadline, an
    /// unknown asset, an unsafe or reused job name, or a topology that does
    /// not contain exactly one canonical and one reproduction job per
    /// [`FIXED_ARTIFACTS`] entry.
    pub(super) fn new(
        run: RunIdentity,
        jobs: Vec<ExpectedJob>,
        deadline_seconds: u64,
    ) -> Result<Self> {
        if run.run_id == 0 || run.run_attempt == 0 || !lower_hex(&run.head_sha, 40) {
            return Err(collection_error("expected run identity is invalid"));
        }
        if !bounded(0, deadline_seconds) {
            return Err(collection_error("collector deadline is not bounded"));
        }
        let mut names = BTreeSet::new();
        let mut slots = BTreeSet::new();
        for job in &jobs {
            if !safe_job_name(&job.name)
                || !lower_hex(&job.recipe_sha256, 64)
                || !FIXED_ARTIFACTS.iter().any(|fixed| fixed.name == job.asset)
                || !names.insert(job.name.as_str())
                || !slots.insert((job.asset.as_str(), job.role.spec_label()))
            {
                return Err(collection_error(
                    "expected job is invalid, unknown or not unique",
                ));
            }
        }
        if slots.len() != FIXED_ARTIFACTS.len() * 2 {
            return Err(collection_error(
                "expected topology must name every fixed canonical and reproduction job",
            ));
        }
        Ok(Self {
            run,
            jobs,
            deadline_seconds,
        })
    }
}

/// The upload step outputs of one job: exact ID, name, digest and members.
///
/// The adapter passes these through unchanged and never derives them from the
/// jobs response; [`super::validate_transfers`] then corroborates them.
#[derive(Clone, Debug)]
pub(super) struct UploadOutput {
    artifact_id: u64,
    artifact_name: String,
    archive_sha256: String,
    members: BTreeMap<String, u64>,
}

/// Upload step outputs keyed by the expected job name that produced them.
#[derive(Clone, Debug)]
pub(super) struct JobUpload {
    job: String,
    output: UploadOutput,
}

/// Typed collector inputs; deliberately not [`super::TrustedDependencies`].
#[derive(Debug)]
pub(super) struct AdaptedJobs {
    required: Vec<Upload>,
    optional: Vec<OptionalBuild>,
}

/// Binds every expected job to one observed job and its upload outputs.
///
/// `elapsed_seconds` is the collector's own bounded wait when it fetched
/// `response`. A job without a terminal conclusion is accepted only once that
/// wait reached the topology deadline, and only for an optional native
/// reproduction. Mandatory jobs must have started, succeeded and uploaded.
///
/// An optional native reproduction never blocks (spec R-REPEAT). Its upload
/// output is used only when the job started and succeeded. Otherwise that
/// output is dropped unused and the job becomes a [`NoUpload`] with its
/// derived outcome (`failed` for a started or self-failed job), so a
/// reproduction that did not succeed can never lend its upload identity to
/// the collection.
///
/// Only a single fresh attempt is attestable: every expected job must have
/// run in the expected attempt, which holds for attempt 1 and for "re-run all
/// jobs". A "re-run failed jobs" attempt is rejected, because GitHub lists
/// its carried-over jobs with their older `run_attempt` (or omits them), and
/// their uploads were not produced by this attempt.
///
/// # Errors
///
/// Returns [`ReleaseError::Collection`] for a non-strict, oversized or
/// incomplete response; a job from another run, attempt or source; a missing,
/// duplicated or reused expected job; an unknown or contradictory GitHub
/// state; a premature or unbounded wait; a mandatory job without a successful
/// upload; or upload outputs naming no expected job or one twice.
pub(super) fn adapt_jobs(
    topology: &ExpectedTopology,
    response: &[u8],
    uploads: &[JobUpload],
    elapsed_seconds: u64,
) -> Result<AdaptedJobs> {
    if !bounded(elapsed_seconds, topology.deadline_seconds) {
        return Err(collection_error("collector wait exceeds its deadline"));
    }
    let observed = observe_jobs(topology, response)?;
    let mut outputs = BTreeMap::new();
    for upload in uploads {
        if !topology.jobs.iter().any(|job| job.name == upload.job)
            || outputs
                .insert(upload.job.as_str(), &upload.output)
                .is_some()
        {
            return Err(collection_error(
                "upload output does not name exactly one expected job",
            ));
        }
    }
    let at_deadline = elapsed_seconds == topology.deadline_seconds;
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for expected in &topology.jobs {
        let job = *observed
            .get(expected.name.as_str())
            .ok_or_else(|| collection_error("expected job is missing from the run"))?;
        if job.conclusion.is_none() && !at_deadline {
            return Err(collection_error(
                "job has no terminal conclusion before the collector deadline",
            ));
        }
        let output = outputs.get(expected.name.as_str()).copied();
        if expected.mandatory() {
            let output = output
                .filter(|_| job == SUCCEEDED)
                .ok_or_else(|| collection_error("mandatory job did not succeed with an upload"))?;
            required.push(upload(topology, expected, output, elapsed_seconds));
        } else if let Some(output) = output.filter(|_| job == SUCCEEDED) {
            optional.push(OptionalBuild::Uploaded {
                upload: upload(topology, expected, output, elapsed_seconds),
                job,
            });
        } else {
            // Any upload output of an unsuccessful optional job is dropped
            // here: it never becomes upload identity.
            let absent = NoUpload {
                asset: expected.asset.clone(),
                job,
                elapsed_seconds,
                deadline_seconds: topology.deadline_seconds,
            };
            if absent.outcome().is_none() {
                return Err(collection_error(
                    "optional job state has no honest failed or unavailable outcome",
                ));
            }
            optional.push(OptionalBuild::NotUploaded(absent));
        }
    }
    Ok(AdaptedJobs { required, optional })
}

fn upload(
    topology: &ExpectedTopology,
    expected: &ExpectedJob,
    output: &UploadOutput,
    elapsed_seconds: u64,
) -> Upload {
    Upload {
        asset: expected.asset.clone(),
        role: expected.role.spec_label().to_owned(),
        job: expected.name.clone(),
        attempt: topology.run.run_attempt,
        recipe_sha256: expected.recipe_sha256.clone(),
        elapsed_seconds,
        deadline_seconds: topology.deadline_seconds,
        artifact_name: output.artifact_name.clone(),
        artifact_id: output.artifact_id,
        archive_sha256: output.archive_sha256.clone(),
        members: output.members.clone(),
    }
}

/// Parses the strict jobs page and observes each expected job exactly once.
///
/// The page must be complete: `total_count` must equal the listed jobs and at
/// most [`JOB_PAGE_LIMIT`] may be listed. The future fetch must therefore
/// request `per_page=100`; GitHub's default page of 30 would be rejected as
/// incomplete for a larger run rather than merged across pages.
///
/// `run_attempt` is optional in GitHub's job schema but required here on every
/// listed job, together with the exact `run_id` and `head_sha`. A job listed
/// with another attempt, as a "re-run failed jobs" attempt lists its
/// carried-over jobs, rejects the whole response: only a single fresh attempt
/// is attestable (see [`adapt_jobs`]).
fn observe_jobs<'topology>(
    topology: &'topology ExpectedTopology,
    response: &[u8],
) -> Result<BTreeMap<&'topology str, ObservedJob>> {
    let value = parse_json(response, documents::Role::Metadata)
        .map_err(|_| collection_error("workflow jobs response is not strict bounded JSON"))?;
    let jobs = value["jobs"]
        .as_array()
        .filter(|jobs| jobs.len() <= JOB_PAGE_LIMIT)
        .filter(|jobs| value["total_count"].as_u64() == Some(jobs.len() as u64))
        .ok_or_else(|| collection_error("workflow jobs response is not one complete page"))?;
    let run = &topology.run;
    let mut ids = BTreeSet::new();
    let mut observed = BTreeMap::new();
    for job in jobs {
        let name = job["name"]
            .as_str()
            .ok_or_else(|| collection_error("workflow job has no name"))?;
        let id = job["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| collection_error("workflow job has no valid ID"))?;
        // Every listed job must belong to this run attempt and source, so a
        // response for another run cannot be mixed in through unrelated jobs.
        if !ids.insert(id)
            || job["run_id"].as_u64() != Some(run.run_id)
            || job["head_sha"].as_str() != Some(run.head_sha.as_str())
        {
            return Err(collection_error(
                "workflow job is reused or belongs to another run or source",
            ));
        }
        match job["run_attempt"].as_u64() {
            Some(attempt) if attempt == run.run_attempt => {}
            Some(_) => {
                return Err(collection_error(
                    "workflow job ran in another attempt; only a single fresh attempt is attestable",
                ));
            }
            None => return Err(collection_error("workflow job has no run_attempt")),
        }
        let Some(expected) = topology.jobs.iter().find(|expected| expected.name == name) else {
            continue;
        };
        if observed
            .insert(expected.name.as_str(), observe_job(job)?)
            .is_some()
        {
            return Err(collection_error("expected job name occurs more than once"));
        }
    }
    Ok(observed)
}

/// Derives the started flag from executed steps and maps the conclusion.
///
/// GitHub's `started_at` is ignored: a job skipped or cancelled before any
/// step may carry it.
fn observe_job(job: &Value) -> Result<ObservedJob> {
    let status = job["status"]
        .as_str()
        .ok_or_else(|| collection_error("workflow job has no status"))?;
    let waiting = match status {
        "queued" | "pending" | "waiting" | "requested" => true,
        "in_progress" | "completed" => false,
        _ => return Err(collection_error("workflow job status is unknown")),
    };
    let conclusion = match (status, &job["conclusion"]) {
        ("completed", Value::String(text)) => Some(job_conclusion(text)?),
        (_, Value::Null) if status != "completed" => None,
        _ => {
            return Err(collection_error(
                "workflow job status and conclusion disagree",
            ));
        }
    };
    let steps = job["steps"]
        .as_array()
        .filter(|steps| steps.len() <= STEP_LIMIT)
        .ok_or_else(|| collection_error("workflow job has no bounded step list"))?;
    let mut numbers = BTreeSet::new();
    let mut started = false;
    for step in steps {
        let number = step["number"].as_u64();
        if step["name"].as_str().is_none() || !number.is_some_and(|number| numbers.insert(number)) {
            return Err(collection_error("workflow step is unnamed or renumbered"));
        }
        started |= step_executed(step)?;
    }
    if started && waiting {
        return Err(collection_error(
            "waiting workflow job reports an executed step",
        ));
    }
    Ok(ObservedJob {
        started,
        conclusion,
    })
}

/// Maps a GitHub job conclusion to [`JobConclusion`].
///
/// `neutral`, `action_required`, `stale`, `startup_failure` and any other value
/// are rejected: the spec defines no failed/unavailable meaning for them, so
/// the collector fails closed rather than guessing an outcome.
fn job_conclusion(text: &str) -> Result<JobConclusion> {
    match text {
        "success" => Ok(JobConclusion::Success),
        "failure" => Ok(JobConclusion::Failure),
        "cancelled" => Ok(JobConclusion::Cancelled),
        "skipped" => Ok(JobConclusion::Skipped),
        "timed_out" => Ok(JobConclusion::TimedOut),
        _ => Err(collection_error(
            "workflow job conclusion has no collector meaning",
        )),
    }
}

/// Reports whether one step actually ran.
///
/// A step ran when it is in progress or completed with any conclusion other
/// than `skipped`. Unknown or contradictory step states are rejected.
fn step_executed(step: &Value) -> Result<bool> {
    match (step["status"].as_str(), &step["conclusion"]) {
        (Some("queued" | "pending"), Value::Null) => Ok(false),
        (Some("in_progress"), Value::Null) => Ok(true),
        (Some("completed"), Value::String(conclusion)) => match conclusion.as_str() {
            "skipped" => Ok(false),
            "success" | "failure" | "cancelled" => Ok(true),
            _ => Err(collection_error(
                "workflow step conclusion has no collector meaning",
            )),
        },
        _ => Err(collection_error(
            "workflow step status and conclusion disagree",
        )),
    }
}

fn safe_job_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= JOB_NAME_LIMIT && !name.chars().any(char::is_control)
}
