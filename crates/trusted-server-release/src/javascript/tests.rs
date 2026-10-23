use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::symlink;

mod adopt;

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
fn full_suite_report_rejects_failed_pending_or_unregistered_results() {
    let expected = BTreeSet::from(["test/example.test.ts".to_owned()]);
    let passing = json!({"success":true,"numTotalTests":1,"numPassedTests":1,"numFailedTests":0,"numPendingTests":0,"testResults":[{"name":"/example/lib/test/example.test.ts","status":"passed"}]});
    let encode = |value: &Value| serde_json::to_vec(value).expect("should encode test report");
    test_report(&encode(&passing), &expected, Path::new("/example/lib"))
        .expect("should accept the passing control report");
    let changes: [(&str, Value); 6] = [
        ("/success", json!(false)),
        ("/numFailedTests", json!(1)),
        ("/numPassedTests", json!(0)),
        ("/numPendingTests", json!(1)),
        ("/testResults/0/status", json!("failed")),
        (
            "/testResults/0/name",
            json!("/example/lib/test/other.test.ts"),
        ),
    ];
    for (pointer, value) in changes {
        let mut report = passing.clone();
        *report
            .pointer_mut(pointer)
            .expect("should select an existing report field") = value;
        assert!(
            test_report(&encode(&report), &expected, Path::new("/example/lib")).is_err(),
            "should reject a report with changed {pointer}"
        );
    }
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
    JsBuildCompleted::fixture_for_tests(&"a".repeat(40))
}

#[test]
fn retained_module_facts_are_derived_from_the_rehashed_owned_handoff() {
    let completed = retained_fixture();
    let modules = completed
        .retained_modules()
        .expect("should derive checked module facts");
    assert_eq!(modules.len(), 1, "should retain the exact module set");
    assert_eq!(
        modules[0].id(),
        "core",
        "should preserve checked module identity"
    );
    assert_eq!(
        modules[0].filename(),
        "tsjs-core.js",
        "should preserve checked filename"
    );
    assert_eq!(
        modules[0].size(),
        b"fictional module".len() as u64,
        "should preserve complete module length"
    );
    assert_eq!(
        modules[0].sha256(),
        hex::encode(Sha256::digest(b"fictional module")),
        "should preserve complete module digest"
    );
    fs::write(
        completed.directory.path().join("bundles/tsjs-core.js"),
        b"changed",
    )
    .expect("should change retained module");
    assert!(
        completed.retained_modules().is_err(),
        "should reject changed bytes before exporting facts"
    );
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

#[test]
fn vitest_failure_diagnostics_include_only_bounded_registered_names() {
    let report = json!({"testResults":[
        {"name":"/work/lib/test/fictional.test.ts","status":"failed","message":"PRIVATE ASSERTION","assertionResults":[
            {"fullName":"fictional suite should reject invalid input","status":"failed","failureMessages":["PRIVATE STACK"]},
            {"fullName":"successful case","status":"passed"}
        ]},
        {"name":"/outside/private-file","status":"failed","assertionResults":[]}
    ]});
    let expected = BTreeSet::from(["test/fictional.test.ts".to_owned()]);
    let bytes = serde_json::to_vec(&report).expect("should encode fictional report");
    let diagnostic = vitest_failure_diagnostic(&bytes, &expected, Path::new("/work/lib"))
        .expect("should parse report");
    assert!(
        diagnostic.contains("test/fictional.test.ts")
            && diagnostic.contains("fictional suite should reject invalid input"),
        "should preserve failed suite and test identities"
    );
    for omitted in [
        "PRIVATE ASSERTION",
        "PRIVATE STACK",
        "successful case",
        "private-file",
    ] {
        assert!(
            !diagnostic.contains(omitted),
            "should omit arbitrary failure context and unregistered paths"
        );
    }
    assert!(
        vitest_failure_diagnostic(b"{", &expected, Path::new("/work/lib")).is_err(),
        "should reject malformed reports"
    );
    let tests: Vec<_> = (0..100)
        .map(|_| json!({"fullName":format!("{}\n\u{1b}","x".repeat(1000)),"status":"failed"}))
        .collect();
    let large = json!({"testResults":[{"name":"/work/lib/test/fictional.test.ts","status":"failed","assertionResults":tests}]});
    let diagnostic = vitest_failure_diagnostic(
        &serde_json::to_vec(&large).expect("should encode bounded fixture"),
        &expected,
        Path::new("/work/lib"),
    )
    .expect("should summarize names");
    assert!(
        diagnostic.len() < 5000 && !diagnostic.contains(['\n', '\u{1b}']),
        "should bound count and length and remove controls"
    );
}
