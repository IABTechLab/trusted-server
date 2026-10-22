use serde_json::{Value, json};

use super::*;
use crate::collection::records::Outcome;

mod support;
mod transfer;

use support::{
    DEADLINE, RUN_ATTEMPT, RUN_ID, all_uploads, archive, assert_rejected, executed_steps,
    expected_jobs, head_sha, job, optional_outcomes, response, run_identity, skipped_steps, step,
    successful_jobs, topology, upload, with_native_reproductions,
};

#[test]
fn successful_run_binds_every_fixed_asset_to_its_exact_job_and_upload() {
    let adapted = adapt_jobs(
        &topology(),
        &response(&successful_jobs()),
        &all_uploads(),
        600,
    )
    .expect("should adapt a complete successful run");

    assert_eq!(
        adapted.required.len(),
        5,
        "should require four canonical builds and the Fastly reproduction"
    );
    assert_eq!(
        adapted.optional.len(),
        3,
        "should carry one optional native reproduction per native asset"
    );
    for upload in &adapted.required {
        assert_eq!(upload.attempt, RUN_ATTEMPT, "should bind the run attempt");
        assert_eq!(upload.elapsed_seconds, 600, "should keep the observed wait");
        assert_eq!(
            upload.deadline_seconds, DEADLINE,
            "should keep the deadline"
        );
        assert!(
            upload.role == "A" || upload.asset == FIXED_ARTIFACTS[0].name,
            "should place only canonical builds and the Fastly reproduction in the mandatory channel"
        );
        let expected = all_uploads()
            .into_iter()
            .find(|candidate| candidate.job == upload.job)
            .expect("should find the upload supplied for this job");
        assert_eq!(
            upload.artifact_id, expected.output.artifact_id,
            "should keep the exact upload step identity"
        );
    }
    assert!(
        adapted.optional.iter().all(|build| matches!(
            build,
            OptionalBuild::Uploaded { upload, job: ObservedJob { started: true, conclusion: Some(JobConclusion::Success) } }
                if upload.role == "B" && upload.asset != FIXED_ARTIFACTS[0].name
        )),
        "should feed successful native reproduction uploads into the optional channel"
    );
}

#[test]
fn only_the_wasm_asset_reproduction_is_mandatory() {
    for expected in expected_jobs() {
        let wasm = FIXED_ARTIFACTS.iter().any(|fixed| {
            fixed.name == expected.asset && fixed.target == ReleaseTarget::Wasm32Wasip1
        });
        assert_eq!(
            expected.mandatory(),
            expected.role == Role::Canonical || wasm,
            "should make every canonical build and only the Wasm reproduction mandatory"
        );
    }
}

/// Label, job status, conclusion, steps, elapsed wait and derived outcome.
type NativeReproductionState = (&'static str, &'static str, Value, Vec<Value>, u64, Outcome);

/// Every native reproduction job state with its derived outcome.
fn native_reproduction_states() -> Vec<NativeReproductionState> {
    let partially_cancelled = vec![
        step(1, "completed", json!("success")),
        step(2, "completed", json!("cancelled")),
        step(3, "completed", json!("skipped")),
    ];
    let running = vec![
        step(1, "completed", json!("success")),
        step(2, "in_progress", Value::Null),
    ];
    vec![
        (
            "succeeded without an upload",
            "completed",
            json!("success"),
            executed_steps(),
            600,
            Outcome::Failed,
        ),
        (
            "failed",
            "completed",
            json!("failure"),
            executed_steps(),
            600,
            Outcome::Failed,
        ),
        (
            "timed out",
            "completed",
            json!("timed_out"),
            executed_steps(),
            600,
            Outcome::Failed,
        ),
        (
            "started then cancelled",
            "completed",
            json!("cancelled"),
            partially_cancelled,
            600,
            Outcome::Failed,
        ),
        (
            "skipped by the scheduler",
            "completed",
            json!("skipped"),
            Vec::new(),
            600,
            Outcome::Unavailable,
        ),
        (
            "cancelled before any step",
            "completed",
            json!("cancelled"),
            skipped_steps(),
            600,
            Outcome::Unavailable,
        ),
        (
            "still queued at the deadline",
            "queued",
            Value::Null,
            Vec::new(),
            DEADLINE,
            Outcome::Unavailable,
        ),
        (
            "still running at the deadline",
            "in_progress",
            Value::Null,
            running,
            DEADLINE,
            Outcome::Failed,
        ),
    ]
}

#[test]
fn native_reproduction_job_states_derive_bounded_honest_outcomes() {
    for (label, status, conclusion, steps, elapsed, outcome) in native_reproduction_states() {
        let (jobs, uploads) = with_native_reproductions(status, &conclusion, &steps);
        let adapted = adapt_jobs(&topology(), &response(&jobs), &uploads, elapsed)
            .unwrap_or_else(|error| panic!("should adapt native reproduction {label}: {error:?}"));
        assert_eq!(
            adapted.required.len(),
            5,
            "should keep every mandatory job when native reproduction {label}"
        );
        assert_eq!(
            optional_outcomes(&adapted),
            vec![Some(outcome); 3],
            "should derive the expected outcome when native reproduction {label}"
        );
    }
}

#[test]
fn started_at_without_an_executed_step_is_not_started() {
    // GitHub sets `started_at` on these jobs; only executed steps count.
    for steps in [Vec::new(), skipped_steps()] {
        let (jobs, uploads) = with_native_reproductions("completed", &json!("cancelled"), &steps);
        let adapted = adapt_jobs(&topology(), &response(&jobs), &uploads, 600)
            .expect("should adapt cancelled native reproductions");
        assert!(
            adapted.optional.iter().all(|build| matches!(
                build,
                OptionalBuild::NotUploaded(absent) if !absent.job.started
            )),
            "should not treat started_at as an executed step"
        );
    }
}

#[test]
fn a_job_without_a_conclusion_is_rejected_before_the_deadline_and_accepted_at_it() {
    for (status, steps) in [
        ("queued", Vec::new()),
        (
            "in_progress",
            vec![
                step(1, "completed", json!("success")),
                step(2, "in_progress", Value::Null),
            ],
        ),
    ] {
        let (jobs, uploads) = with_native_reproductions(status, &Value::Null, &steps);
        assert_rejected(
            adapt_jobs(&topology(), &response(&jobs), &uploads, DEADLINE - 1),
            "before the collector deadline",
        );
        adapt_jobs(&topology(), &response(&jobs), &uploads, DEADLINE)
            .expect("should accept a conclusion-less observation at the deadline");
    }
    assert_rejected(
        adapt_jobs(
            &topology(),
            &response(&successful_jobs()),
            &all_uploads(),
            DEADLINE + 1,
        ),
        "exceeds its deadline",
    );
}

#[test]
fn mandatory_jobs_must_start_succeed_and_upload_under_every_wait() {
    let mut cases = Vec::new();
    for (status, conclusion, steps) in [
        ("completed", json!("failure"), executed_steps()),
        ("completed", json!("cancelled"), skipped_steps()),
        ("completed", json!("skipped"), Vec::new()),
        ("queued", Value::Null, Vec::new()),
        ("waiting", Value::Null, Vec::new()),
        ("requested", Value::Null, Vec::new()),
    ] {
        for index in [0, 1, 2] {
            let mut jobs = successful_jobs();
            let name = expected_jobs()[index].name.clone();
            jobs[index] = job(
                100 + index as u64,
                &name,
                status,
                conclusion.clone(),
                steps.clone(),
            );
            cases.push((jobs, all_uploads()));
        }
    }
    for index in [0, 1, 2] {
        let mut uploads = all_uploads();
        uploads.remove(index);
        cases.push((successful_jobs(), uploads));
    }
    for (jobs, uploads) in cases {
        let unconcluded = jobs.iter().any(|job| job["conclusion"].is_null());
        for elapsed in [600, DEADLINE] {
            let reason = if unconcluded && elapsed < DEADLINE {
                "before the collector deadline"
            } else {
                "mandatory job did not succeed with an upload"
            };
            assert_rejected(
                adapt_jobs(&topology(), &response(&jobs), &uploads, elapsed),
                reason,
            );
        }
    }
}

#[test]
fn completed_job_without_a_conclusion_is_rejected_for_every_role() {
    for index in [0, 1, 3] {
        let mut jobs = successful_jobs();
        jobs[index]["conclusion"] = Value::Null;
        for elapsed in [600, DEADLINE] {
            assert_rejected(
                adapt_jobs(&topology(), &response(&jobs), &all_uploads(), elapsed),
                "job status and conclusion disagree",
            );
        }
    }
}

#[test]
fn jobs_from_another_run_or_source_are_rejected() {
    for (field, value) in [
        ("run_id", json!(RUN_ID + 1)),
        ("head_sha", json!("d".repeat(40))),
    ] {
        for index in [0, 3, 8] {
            let mut jobs = successful_jobs();
            jobs[index][field] = value.clone();
            assert_rejected(
                adapt_jobs(&topology(), &response(&jobs), &all_uploads(), 600),
                "another run or source",
            );
        }
    }
}

#[test]
fn only_a_single_fresh_attempt_is_attestable() {
    // A "re-run failed jobs" attempt lists carried-over jobs with their older
    // attempt, whether they are expected or unrelated.
    for index in [0, 3, 8] {
        let mut carried_over = successful_jobs();
        carried_over[index]["run_attempt"] = json!(RUN_ATTEMPT - 1);
        assert_rejected(
            adapt_jobs(&topology(), &response(&carried_over), &all_uploads(), 600),
            "only a single fresh attempt is attestable",
        );
        let mut unattributed = successful_jobs();
        unattributed[index]
            .as_object_mut()
            .expect("should be a job object")
            .remove("run_attempt");
        assert_rejected(
            adapt_jobs(&topology(), &response(&unattributed), &all_uploads(), 600),
            "has no run_attempt",
        );
    }
}

#[test]
fn duplicate_missing_or_extra_job_names_are_rejected() {
    let expected = expected_jobs();
    let mut duplicate = successful_jobs();
    duplicate.push(job(
        901,
        &expected[0].name,
        "completed",
        json!("success"),
        executed_steps(),
    ));
    let mut extra_skipped = successful_jobs();
    extra_skipped.push(job(
        902,
        &expected[3].name,
        "completed",
        json!("skipped"),
        Vec::new(),
    ));
    let mut missing_mandatory = successful_jobs();
    missing_mandatory.remove(0);
    let mut missing_optional = successful_jobs();
    missing_optional.remove(3);
    let mut reused_id = successful_jobs();
    reused_id[1]["id"] = json!(100);
    for (jobs, reason) in [
        (duplicate, "occurs more than once"),
        (extra_skipped, "occurs more than once"),
        (missing_mandatory, "missing from the run"),
        (missing_optional, "missing from the run"),
        (reused_id, "is reused"),
    ] {
        assert_rejected(
            adapt_jobs(&topology(), &response(&jobs), &all_uploads(), 600),
            reason,
        );
    }
}

#[test]
fn upload_outputs_must_name_exactly_one_expected_job() {
    let mut unknown = all_uploads();
    unknown.push(upload("unrelated JavaScript job", 990));
    let mut duplicate = all_uploads();
    duplicate.push(upload(&expected_jobs()[0].name, 991));
    for uploads in [unknown, duplicate] {
        assert_rejected(
            adapt_jobs(&topology(), &response(&successful_jobs()), &uploads, 600),
            "does not name exactly one expected job",
        );
    }
}

#[test]
fn unsuccessful_optional_reproduction_upload_output_is_dropped_without_blocking() {
    for (status, conclusion, steps, elapsed, outcome) in [
        (
            "completed",
            json!("failure"),
            executed_steps(),
            600,
            Outcome::Failed,
        ),
        (
            "completed",
            json!("timed_out"),
            executed_steps(),
            600,
            Outcome::Failed,
        ),
        (
            "in_progress",
            Value::Null,
            vec![step(1, "in_progress", Value::Null)],
            DEADLINE,
            Outcome::Failed,
        ),
        (
            "completed",
            json!("cancelled"),
            skipped_steps(),
            600,
            Outcome::Unavailable,
        ),
        (
            "queued",
            Value::Null,
            Vec::new(),
            DEADLINE,
            Outcome::Unavailable,
        ),
    ] {
        let (jobs, _) = with_native_reproductions(status, &conclusion, &steps);
        let adapted = adapt_jobs(&topology(), &response(&jobs), &all_uploads(), elapsed)
            .expect("should not let an optional reproduction block the collection");
        assert_eq!(
            adapted.required.len(),
            5,
            "should keep every mandatory upload"
        );
        assert_eq!(
            optional_outcomes(&adapted),
            vec![Some(outcome); 3],
            "should record the derived outcome instead of the upload identity"
        );
    }
}

#[test]
fn unknown_or_inconsistent_github_states_are_rejected() {
    let mut cases = Vec::new();
    for conclusion in [
        "neutral",
        "action_required",
        "stale",
        "startup_failure",
        "SUCCESS",
    ] {
        cases.push((
            with_native_reproductions("completed", &json!(conclusion), &executed_steps()),
            "job conclusion has no collector meaning",
        ));
    }
    for (status, conclusion, steps, reason) in [
        (
            "completed",
            Value::Null,
            executed_steps(),
            "job status and conclusion disagree",
        ),
        (
            "in_progress",
            json!("success"),
            executed_steps(),
            "job status and conclusion disagree",
        ),
        (
            "queued",
            Value::Null,
            executed_steps(),
            "waiting workflow job reports an executed step",
        ),
        ("unknown", Value::Null, Vec::new(), "job status is unknown"),
        (
            "completed",
            json!("failure"),
            vec![step(1, "completed", json!("neutral"))],
            "step conclusion has no collector meaning",
        ),
        (
            "completed",
            json!("failure"),
            vec![step(1, "completed", Value::Null)],
            "step status and conclusion disagree",
        ),
        (
            "completed",
            json!("failure"),
            vec![step(1, "queued", json!("success"))],
            "step status and conclusion disagree",
        ),
        (
            "completed",
            json!("failure"),
            vec![
                step(1, "completed", json!("success")),
                step(1, "completed", json!("success")),
            ],
            "unnamed or renumbered",
        ),
    ] {
        cases.push((
            with_native_reproductions(status, &conclusion, &steps),
            reason,
        ));
    }
    let (mut jobs, uploads) = with_native_reproductions("completed", &json!("failure"), &[]);
    jobs[3]
        .as_object_mut()
        .expect("should be a job object")
        .remove("steps");
    cases.push(((jobs, uploads), "no bounded step list"));
    for ((jobs, uploads), reason) in cases {
        assert_rejected(
            adapt_jobs(&topology(), &response(&jobs), &uploads, DEADLINE),
            reason,
        );
    }
}

#[test]
fn a_truncated_default_page_is_rejected_as_incomplete() {
    // A fetch without `per_page=100` lists at most 30 of a larger run.
    let jobs = successful_jobs();
    let truncated = serde_json::to_vec(&json!({"total_count": jobs.len() + 31, "jobs": jobs}))
        .expect("should serialize fictional jobs response");
    assert_rejected(
        adapt_jobs(&topology(), &truncated, &all_uploads(), 600),
        "not one complete page",
    );
}

#[test]
fn malformed_oversized_incomplete_or_duplicate_key_responses_are_rejected() {
    const STRICT: &str = "not strict bounded JSON";
    const PAGE: &str = "not one complete page";
    let mut cases = vec![
        (b"not json".to_vec(), STRICT),
        (
            b"{\"total_count\":0,\"jobs\":[],\"jobs\":[]}".to_vec(),
            STRICT,
        ),
        (
            serde_json::to_vec(&json!({"jobs": successful_jobs()})).expect("should serialize"),
            PAGE,
        ),
        (
            serde_json::to_vec(&json!({"total_count": 99, "jobs": successful_jobs()}))
                .expect("should serialize"),
            PAGE,
        ),
        (
            serde_json::to_vec(&json!([successful_jobs()])).expect("should serialize"),
            PAGE,
        ),
    ];
    let mut oversized = response(&successful_jobs());
    oversized.extend(vec![b' '; 1024 * 1024]);
    cases.push((oversized, STRICT));
    let many = (0..101)
        .map(|index| {
            job(
                2000 + index,
                &format!("unrelated {index}"),
                "completed",
                json!("success"),
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    cases.push((response(&many), PAGE));
    let duplicate_job_key = String::from_utf8(response(&successful_jobs()))
        .expect("should be UTF-8 fixture")
        .replacen("\"name\":", "\"name\":\"shadow\",\"name\":", 1);
    cases.push((duplicate_job_key.into_bytes(), STRICT));
    for (body, reason) in cases {
        assert_rejected(adapt_jobs(&topology(), &body, &all_uploads(), 600), reason);
    }
}

#[test]
fn expected_topology_must_name_every_fixed_build_exactly_once() {
    let mut missing = expected_jobs();
    missing.pop();
    assert_rejected(
        ExpectedTopology::new(run_identity(), missing, DEADLINE),
        "must name every fixed canonical and reproduction job",
    );
    let mut cases = Vec::new();
    let mut duplicate_slot = expected_jobs();
    duplicate_slot[7].role = Role::Canonical;
    cases.push(duplicate_slot);
    let mut duplicate_name = expected_jobs();
    duplicate_name[7].name = duplicate_name[6].name.clone();
    cases.push(duplicate_name);
    let mut unknown_asset = expected_jobs();
    unknown_asset[7].asset = "trusted-server-other".to_owned();
    cases.push(unknown_asset);
    for name in [String::new(), "x".repeat(129), "line\nbreak".to_owned()] {
        let mut invalid_name = expected_jobs();
        invalid_name[7].name = name;
        cases.push(invalid_name);
    }
    let mut invalid_recipe = expected_jobs();
    invalid_recipe[7].recipe_sha256 = "C".repeat(64);
    cases.push(invalid_recipe);
    for jobs in cases {
        assert_rejected(
            ExpectedTopology::new(run_identity(), jobs, DEADLINE),
            "expected job is invalid, unknown or not unique",
        );
    }
    const IDENTITY: &str = "expected run identity is invalid";
    const DEADLINE_BOUND: &str = "collector deadline is not bounded";
    for (identity, deadline, reason) in [
        (
            RunIdentity {
                run_id: 0,
                ..run_identity()
            },
            DEADLINE,
            IDENTITY,
        ),
        (
            RunIdentity {
                run_attempt: 0,
                ..run_identity()
            },
            DEADLINE,
            IDENTITY,
        ),
        (
            RunIdentity {
                head_sha: "a".repeat(39),
                ..run_identity()
            },
            DEADLINE,
            IDENTITY,
        ),
        (run_identity(), 0, DEADLINE_BOUND),
        (run_identity(), 5401, DEADLINE_BOUND),
    ] {
        assert_rejected(
            ExpectedTopology::new(identity, expected_jobs(), deadline),
            reason,
        );
    }
}
