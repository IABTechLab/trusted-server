use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::symlink;

#[test]
fn one_byte_final_module_difference_rejects_the_complete_comparison() {
    let first = tempfile::tempdir().expect("should create A outputs");
    let second = tempfile::tempdir().expect("should create B outputs");
    fs::write(first.path().join("tsjs-core.js"), b"one").expect("should write A module");
    fs::write(second.path().join("tsjs-core.js"), b"one").expect("should write B module");
    let modules = BTreeMap::from([(
        "core".to_owned(),
        Module {
            filename: "tsjs-core.js".to_owned(),
            size: 3,
            sha256: hex::encode(Sha256::digest(b"one")),
        },
    )]);
    compare_modules(first.path(), second.path(), &modules, &|| Ok(()))
        .expect("should compare complete equal bytes");
    fs::write(second.path().join("tsjs-core.js"), b"onE").expect("should change one final byte");
    assert!(
        compare_modules(first.path(), second.path(), &modules, &|| Ok(())).is_err(),
        "should reject one differing final byte"
    );
}
#[test]
fn full_suite_report_rejects_zero_selection_or_missing_typecheck_file() {
    let expected = BTreeSet::from([
        "test/example.test.ts".to_owned(),
        "test/types.test.ts".to_owned(),
    ]);
    let mut report = json!({"success":true,"numTotalTests":2,"numPassedTests":2,"numFailedTests":0,"numPendingTests":0,"testResults":[{"name":"/example/lib/test/example.test.ts","status":"passed"},{"name":"/example/lib/test/types.test.ts","status":"passed"}]});
    let encode = |value: &Value| serde_json::to_vec(value).expect("should encode test report");
    assert_eq!(
        test_report(&encode(&report), &expected, Path::new("/example/lib"))
            .expect("should join all registered results"),
        2,
        "should retain nonzero passed count"
    );
    report["numTotalTests"] = json!(0);
    assert!(
        test_report(&encode(&report), &expected, Path::new("/example/lib")).is_err(),
        "should reject zero selected tests"
    );
    report["numTotalTests"] = json!(2);
    report["testResults"]
        .as_array_mut()
        .expect("should retain array")
        .pop();
    assert!(
        test_report(&encode(&report), &expected, Path::new("/example/lib")).is_err(),
        "should reject an omitted registered type-check suite"
    );
}
#[test]
fn parsed_inventory_cannot_choose_an_untracked_module_or_filename() {
    let mut inventory = json!({"modules":[{"id":"core","filename":"tsjs-core.js","size":1,"sha256":"a".repeat(64)}]});
    let expected = vec!["core".to_owned()];
    let encode =
        |value: &Value| serde_json::to_vec(value).expect("should encode inventory fixture");
    inventory_modules(&encode(&inventory), &expected).expect("should parse tracked module facts");
    inventory["modules"][0]["filename"] = json!("../outside");
    assert!(
        inventory_modules(&encode(&inventory), &expected).is_err(),
        "should reject path-selected bundle output"
    );
}

fn retained_fixture() -> JsBuildCompleted {
    let directory = tempfile::tempdir().expect("should create retained fixture");
    fs::create_dir(directory.path().join("bundles")).expect("should create bundle root");
    let inventory = b"fictional original inventory";
    let observation = b"fictional private observation";
    let module = b"fictional module";
    fs::write(directory.path().join("tsjs-inventory.json"), inventory)
        .expect("should write exact fixture inventory");
    fs::write(directory.path().join("npm-observation.json"), observation)
        .expect("should write exact fixture observation");
    fs::write(directory.path().join("bundles/tsjs-core.js"), module)
        .expect("should write fixture module");
    JsBuildCompleted {
        directory,
        source_commit: "a".repeat(40),
        tools_sha256: "b".repeat(64),
        settings_sha256: "c".repeat(64),
        inventory_sha256: hex::encode(Sha256::digest(inventory)),
        observation_sha256: hex::encode(Sha256::digest(observation)),
        modules: BTreeMap::from([(
            "core".to_owned(),
            Module {
                filename: "tsjs-core.js".to_owned(),
                size: module.len() as u64,
                sha256: hex::encode(Sha256::digest(module)),
            },
        )]),
        tests: [1, 1],
        attempt: JsAttempt::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            crate::process::Cancellation::default(),
        ),
    }
}

#[test]
fn retained_handoff_rejects_extra_root_files() {
    let completed = retained_fixture();
    completed
        .rehash_for_handoff()
        .expect("should rejoin exact fixture bytes");
    fs::write(completed.directory.path().join("unrecorded"), b"extra")
        .expect("should inject an extra retained file");
    assert!(
        completed.rehash_for_handoff().is_err(),
        "should reject unrecorded retained root entries"
    );
}

#[test]
fn retained_handoff_rejects_identical_directory_aliases() {
    for alias_bundles in [false, true] {
        let completed = retained_fixture();
        let external = tempfile::tempdir().expect("should create replacement directory");
        let original = if alias_bundles {
            completed.directory.path().join("bundles")
        } else {
            completed.directory.path().to_owned()
        };
        let destination = external.path().join("retained");
        fs::rename(&original, &destination).expect("should relocate identical retained bytes");
        symlink(&destination, &original).expect("should alias identical retained directory");
        assert!(
            completed.rehash_for_handoff().is_err(),
            "should reject directory aliases despite identical bytes"
        );
    }
}

#[test]
fn original_collector_bytes_cannot_be_replaced_by_later_child_output() {
    let completed = retained_fixture();
    let output = completed.directory.path();
    let raw = read_bounded_with_check(&output.join("tsjs-inventory.json"), Role::Metadata, &|| {
        Ok(())
    })
    .expect("should capture original collector bytes");
    let captured = CapturedInventory {
        raw,
        modules: completed.modules,
    };
    verify_captured_output(output, &captured, &|| Ok(()))
        .expect("should rejoin original collector outputs");
    fs::write(output.join("tsjs-inventory.json"), b"replacement inventory")
        .expect("should simulate later child inventory replacement");
    assert!(
        verify_captured_output(output, &captured, &|| Ok(())).is_err(),
        "should reject later inventory replacement before retaining it as original"
    );
}

#[test]
fn completion_settings_bind_both_actual_boundary_instances() {
    let first = settings_digest(&[[1_u8; 32], [2_u8; 32]]);
    assert_ne!(
        first,
        settings_digest(&[[3_u8; 32], [2_u8; 32]]),
        "should bind A controls"
    );
    assert_ne!(
        first,
        settings_digest(&[[1_u8; 32], [3_u8; 32]]),
        "should bind B controls"
    );
    assert_ne!(
        first,
        settings_digest(&[[2_u8; 32], [1_u8; 32]]),
        "should bind comparison-side ordering"
    );
}

#[test]
fn failed_cleanup_cannot_return_or_retain_a_completed_build() {
    let completed = retained_fixture();
    let retained = completed.directory.path().to_owned();
    assert!(
        finalize_execution(
            Ok(completed),
            Err(preparation_error("fictional cleanup failure"))
        )
        .is_err(),
        "should deny completion when private resource cleanup fails"
    );
    assert!(
        !retained.exists(),
        "should discard private retained output when cleanup fails"
    );
    let error = finalize_execution(
        Err(preparation_error("fictional execution failure")),
        Err(preparation_error("fictional cleanup failure")),
    )
    .err()
    .expect("should reject both failures");
    let message = format!("{error:?}");
    assert!(
        message.contains("fictional execution failure")
            && message.contains("fictional cleanup failure"),
        "should retain both failure contexts"
    );
}
