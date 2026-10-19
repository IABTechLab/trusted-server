use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::policy::ReleaseTarget;

const PACKAGE: &str = "registry+https://example.com/index#example@1.0.0";
const OTHER: &str = "git+https://example.com/source#example@1.0.0";

fn layout() -> CargoLayout {
    CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept the explicit build layout")
}

fn target(kind: &str) -> Value {
    json!({
        "name": "example", "kind": [kind], "crate_types": [kind],
        "src_path": "/build/vendor/example/src/lib.rs"
    })
}

fn metadata() -> Value {
    let package = |id: &str| {
        json!({
            "id": id, "name": "example", "version": "1.0.0",
        "source": if id == OTHER { "git+https://example.com/source" } else { "registry+https://example.com/index" },
            "manifest_path": "/build/vendor/example/Cargo.toml",
            "license": "MIT", "targets": [target("lib"), target("custom-build"), target("proc-macro")]
        })
    };
    json!({
        "version": 1, "workspace_root": "/build/source", "target_directory": "/build/target",
        "packages": [package(PACKAGE), package(OTHER)],
        "resolve": {"nodes": [
            {"id": PACKAGE, "features": ["host", "target", "unused-union-feature"], "dependencies": []},
            {"id": OTHER, "features": [], "dependencies": []}
        ]}
    })
}

fn unit(id: &str, kind: &str, host: bool, features: &[&str]) -> Value {
    let prefix = if host {
        "/build/target/release"
    } else {
        "/build/target/wasm32-wasip1/release"
    };
    json!({
        "reason": "compiler-artifact", "package_id": id,
        "manifest_path": "/build/vendor/example/Cargo.toml", "target": target(kind),
        "profile": {"test": false, "opt_level": "3", "debuginfo": 1},
        "features": features, "filenames": [format!("{prefix}/deps/example.rlib")],
        "executable": null, "fresh": false
    })
}

fn stream(messages: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for message in messages
        .iter()
        .chain([&json!({"reason": "build-finished", "success": true})])
    {
        bytes.extend(serde_json::to_vec(message).expect("should encode a message"));
        bytes.push(b'\n');
    }
    bytes
}

fn observe(meta: &Value, messages: &[Value]) -> CargoObservations {
    observe_cargo(
        &serde_json::to_vec(meta).expect("should encode metadata"),
        &stream(messages),
        &layout(),
    )
    .expect("should join the observed compilation units")
}

#[test]
fn host_and_target_units_keep_observed_features_instead_of_metadata_union() {
    let observed = observe(
        &metadata(),
        &[
            unit(PACKAGE, "lib", true, &["host"]),
            unit(PACKAGE, "lib", false, &["target"]),
        ],
    );
    assert_eq!(
        observed.units().len(),
        2,
        "should retain both compilation contexts"
    );
    let features: Vec<_> = observed
        .units()
        .iter()
        .map(|unit| unit.features().to_vec())
        .collect();
    assert!(
        features.contains(&vec!["host".to_owned()]),
        "should retain host features"
    );
    assert!(
        features.contains(&vec!["target".to_owned()]),
        "should retain target features"
    );
    assert!(
        observed
            .units()
            .iter()
            .all(|unit| !unit.features().contains(&"unused-union-feature".to_owned())),
        "should not substitute package-union features"
    );
    assert_eq!(
        observed.packages().len(),
        1,
        "should exclude uncompiled metadata packages"
    );
}

#[test]
fn identical_names_and_versions_from_distinct_sources_remain_separate() {
    let observed = observe(
        &metadata(),
        &[
            unit(PACKAGE, "lib", false, &[]),
            unit(OTHER, "lib", false, &[]),
        ],
    );
    assert_eq!(
        observed.packages.len(),
        2,
        "should join by exact opaque package ID"
    );
}

#[test]
fn ambiguous_or_missing_package_and_target_joins_are_rejected() {
    let cases = [
        "missing-package",
        "duplicate-package",
        "manifest",
        "target",
        "workspace",
        "target-directory",
    ];
    for case in cases {
        let mut meta = metadata();
        let mut message = unit(PACKAGE, "lib", false, &[]);
        match case {
            "missing-package" => message["package_id"] = json!("missing"),
            "duplicate-package" => {
                let duplicate = meta["packages"][0].clone();
                meta["packages"]
                    .as_array_mut()
                    .expect("should hold packages")
                    .push(duplicate);
            }
            "manifest" => message["manifest_path"] = json!("/build/other/Cargo.toml"),
            "target" => message["target"]["src_path"] = json!("/build/other/lib.rs"),
            "workspace" => meta["workspace_root"] = json!("/another/source"),
            "target-directory" => meta["target_directory"] = json!("/another/target"),
            _ => unreachable!("should enumerate each case"),
        }
        assert!(
            observe_cargo(
                &serde_json::to_vec(&meta).expect("should encode metadata"),
                &stream(&[message]),
                &layout()
            )
            .is_err(),
            "should reject {case}"
        );
    }
}

#[test]
fn invalid_unit_context_freshness_and_features_are_rejected() {
    for case in [
        "outside",
        "mixed",
        "proc-macro-target",
        "cached",
        "test",
        "duplicate-feature",
        "duplicate-unit",
    ] {
        let mut message = unit(PACKAGE, "lib", false, &[]);
        match case {
            "outside" => message["filenames"] = json!(["/elsewhere/example.rlib"]),
            "mixed" => {
                message["filenames"] = json!([
                    "/build/target/release/deps/a.rlib",
                    "/build/target/wasm32-wasip1/release/deps/a.rlib"
                ])
            }
            "proc-macro-target" => message["target"] = target("proc-macro"),
            "cached" => message["fresh"] = json!(true),
            "test" => message["profile"]["test"] = json!(true),
            "duplicate-feature" => message["features"] = json!(["same", "same"]),
            "duplicate-unit" => {}
            _ => unreachable!("should enumerate each case"),
        }
        let messages = if case == "duplicate-unit" {
            vec![message.clone(), message]
        } else {
            vec![message]
        };
        assert!(
            observe_cargo(
                &serde_json::to_vec(&metadata()).expect("should encode metadata"),
                &stream(&messages),
                &layout()
            )
            .is_err(),
            "should reject {case}"
        );
    }
}

#[test]
fn build_script_execution_keeps_link_and_context_observations_separate() {
    let script = json!({
        "reason": "build-script-executed", "package_id": PACKAGE,
        "out_dir": "/build/target/wasm32-wasip1/release/build/example-123/out",
        "linked_libs": ["static=example"], "linked_paths": ["native=/build/target/wasm32-wasip1/release/build/example-123/out"],
        "cfgs": ["example"], "env": [["EXAMPLE", "observed"]]
    });
    let observed = observe(
        &metadata(),
        &[unit(PACKAGE, "custom-build", true, &["host"]), script],
    );
    assert_eq!(
        observed.build_scripts.len(),
        1,
        "should retain the execution record"
    );
    assert_eq!(observed.build_scripts[0].context, UnitContext::Target);
    assert_eq!(observed.build_scripts[0].linked_libs, ["static=example"]);
    assert_eq!(observed.units[0].context, UnitContext::Host);
}

#[test]
fn duplicate_json_and_failed_or_unterminated_streams_are_rejected() {
    let meta = serde_json::to_vec(&metadata()).expect("should encode metadata");
    for bytes in [
        b"{\"reason\":\"build-finished\",\"success\":false}\n".as_slice(),
        b"{\"reason\":\"build-finished\",\"reason\":\"build-finished\",\"success\":true}\n",
        b"{\"reason\":\"build-finished\",\"success\":true}",
    ] {
        assert!(
            observe_cargo(&meta, bytes, &layout()).is_err(),
            "should reject invalid stream"
        );
    }
    assert!(
        observe_cargo(b"{\"version\":1,\"version\":1}", &stream(&[]), &layout()).is_err(),
        "should reject duplicate metadata keys before insertion"
    );
}

#[test]
fn retained_raw_and_normalized_observations_are_rehashed_before_handoff() {
    let meta = serde_json::to_vec(&metadata()).expect("should encode metadata");
    let capture = CargoCapture::new(
        meta,
        b"0example v1.0.0\n".to_vec(),
        &stream(&[unit(PACKAGE, "lib", false, &[])]),
        &layout(),
        ["a".repeat(64), "b".repeat(64)],
    )
    .expect("should capture checked observations");
    let retained = tempfile::tempdir().expect("should create export storage");
    capture
        .retain(retained.path())
        .expect("should retain the observations");
    assert!(
        capture.export(retained.path()).is_err(),
        "should not export summary-only observations as actual composition receipts"
    );
    assert!(
        capture.view(retained.path()).is_err(),
        "should not expose a typed owner view without sealed receipts"
    );
    for name in [
        "cargo-metadata.json",
        "cargo-tree.txt",
        "cargo-observations.json",
    ] {
        let directory = tempfile::tempdir().expect("should create retained storage");
        capture
            .retain(directory.path())
            .expect("should retain all observations");
        capture
            .rehash(directory.path())
            .expect("should rehash the retained observations");
        std::fs::write(directory.path().join(name), b"changed")
            .expect("should alter one observation");
        assert!(
            capture.rehash(directory.path()).is_err(),
            "should reject changed {name}"
        );
        let error = match capture.view(directory.path()) {
            Ok(_) => panic!("should refuse a typed owner view after changing {name}"),
            Err(error) => error,
        };
        assert!(
            format!("{error:?}").contains(&format!("retained {name} changed")),
            "should rehash before checking receipt availability: {error:?}"
        );
    }
    assert_eq!(
        capture.summary()["material_native_coverage_complete"],
        false
    );
    assert!(
        capture
            .require_binary(
                std::path::Path::new("/build/vendor/example/Cargo.toml"),
                "example",
                std::path::Path::new("/build/target/wasm32-wasip1/release/example")
            )
            .is_err(),
        "should not substitute a library for the accepted executable"
    );
}

#[test]
fn actual_cargo_195_observations_join_after_only_root_path_relocation() {
    let metadata = include_bytes!("../../tests/fixtures/composition/cargo-1.95-metadata.json");
    let messages = include_bytes!("../../tests/fixtures/composition/cargo-1.95-messages.jsonl");
    let observed = observe_cargo(metadata, messages, &layout())
        .expect("should parse actual Cargo 1.95 compilation and metadata shapes");
    assert_eq!(
        observed.units.len(),
        2,
        "should retain the build script and selected binary"
    );
    assert_eq!(
        observed.build_scripts.len(),
        1,
        "should retain actual build-script execution"
    );
    assert_eq!(
        observed.packages.len(),
        1,
        "should exclude uncompiled workspace members"
    );
    let captured = CargoCapture::new(
        metadata.to_vec(),
        b"0control v0.1.0\n".to_vec(),
        messages,
        &layout(),
        ["a".repeat(64), "b".repeat(64)],
    )
    .expect("should bind actual observed membership");
    let selected = captured
        .require_binary(
            std::path::Path::new("/build/source/crates/control/Cargo.toml"),
            "control",
            std::path::Path::new("/build/target/wasm32-wasip1/release/control.wasm"),
        )
        .expect("should name the exact selected target binary");
    assert_eq!(
        selected, observed.units[0].package_id,
        "should return the exact observed package identity for graph selection"
    );
}

#[test]
fn duplicate_metadata_targets_and_uncompiled_or_duplicate_scripts_are_rejected() {
    let mut meta = metadata();
    let duplicate = meta["packages"][0]["targets"][0].clone();
    meta["packages"][0]["targets"]
        .as_array_mut()
        .expect("should hold targets")
        .push(duplicate);
    assert!(
        observe_cargo(
            &serde_json::to_vec(&meta).expect("should encode metadata"),
            &stream(&[unit(PACKAGE, "lib", false, &[])]),
            &layout()
        )
        .is_err(),
        "should reject ambiguous metadata targets"
    );
    let script = json!({"reason": "build-script-executed", "package_id": PACKAGE,
        "out_dir": "/build/target/wasm32-wasip1/release/build/example-123/out",
        "linked_libs": [], "linked_paths": [], "cfgs": [], "env": []});
    let meta = serde_json::to_vec(&metadata()).expect("should encode metadata");
    assert!(
        observe_cargo(&meta, &stream(std::slice::from_ref(&script)), &layout()).is_err(),
        "should reject a script with no compiled host build-script unit"
    );
    assert!(
        observe_cargo(
            &meta,
            &stream(&[
                unit(PACKAGE, "custom-build", true, &[]),
                script.clone(),
                script
            ]),
            &layout()
        )
        .is_err(),
        "should reject duplicate execution observations"
    );
    assert!(
        observe_cargo(
            &meta,
            &stream(&[unit(PACKAGE, "custom-build", false, &[])]),
            &layout()
        )
        .is_err(),
        "should reject a build-script compiler output in the target context"
    );
}

#[test]
fn actual_cross_build_needs_unfiltered_identities_for_host_only_dependencies() {
    let metadata =
        include_bytes!("../../tests/fixtures/composition/cargo-1.95-cross-host-metadata.json");
    let filtered = include_bytes!(
        "../../tests/fixtures/composition/cargo-1.95-cross-target-filtered-metadata.json"
    );
    let messages =
        include_bytes!("../../tests/fixtures/composition/cargo-1.95-cross-messages.jsonl");
    let rejected = observe_cargo(filtered, messages, &layout()).expect_err(
        "should reproduce target-filtered metadata omitting a compiled host dependency",
    );
    assert!(
        format!("{rejected:?}").contains("example-host-unix"),
        "should identify the missing compiled dependency"
    );
    let observed = observe_cargo(metadata, messages, &layout())
        .expect("should join actual cross-build units to the complete identity catalog");
    let host = observed
        .packages
        .iter()
        .find(|package| package.name == "example-host-unix")
        .expect("should retain observed host-only dependency");
    assert!(
        observed
            .units
            .iter()
            .any(|unit| unit.package_id == host.id && unit.context == UnitContext::Host),
        "should classify host-only dependency from actual compiler output"
    );
    assert_eq!(
        observed.packages.len(),
        3,
        "should record the three actually compiled packages"
    );
    assert_eq!(
        observed.units.len(),
        4,
        "should retain distinct app host build-script and target binary units"
    );
    let tree = include_bytes!("../../tests/fixtures/composition/cargo-1.95-cross-tree.txt");
    let captured = CargoCapture::new(
        metadata.to_vec(),
        tree.to_vec(),
        messages,
        &layout(),
        ["a".repeat(64), "b".repeat(64)],
    )
    .expect("should retain the catalog with actual explicit-target tree and build observations");
    captured
        .require_binary(
            Path::new("/build/source/app/Cargo.toml"),
            "example-app",
            Path::new("/build/target/wasm32-wasip1/release/example-app.wasm"),
        )
        .expect("should bind the exact selected cross-build executable");
}

#[test]
fn internal_metadata_and_stream_observation_bounds_are_enforced() {
    let oversized = vec![b' '; 16 * 1024 * 1024 + 1];
    let meta = serde_json::to_vec(&metadata()).expect("should encode metadata");
    assert!(
        observe_cargo(&oversized, &stream(&[]), &layout()).is_err(),
        "should bound raw metadata"
    );
    assert!(
        observe_cargo(&meta, &oversized, &layout()).is_err(),
        "should bound total JSONL before splitting"
    );
}
