use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::collection::tests::{cohort_fixture, composition_receipt};
use crate::collection::{ValidatedTransfers, diagnose_collection, digest};

// Fictional facts mirroring what a completed build owns. Pure tests cannot
// construct measured socket receipts, composition receipts or runtime
// acceptance, so these exercise the rendering and the unchanged collector only.
fn observed_ambient(role: Role) -> ObservedAmbient {
    let (umask, jobs, root, started, locale, timezone) = match role {
        Role::Canonical => (0o022, 4, "/external-release-A", 100, "C", "UTC"),
        Role::Reproduction => (
            0o027,
            2,
            "/external-release-B",
            101,
            "en_US.UTF-8",
            "Pacific/Auckland",
        ),
    };
    ObservedAmbient {
        umask,
        jobs,
        host_locale: locale.to_owned(),
        host_timezone: timezone.to_owned(),
        child_locale: "C".to_owned(),
        child_timezone: "UTC".to_owned(),
        execution_uid: 1000,
        checkout_root: PathBuf::from(root),
        started_at_seconds: started,
    }
}

/// Producer facts for one fixture transfer, agreeing with its trusted upload.
fn producer_record(fixture: &ValidatedTransfers, index: usize) -> StrictBuildRecord {
    let transfer = &fixture.transfers[index];
    let upload = &transfer.upload;
    let role = match upload.role.as_str() {
        "A" => Role::Canonical,
        "B" => Role::Reproduction,
        other => panic!("should use a known fixture role, got {other}"),
    };
    let artifact = &transfer.members["artifact"];
    let (fingerprint, invocation_digest) = match role {
        Role::Canonical => ("3".repeat(64), "5".repeat(64)),
        Role::Reproduction => ("4".repeat(64), "6".repeat(64)),
    };
    let runtime = (upload.asset == FIXED_ARTIFACTS[0].name).then(|| RuntimeFacts {
        role,
        source_commit: fixture.invocation.source_commit.clone(),
        final_wasm_sha256: digest(artifact),
        final_wasm_size: artifact.len() as u64,
        publisher_status: 200,
        viceroy_version: "0.21.0".to_owned(),
    });
    StrictBuildRecord {
        source_commit: fixture.invocation.source_commit.clone(),
        recipe_sha256: upload.recipe_sha256.clone(),
        asset: upload.asset.clone(),
        role,
        job: ProducerJob::new(&upload.job, upload.attempt)
            .expect("should accept the fixture job identity"),
        inputs: InputDigests {
            cargo_lock: "d".repeat(64),
            materials: "e".repeat(64),
            rust_tools: "f".repeat(64),
            js_settings: "1".repeat(64),
            js_tools: "2".repeat(64),
        },
        inventory_sha256: digest(&transfer.members["tsjs-inventory.json"]),
        artifact_sha256: digest(artifact),
        artifact_size: artifact.len() as u64,
        composition_sha256: digest(&transfer.members["composition.json"]),
        ambient: observed_ambient(role),
        boundary: SocketControls {
            fingerprint: fingerprint.clone(),
            execution_fingerprint: fingerprint,
            invocation_digest,
            control_exit: 0,
            control_accepts: 1,
            probe_exit: 3,
            probe_errno: 1,
            probe_accepts: 0,
        },
        runtime,
    }
}

fn install(fixture: &mut ValidatedTransfers, index: usize, record: &StrictBuildRecord) {
    let bytes = record
        .to_bytes()
        .expect("should render the strict build record");
    fixture.transfers[index]
        .members
        .insert(STRICT_RECORD.to_owned(), bytes);
}

/// The collector fixture cohort with every build record rendered by the producer.
fn producer_cohort() -> ValidatedTransfers {
    let mut fixture = cohort_fixture();
    for index in 0..fixture.transfers.len() {
        let record = producer_record(&fixture, index);
        install(&mut fixture, index, &record);
    }
    fixture
}

#[test]
fn producer_rendered_records_pass_the_unchanged_strict_collector() {
    let fixture = producer_cohort();
    for (index, transfer) in fixture.transfers.iter().enumerate() {
        assert_eq!(
            transfer.members[STRICT_RECORD],
            producer_record(&fixture, index)
                .to_bytes()
                .expect("should render again"),
            "should carry the producer rendering for transfer {index}"
        );
    }
    let diagnostic =
        diagnose_collection(&fixture).expect("should accept every producer-rendered record");
    assert_eq!(
        diagnostic.canonical.len(),
        4,
        "should retain all four canonical artifacts from producer records"
    );
}

#[test]
fn rendered_claim_and_runtime_follow_owned_policy_and_acceptance() {
    let fixture = cohort_fixture();
    let rendered: Value = serde_json::from_slice(
        &producer_record(&fixture, 0)
            .to_bytes()
            .expect("should render Fastly record"),
    )
    .expect("should parse rendered record");
    assert_eq!(
        rendered["claim"],
        json!({
            "varied": ["umask", "jobs", "checkout_root", "start_time"],
            "normalized": ["locale", "timezone"],
            "unsupported": ["execution_user"],
        }),
        "should render the compiled placement in collector names"
    );
    assert_eq!(
        (
            &rendered["runtime"]["injected_scripts"],
            &rendered["runtime"]["origin_marker_matched"],
            &rendered["boundary"]["confined"],
        ),
        (&json!(1), &json!(true), &json!(true)),
        "should state only facts implied by acceptance and denial receipts"
    );
    let native: Value = serde_json::from_slice(
        &producer_record(&fixture, 2)
            .to_bytes()
            .expect("should render native record"),
    )
    .expect("should parse native record");
    assert!(
        native["runtime"].is_null(),
        "should render no runtime block for a native build"
    );
}

type Mutation = fn(&mut StrictBuildRecord);

#[test]
fn each_changed_producer_fact_is_rejected_by_the_collector() {
    let cases: [(&str, usize, Mutation); 22] = [
        ("missing Fastly runtime", 0, |record| record.runtime = None),
        ("runtime of the other role", 0, |record| {
            if let Some(runtime) = &mut record.runtime {
                runtime.role = Role::Reproduction;
            }
        }),
        ("runtime for other bytes", 0, |record| {
            if let Some(runtime) = &mut record.runtime {
                runtime.final_wasm_size += 1;
            }
        }),
        ("failed publisher status", 0, |record| {
            if let Some(runtime) = &mut record.runtime {
                runtime.publisher_status = 500;
            }
        }),
        ("unpinned Viceroy", 0, |record| {
            if let Some(runtime) = &mut record.runtime {
                runtime.viceroy_version = "0.16.0".to_owned();
            }
        }),
        ("native runtime block", 2, |record| {
            record.runtime = Some(RuntimeFacts {
                role: Role::Canonical,
                source_commit: record.source_commit.clone(),
                final_wasm_sha256: record.artifact_sha256.clone(),
                final_wasm_size: record.artifact_size,
                publisher_status: 200,
                viceroy_version: "0.21.0".to_owned(),
            });
        }),
        ("unnormalized child locale", 0, |record| {
            record.ambient.child_locale = "en_US.UTF-8".to_owned();
        }),
        ("profile umask of another role", 0, |record| {
            record.ambient.umask = 0o027;
        }),
        ("relative checkout root", 0, |record| {
            record.ambient.checkout_root = PathBuf::from("external-release-A");
        }),
        ("execution outside the measured boundary", 0, |record| {
            record.boundary.execution_fingerprint = "9".repeat(64);
        }),
        ("malformed invocation digest", 0, |record| {
            record.boundary.invocation_digest = "5".repeat(63);
        }),
        ("probe errno other than a denial", 0, |record| {
            record.boundary.probe_errno = 111;
        }),
        ("failed socket control", 0, |record| {
            record.boundary.control_accepts = 0;
        }),
        ("reproduction reusing the canonical boundary", 1, |record| {
            record.boundary.fingerprint = "3".repeat(64);
            record.boundary.execution_fingerprint = "3".repeat(64);
        }),
        (
            "reproduction reusing the canonical invocation",
            1,
            |record| {
                record.boundary.invocation_digest = "5".repeat(64);
            },
        ),
        ("reproduction with the canonical start time", 1, |record| {
            record.ambient.started_at_seconds = 100;
        }),
        ("another job", 0, |record| {
            record.job = ProducerJob::new("another-job", 1).expect("should accept job");
        }),
        ("another attempt", 0, |record| {
            record.job = ProducerJob::new(&record.job.job.clone(), 2).expect("should accept job");
        }),
        ("pair input digest", 0, |record| {
            record.inputs.cargo_lock = "9".repeat(64);
        }),
        ("composition digest", 0, |record| {
            record.composition_sha256 = "9".repeat(64);
        }),
        ("role of another upload", 0, |record| {
            record.role = Role::Reproduction;
        }),
        ("inventory digest", 0, |record| {
            record.inventory_sha256 = "9".repeat(64);
        }),
    ];
    for (name, index, mutate) in cases {
        let mut fixture = producer_cohort();
        let mut record = producer_record(&fixture, index);
        mutate(&mut record);
        install(&mut fixture, index, &record);
        let error = format!(
            "{:?}",
            diagnose_collection(&fixture).expect_err("should reject a changed producer record")
        );
        assert!(
            error.contains(expected_rejection(name)),
            "should reject producer record with {name} at its own gate: {error}"
        );
    }
}

/// The collector gate each changed producer fact must reach.
fn expected_rejection(name: &str) -> &'static str {
    match name {
        "missing Fastly runtime" => "missing final-byte runtime proof",
        "runtime of the other role"
        | "runtime for other bytes"
        | "failed publisher status"
        | "unpinned Viceroy" => "runtime diagnostic is not bound",
        "native runtime block" => "native transfer carries a Fastly runtime",
        "unnormalized child locale" | "relative checkout root" => {
            "child normalization or observed checkout root"
        }
        "profile umask of another role" => "observed ambient differs from selected role profile",
        "execution outside the measured boundary"
        | "malformed invocation digest"
        | "probe errno other than a denial"
        | "failed socket control" => "missing/failed socket controls",
        "reproduction reusing the canonical boundary" => "A/B share one boundary instance",
        "reproduction reusing the canonical invocation" => "A/B share one exact invocation",
        "reproduction with the canonical start time" => "supported A/B variation is missing",
        "another job" | "another attempt" | "role of another upload" => {
            "build record differs from trusted dependency identity"
        }
        "pair input digest" => "A/B build inputs or recipe differ",
        "composition digest" => "composition document digest changed",
        "inventory digest" => "shared JS inventory changed",
        other => panic!("should name an expected gate for {other}"),
    }
}

/// Installs a sealed receipt and the producer record naming its exact digest.
fn install_receipt(fixture: &mut ValidatedTransfers, index: usize, receipt: Vec<u8>) {
    let mut record = producer_record(fixture, index);
    record.composition_sha256 = digest(&receipt);
    fixture.transfers[index]
        .members
        .insert("composition.json".to_owned(), receipt);
    install(fixture, index, &record);
}

/// The producer's receipt shape for one fixture transfer, with its own bindings.
fn producer_receipt(fixture: &ValidatedTransfers, index: usize) -> Value {
    let transfer = &fixture.transfers[index];
    let target = FIXED_ARTIFACTS
        .iter()
        .find(|asset| asset.name == transfer.upload.asset)
        .expect("should use a fixed fixture asset")
        .target
        .triple();
    serde_json::from_slice(&composition_receipt(
        target,
        &digest(&transfer.members["artifact"]),
        &fixture.invocation.source_commit,
        &digest(&transfer.members["tsjs-inventory.json"]),
    ))
    .expect("should parse producer receipt")
}

#[test]
fn producer_composition_receipt_passes_the_strict_collector() {
    // The producer's `composition.json` is its sealed normalized receipt
    // (`metadata_plan_sha256`, `tree_plan_sha256`, `observations`,
    // `composition`). Each record's `composition_sha256` names its exact bytes.
    let mut fixture = producer_cohort();
    for index in 0..fixture.transfers.len() {
        let receipt = serde_json::to_vec(&producer_receipt(&fixture, index))
            .expect("should serialize producer receipt");
        install_receipt(&mut fixture, index, receipt);
    }
    for transfer in &fixture.transfers {
        let receipt: Value = serde_json::from_slice(&transfer.members["composition.json"])
            .expect("should parse installed receipt");
        let keys = receipt
            .as_object()
            .expect("should be an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "composition",
                "metadata_plan_sha256",
                "observations",
                "tree_plan_sha256"
            ],
            "should carry exactly the producer receipt members"
        );
    }
    let diagnostic =
        diagnose_collection(&fixture).expect("should accept producer records and receipts");
    assert_eq!(
        diagnostic.canonical.len(),
        4,
        "should retain all four canonical artifacts from producer receipts"
    );
}

/// Changes one receipt fact, given the dependency and workspace package IDs.
type ReceiptMutation = fn(&mut Value, &str, &str);

#[test]
fn each_changed_producer_receipt_fact_is_rejected_by_the_collector() {
    let dependency = "registry+https://registry.example.com/index#example-dep@1.0.0";
    let workspace = "path+file:///workspace/crates/example-app#0.1.0";
    let cases: [(&str, ReceiptMutation, &str); 9] = [
        (
            "another artifact",
            |receipt, _, _| {
                receipt["composition"]["javascript"]["artifact_sha256"] = json!("9".repeat(64));
            },
            "composition receipt is not bound to the final artifact",
        ),
        (
            "another source",
            |receipt, _, workspace| {
                receipt["composition"]["package_origins"]["packages"][workspace]["origin"]["source_commit"] =
                    json!("9".repeat(40));
            },
            "composition receipt is not bound to the approved source",
        ),
        (
            "another inventory",
            |receipt, _, _| {
                receipt["composition"]["javascript"]["inventory_sha256"] = json!("9".repeat(64));
            },
            "composition receipt is not bound to the shared JS inventory",
        ),
        (
            "malformed plan digest",
            |receipt, _, _| {
                receipt["metadata_plan_sha256"] = json!("A".repeat(64));
            },
            "composition receipt digests are malformed",
        ),
        (
            "malformed origin digest",
            |receipt, dependency, _| {
                receipt["composition"]["package_origins"]["packages"][dependency]["prepared_manifest_sha256"] =
                    json!("b2".repeat(31));
            },
            "composition receipt package origins are malformed or ambiguous",
        ),
        (
            "empty observations",
            |receipt, _, _| {
                receipt["observations"]["units"] = json!([]);
            },
            "composition receipt has no compiled units",
        ),
        (
            "no package origins",
            |receipt, _, _| {
                receipt["observations"]["packages"] = json!([]);
                receipt["composition"]["package_origins"]["packages"] = json!({});
            },
            "composition receipt package origins are malformed or ambiguous",
        ),
        (
            "extra field",
            |receipt, _, _| {
                receipt["composition"]["coverage_complete"] = json!(true);
            },
            "unknown field",
        ),
        (
            "unjoined dependency",
            |receipt, dependency, _| {
                receipt["composition"]["package_origins"]["packages"]
                    .as_object_mut()
                    .expect("should find package origins")
                    .remove(dependency);
            },
            "composition receipt package origins are malformed or ambiguous",
        ),
    ];
    for (name, mutate, expected) in cases {
        for index in [0, 2] {
            let mut fixture = producer_cohort();
            let mut receipt = producer_receipt(&fixture, index);
            mutate(&mut receipt, dependency, workspace);
            let bytes = serde_json::to_vec(&receipt).expect("should serialize changed receipt");
            install_receipt(&mut fixture, index, bytes);
            let error = format!(
                "{:?}",
                diagnose_collection(&fixture).expect_err("should reject a changed receipt")
            );
            assert!(
                error.contains(expected),
                "should reject receipt with {name} for transfer {index}: {error}"
            );
        }
    }
}

#[test]
fn producer_job_identity_matches_the_collector_upload_check_and_checkout_root_is_bounded() {
    // The collector accepts a non-empty job ID of at most 128 bytes without
    // control characters, and a positive attempt; the producer mirrors it.
    let longest = "j".repeat(128);
    for job in [
        "fastly-a",
        "candidate (macos-14)",
        "kandidat-ü",
        longest.as_str(),
    ] {
        assert!(
            ProducerJob::new(job, 1).is_ok(),
            "should accept collector-valid job {job:?}"
        );
    }
    let oversized = "j".repeat(129);
    for (job, attempt) in [
        ("", 1),
        ("fastly-a", 0),
        ("fastly-a\n", 1),
        ("fastly\u{7f}a", 1),
        ("fastly\u{85}a", 1),
        (oversized.as_str(), 1),
    ] {
        assert!(
            ProducerJob::new(job, attempt).is_err(),
            "should reject collector-invalid job {job:?} attempt {attempt}"
        );
    }
    let mut fixture = producer_cohort();
    fixture.transfers[0].upload.job = "candidate (macos-14)".to_owned();
    let mut record = producer_record(&fixture, 0);
    record.job = ProducerJob::new("candidate (macos-14)", 1).expect("should accept spaced job");
    install(&mut fixture, 0, &record);
    diagnose_collection(&fixture).expect("should bind a spaced job ID the collector accepts");
    let mut record = producer_record(&cohort_fixture(), 0);
    record.ambient.checkout_root = PathBuf::from(OsStr::from_bytes(b"/external-\xff"));
    assert!(
        record.to_bytes().is_err(),
        "should reject a checkout root the collector schema cannot state"
    );
}
