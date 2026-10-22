use std::collections::BTreeMap;
use std::io::Write as _;

use flate2::Crc;
use serde_json::{Value, json};

use super::*;

pub(super) fn zip(members: &[(&str, &[u8])], descriptor: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, body) in members {
        let offset = bytes.len() as u32;
        let mut crc = Crc::new();
        crc.update(body);
        let crc = crc.sum();
        let size = body.len() as u32;
        let flags = if descriptor { 8_u16 } else { 0 };
        bytes.extend(0x04034b50_u32.to_le_bytes());
        bytes.extend(20_u16.to_le_bytes());
        bytes.extend(flags.to_le_bytes());
        bytes.extend([0; 6]);
        for field in if descriptor {
            [0, 0, 0]
        } else {
            [crc, size, size]
        } {
            bytes.extend(field.to_le_bytes());
        }
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(*body);
        if descriptor {
            for field in [0x08074b50, crc, size, size] {
                bytes.extend(field.to_le_bytes());
            }
        }
        central.extend(0x02014b50_u32.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend(flags.to_le_bytes());
        central.extend([0; 6]);
        for field in [crc, size, size] {
            central.extend(field.to_le_bytes());
        }
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let offset = bytes.len() as u32;
    let size = central.len() as u32;
    bytes.extend(central);
    bytes.extend(0x06054b50_u32.to_le_bytes());
    bytes.extend([0; 4]);
    for _ in 0..2 {
        bytes.extend((members.len() as u16).to_le_bytes());
    }
    bytes.extend(size.to_le_bytes());
    bytes.extend(offset.to_le_bytes());
    bytes.extend([0; 2]);
    bytes
}

#[test]
fn bounded_zip_accepts_exact_members_and_streaming_descriptor() {
    for descriptor in [false, true] {
        let bytes = zip(&[("artifact", b"final bytes")], descriptor);
        let declared = BTreeMap::from([("artifact".to_owned(), 11)]);
        let members = archive::read(&bytes, &declared).expect("should validate bounded ZIP");
        assert_eq!(
            members["artifact"], b"final bytes",
            "should retain exact bytes"
        );
    }
}

#[test]
fn zip_rejects_paths_duplicates_hidden_data_and_corrupt_descriptors() {
    for name in [
        "../artifact",
        "/artifact",
        "a/artifact",
        "a\\artifact",
        "Artifact",
        ".",
        "",
    ] {
        let bytes = zip(&[(name, b"x")], false);
        assert!(
            archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject unsafe or undeclared {name:?}"
        );
    }
    for descriptor in [false, true] {
        let bytes = zip(&[("artifact", b"x"), ("artifact", b"x")], descriptor);
        assert!(
            archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject duplicate members"
        );
    }
    let mut bytes = zip(&[("artifact", b"x")], true);
    bytes[38] ^= 1;
    assert!(
        archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject corrupt member bytes"
    );
    let mut bytes = zip(&[("artifact", b"x")], true);
    bytes[43] ^= 1;
    assert!(
        archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject descriptor mismatch"
    );
    let mut bytes = zip(&[("artifact", b"x")], false);
    bytes.insert(0, 0);
    assert!(
        archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject leading hidden bytes"
    );
    let mut bytes = zip(&[("artifact", b"x")], false);
    bytes.push(0);
    assert!(
        archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject trailing hidden bytes"
    );
}

#[test]
fn zip_rejects_oversized_declarations_and_unsupported_features() {
    let declared = BTreeMap::from([("artifact".to_owned(), 1)]);
    for index in [6, 8] {
        let mut bytes = zip(&[("artifact", b"x")], false);
        bytes[index] = 1;
        assert!(
            archive::read(&bytes, &declared).is_err(),
            "should reject encryption or unsupported method"
        );
    }
    let bytes = zip(&[("artifact", b"x")], false);
    assert!(
        archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), u64::MAX)])).is_err(),
        "should reject declaration before allocation"
    );
    for length in 0..bytes.len() {
        assert!(
            archive::read(&bytes[..length], &declared).is_err(),
            "should reject truncation at {length}"
        );
    }
}

struct FakeSource {
    metadata: Value,
    archive: Vec<u8>,
    warning: bool,
    requested: Vec<u64>,
    repositories: Vec<String>,
}

impl ArtifactSource for FakeSource {
    fn metadata(&mut self, repository: &str, id: u64) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.repositories.push(repository.to_owned());
        self.requested.push(id);
        Ok(serde_json::to_vec(&self.metadata).expect("should serialize metadata"))
    }
    fn download(&mut self, repository: &str, id: u64) -> Result<Download, Report<ReleaseError>> {
        self.repositories.push(repository.to_owned());
        self.requested.push(id);
        Ok(Download {
            archive: self.archive.clone(),
            transfer_warning: self.warning,
        })
    }
}

fn transfer_fixture() -> (TrustedDependencies, FakeSource) {
    let archive = zip(&[("artifact", b"x")], false);
    let dependencies = TrustedDependencies {
        invocation: Invocation {
            repository: "example/project".to_owned(),
            repository_id: 17,
            run_id: 29,
            source_commit: "a".repeat(40),
            inventory_sha256: "b".repeat(64),
            profiles: records::RoleProfile::fixtures(),
        },
        required: vec![Upload {
            asset: "trusted-server-fastly.wasm".to_owned(),
            role: "A".to_owned(),
            job: "fastly-a".to_owned(),
            attempt: 1,
            recipe_sha256: "c".repeat(64),
            elapsed_seconds: 10,
            deadline_seconds: 1800,
            artifact_name: "upload".to_owned(),
            artifact_id: 41,
            archive_sha256: digest(&archive),
            members: BTreeMap::from([("artifact".to_owned(), 1)]),
        }],
        optional: vec![],
    };
    let source = FakeSource {
        metadata: json!({"id":41,"name":"upload","expired":false,"digest":format!("sha256:{}",digest(&archive)),"workflow_run":{"id":29,"head_sha":"a".repeat(40),"repository_id":17,"head_repository_id":17}}),
        archive,
        warning: false,
        requested: vec![],
        repositories: vec![],
    };
    (dependencies, source)
}

#[test]
fn transfer_checks_exact_dependency_id_and_archive_bytes() {
    let (dependencies, mut source) = transfer_fixture();
    let checked = validate_transfers(&dependencies, &mut source).expect("should validate transfer");
    assert_eq!(
        checked.transfers[0].members["artifact"], b"x",
        "should retain exact bytes"
    );
    assert_eq!(
        source.requested,
        [41, 41],
        "should re-fetch exact selected ID"
    );
    assert_eq!(
        source.repositories,
        ["example/project", "example/project"],
        "should use trusted repository for metadata and download"
    );
}

#[test]
fn transfer_rejects_stale_metadata_warnings_reused_ids_and_changed_bytes() {
    let mut cases = Vec::new();
    for (pointer, value) in [
        ("/id", json!(42)),
        ("/name", json!("other-upload")),
        ("/expired", json!(true)),
        ("/workflow_run/id", json!(30)),
        ("/workflow_run/head_sha", json!("d".repeat(40))),
        ("/workflow_run/repository_id", json!(18)),
        ("/workflow_run/head_repository_id", json!(18)),
        ("/digest", json!(format!("sha256:{}", "e".repeat(64)))),
    ] {
        let (dependencies, mut source) = transfer_fixture();
        *source
            .metadata
            .pointer_mut(pointer)
            .expect("should find fixture field") = value;
        cases.push((dependencies, source));
    }
    let (dependencies, mut source) = transfer_fixture();
    source.warning = true;
    cases.push((dependencies, source));
    let (dependencies, mut source) = transfer_fixture();
    source.archive[0] ^= 1;
    cases.push((dependencies, source));
    let (mut dependencies, source) = transfer_fixture();
    let mut reused = dependencies.required[0].clone();
    reused.role = "B".to_owned();
    dependencies.required.push(reused);
    cases.push((dependencies, source));
    for (dependencies, mut source) in cases {
        assert!(
            validate_transfers(&dependencies, &mut source).is_err(),
            "should reject transfer substitution"
        );
    }
}

fn wasm() -> Vec<u8> {
    let mut bytes = crate::inspect::tests::builders::command_module(
        &[("fastly_http_req", "call"), ("fastly_http_resp", "call")],
        crate::inspect::tests::builders::Entry::Nullary,
        true,
    );
    for name in [".debug_info", ".debug_line"] {
        bytes.extend([0, (name.len() + 2) as u8, name.len() as u8]);
        bytes.extend(name.as_bytes());
        bytes.push(1);
    }
    bytes
}

/// Fills the fictional sealed composition receipt with one build's bindings.
///
/// The fixture has the producer's `composition_export()` shape; only the
/// target, final artifact, approved source and shared inventory vary per build.
pub(super) fn composition_receipt(
    target: &str,
    artifact_sha256: &str,
    source_commit: &str,
    inventory_sha256: &str,
) -> Vec<u8> {
    let mut receipt: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/collection/composition.json"
    ))
    .expect("should parse fictional composition receipt");
    for (pointer, value) in [
        ("/composition/platform/target", target),
        ("/composition/platform/rust/target", target),
        ("/composition/platform/external_requirements/target", target),
        ("/composition/javascript/artifact_sha256", artifact_sha256),
        ("/composition/platform/artifact_sha256", artifact_sha256),
        (
            "/composition/platform/external_requirements/artifact_sha256",
            artifact_sha256,
        ),
        ("/composition/javascript/inventory_sha256", inventory_sha256),
        ("/composition/package_origins/source_commit", source_commit),
    ] {
        *receipt
            .pointer_mut(pointer)
            .expect("should find receipt binding") = json!(value);
    }
    let platform = &mut receipt["composition"]["platform"];
    if target.ends_with("-apple-darwin") {
        platform["apple"] = json!({"sdk_version": "15.0", "qualification": "unqualified"});
        platform["external_requirements"]["runtime"] = json!({
            "format": "macho",
            "loader": "/usr/lib/dyld",
            "cpu": "fictional",
            "dylibs": ["/usr/lib/libSystem.B.dylib"],
            "minimum_os": "13.0",
            "sdk": "15.0",
        });
    } else if target.ends_with("-linux-gnu") {
        platform["external_requirements"]["runtime"] = json!({
            "format": "elf",
            "interpreter": "/lib64/ld-linux-x86-64.so.2",
            "needed": ["libc.so.6"],
            "version_requirements": [],
            "max_glibc": "2.34",
        });
    }
    let packages = receipt
        .pointer_mut("/composition/package_origins/packages")
        .and_then(Value::as_object_mut)
        .expect("should find package origins");
    for origin in packages.values_mut().map(|package| &mut package["origin"]) {
        if origin["kind"] == "workspace" {
            origin["source_commit"] = json!(source_commit);
        }
    }
    serde_json::to_vec(&receipt).expect("should serialize composition receipt")
}

pub(super) fn cohort_fixture() -> ValidatedTransfers {
    let inventory = b"{\"schema_version\":1}".to_vec();
    let mut invocation = transfer_fixture().0.invocation;
    invocation.inventory_sha256 = digest(&inventory);
    let mut transfers = Vec::new();
    for (index, asset) in crate::policy::FIXED_ARTIFACTS.iter().enumerate() {
        for role in if index == 0 {
            vec!["A", "B"]
        } else {
            vec!["A"]
        } {
            let artifact = if index == 0 {
                wasm()
            } else {
                b"native final bytes".to_vec()
            };
            let composition = composition_receipt(
                asset.target.triple(),
                &digest(&artifact),
                &invocation.source_commit,
                &invocation.inventory_sha256,
            );
            let mut record: Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/collection/build-record.json"
            ))
            .expect("should parse fictional build record");
            record["asset"] = json!(asset.name);
            record["role"] = json!(role);
            record["job"] = json!(format!("job-{index}-{role}"));
            record["artifact_sha256"] = json!(digest(&artifact));
            record["artifact_size"] = json!(artifact.len());
            record["inventory_sha256"] = json!(digest(&inventory));
            record["composition_sha256"] = json!(digest(&composition));
            if role == "B" {
                record["boundary"]["fingerprint"] = json!("4".repeat(64));
                record["boundary"]["execution_fingerprint"] = json!("4".repeat(64));
                record["boundary"]["invocation_digest"] = json!("6".repeat(64));
                record["ambient"]["umask"] = json!(23);
                record["ambient"]["jobs"] = json!(2);
                record["ambient"]["checkout_root"] = json!("/external-release-B");
                record["ambient"]["started_at_seconds"] = json!(101);
                record["ambient"]["host_locale"] = json!("en_US.UTF-8");
                record["ambient"]["host_timezone"] = json!("Pacific/Auckland");
            }
            if index == 0 {
                record["runtime"] = json!({"role":role,"source_commit":invocation.source_commit,"final_wasm_sha256":digest(&artifact),"final_wasm_size":artifact.len(),"publisher_status":200,"injected_scripts":1,"origin_marker_matched":true,"viceroy_version":"0.21.0"});
            }
            let upload = Upload {
                asset: asset.name.to_owned(),
                role: role.to_owned(),
                job: format!("job-{index}-{role}"),
                attempt: 1,
                recipe_sha256: "c".repeat(64),
                elapsed_seconds: 10,
                deadline_seconds: 1800,
                artifact_name: "upload".to_owned(),
                artifact_id: (transfers.len() + 1) as u64,
                archive_sha256: "f".repeat(64),
                members: BTreeMap::new(),
            };
            transfers.push(Transfer {
                upload,
                members: BTreeMap::from([
                    ("artifact".to_owned(), artifact),
                    (
                        "build-record.json".to_owned(),
                        serde_json::to_vec(&record).expect("should serialize build record"),
                    ),
                    ("tsjs-inventory.json".to_owned(), inventory.clone()),
                    ("composition.json".to_owned(), composition),
                ]),
            });
        }
    }
    let absent = crate::policy::FIXED_ARTIFACTS
        .iter()
        .skip(1)
        .map(|asset| no_upload(asset.name, QUEUED, 1800))
        .collect();
    ValidatedTransfers {
        invocation,
        transfers,
        absent,
    }
}

/// Still queued, with no runner and no conclusion, when the collector deadline expired.
const QUEUED: ObservedJob = ObservedJob {
    started: false,
    conclusion: None,
};

/// A started job ended by its own failure before the upload step.
const FAILED_BEFORE_UPLOAD: ObservedJob = ObservedJob {
    started: true,
    conclusion: Some(JobConclusion::Failure),
};

/// A no-upload optional reproduction observed within a fictional 1800 s deadline.
fn no_upload(asset: &str, job: ObservedJob, elapsed_seconds: u64) -> NoUpload {
    NoUpload {
        asset: asset.to_owned(),
        job,
        elapsed_seconds,
        deadline_seconds: 1800,
    }
}

fn mutate_record(transfers: &mut ValidatedTransfers, index: usize, pointer: &str, value: Value) {
    let bytes = transfers.transfers[index]
        .members
        .get_mut("build-record.json")
        .expect("should find record");
    let mut record: Value = serde_json::from_slice(bytes).expect("should parse record");
    *record
        .pointer_mut(pointer)
        .expect("should find record field") = value;
    *bytes = serde_json::to_vec(&record).expect("should serialize changed record");
}

#[test]
fn mandatory_fastly_and_native_a_pass_with_never_started_native_b_absent() {
    let checked =
        diagnose_collection(&cohort_fixture()).expect("should diagnose complete mandatory cohort");
    assert_eq!(
        checked.canonical.len(),
        4,
        "should retain exactly four canonical artifacts"
    );
    assert_eq!(
        checked.native.len(),
        3,
        "should record all three native measurements"
    );
    assert!(
        checked
            .native
            .values()
            .all(|outcome| *outcome == records::Outcome::Unavailable),
        "should honestly retain never-started native B as unavailable"
    );
}

#[test]
fn collection_rejects_missing_a_and_every_mandatory_gate_even_with_optional_b_absent() {
    let mut missing = cohort_fixture();
    missing.transfers.remove(2);
    assert!(
        diagnose_collection(&missing).is_err(),
        "should reject missing native A"
    );
    for (pointer, value) in [
        ("/inventory_sha256", json!("9".repeat(64))),
        ("/composition_sha256", json!("9".repeat(64))),
        ("/boundary/confined", json!(false)),
        ("/boundary/control_exit", json!(1)),
        ("/boundary/control_accepts", json!(0)),
        ("/boundary/probe_exit", json!(0)),
        ("/boundary/probe_accepts", json!(1)),
        ("/boundary/execution_fingerprint", json!("9".repeat(64))),
        ("/boundary/invocation_digest", json!("5".repeat(63))),
        ("/boundary/invocation_digest", json!("A".repeat(64))),
        ("/runtime", Value::Null),
        ("/runtime/final_wasm_sha256", json!("9".repeat(64))),
        ("/runtime/publisher_status", json!(500)),
        ("/runtime/injected_scripts", json!(0)),
        ("/runtime/origin_marker_matched", json!(false)),
        ("/source_commit", json!("9".repeat(40))),
        ("/job", json!("wrong-job")),
        ("/attempt", json!(2)),
    ] {
        let mut fixture = cohort_fixture();
        mutate_record(&mut fixture, 0, pointer, value);
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject mandatory gate {pointer}"
        );
    }
    let mut fixture = cohort_fixture();
    fixture.transfers[3]
        .members
        .get_mut("artifact")
        .expect("should have native A")
        .push(0);
    assert!(
        diagnose_collection(&fixture).is_err(),
        "should not hide native A byte changes behind optional B absence"
    );
}

#[test]
fn collection_rejects_unchanged_supported_variations_and_dishonest_claims() {
    for (pointer, value) in [
        ("/ambient/umask", json!(18)),
        ("/ambient/jobs", json!(4)),
        ("/ambient/checkout_root", json!("/external-release-A")),
        ("/ambient/started_at_seconds", json!(100)),
        ("/ambient/child_locale", json!("en_US.UTF-8")),
        ("/ambient/child_timezone", json!("Pacific/Auckland")),
        ("/ambient/execution_uid", json!(1001)),
        ("/claim/varied", json!(["umask", "jobs", "checkout_root"])),
        (
            "/claim/normalized",
            json!(["locale", "timezone", "start_time"]),
        ),
        ("/claim/unsupported", json!(["execution_user", "jobs"])),
    ] {
        let mut fixture = cohort_fixture();
        mutate_record(&mut fixture, 1, pointer, value);
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject dishonest or unchanged variation {pointer}"
        );
    }
}

#[test]
fn collection_compares_complete_fastly_final_bytes_without_exclusions() {
    let mut fixture = cohort_fixture();
    let artifact = fixture.transfers[1]
        .members
        .get_mut("artifact")
        .expect("should find Fastly B");
    *artifact.last_mut().expect("should have bytes") ^= 1;
    let sha = digest(artifact);
    let size = artifact.len();
    mutate_record(&mut fixture, 1, "/artifact_sha256", json!(sha));
    mutate_record(&mut fixture, 1, "/runtime/final_wasm_sha256", json!(sha));
    mutate_record(&mut fixture, 1, "/runtime/final_wasm_size", json!(size));
    let composition = composition_receipt(
        crate::policy::FIXED_ARTIFACTS[0].target.triple(),
        &sha,
        &fixture.invocation.source_commit,
        &fixture.invocation.inventory_sha256,
    );
    let composition_sha = digest(&composition);
    fixture.transfers[1]
        .members
        .insert("composition.json".to_owned(), composition);
    mutate_record(
        &mut fixture,
        1,
        "/composition_sha256",
        json!(composition_sha),
    );
    let error = diagnose_collection(&fixture).expect_err("should reject one changed final byte");
    assert!(
        format!("{error:?}").contains("final Fastly bytes differ"),
        "should reach complete final-byte equality gate"
    );
}

#[test]
fn zip_deflate_preserves_full_bytes_and_rejects_extra_compressed_data() {
    let body = vec![b'x'; 128 * 1024 + 17];
    for descriptor in [false, true] {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&body).expect("should compress fixture");
        let compressed = encoder.finish().expect("should finish compression");
        let mut bytes = zip(&[("artifact", &body)], descriptor);
        bytes.splice(38..38 + body.len(), compressed.iter().copied());
        let central = 38 + compressed.len() + if descriptor { 16 } else { 0 };
        bytes[8..10].copy_from_slice(&8_u16.to_le_bytes());
        bytes[18..22].copy_from_slice(
            &(if descriptor {
                0
            } else {
                compressed.len() as u32
            })
            .to_le_bytes(),
        );
        bytes[central + 10..central + 12].copy_from_slice(&8_u16.to_le_bytes());
        bytes[central + 20..central + 24].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        if descriptor {
            bytes[38 + compressed.len() + 8..38 + compressed.len() + 12]
                .copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        }
        let end = bytes.len() - 22;
        bytes[end + 16..end + 20].copy_from_slice(&(central as u32).to_le_bytes());
        let declared = BTreeMap::from([("artifact".to_owned(), body.len() as u64)]);
        assert_eq!(
            archive::read(&bytes, &declared).expect("should validate deflated stream")["artifact"],
            body,
            "should decompress exact full bytes"
        );
        bytes[central + 24..central + 28].copy_from_slice(&1_u32.to_le_bytes());
        assert!(
            archive::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject expanded bytes beyond declared bound"
        );
    }
}

#[test]
fn native_b_records_match_mismatch_failed_and_unavailable_without_masking_a() {
    for same in [true, false] {
        let fixture = with_uploaded_native_b(same);
        let diagnostic = diagnose_collection(&fixture)
            .expect("should measure native B without requiring equality");
        assert_eq!(
            diagnostic.native[crate::policy::FIXED_ARTIFACTS[1].name],
            if same {
                records::Outcome::Matched
            } else {
                records::Outcome::Mismatched
            },
            "should report actual complete-byte native result"
        );
    }
    for (job, elapsed, outcome) in [
        (FAILED_BEFORE_UPLOAD, 7, records::Outcome::Failed),
        (QUEUED, 1800, records::Outcome::Unavailable),
    ] {
        let mut fixture = cohort_fixture();
        fixture.absent[0] = no_upload(crate::policy::FIXED_ARTIFACTS[1].name, job, elapsed);
        assert_eq!(
            diagnose_collection(&fixture)
                .expect("should retain bounded native B failure")
                .native[crate::policy::FIXED_ARTIFACTS[1].name],
            outcome,
            "should report honest bounded outcome"
        );
        fixture.absent[0].elapsed_seconds = 1801;
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject unbounded optional B outcome"
        );
    }
}

#[test]
fn uploaded_build_deadlines_are_bounded_before_rest_or_comparison() {
    let (mut dependencies, mut source) = transfer_fixture();
    dependencies.required[0].elapsed_seconds = 1801;
    assert!(
        validate_transfers(&dependencies, &mut source).is_err(),
        "should reject a successful upload outside its job deadline"
    );
    assert!(
        source.requested.is_empty(),
        "should reject before fetching any unbounded result"
    );
    let mut fixture = cohort_fixture();
    fixture.transfers[1].upload.deadline_seconds = 0;
    assert!(
        diagnose_collection(&fixture).is_err(),
        "should reject missing bounded completion metadata"
    );
}

#[test]
fn optional_native_b_cannot_hide_any_canonical_composition_or_js_failure() {
    for index in [0, 2, 3, 4] {
        for pointer in ["/composition_sha256", "/inventory_sha256"] {
            let mut fixture = cohort_fixture();
            mutate_record(&mut fixture, index, pointer, json!("9".repeat(64)));
            assert!(
                diagnose_collection(&fixture).is_err(),
                "should block failed mandatory A {index} {pointer}"
            );
        }
        let mut fixture = cohort_fixture();
        fixture.transfers[index]
            .members
            .insert("tsjs-inventory.json".to_owned(), b"{}".to_vec());
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject changed actual shared inventory bytes in A {index}"
        );
        let mut fixture = cohort_fixture();
        fixture.transfers[index].members.remove("composition.json");
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject absent canonical composition despite optional B unavailability"
        );
    }
}

#[test]
fn zip_rejects_symlinks_zip64_and_local_central_disagreement() {
    let original = zip(&[("artifact", b"x")], false);
    let central = 39;
    let declared = BTreeMap::from([("artifact".to_owned(), 1)]);
    for (offset, bytes) in [
        (central + 38, (0o120777_u32 << 16).to_le_bytes().to_vec()),
        (central + 20, u32::MAX.to_le_bytes().to_vec()),
        (central + 42, 1_u32.to_le_bytes().to_vec()),
        (central + 6, 45_u16.to_le_bytes().to_vec()),
        (14, 1_u32.to_le_bytes().to_vec()),
        (30, b"different".to_vec()),
    ] {
        let mut changed = original.clone();
        changed[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(
            archive::read(&changed, &declared).is_err(),
            "should reject ZIP ambiguity at offset {offset}"
        );
    }
    let bytes = zip(&[("artifact", b"x"), ("Artifact", b"x")], false);
    let declared = BTreeMap::from([("artifact".to_owned(), 1), ("Artifact".to_owned(), 1)]);
    assert!(
        archive::read(&bytes, &declared).is_err(),
        "should reject case-colliding declared identities"
    );
}

#[test]
fn dependency_repository_cannot_change_the_fixed_api_path() {
    for repository in [
        "../project",
        "example/..",
        "example/project?other=1",
        "example/project#fragment",
        "example/project%2fother",
        "example/",
        "/project",
    ] {
        let (mut dependencies, mut source) = transfer_fixture();
        dependencies.invocation.repository = repository.to_owned();
        let result = validate_transfers(&dependencies, &mut source);
        assert!(
            result.is_err(),
            "should reject unsafe repository endpoint {repository}"
        );
        assert!(
            source.requested.is_empty(),
            "should reject before any endpoint call"
        );
    }
}

#[test]
fn candidate_rebuild_details_preserve_observations_and_absence_without_authority() {
    let checked = diagnose_collection(&cohort_fixture()).expect("should derive candidate details");
    let fastly = checked
        .rebuild
        .get(crate::policy::FIXED_ARTIFACTS[0].name)
        .expect("should retain Fastly rebuild diagnostic");
    assert_eq!(
        fastly["release_qualification"], false,
        "should never imply official qualification"
    );
    assert_eq!(
        fastly["outcome"], "matched",
        "should report complete Fastly equality"
    );
    assert_eq!(
        fastly["canonical"]["artifact_id"], 1,
        "should retain trusted A upload identity"
    );
    assert_eq!(
        fastly["reproduction"]["artifact_id"], 2,
        "should retain trusted B upload identity"
    );
    assert_eq!(
        fastly["reproduction"]["ambient"]["jobs"], 2,
        "should retain checked observed B profile"
    );
    let native = checked
        .rebuild
        .get(crate::policy::FIXED_ARTIFACTS[1].name)
        .expect("should retain native rebuild diagnostic");
    assert_eq!(
        native["outcome"], "unavailable",
        "should retain never-started outcome"
    );
    let b = native["reproduction"]
        .as_object()
        .expect("should retain honest bounded absence");
    assert_eq!(
        b.len(),
        4,
        "should emit only outcome, observed job state, elapsed and deadline without invented identity or observations"
    );
    assert_eq!(
        b["job_state"],
        json!({"started": false, "conclusion": null}),
        "should retain the observed never-started job state honestly"
    );
    for field in [
        "artifact_id",
        "artifact_name",
        "job",
        "attempt",
        "archive_sha256",
        "ambient",
        "artifact_sha256",
    ] {
        assert!(!b.contains_key(field), "should omit unavailable B {field}");
    }
}

struct FakeRun(BTreeMap<u64, FakeSource>);
impl ArtifactSource for FakeRun {
    fn metadata(&mut self, repository: &str, id: u64) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.0
            .get_mut(&id)
            .expect("should request exact current-run ID")
            .metadata(repository, id)
    }
    fn download(&mut self, repository: &str, id: u64) -> Result<Download, Report<ReleaseError>> {
        self.0
            .get_mut(&id)
            .expect("should request exact current-run ID")
            .download(repository, id)
    }
}

#[test]
fn exact_current_run_archives_feed_the_complete_candidate_comparison() {
    let fixture = cohort_fixture();
    let mut dependencies = TrustedDependencies {
        invocation: fixture.invocation,
        required: Vec::new(),
        optional: fixture
            .absent
            .into_iter()
            .map(OptionalBuild::NotUploaded)
            .collect(),
    };
    let mut source = FakeRun(BTreeMap::new());
    for transfer in fixture.transfers {
        let mut upload = transfer.upload;
        let members = transfer
            .members
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_slice()))
            .collect::<Vec<_>>();
        let archive = zip(&members, true);
        upload.archive_sha256 = digest(&archive);
        upload.members = transfer
            .members
            .iter()
            .map(|(name, body)| (name.clone(), body.len() as u64))
            .collect();
        source.0.insert(upload.artifact_id,FakeSource {metadata:json!({"id":upload.artifact_id,"name":upload.artifact_name,"expired":false,"digest":format!("sha256:{}",upload.archive_sha256),"workflow_run":{"id":dependencies.invocation.run_id,"head_sha":dependencies.invocation.source_commit,"repository_id":dependencies.invocation.repository_id,"head_repository_id":dependencies.invocation.repository_id}}),archive,warning:false,requested:Vec::new(),repositories:Vec::new()});
        dependencies.required.push(upload);
    }
    let transfers = validate_transfers(&dependencies, &mut source)
        .expect("should re-download exact complete current-run transfers");
    let checked = diagnose_collection(&transfers)
        .expect("should compare declared full final bytes from validated archives");
    assert_eq!(
        checked.canonical.len(),
        4,
        "should retain full canonical cohort"
    );
    assert!(
        source.0.values().all(|source| source.requested.len() == 2),
        "should corroborate REST and re-download every exact-ID upload"
    );
}

fn raw_deflate_zip(body: &[u8], compressed: &[u8], descriptor: bool) -> Vec<u8> {
    let mut bytes = zip(&[("artifact", body)], descriptor);
    bytes.splice(38..38 + body.len(), compressed.iter().copied());
    let central = 38 + compressed.len() + if descriptor { 16 } else { 0 };
    bytes[8..10].copy_from_slice(&8_u16.to_le_bytes());
    bytes[18..22].copy_from_slice(
        &(if descriptor {
            0
        } else {
            compressed.len() as u32
        })
        .to_le_bytes(),
    );
    bytes[central + 10..central + 12].copy_from_slice(&8_u16.to_le_bytes());
    bytes[central + 20..central + 24].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    if descriptor {
        bytes[38 + compressed.len() + 8..38 + compressed.len() + 12]
            .copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    }
    let end = bytes.len() - 22;
    bytes[end + 16..end + 20].copy_from_slice(&(central as u32).to_le_bytes());
    bytes
}

#[test]
fn unix_zip32_deflate_upload_profile_accepts_flat_files_and_rejects_nested_names() {
    let body = b"x";
    let compressed = b"\x01\x01\x00\xfe\xffx";
    let mut bytes = raw_deflate_zip(body, compressed, true);
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(
        bytes[end + 16..end + 20]
            .try_into()
            .expect("should hold the central directory offset"),
    ) as usize;
    bytes[central + 4..central + 6].copy_from_slice(&0x032d_u16.to_le_bytes());
    let attributes = (0o100644_u32 << 16) | 0x20;
    bytes[central + 38..central + 42].copy_from_slice(&attributes.to_le_bytes());
    let declared = BTreeMap::from([("artifact".to_owned(), body.len() as u64)]);

    assert_eq!(
        archive::read(&bytes, &declared)
            .expect("should accept regular Unix files with ZIP32 signed streaming descriptors")["artifact"],
        body,
        "should retain exact fictional member bytes"
    );

    bytes[30..38].copy_from_slice(b"a/binary");
    bytes[central + 46..central + 54].copy_from_slice(b"a/binary");
    let nested = BTreeMap::from([("a/binary".to_owned(), body.len() as u64)]);
    assert!(
        archive::read(&bytes, &nested).is_err(),
        "should reject nested member names without relaxing the flat-file policy"
    );
}

#[test]
fn zip_requires_deflate_stream_end_even_when_crc_size_and_consumption_match() {
    for descriptor in [false, true] {
        for (body, nonfinal) in [
            (b"x".as_slice(), b"\x00\x01\x00\xfe\xffx".as_slice()),
            (b"".as_slice(), b"\x00\x00\x00\xff\xff".as_slice()),
        ] {
            let declared = BTreeMap::from([("artifact".to_owned(), body.len() as u64)]);
            let truncated = raw_deflate_zip(body, nonfinal, descriptor);
            assert!(
                archive::read(&truncated, &declared).is_err(),
                "should reject nonfinal raw DEFLATE block despite exact output, CRC and consumed bytes"
            );
            let mut final_block = nonfinal.to_vec();
            final_block[0] |= 1;
            let valid = raw_deflate_zip(body, &final_block, descriptor);
            let mut trailing = final_block.clone();
            trailing.push(0);
            assert!(
                archive::read(&raw_deflate_zip(body, &trailing, descriptor), &declared).is_err(),
                "should reject compressed bytes after the final DEFLATE marker"
            );
            for length in 0..final_block.len() {
                assert!(
                    archive::read(
                        &raw_deflate_zip(body, &final_block[..length], descriptor),
                        &declared
                    )
                    .is_err(),
                    "should reject every truncated final DEFLATE block"
                );
            }
            assert_eq!(
                archive::read(&valid, &declared)
                    .expect("should accept explicit final DEFLATE block")["artifact"],
                body,
                "should retain exact terminated stream output"
            );
        }
    }
}

/// Selects one reproduction profile and makes Fastly B observe it exactly, so
/// only the A/B pair comparison can reject the record.
fn fixture_with_selected_reproduction(
    profile: records::RoleProfile,
    ambient: &Value,
) -> ValidatedTransfers {
    let mut fixture = cohort_fixture();
    fixture.invocation.profiles[1] = profile;
    for (field, value) in ambient.as_object().expect("should hold ambient fields") {
        mutate_record(&mut fixture, 1, &format!("/ambient/{field}"), value.clone());
    }
    fixture
}

#[test]
fn reproduction_selected_without_a_supported_variation_is_rejected_by_the_pair_check() {
    for (dimension, profile, ambient) in [
        (
            "umask",
            records::RoleProfile::for_tests(18, 2, "en_US.UTF-8", "Pacific/Auckland"),
            json!({"umask": 18}),
        ),
        (
            "jobs",
            records::RoleProfile::for_tests(23, 4, "en_US.UTF-8", "Pacific/Auckland"),
            json!({"jobs": 4}),
        ),
        (
            "host locale",
            records::RoleProfile::for_tests(23, 2, "C", "Pacific/Auckland"),
            json!({"host_locale": "C"}),
        ),
        (
            "host timezone",
            records::RoleProfile::for_tests(23, 2, "en_US.UTF-8", "UTC"),
            json!({"host_timezone": "UTC"}),
        ),
        (
            "start time",
            records::RoleProfile::for_tests(23, 2, "en_US.UTF-8", "Pacific/Auckland"),
            json!({"started_at_seconds": 100}),
        ),
    ] {
        let fixture = fixture_with_selected_reproduction(profile, &ambient);
        let error = diagnose_collection(&fixture)
            .expect_err("should reject a reproduction that omits a supported variation");
        assert!(
            format!("{error:?}").contains("A/B variation is missing"),
            "should reach the pair check for unvaried {dimension}: {error:?}"
        );
    }
}

#[test]
fn reproduction_reusing_the_canonical_exact_invocation_is_rejected() {
    // Distinct boundary instances do not make one exact invocation two.
    let mut fixture = cohort_fixture();
    mutate_record(
        &mut fixture,
        1,
        "/boundary/invocation_digest",
        json!("5".repeat(64)),
    );
    let error = diagnose_collection(&fixture)
        .expect_err("should reject identical role-specific invocation digests");
    assert!(
        format!("{error:?}").contains("A/B share one exact invocation"),
        "should reject a reproduction claiming the canonical invocation: {error:?}"
    );
}

#[test]
fn reproduction_reusing_the_canonical_boundary_instance_is_rejected() {
    let mut fixture = cohort_fixture();
    for pointer in ["/boundary/fingerprint", "/boundary/execution_fingerprint"] {
        mutate_record(&mut fixture, 1, pointer, json!("3".repeat(64)));
    }
    let error = diagnose_collection(&fixture)
        .expect_err("should reject one boundary instance serving both builds");
    assert!(
        format!("{error:?}").contains("A/B share one boundary instance"),
        "should reject a reproduction run in the canonical boundary: {error:?}"
    );
}

/// Rebinds a changed imported composition summary so only its content is wrong.
fn rebind_composition(fixture: &mut ValidatedTransfers, index: usize, pointer: &str, value: Value) {
    let members = &mut fixture.transfers[index].members;
    let mut composition: Value = serde_json::from_slice(&members["composition.json"])
        .expect("should parse fictional composition");
    *composition
        .pointer_mut(pointer)
        .expect("should find composition field") = value;
    let bytes = serde_json::to_vec(&composition).expect("should serialize changed composition");
    let hash = digest(&bytes);
    members.insert("composition.json".to_owned(), bytes);
    mutate_record(fixture, index, "/composition_sha256", json!(hash));
}

/// Adds an uploaded first-native reproduction with matched or mismatched bytes.
fn with_uploaded_native_b(same: bool) -> ValidatedTransfers {
    let mut fixture = cohort_fixture();
    let mut reproduction = Transfer {
        upload: fixture.transfers[2].upload.clone(),
        members: fixture.transfers[2].members.clone(),
    };
    reproduction.upload.role = "B".to_owned();
    reproduction.upload.job = "native-b".to_owned();
    reproduction.upload.artifact_id = 99;
    fixture.transfers.push(reproduction);
    fixture.absent.remove(0);
    let index = fixture.transfers.len() - 1;
    for (pointer, value) in [
        ("/role", json!("B")),
        ("/job", json!("native-b")),
        ("/boundary/fingerprint", json!("4".repeat(64))),
        ("/boundary/execution_fingerprint", json!("4".repeat(64))),
        ("/boundary/invocation_digest", json!("6".repeat(64))),
        ("/ambient/umask", json!(23)),
        ("/ambient/jobs", json!(2)),
        ("/ambient/checkout_root", json!("/external-release-B")),
        ("/ambient/started_at_seconds", json!(101)),
        ("/ambient/host_locale", json!("en_US.UTF-8")),
        ("/ambient/host_timezone", json!("Pacific/Auckland")),
    ] {
        mutate_record(&mut fixture, index, pointer, value);
    }
    if !same {
        let artifact = fixture.transfers[index]
            .members
            .get_mut("artifact")
            .expect("should find native B");
        artifact.push(1);
        let (hash, size) = (digest(artifact), artifact.len());
        mutate_record(&mut fixture, index, "/artifact_sha256", json!(hash));
        mutate_record(&mut fixture, index, "/artifact_size", json!(size));
        for pointer in RECEIPT_ARTIFACT_BINDINGS {
            rebind_composition(&mut fixture, index, pointer, json!(hash));
        }
    }
    fixture
}

/// Every receipt field the producer binds to the exact final artifact digest.
const RECEIPT_ARTIFACT_BINDINGS: [&str; 3] = [
    "/composition/javascript/artifact_sha256",
    "/composition/platform/artifact_sha256",
    "/composition/platform/external_requirements/artifact_sha256",
];

/// Fixture receipt pointer to the registry dependency's package origin.
const DEPENDENCY_ORIGIN: &str = "/composition/package_origins/packages/registry+https:~1~1registry.example.com~1index#example-dep@1.0.0";
/// Fixture receipt pointer to the workspace package's origin.
const WORKSPACE_ORIGIN: &str =
    "/composition/package_origins/packages/path+file:~1~1~1workspace~1crates~1example-app#0.1.0";

/// Receipt changes that each break one collector composition check.
fn composition_gates() -> Vec<(String, Value)> {
    let mut gates = RECEIPT_ARTIFACT_BINDINGS
        .iter()
        .map(|pointer| ((*pointer).to_owned(), json!("9".repeat(64))))
        .collect::<Vec<_>>();
    gates.extend([
        (
            "/composition/package_origins/source_commit".to_owned(),
            json!("9".repeat(40)),
        ),
        (
            format!("{WORKSPACE_ORIGIN}/origin/source_commit"),
            json!("9".repeat(40)),
        ),
        (
            "/composition/javascript/inventory_sha256".to_owned(),
            json!("9".repeat(64)),
        ),
        (
            "/composition/package_origins/materials_sha256".to_owned(),
            json!("9".repeat(64)),
        ),
        (
            "/composition/platform/target".to_owned(),
            json!("example-unknown-none"),
        ),
        (
            "/composition/platform/external_requirements/target".to_owned(),
            json!("example-unknown-none"),
        ),
        (
            "/composition/native/material_native_coverage_complete".to_owned(),
            json!(true),
        ),
        (
            "/composition/platform/material_native_coverage_complete".to_owned(),
            json!(true),
        ),
        ("/observations/units".to_owned(), json!([])),
        (
            "/composition/package_origins/packages".to_owned(),
            json!({}),
        ),
        (
            format!("{DEPENDENCY_ORIGIN}/manifest_path"),
            json!("crates/example-app/Cargo.toml"),
        ),
        (
            format!("{DEPENDENCY_ORIGIN}/prepared_manifest_sha256"),
            json!("not-a-digest"),
        ),
        ("/metadata_plan_sha256".to_owned(), json!("7".repeat(63))),
        ("/tree_plan_sha256".to_owned(), json!("8".repeat(65))),
        (
            "/composition/javascript/modules/0/module/sha256".to_owned(),
            json!("not-a-digest"),
        ),
        ("/composition/javascript/modules".to_owned(), json!([])),
        (
            "/composition/graph/selected_package_id".to_owned(),
            json!("path+file:///workspace/crates/another#0.1.0"),
        ),
        (
            "/observations/units/0/package_id".to_owned(),
            json!("path+file:///workspace/crates/another#0.1.0"),
        ),
        (
            "/composition/native/contributors/0/original_sha256".to_owned(),
            json!("c3".repeat(31)),
        ),
    ]);
    gates
}

#[test]
fn every_composition_receipt_gate_rejects_its_changed_fact() {
    for (pointer, value) in composition_gates() {
        let mut fixture = cohort_fixture();
        rebind_composition(&mut fixture, 0, &pointer, value);
        let error = diagnose_collection(&fixture)
            .expect_err("should reject a changed composition receipt fact");
        assert!(
            format!("{error:?}").contains("composition receipt"),
            "should reject receipt {pointer} at a composition receipt gate: {error:?}"
        );
    }
}

#[test]
fn composition_receipt_shape_is_strict() {
    let fixture = cohort_fixture();
    let receipt: Value = serde_json::from_slice(&fixture.transfers[0].members["composition.json"])
        .expect("should parse fixture receipt");
    let summary = json!({
        "artifact_sha256": digest(&fixture.transfers[0].members["artifact"]),
        "source_commit": fixture.invocation.source_commit,
        "inventory_sha256": fixture.invocation.inventory_sha256,
        "contributing_sources": ["d".repeat(64)],
        "compiled_units": 1,
    });
    let mut extra_top = receipt.clone();
    extra_top["coverage_complete"] = json!(true);
    let mut extra_nested = receipt.clone();
    extra_nested["composition"]["javascript"]["function_reachability"] = json!(true);
    let mut missing = receipt.clone();
    missing["composition"]
        .as_object_mut()
        .expect("should find composition")
        .remove("native");
    let mut extra_origin = receipt.clone();
    extra_origin
        .pointer_mut(&format!("{DEPENDENCY_ORIGIN}/origin/original"))
        .and_then(Value::as_object_mut)
        .expect("should find the registry original")
        .insert("verified".to_owned(), json!(true));
    let mut missing_apple = receipt.clone();
    missing_apple["composition"]["platform"]
        .as_object_mut()
        .expect("should find platform")
        .remove("apple");
    let serialized = |value: &Value| serde_json::to_vec(value).expect("should serialize receipt");
    let text = String::from_utf8(serialized(&receipt)).expect("should be UTF-8");
    let duplicate = text
        .replacen(
            "\"metadata_plan_sha256\"",
            &format!(
                "\"metadata_plan_sha256\":\"{}\",\"metadata_plan_sha256\"",
                "7".repeat(64)
            ),
            1,
        )
        .into_bytes();
    for (name, bytes) in [
        ("collector summary", serialized(&summary)),
        ("extra top-level field", serialized(&extra_top)),
        ("extra nested field", serialized(&extra_nested)),
        ("missing component", serialized(&missing)),
        ("extra tagged origin field", serialized(&extra_origin)),
        ("missing opaque platform member", serialized(&missing_apple)),
        ("duplicate key", duplicate),
    ] {
        let mut changed = cohort_fixture();
        let hash = digest(&bytes);
        changed.transfers[0]
            .members
            .insert("composition.json".to_owned(), bytes);
        mutate_record(&mut changed, 0, "/composition_sha256", json!(hash));
        let error = format!(
            "{:?}",
            diagnose_collection(&changed).expect_err("should reject a malformed receipt")
        );
        assert!(
            !error.contains("composition receipt") && !error.contains("digest changed"),
            "should reject a receipt with {name} while parsing it: {error}"
        );
    }
}

#[test]
fn receipt_coverage_claims_apple_evidence_and_runtime_format_follow_the_target() {
    // Transfers 0, 2, 3 and 4 are Fastly, both Darwin CLIs and the Linux CLI.
    let cases = [
        (
            0,
            "/composition/native/material_native_coverage_complete",
            json!(true),
            "claims complete material native coverage",
        ),
        (
            4,
            "/composition/platform/material_native_coverage_complete",
            json!(true),
            "claims complete material native coverage",
        ),
        (
            0,
            "/composition/platform/apple",
            json!({"sdk_version": "15.0"}),
            "Apple evidence does not match its target",
        ),
        (
            4,
            "/composition/platform/apple",
            json!({"sdk_version": "15.0"}),
            "Apple evidence does not match its target",
        ),
        (
            2,
            "/composition/platform/apple",
            Value::Null,
            "Apple evidence does not match its target",
        ),
        (
            0,
            "/composition/platform/external_requirements/runtime/format",
            json!("elf"),
            "runtime format does not match its target",
        ),
        (
            3,
            "/composition/platform/external_requirements/runtime/format",
            json!("wasm"),
            "runtime format does not match its target",
        ),
        (
            4,
            "/composition/platform/external_requirements/runtime/format",
            json!("macho"),
            "runtime format does not match its target",
        ),
    ];
    for (index, pointer, value, expected) in cases {
        let mut fixture = cohort_fixture();
        rebind_composition(&mut fixture, index, pointer, value);
        let error = diagnose_collection(&fixture).expect_err("should reject the receipt");
        assert!(
            format!("{error:?}").contains(expected),
            "should reject transfer {index} receipt {pointer} with {expected}: {error:?}"
        );
    }
}

#[test]
fn receipt_entry_point_binds_producer_facts_for_every_fixed_asset() {
    let (artifact, source, inventory, materials) = (
        "a".repeat(64),
        "b".repeat(40),
        "c".repeat(64),
        "e".repeat(64),
    );
    let facts = |asset: &'static str| ReceiptFacts {
        artifact_sha256: &artifact,
        source_commit: &source,
        inventory_sha256: &inventory,
        materials_sha256: &materials,
        asset,
    };
    for fixed in &crate::policy::FIXED_ARTIFACTS {
        let receipt = composition_receipt(fixed.target.triple(), &artifact, &source, &inventory);
        check_composition_receipt(&receipt, &facts(fixed.name)).unwrap_or_else(|error| {
            panic!("should accept the receipt for {}: {error:?}", fixed.name)
        });
        let other_materials = "9".repeat(64);
        let error = check_composition_receipt(
            &receipt,
            &ReceiptFacts {
                materials_sha256: &other_materials,
                ..facts(fixed.name)
            },
        )
        .expect_err("should reject other build materials");
        assert!(
            format!("{error:?}").contains("not bound to the build materials"),
            "should reject {} receipt for other materials: {error:?}",
            fixed.name
        );
    }
    let fastly = &crate::policy::FIXED_ARTIFACTS[0];
    let receipt = composition_receipt(fastly.target.triple(), &artifact, &source, &inventory);
    let error = check_composition_receipt(&receipt, &facts(crate::policy::FIXED_ARTIFACTS[3].name))
        .expect_err("should reject a receipt for another asset target");
    assert!(
        format!("{error:?}").contains("composition receipt names another target"),
        "should derive the target from the asset: {error:?}"
    );
    let error = check_composition_receipt(&receipt, &facts("unknown-asset"))
        .expect_err("should reject an unknown asset");
    assert!(
        format!("{error:?}").contains("unknown cohort artifact"),
        "should reject an asset outside the fixed set: {error:?}"
    );
}

#[test]
fn fastly_receipt_must_embed_every_js_module_while_native_may_not() {
    let pointer = "/composition/javascript/modules/0/classification";
    let mut fastly = cohort_fixture();
    rebind_composition(&mut fastly, 0, pointer, json!("js_build_material"));
    let error = diagnose_collection(&fastly).expect_err("should reject unembedded Fastly JS");
    assert!(
        format!("{error:?}").contains("composition receipt omits a complete Fastly JS module"),
        "should reject a Fastly receipt with an unembedded module: {error:?}"
    );
    let mut native = cohort_fixture();
    rebind_composition(&mut native, 2, pointer, json!("js_build_material"));
    diagnose_collection(&native).expect("should accept CLI-discarded JS build material");
}

/// Every optional reproduction state: never started, failed, matched, mismatched.
fn every_optional_reproduction_state() -> Vec<(&'static str, ValidatedTransfers)> {
    let mut failed = cohort_fixture();
    for absent in &mut failed.absent {
        absent.job = FAILED_BEFORE_UPLOAD;
        absent.elapsed_seconds = 7;
    }
    vec![
        ("unavailable", cohort_fixture()),
        ("failed", failed),
        ("matched", with_uploaded_native_b(true)),
        ("mismatched", with_uploaded_native_b(false)),
    ]
}

#[test]
fn every_optional_reproduction_state_is_accepted_with_honest_outcomes() {
    for (state, fixture) in every_optional_reproduction_state() {
        let diagnostic = diagnose_collection(&fixture)
            .unwrap_or_else(|error| panic!("should accept optional B state {state}: {error:?}"));
        assert_eq!(
            serde_json::to_value(diagnostic.native[crate::policy::FIXED_ARTIFACTS[1].name])
                .expect("should serialize outcome"),
            json!(state),
            "should record exactly the {state} outcome"
        );
        assert_eq!(
            diagnostic.canonical.len(),
            4,
            "should preserve all canonical artifacts once mandatory gates pass"
        );
    }
}

#[test]
fn no_optional_reproduction_state_hides_a_mandatory_canonical_failure() {
    let record_gates = [
        ("/boundary/confined", json!(false)),
        ("/boundary/control_exit", json!(1)),
        ("/boundary/control_accepts", json!(0)),
        ("/boundary/control_accepts", json!(2)),
        ("/boundary/probe_exit", json!(0)),
        ("/boundary/probe_errno", json!(111)),
        ("/boundary/probe_errno", json!(0)),
        ("/boundary/probe_accepts", json!(1)),
        ("/boundary/execution_fingerprint", json!("9".repeat(64))),
        ("/boundary/invocation_digest", json!("5".repeat(63))),
        ("/inventory_sha256", json!("9".repeat(64))),
        ("/composition_sha256", json!("9".repeat(64))),
        ("/recipe_sha256", json!("9".repeat(64))),
        ("/artifact_size", json!(1)),
        ("/ambient/child_locale", json!("en_US.UTF-8")),
        ("/claim/varied", json!(["umask", "jobs", "checkout_root"])),
    ];
    let composition_gates = composition_gates();
    for canonical in [0, 2, 3, 4] {
        for (state, fixture) in every_optional_reproduction_state() {
            for (pointer, value) in &record_gates {
                let mut changed = fixture_clone(&fixture);
                mutate_record(&mut changed, canonical, pointer, value.clone());
                assert!(
                    diagnose_collection(&changed).is_err(),
                    "should block canonical {canonical} {pointer} with optional B {state}"
                );
            }
            for (pointer, value) in &composition_gates {
                let mut changed = fixture_clone(&fixture);
                rebind_composition(&mut changed, canonical, pointer, value.clone());
                assert!(
                    diagnose_collection(&changed).is_err(),
                    "should block canonical {canonical} composition {pointer} with optional B {state}"
                );
            }
            let mut changed = fixture_clone(&fixture);
            changed.transfers[canonical]
                .members
                .insert("tsjs-inventory.json".to_owned(), b"{}".to_vec());
            assert!(
                diagnose_collection(&changed).is_err(),
                "should block changed JS inventory bytes in canonical {canonical} with optional B {state}"
            );
            let mut changed = fixture_clone(&fixture);
            changed.transfers[canonical]
                .members
                .get_mut("artifact")
                .expect("should find canonical artifact")
                .push(0);
            assert!(
                diagnose_collection(&changed).is_err(),
                "should block changed final bytes in canonical {canonical} with optional B {state}"
            );
        }
    }
}

#[test]
fn fastly_runtime_proof_is_required_for_both_roles_and_absent_from_native() {
    for index in [0, 1] {
        for (pointer, value) in [
            ("/runtime", Value::Null),
            ("/runtime/source_commit", json!("9".repeat(40))),
            ("/runtime/final_wasm_size", json!(1)),
            ("/runtime/viceroy_version", json!("0.0.0")),
            ("/runtime/viceroy_version", json!("0.17.0")),
        ] {
            let mut fixture = cohort_fixture();
            mutate_record(&mut fixture, index, pointer, value);
            assert!(
                diagnose_collection(&fixture).is_err(),
                "should reject Fastly role {index} without bound final-byte runtime {pointer}"
            );
        }
    }
    let runtime = serde_json::from_slice::<Value>(
        &cohort_fixture().transfers[0].members["build-record.json"],
    )
    .expect("should parse Fastly record")["runtime"]
        .clone();
    for index in [2, 3, 4] {
        let mut fixture = cohort_fixture();
        mutate_record(&mut fixture, index, "/runtime", runtime.clone());
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject a native record carrying a Fastly runtime diagnostic"
        );
    }
}

#[test]
fn fastly_runtime_proof_must_belong_to_its_own_build_role() {
    let runtime_of = |fixture: &ValidatedTransfers, index: usize| {
        serde_json::from_slice::<Value>(&fixture.transfers[index].members["build-record.json"])
            .expect("should parse Fastly record")["runtime"]
            .clone()
    };
    let fixture = cohort_fixture();
    assert_eq!(
        runtime_of(&fixture, 0)["role"],
        "A",
        "should bind the canonical runtime proof to Role::Canonical"
    );
    assert_eq!(
        runtime_of(&fixture, 1)["role"],
        "B",
        "should bind the reproduction runtime proof to Role::Reproduction"
    );
    // Equal final bytes make the proofs otherwise interchangeable, so each
    // case swaps in the other role's run or an unbound role.
    for (index, other) in [(0, 1), (1, 0)] {
        let mut fixture = cohort_fixture();
        let reused = runtime_of(&fixture, other);
        mutate_record(&mut fixture, index, "/runtime", reused);
        let error = diagnose_collection(&fixture)
            .expect_err("should reject one role's runtime run reused for the other");
        assert!(
            format!("{error:?}").contains("runtime diagnostic"),
            "should reject reused runtime proof at the runtime gate: {error:?}"
        );
        for role in [json!("C"), json!(""), Value::Null] {
            let mut fixture = cohort_fixture();
            mutate_record(&mut fixture, index, "/runtime/role", role.clone());
            assert!(
                diagnose_collection(&fixture).is_err(),
                "should reject Fastly record {index} with runtime role {role}"
            );
        }
        let mut fixture = cohort_fixture();
        let mut unbound = runtime_of(&fixture, index);
        unbound
            .as_object_mut()
            .expect("should hold runtime fields")
            .remove("role");
        mutate_record(&mut fixture, index, "/runtime", unbound);
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject Fastly record {index} whose runtime proof carries no role"
        );
    }
}

/// Optional reproduction jobs that never uploaded, with their spec outcomes.
///
/// `unavailable` means no reproduction ran: still queued at the deadline, or
/// skipped/cancelled by the scheduler before any step started. `failed` means
/// the job started, or its observed conclusion itself states failure.
fn no_upload_job_states() -> Vec<(&'static str, ObservedJob, u64, records::Outcome)> {
    let started = |conclusion| ObservedJob {
        started: true,
        conclusion,
    };
    let not_started = |conclusion| ObservedJob {
        started: false,
        conclusion,
    };
    vec![
        (
            "queued at deadline",
            QUEUED,
            1800,
            records::Outcome::Unavailable,
        ),
        (
            "scheduler skipped",
            not_started(Some(JobConclusion::Skipped)),
            3,
            records::Outcome::Unavailable,
        ),
        (
            "scheduler cancelled before start",
            not_started(Some(JobConclusion::Cancelled)),
            3,
            records::Outcome::Unavailable,
        ),
        (
            "failed before any step",
            not_started(Some(JobConclusion::Failure)),
            3,
            records::Outcome::Failed,
        ),
        (
            "timed out before any step",
            not_started(Some(JobConclusion::TimedOut)),
            1800,
            records::Outcome::Failed,
        ),
        (
            "failed before upload",
            FAILED_BEFORE_UPLOAD,
            7,
            records::Outcome::Failed,
        ),
        (
            "cancelled after start",
            started(Some(JobConclusion::Cancelled)),
            7,
            records::Outcome::Failed,
        ),
        (
            "timed out after start",
            started(Some(JobConclusion::TimedOut)),
            1800,
            records::Outcome::Failed,
        ),
        (
            "succeeded without upload output",
            started(Some(JobConclusion::Success)),
            7,
            records::Outcome::Failed,
        ),
        (
            "still running at deadline",
            started(None),
            1800,
            records::Outcome::Failed,
        ),
    ]
}

#[test]
fn optional_reproduction_job_states_map_to_bounded_honest_outcomes() {
    for (state, job, elapsed, expected) in no_upload_job_states() {
        let mut fixture = cohort_fixture();
        fixture.absent[0] = no_upload(crate::policy::FIXED_ARTIFACTS[1].name, job, elapsed);
        let diagnostic = diagnose_collection(&fixture)
            .unwrap_or_else(|error| panic!("should accept optional B {state}: {error:?}"));
        assert_eq!(
            diagnostic.native[crate::policy::FIXED_ARTIFACTS[1].name],
            expected,
            "should map optional B {state} to its spec outcome"
        );
        assert_eq!(
            diagnostic.canonical.len(),
            4,
            "should keep every canonical artifact with optional B {state}"
        );
        let reproduction =
            &diagnostic.rebuild[crate::policy::FIXED_ARTIFACTS[1].name]["reproduction"];
        assert_eq!(
            reproduction["outcome"],
            serde_json::to_value(expected).expect("should serialize outcome"),
            "should record the derived outcome for {state}"
        );
        assert_eq!(
            reproduction["job_state"]["started"], job.started,
            "should record whether {state} started"
        );
        for field in ["artifact_id", "artifact_name", "archive_sha256", "ambient"] {
            assert!(
                reproduction.get(field).is_none(),
                "should not synthesize {field} for optional B {state}"
            );
        }
    }
}

#[test]
fn contradictory_premature_or_unbounded_optional_job_states_are_rejected() {
    let cases = [
        (
            "success without starting",
            ObservedJob {
                started: false,
                conclusion: Some(JobConclusion::Success),
            },
            7,
            1800,
        ),
        (
            "skipped after starting",
            ObservedJob {
                started: true,
                conclusion: Some(JobConclusion::Skipped),
            },
            7,
            1800,
        ),
        ("queued before the deadline", QUEUED, 1799, 1800),
        (
            "running before the deadline",
            ObservedJob {
                started: true,
                conclusion: None,
            },
            1799,
            1800,
        ),
        ("waited past the deadline", QUEUED, 1801, 1800),
        ("missing deadline", FAILED_BEFORE_UPLOAD, 0, 0),
        ("deadline beyond the cohort cap", QUEUED, 5401, 5401),
    ];
    for (state, job, elapsed, deadline) in cases {
        let absent = NoUpload {
            asset: crate::policy::FIXED_ARTIFACTS[1].name.to_owned(),
            job,
            elapsed_seconds: elapsed,
            deadline_seconds: deadline,
        };
        let mut fixture = cohort_fixture();
        fixture.absent[0] = absent.clone();
        assert!(
            diagnose_collection(&fixture).is_err(),
            "should reject optional B {state} in the measurement"
        );
        let (mut dependencies, mut source) = transfer_fixture();
        dependencies.optional = vec![OptionalBuild::NotUploaded(absent)];
        assert!(
            validate_transfers(&dependencies, &mut source).is_err(),
            "should reject optional B {state} before any transfer"
        );
        assert!(
            source.requested.is_empty(),
            "should reject optional B {state} before fetching any artifact"
        );
    }
}

#[test]
fn optional_upload_identity_requires_a_started_successful_job() {
    let uploaded_as_optional = |job| {
        let (mut dependencies, source) = transfer_fixture();
        let mut upload = dependencies.required.remove(0);
        upload.role = "B".to_owned();
        upload.asset = crate::policy::FIXED_ARTIFACTS[1].name.to_owned();
        dependencies.optional = vec![OptionalBuild::Uploaded { upload, job }];
        (dependencies, source)
    };
    let (dependencies, mut source) = uploaded_as_optional(ObservedJob {
        started: true,
        conclusion: Some(JobConclusion::Success),
    });
    validate_transfers(&dependencies, &mut source)
        .expect("should accept an upload from a started successful optional job");
    for (state, job, _, _) in no_upload_job_states()
        .into_iter()
        .filter(|(state, ..)| *state != "succeeded without upload output")
    {
        let (dependencies, mut source) = uploaded_as_optional(job);
        assert!(
            validate_transfers(&dependencies, &mut source).is_err(),
            "should reject an upload identity claimed by optional B {state}"
        );
        assert!(
            source.requested.is_empty(),
            "should reject the claimed upload for {state} before fetching it"
        );
    }
}

#[test]
fn dependency_channels_only_accept_their_own_build_roles() {
    let succeeded = ObservedJob {
        started: true,
        conclusion: Some(JobConclusion::Success),
    };
    let native = crate::policy::FIXED_ARTIFACTS[1].name;
    let fastly = crate::policy::FIXED_ARTIFACTS[0].name;
    // The optional channel carries only native reproductions.
    for (asset, role) in [(native, "A"), (fastly, "B"), (fastly, "A")] {
        let (mut dependencies, mut source) = transfer_fixture();
        let mut upload = dependencies.required.remove(0);
        upload.asset = asset.to_owned();
        upload.role = role.to_owned();
        dependencies.optional = vec![OptionalBuild::Uploaded {
            upload,
            job: succeeded,
        }];
        assert!(
            validate_transfers(&dependencies, &mut source).is_err(),
            "should reject {asset} role {role} supplied through the optional channel"
        );
        assert!(
            source.requested.is_empty(),
            "should reject optional {asset} role {role} before any fetch"
        );
    }
    // The mandatory channel carries every A and only the Fastly reproduction.
    let (dependencies, mut source) = transfer_fixture();
    validate_transfers(&dependencies, &mut source).expect("should accept a mandatory Fastly A");
    for (asset, role, accepted) in [(fastly, "B", true), (native, "B", false)] {
        let (mut dependencies, mut source) = transfer_fixture();
        dependencies.required[0].asset = asset.to_owned();
        dependencies.required[0].role = role.to_owned();
        let result = validate_transfers(&dependencies, &mut source);
        assert_eq!(
            result.is_ok(),
            accepted,
            "should only accept mandatory B for the Fastly asset ({asset})"
        );
        if !accepted {
            assert!(
                source.requested.is_empty(),
                "should reject mandatory native B before any fetch"
            );
        }
    }
}

#[test]
fn missing_mandatory_canonical_rejects_under_every_optional_reproduction_state() {
    let mut states = every_optional_reproduction_state();
    for (state, job, elapsed, _) in no_upload_job_states() {
        let mut fixture = cohort_fixture();
        for absent in &mut fixture.absent {
            absent.job = job;
            absent.elapsed_seconds = elapsed;
        }
        states.push((state, fixture));
    }
    for (state, fixture) in &states {
        for asset in crate::policy::FIXED_ARTIFACTS {
            let mut changed = fixture_clone(fixture);
            changed.transfers.retain(|transfer| {
                transfer.upload.asset != asset.name || transfer.upload.role != "A"
            });
            let error = diagnose_collection(&changed)
                .expect_err("should reject a missing, failed or cancelled mandatory A");
            assert!(
                format!("{error:?}").contains("missing mandatory A"),
                "should block missing A {} with optional B {state}: {error:?}",
                asset.name
            );
        }
    }
}

/// Copies one fixture so each mandatory gate mutates an independent cohort.
fn fixture_clone(fixture: &ValidatedTransfers) -> ValidatedTransfers {
    ValidatedTransfers {
        invocation: fixture.invocation.clone(),
        transfers: fixture
            .transfers
            .iter()
            .map(|transfer| Transfer {
                upload: transfer.upload.clone(),
                members: transfer.members.clone(),
            })
            .collect(),
        absent: fixture.absent.clone(),
    }
}
