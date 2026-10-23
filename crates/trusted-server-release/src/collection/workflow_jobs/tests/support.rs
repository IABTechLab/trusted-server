//! Fictional GitHub Actions jobs fixtures shared by the adapter tests.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::collection::records::Outcome;
use crate::collection::tests::zip;
use crate::collection::workflow_jobs::{
    AdaptedJobs, ExpectedJob, ExpectedTopology, JobUpload, Result, RunIdentity, UploadOutput,
};
use crate::collection::{OptionalBuild, digest};
use crate::policy::FIXED_ARTIFACTS;
use crate::recipe::Role;

/// Fictional run identity used by every fixture job.
pub(super) const RUN_ID: u64 = 7001;
pub(super) const RUN_ATTEMPT: u64 = 2;
pub(super) const DEADLINE: u64 = 1800;

pub(super) fn head_sha() -> String {
    "a".repeat(40)
}

fn job_name(asset: &str, role: Role) -> String {
    let label = match role {
        Role::Canonical => "canonical",
        Role::Reproduction => "reproduction",
    };
    format!("build {asset} {label}")
}

pub(super) fn expected_jobs() -> Vec<ExpectedJob> {
    FIXED_ARTIFACTS
        .iter()
        .flat_map(|asset| {
            [Role::Canonical, Role::Reproduction].map(|role| ExpectedJob {
                asset: asset.name.to_owned(),
                role,
                name: job_name(asset.name, role),
                recipe_sha256: "c".repeat(64),
            })
        })
        .collect()
}

pub(super) fn run_identity() -> RunIdentity {
    RunIdentity {
        run_id: RUN_ID,
        run_attempt: RUN_ATTEMPT,
        head_sha: head_sha(),
    }
}

pub(super) fn topology() -> ExpectedTopology {
    ExpectedTopology::new(run_identity(), expected_jobs(), DEADLINE)
        .expect("should accept the complete fictional topology")
}

pub(super) fn step(number: u64, status: &str, conclusion: Value) -> Value {
    let mut step = json!({
        "name": format!("Step {number}"),
        "status": status,
        "number": number,
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": "2026-01-01T00:01:00Z",
    });
    step["conclusion"] = conclusion;
    step
}

pub(super) fn executed_steps() -> Vec<Value> {
    (1..=3)
        .map(|number| step(number, "completed", json!("success")))
        .collect()
}

pub(super) fn skipped_steps() -> Vec<Value> {
    (1..=3)
        .map(|number| step(number, "completed", json!("skipped")))
        .collect()
}

pub(super) fn job(
    id: u64,
    name: &str,
    status: &str,
    conclusion: Value,
    steps: Vec<Value>,
) -> Value {
    let mut job = json!({
        "id": id,
        "name": name,
        "run_id": RUN_ID,
        "run_attempt": RUN_ATTEMPT,
        "head_sha": head_sha(),
        "status": status,
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": "2026-01-01T00:05:00Z",
        "html_url": "https://example.com/job",
    });
    job["conclusion"] = conclusion;
    job["steps"] = Value::Array(steps);
    job
}

/// Every expected job succeeded with executed steps, plus one unrelated job.
pub(super) fn successful_jobs() -> Vec<Value> {
    let mut jobs = expected_jobs()
        .iter()
        .enumerate()
        .map(|(index, expected)| {
            job(
                100 + index as u64,
                &expected.name,
                "completed",
                json!("success"),
                executed_steps(),
            )
        })
        .collect::<Vec<_>>();
    jobs.push(job(
        900,
        "unrelated JavaScript job",
        "completed",
        json!("success"),
        executed_steps(),
    ));
    jobs
}

pub(super) fn response(jobs: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({"total_count": jobs.len(), "jobs": jobs}))
        .expect("should serialize fictional jobs response")
}

pub(super) fn upload(name: &str, artifact_id: u64) -> JobUpload {
    JobUpload {
        job: name.to_owned(),
        output: UploadOutput {
            artifact_id,
            artifact_name: format!("upload-{artifact_id}"),
            archive_sha256: digest(&archive()),
            members: BTreeMap::from([("artifact".to_owned(), 1)]),
        },
    }
}

pub(super) fn archive() -> Vec<u8> {
    zip(&[("artifact", b"x")], false)
}

pub(super) fn all_uploads() -> Vec<JobUpload> {
    expected_jobs()
        .iter()
        .enumerate()
        .map(|(index, expected)| upload(&expected.name, 500 + index as u64))
        .collect()
}

pub(super) fn is_native_reproduction(expected: &ExpectedJob) -> bool {
    expected.role == Role::Reproduction && !expected.mandatory()
}

/// Replaces every native reproduction job and drops its upload.
pub(super) fn with_native_reproductions(
    status: &str,
    conclusion: &Value,
    steps: &[Value],
) -> (Vec<Value>, Vec<JobUpload>) {
    let expected = expected_jobs();
    let mut jobs = successful_jobs();
    for (index, slot) in expected.iter().enumerate() {
        if is_native_reproduction(slot) {
            jobs[index] = job(
                100 + index as u64,
                &slot.name,
                status,
                conclusion.clone(),
                steps.to_vec(),
            );
        }
    }
    let uploads = all_uploads()
        .into_iter()
        .filter(|upload| {
            !expected
                .iter()
                .any(|slot| slot.name == upload.job && is_native_reproduction(slot))
        })
        .collect();
    (jobs, uploads)
}

/// Requires a rejection whose report names the expected reason.
///
/// A rejection for an unrelated reason would let a mutated fixture pass
/// without exercising the check under test.
pub(super) fn assert_rejected<T: std::fmt::Debug>(result: Result<T>, reason: &str) {
    let error = result.expect_err("should reject the observation");
    assert!(
        format!("{error:?}").contains(reason),
        "should reject because {reason:?}, got {error:?}"
    );
}

pub(super) fn optional_outcomes(adapted: &AdaptedJobs) -> Vec<Option<Outcome>> {
    adapted
        .optional
        .iter()
        .map(|build| match build {
            OptionalBuild::Uploaded { .. } => None,
            OptionalBuild::NotUploaded(absent) => absent.outcome(),
        })
        .collect()
}
