use serde_json::{Value, json};

use super::bind;

fn encoded(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("should encode fixture")
}

pub(in crate::ordinary) fn request() -> Value {
    json!({
        "repository": "example-owner/example-server",
        "repository_id": 17,
        "head_repository_id": 17,
        "source_commit": "a".repeat(40),
        "run_id": 42,
        "run_attempt": 1,
        "canonical": {
            "artifact_id": 200,
            "artifact_digest": "b".repeat(64),
            "observation_id": 201,
            "observation_digest": "c".repeat(64)
        },
        "reproduction": {
            "artifact_id": 202,
            "artifact_digest": "d".repeat(64),
            "observation_id": 203,
            "observation_digest": "e".repeat(64)
        }
    })
}

pub(in crate::ordinary) fn jobs() -> Value {
    let jobs: Vec<_> = [
        (100, "Build trusted-server-fastly.wasm", "canonical", "2023-11-14T22:13:10Z", "2023-11-14T22:13:30Z"),
        (101, "Reproduce trusted-server-fastly.wasm", "reproduction", "2023-11-14T22:28:10Z", "2023-11-14T22:28:30Z"),
    ]
    .into_iter()
    .map(|(id, name, role, start, end)| {
        json!({
            "id": id, "name": format!("build / {name}"), "run_id": 42,
            "run_attempt": 1, "head_sha": "a".repeat(40),
            "status": "completed", "conclusion": "success",
            "started_at": start, "completed_at": end,
            "steps": [
                {"name": format!("Upload {role} release artifact"), "status": "completed", "conclusion": "success"},
                {"name": format!("Upload {role} observation"), "status": "completed", "conclusion": "success"}
            ]
        })
    }).collect();
    json!({"total_count": jobs.len(), "jobs": jobs})
}

pub(in crate::ordinary) fn artifacts() -> Value {
    let artifacts: Vec<_> = [
        (200, "release-trusted-server-fastly.wasm", "b", "2023-11-14T22:13:25Z"),
        (201, "fastly-observation-canonical", "c", "2023-11-14T22:13:26Z"),
        (202, "reproduction-trusted-server-fastly.wasm", "d", "2023-11-14T22:28:25Z"),
        (203, "fastly-observation-reproduction", "e", "2023-11-14T22:28:26Z"),
    ].into_iter().map(|(id, name, hex, created)| json!({
        "id": id, "name": name, "digest": format!("sha256:{}", hex.repeat(64)),
        "size_in_bytes": 42, "expired": false, "created_at": created,
        "workflow_run": {"id": 42, "repository_id": 17, "head_repository_id": 17, "head_sha": "a".repeat(40)}
    })).collect();
    json!({"total_count": artifacts.len(), "artifacts": artifacts})
}

#[test]
fn binds_direct_upload_outputs_to_successful_current_attempt_jobs() {
    let bound = bind(
        &encoded(&request()),
        &encoded(&jobs()),
        &encoded(&artifacts()),
    )
    .expect("should bind exact current-run upload identities");
    assert_eq!(
        bound.job_ids(),
        [100, 101],
        "should retain distinct producing jobs"
    );
}

#[test]
fn rejects_duplicate_service_job_ids_even_under_an_unrelated_name() {
    let mut response = jobs();
    let mut duplicate = response["jobs"][0].clone();
    duplicate["name"] = json!("Unrelated job");
    response["jobs"]
        .as_array_mut()
        .expect("should hold jobs")
        .push(duplicate);
    response["total_count"] = json!(3);
    assert!(
        bind(
            &encoded(&request()),
            &encoded(&response),
            &encoded(&artifacts())
        )
        .is_err(),
        "should reject globally inconsistent service job identities"
    );
}

#[test]
fn rejects_uploads_before_job_start_even_within_the_same_second() {
    let mut jobs = jobs();
    let mut artifacts = artifacts();
    jobs["jobs"][0]["started_at"] = json!("2023-11-14T22:13:10.900Z");
    artifacts["artifacts"][0]["created_at"] = json!("2023-11-14T22:13:10.100Z");
    assert!(
        bind(&encoded(&request()), &encoded(&jobs), &encoded(&artifacts)).is_err(),
        "should retain timestamp precision when binding an upload to its job"
    );
}

#[test]
fn rejects_foreign_source_runs_attempts_and_unsuccessful_jobs() {
    for (pointer, value) in [
        ("/jobs/0/run_id", json!(43)),
        ("/jobs/0/run_attempt", json!(2)),
        ("/jobs/0/head_sha", json!("f".repeat(40))),
        ("/jobs/0/status", json!("in_progress")),
        ("/jobs/0/conclusion", json!("failure")),
        ("/jobs/0/run_attempt", Value::Null),
    ] {
        let mut response = jobs();
        *response
            .pointer_mut(pointer)
            .expect("should locate fixture field") = value;
        let error = bind(
            &encoded(&request()),
            &encoded(&response),
            &encoded(&artifacts()),
        )
        .expect_err("should reject a stale, foreign or unsuccessful producing job");
        assert!(
            format!("{error:?}")
                .contains("producing job is unsuccessful, stale or from another source"),
            "should reach the source/run/attempt join for {pointer}"
        );
    }
}

#[test]
fn rejects_reused_upload_outputs_and_malformed_workflow_context() {
    for (pointer, value) in [
        ("/reproduction/artifact_id", json!(200)),
        ("/canonical/artifact_id", json!(0)),
        ("/canonical/artifact_digest", json!("B".repeat(64))),
        ("/repository", json!("../example-server")),
        ("/source_commit", json!("a".repeat(39))),
        ("/run_attempt", json!(0)),
        ("/repository_id", json!(9_007_199_254_740_992_u64)),
    ] {
        let mut response = request();
        *response
            .pointer_mut(pointer)
            .expect("should locate fixture field") = value;
        assert!(
            bind(
                &encoded(&response),
                &encoded(&jobs()),
                &encoded(&artifacts())
            )
            .is_err(),
            "should reject an invalid trusted-context/output shape at {pointer}"
        );
    }
}

#[test]
fn rejects_upload_metadata_disagreeing_with_the_direct_output_bridge() {
    for (pointer, value) in [
        ("/artifacts/0/name", json!("other-build")),
        (
            "/artifacts/0/digest",
            json!(format!("sha256:{}", "f".repeat(64))),
        ),
        ("/artifacts/0/expired", json!(true)),
        ("/artifacts/0/size_in_bytes", json!(0)),
        (
            "/artifacts/0/size_in_bytes",
            json!(768 * 1024 * 1024 + 1_u64),
        ),
        ("/artifacts/0/workflow_run/id", json!(43)),
        ("/artifacts/0/workflow_run/repository_id", json!(18)),
        ("/artifacts/0/workflow_run/head_repository_id", json!(18)),
        ("/artifacts/0/workflow_run/head_sha", json!("f".repeat(40))),
    ] {
        let mut response = artifacts();
        *response
            .pointer_mut(pointer)
            .expect("should locate fixture field") = value;
        assert!(
            bind(&encoded(&request()), &encoded(&jobs()), &encoded(&response)).is_err(),
            "should reject service metadata mismatching the direct output at {pointer}"
        );
    }
}

#[test]
fn rejects_uploads_outside_their_successful_producing_job() {
    for value in [
        "2023-11-14T22:13:09Z",
        "2023-11-14T22:13:31Z",
        "not-a-service-timestamp",
    ] {
        let mut response = artifacts();
        response["artifacts"][0]["created_at"] = json!(value);
        assert!(
            bind(&encoded(&request()), &encoded(&jobs()), &encoded(&response)).is_err(),
            "should reject stale, future or invalid upload creation time"
        );
    }
}

#[test]
fn rejects_missing_failed_and_duplicate_upload_steps() {
    for value in [json!("failure"), json!("skipped"), Value::Null] {
        let mut response = jobs();
        response["jobs"][0]["steps"][0]["conclusion"] = value;
        assert!(
            bind(
                &encoded(&request()),
                &encoded(&response),
                &encoded(&artifacts())
            )
            .is_err(),
            "should require the exact successful upload step"
        );
    }
    let mut response = jobs();
    let duplicate = response["jobs"][0]["steps"][0].clone();
    response["jobs"][0]["steps"]
        .as_array_mut()
        .expect("should list steps")
        .push(duplicate);
    assert!(
        bind(
            &encoded(&request()),
            &encoded(&response),
            &encoded(&artifacts())
        )
        .is_err(),
        "should reject ambiguous duplicate upload steps"
    );
}

#[test]
fn rejects_truncated_pages_duplicate_ids_and_missing_expected_jobs() {
    let mut response = jobs();
    response["total_count"] = json!(3);
    assert!(
        bind(
            &encoded(&request()),
            &encoded(&response),
            &encoded(&artifacts())
        )
        .is_err(),
        "should reject a truncated jobs page"
    );
    let mut response = artifacts();
    response["artifacts"][3]["id"] = json!(200);
    assert!(
        bind(&encoded(&request()), &encoded(&jobs()), &encoded(&response)).is_err(),
        "should reject globally duplicated service artifact IDs"
    );
    let mut response = jobs();
    response["jobs"][1]["name"] = json!("Other unrelated job");
    assert!(
        bind(
            &encoded(&request()),
            &encoded(&response),
            &encoded(&artifacts())
        )
        .is_err(),
        "should never infer another job as reproduction from its uploaded filename"
    );
}

#[test]
fn rejects_duplicate_json_keys_before_service_or_context_interpretation() {
    let duplicate = String::from_utf8(encoded(&request()))
        .expect("should encode UTF-8")
        .replace("\"run_attempt\":1", "\"run_attempt\":1,\"run_attempt\":1");
    assert!(
        bind(
            duplicate.as_bytes(),
            &encoded(&jobs()),
            &encoded(&artifacts())
        )
        .is_err(),
        "should reject duplicate context keys before selecting identities"
    );
}
