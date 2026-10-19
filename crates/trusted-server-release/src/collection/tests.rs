use std::collections::BTreeMap;
use std::io::Write as _;

use flate2::Crc;
use serde_json::{Value, json};

use super::*;

fn zip(members: &[(&str, &[u8])], descriptor: bool) -> Vec<u8> {
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

fn cohort_fixture() -> ValidatedTransfers {
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
            let composition=serde_json::to_vec(&json!({"artifact_sha256":digest(&artifact),"source_commit":invocation.source_commit,"inventory_sha256":invocation.inventory_sha256,"contributing_sources":["d".repeat(64)],"compiled_units":1})).expect("should serialize composition");
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
                record["ambient"]["umask"] = json!(23);
                record["ambient"]["jobs"] = json!(2);
                record["ambient"]["checkout_root"] = json!("/external-release-B");
                record["ambient"]["started_at_seconds"] = json!(101);
                record["ambient"]["host_locale"] = json!("en_US.UTF-8");
                record["ambient"]["host_timezone"] = json!("Pacific/Auckland");
            }
            if index == 0 {
                record["runtime"] = json!({"source_commit":invocation.source_commit,"final_wasm_sha256":digest(&artifact),"final_wasm_size":artifact.len(),"publisher_status":200,"injected_scripts":1,"origin_marker_matched":true,"viceroy_version":"0.17.0"});
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
        .map(|asset| OptionalBuild::NotUploaded {
            asset: asset.name.to_owned(),
            outcome: records::Outcome::Unavailable,
            elapsed_seconds: 0,
            deadline_seconds: 1800,
        })
        .collect();
    ValidatedTransfers {
        invocation,
        transfers,
        absent,
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
        ("/boundary/invocation_fingerprint", json!("9".repeat(64))),
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
    let composition=serde_json::to_vec(&json!({"artifact_sha256":sha,"source_commit":fixture.invocation.source_commit,"inventory_sha256":fixture.invocation.inventory_sha256,"contributing_sources":["d".repeat(64)],"compiled_units":1})).expect("should bind changed composition");
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
        let mut fixture = cohort_fixture();
        let mut b = Transfer {
            upload: fixture.transfers[2].upload.clone(),
            members: fixture.transfers[2].members.clone(),
        };
        b.upload.role = "B".to_owned();
        b.upload.job = "native-b".to_owned();
        b.upload.artifact_id = 99;
        fixture.transfers.push(b);
        let index = fixture.transfers.len() - 1;
        for (pointer, value) in [
            ("/role", json!("B")),
            ("/job", json!("native-b")),
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
            fixture.transfers[index]
                .members
                .get_mut("artifact")
                .expect("should find native B")
                .push(1);
            let artifact = fixture.transfers[index].members["artifact"].clone();
            let hash = digest(&artifact);
            mutate_record(&mut fixture, index, "/artifact_sha256", json!(hash));
            mutate_record(&mut fixture, index, "/artifact_size", json!(artifact.len()));
            let composition=serde_json::to_vec(&json!({"artifact_sha256":hash,"source_commit":fixture.invocation.source_commit,"inventory_sha256":fixture.invocation.inventory_sha256,"contributing_sources":["d".repeat(64)],"compiled_units":1})).expect("should bind native B bytes");
            let hash = digest(&composition);
            fixture.transfers[index]
                .members
                .insert("composition.json".to_owned(), composition);
            mutate_record(&mut fixture, index, "/composition_sha256", json!(hash));
        }
        fixture.absent.remove(0);
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
    for outcome in [records::Outcome::Failed, records::Outcome::Unavailable] {
        let mut fixture = cohort_fixture();
        fixture.absent[0] = OptionalBuild::NotUploaded {
            asset: crate::policy::FIXED_ARTIFACTS[1].name.to_owned(),
            outcome,
            elapsed_seconds: 7,
            deadline_seconds: 1800,
        };
        assert_eq!(
            diagnose_collection(&fixture)
                .expect("should retain bounded native B failure")
                .native[crate::policy::FIXED_ARTIFACTS[1].name],
            outcome,
            "should report honest bounded outcome"
        );
        if let OptionalBuild::NotUploaded {
            elapsed_seconds, ..
        } = &mut fixture.absent[0]
        {
            *elapsed_seconds = 1801;
        }
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
        3,
        "should emit only outcome, elapsed and deadline without invented identity or observations"
    );
    for field in [
        "artifact_id",
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
        optional: fixture.absent,
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
