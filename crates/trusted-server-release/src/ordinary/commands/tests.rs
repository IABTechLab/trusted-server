use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use super::{compare_transfers, transfer_plan};

struct Fixture {
    _temporary: tempfile::TempDir,
    source: crate::ordinary::inputs::tests::Fixture,
    input: PathBuf,
    archives: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("should create command fixture");
        let root = fs::canonicalize(temporary.path()).expect("should resolve fixture root");
        let input = root.join("input");
        let archives = root.join("archives");
        fs::create_dir(&input).expect("should create private metadata root");
        fs::create_dir(&archives).expect("should create private archives root");
        let source = crate::ordinary::inputs::tests::Fixture::new();
        fs::write(
            source.checkout.join("rust-toolchain.toml"),
            b"[toolchain]\nchannel = \"1.98.1\"\n",
        )
        .expect("should retain comparison compiler pin");
        fs::write(source.checkout.join(".tool-versions"), b"nodejs 24.20.0\n")
            .expect("should retain comparison Node pin");
        let (metadata, zip) = crate::ordinary::transfer::tests::command_fixture(&source.checkout);
        for (name, bytes) in ["context.json", "jobs.json", "artifacts.json"]
            .into_iter()
            .zip(metadata)
        {
            fs::write(input.join(name), bytes).expect("should retain service fixture bytes");
        }
        for (name, bytes) in [
            "canonical.zip",
            "canonical-observation.zip",
            "reproduction.zip",
            "reproduction-observation.zip",
        ]
        .into_iter()
        .zip(zip)
        {
            fs::write(archives.join(name), bytes).expect("should retain exact ZIP fixture bytes");
        }
        Self {
            _temporary: temporary,
            source,
            input,
            archives,
            output: root.join("output"),
        }
    }
}

#[test]
fn validated_download_plan_uses_four_fixed_names_and_direct_upload_ids() {
    let fixture = Fixture::new();
    let value = transfer_plan(&fixture.input).expect("should validate exact current-run metadata");
    assert_eq!(value["version"], 1, "should version the private handoff");
    let archives = value["archives"]
        .as_array()
        .expect("should list exact downloads");
    assert_eq!(archives.len(), 4, "should plan only four bound archives");
    assert_eq!(
        archives[0]["file"], "canonical.zip",
        "should use a fixed local name"
    );
    assert!(
        archives
            .iter()
            .all(|item| item["id"].as_u64().is_some_and(|id| id > 0)
                && item["size"].as_u64().is_some_and(|size| size > 0)),
        "should retain service-correlated IDs and sizes"
    );
}

#[test]
fn complete_transfer_command_seals_only_real_equality() {
    let fixture = Fixture::new();
    let path = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect("should compare and seal real bound archives");
    assert_eq!(
        path.file_name().expect("should have fixed filename"),
        "trusted-server-fastly.wasm.rebuild-check.json",
        "should preserve the release contract"
    );
    assert!(
        !fixture
            .output
            .parent()
            .expect("should have parent")
            .join("ordinary-diagnostics")
            .exists(),
        "should write no diagnostic after success"
    );
    let error = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should reject the reused output directory");
    assert!(
        format!("{error:?}").contains("ordinary output must use an absent root"),
        "should reject reused outputs"
    );
}

#[test]
fn altered_archive_or_service_identity_emits_no_publishable_companion() {
    for alter_archive in [false, true] {
        let fixture = Fixture::new();
        if alter_archive {
            fs::write(fixture.archives.join("canonical.zip"), b"changed bytes")
                .expect("should corrupt fixture archive");
        } else {
            fs::write(fixture.input.join("jobs.json"), b"{}")
                .expect("should remove authenticated job records");
        }
        assert!(
            compare_transfers(
                &fixture.input,
                &fixture.archives,
                &fixture.source.checkout,
                &fixture.output
            )
            .is_err(),
            "should fail before creating a result"
        );
        assert!(
            !fixture.output.exists(),
            "should emit no placeholder or failed receipt"
        );
    }
}

#[test]
fn aliased_metadata_and_archive_roots_are_rejected() {
    let fixture = Fixture::new();
    let root = fixture
        .input
        .parent()
        .expect("should retain fixture parent");
    symlink(&fixture.input, root.join("input-alias")).expect("should alias metadata root");
    symlink(&fixture.archives, root.join("archive-alias")).expect("should alias archive root");
    assert!(
        transfer_plan(&root.join("input-alias")).is_err(),
        "should reject a metadata root alias"
    );
    assert!(
        compare_transfers(
            &fixture.input,
            &root.join("archive-alias"),
            &fixture.source.checkout,
            &fixture.output
        )
        .is_err(),
        "should reject an archive root alias"
    );
    assert!(
        !fixture.output.exists(),
        "should leave no output after rejection"
    );
}

#[test]
fn equally_claimed_inputs_must_match_the_independent_comparison_source() {
    for (file, bytes) in [
        ("Cargo.lock", "changed lock"),
        ("scripts/release/phase1-build.sh", "changed recipe"),
        (".tool-versions", "nodejs 24.12.0\n"),
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.source.checkout.join(file), bytes)
            .expect("should change comparison source");
        assert!(
            compare_transfers(
                &fixture.input,
                &fixture.archives,
                &fixture.source.checkout,
                &fixture.output
            )
            .is_err(),
            "should reject equal observations that do not match independently read source"
        );
        assert!(
            !fixture.output.exists(),
            "should create no companion for invented input facts"
        );
    }
}

#[test]
fn malformed_measurement_request_fails_before_any_tool_query() {
    let fixture = Fixture::new();
    let request = fixture.input.join("malformed.json");
    fs::write(&request, b"{}").expect("should write malformed capture without tool paths");
    assert!(
        super::measure_build(&request, &fixture.output).is_err(),
        "should reject missing fields before querying any executable"
    );
    assert!(
        !fixture.output.exists(),
        "should create no observation after invalid capture"
    );
}

#[test]
fn observation_archive_limit_is_applied_before_reading_its_body() {
    let fixture = Fixture::new();
    fs::OpenOptions::new()
        .write(true)
        .open(fixture.archives.join("canonical-observation.zip"))
        .expect("should open the retained observation ZIP")
        .set_len(2 * 1024 * 1024 + 1)
        .expect("should create a sparse oversized observation");
    let error = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should reject the oversized observation before parsing");
    assert!(
        format!("{error:?}").contains("input exceeds role byte limit"),
        "should reject observation archives at their smaller bound"
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .output
                .parent()
                .expect("should have parent")
                .join("ordinary-diagnostics/ordinary-comparison-diagnostic.json"),
        )
        .expect("should retain rejected-transfer diagnostic"),
    )
    .expect("should parse bounded diagnostic");
    assert_eq!(
        diagnostic["code"], "transfer_rejected",
        "should distinguish unsafe oversized transfer from unavailable input"
    );
    assert!(
        !fixture.output.exists(),
        "should write no companion for oversized input"
    );
}

#[test]
fn failed_comparison_retains_bounded_diagnostic_with_explicit_unavailable_facts() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.archives.join("canonical.zip"))
        .expect("should remove one unavailable transfer");
    let _ = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should reject unavailable transfer");
    let bytes = fs::read(
        fixture
            .output
            .parent()
            .expect("should have parent")
            .join("ordinary-diagnostics/ordinary-comparison-diagnostic.json"),
    )
    .expect("should retain the structured failure diagnostic");
    assert!(bytes.len() <= 4096, "should enforce diagnostic byte cap");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("should parse the diagnostic");
    assert_eq!(
        value["outcome"], "unavailable",
        "should distinguish unavailable transfer"
    );
    assert_eq!(value["stage"], "transfer", "should identify transfer stage");
    assert_eq!(
        value["code"], "transfer_unavailable",
        "should use fixed transfer code"
    );
    assert_eq!(
        value["facts"]["canonical_sha256"], "unavailable",
        "should not invent unobserved binary facts"
    );
    assert!(
        !fixture.output.exists(),
        "should never write a failed companion"
    );
}

#[test]
fn failed_comparison_records_independent_source_input_failure() {
    let fixture = Fixture::new();
    fs::write(fixture.source.checkout.join("Cargo.lock"), b"changed lock")
        .expect("should alter independent source input");
    let _ = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should fail independent source check");
    let bytes = fs::read(
        fixture
            .output
            .parent()
            .expect("should have parent")
            .join("ordinary-diagnostics/ordinary-comparison-diagnostic.json"),
    )
    .expect("should retain source input diagnostic");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("should parse diagnostic");
    assert_eq!(value["outcome"], "failed", "should report failure");
    assert_eq!(value["stage"], "inputs", "should report input stage");
    assert!(
        value["facts"]["canonical_sha256"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64),
        "should retain observed digest after source-input failure"
    );
    assert_eq!(
        value["facts"]["first_difference_offset"], "unavailable",
        "should not invent difference after verified equality"
    );
    assert_eq!(
        value["code"], "source_inputs_rejected",
        "should use fixed source code"
    );
    assert!(!fixture.output.exists(), "should retain no companion");
}

#[test]
fn rejected_companion_writer_retains_observed_facts_without_fabricating_a_difference() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.output).expect("should occupy companion destination");
    let _ = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should refuse reused output");
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .output
                .parent()
                .expect("should have parent")
                .join("ordinary-diagnostics/ordinary-comparison-diagnostic.json"),
        )
        .expect("should retain writer failure facts"),
    )
    .expect("should parse writer diagnostic");
    assert_eq!(
        value["stage"], "output",
        "should identify failed companion sealing"
    );
    assert_eq!(
        value["code"], "companion_write_rejected",
        "should retain fixed writer code"
    );
    assert!(
        value["facts"]["canonical_size"]
            .as_u64()
            .is_some_and(|size| size > 0),
        "should retain actually observed binary size"
    );
    assert_eq!(
        value["facts"]["canonical_sha256"], value["facts"]["reproduction_sha256"],
        "should retain established equal digests"
    );
    assert_eq!(
        value["facts"]["first_difference_offset"], "unavailable",
        "should never invent a byte difference after equality"
    );
    assert!(
        !fixture
            .output
            .join("trusted-server-fastly.wasm.rebuild-check.json")
            .exists(),
        "should emit no companion on writer failure"
    );
}

#[test]
fn diagnostic_writer_failure_remains_a_failed_comparison() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.archives.join("canonical.zip")).expect("should remove transfer");
    fs::write(
        fixture
            .output
            .parent()
            .expect("should have parent")
            .join("ordinary-diagnostics"),
        b"occupied",
    )
    .expect("should prevent diagnostic directory creation");
    let error = compare_transfers(
        &fixture.input,
        &fixture.archives,
        &fixture.source.checkout,
        &fixture.output,
    )
    .expect_err("should remain failed when diagnostic persistence fails");
    assert!(
        format!("{error:?}").contains("outcome=failed: ordinary diagnostic writer failed"),
        "should state failed outcome in human text"
    );
    assert!(
        !fixture.output.exists(),
        "should never turn writer failure into a companion"
    );
}
