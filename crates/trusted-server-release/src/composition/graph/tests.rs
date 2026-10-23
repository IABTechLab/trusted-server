use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::composition::{CargoLayout, UnitContext, observe_cargo};
use crate::policy::ReleaseTarget;

const METADATA: &[u8] =
    include_bytes!("../../../tests/fixtures/composition/cargo-1.95-cross-host-metadata.json");
const TREE: &[u8] = include_bytes!("../../../tests/fixtures/composition/cargo-1.95-cross-tree.txt");
const MESSAGES: &[u8] =
    include_bytes!("../../../tests/fixtures/composition/cargo-1.95-cross-messages.jsonl");
const ROOT: &str = "path+file:///build/source/app#example-app@0.1.0";

fn observed() -> CargoObservations {
    let layout = CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept fixture layout");
    observe_cargo(METADATA, MESSAGES, &layout).expect("should observe actual compiler units")
}

#[test]
fn compiled_build_script_requires_its_execution_observation() {
    let mut messages = std::str::from_utf8(MESSAGES)
        .expect("should read actual Cargo fixture")
        .lines()
        .filter(|line| {
            let message: Value = serde_json::from_str(line).expect("should parse Cargo message");
            message["reason"] != "build-script-executed"
        })
        .collect::<Vec<_>>()
        .join("\n");
    messages.push('\n');
    let layout = CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept fixture layout");
    let observed = observe_cargo(METADATA, messages.as_bytes(), &layout)
        .expect("should parse the remaining compiler observations");
    assert!(
        reconcile(METADATA, TREE, &observed, ROOT).is_err(),
        "should not seal a graph that loses build-script cfg, environment and link evidence"
    );
}

#[test]
fn build_script_execution_requires_a_matching_consumer_context() {
    let messages = std::str::from_utf8(MESSAGES)
        .expect("should read Cargo messages")
        .replace(
            "/build/target/wasm32-wasip1/release/build/example-app-f1024798a111146f/out",
            "/build/target/release/build/example-app-f1024798a111146f/out",
        );
    let layout = CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept fixture layout");
    let observed = observe_cargo(METADATA, messages.as_bytes(), &layout)
        .expect("should parse host-context script execution");
    assert!(
        reconcile(METADATA, TREE, &observed, ROOT).is_err(),
        "should reject a host execution when only a target consumer was observed"
    );
}

#[test]
fn dual_role_build_scripts_require_execution_evidence_for_each_consumer_context() {
    let helper = "path+file:///build/source/host-helper#example-host-helper@0.1.0";
    let mut metadata: Value =
        serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    metadata["resolve"]["nodes"][0]["deps"][0]["dep_kinds"]
        .as_array_mut()
        .expect("should hold dependency kinds")
        .push(json!({"kind":null,"target":null}));
    let metadata = serde_json::to_vec(&metadata).expect("should encode dual-role metadata");
    let mut observations = observed();
    let script_index = observations
        .units
        .iter()
        .position(|unit| unit.target_kind.iter().any(|kind| kind == "custom-build"))
        .expect("should contain script compilation");
    let mut script_unit = observed().units.remove(script_index);
    script_unit.package_id = helper.to_owned();
    observations.units.push(script_unit);
    let helper_execution = |context, out_dir| {
        let mut execution = observed().build_scripts.remove(0);
        execution.package_id = helper.to_owned();
        execution.context = context;
        execution.out_dir = PathBuf::from(out_dir);
        execution
    };
    let execution = helper_execution(
        UnitContext::Host,
        "/build/target/release/build/example-helper/out",
    );
    observations.build_scripts.push(execution);
    reconcile(&metadata, TREE, &observations, ROOT)
        .expect("should accept matching host-only script and consumer");

    let helper_index = observations
        .units
        .iter()
        .position(|unit| unit.package_id == helper && unit.target_kind == ["lib"])
        .expect("should contain host helper consumer");
    let mut target_unit = observed().units.remove(helper_index);
    target_unit.context = UnitContext::Target;
    target_unit.features = vec!["target-feature".to_owned()];
    observations.units.push(target_unit);
    assert!(
        reconcile(&metadata, TREE, &observations, ROOT).is_err(),
        "should reject missing execution evidence for the second consuming context"
    );

    observations.build_scripts.push(helper_execution(
        UnitContext::Target,
        "/build/target/wasm32-wasip1/release/build/example-helper/out",
    ));
    reconcile(&metadata, TREE, &observations, ROOT)
        .expect("should accept separate host and target executions without merging features");
    observations.build_scripts.push(helper_execution(
        UnitContext::Target,
        "/build/target/wasm32-wasip1/release/build/example-helper-other/out",
    ));
    reconcile(&metadata, TREE, &observations, ROOT)
        .expect("should allow distinct executions within the same consuming context");
}

#[test]
fn actual_cross_build_preserves_host_only_dependencies_and_observed_units() {
    let observed = observed();
    let graph = reconcile(METADATA, TREE, &observed, ROOT)
        .expect("should reconcile actual cross-build graph");
    assert_eq!(
        graph.observed_package_ids().len(),
        3,
        "should retain observed membership"
    );
    assert_eq!(
        graph.potential_edges().len(),
        2,
        "should retain potential package edges"
    );
    assert_eq!(
        graph.potential_edges()[0].kinds(),
        ["build"],
        "should record build dependency transition"
    );
    assert!(
        observed
            .units
            .iter()
            .any(|unit| unit.context == UnitContext::Host
                && unit.package_id.contains("example-host-unix")),
        "should preserve observed host-only unit"
    );
}

#[test]
fn invalid_tree_structure_and_missing_observed_membership_are_rejected() {
    for tree in [b"".as_slice(), b"0example-app v0.1.0 (/build/source/app)", b"1example-app v0.1.0 (/build/source/app)\n", b"0example-app v0.1.0 (/build/source/app)\n2example-host-helper v0.1.0 (/build/source/host-helper)\n", b"0example-app v0.1.0 (/build/source/app)\n", b"0example-app v0.1.0 (/wrong)\n", b"0example-app v0.1.0 (/build/source/app)\n0example-app v0.1.0 (/build/source/app)\n"] {
        assert!(reconcile(METADATA, tree, &observed(), ROOT).is_err(), "should reject invalid tree or missing compiled nodes");
    }
}

#[test]
fn unknown_duplicate_and_dev_only_metadata_edges_are_rejected() {
    for case in [
        "unknown-node",
        "duplicate-node",
        "unknown-endpoint",
        "missing-edge",
        "dev-only",
        "ambiguous-source",
        "dependencies-disagree",
    ] {
        let mut meta: Value =
            serde_json::from_slice(METADATA).expect("should parse fixture metadata");
        match case {
            "unknown-node" => meta["resolve"]["nodes"][0]["id"] = json!("unknown"),
            "duplicate-node" => {
                let duplicate = meta["resolve"]["nodes"][0].clone();
                meta["resolve"]["nodes"]
                    .as_array_mut()
                    .expect("should hold nodes")
                    .push(duplicate);
            }
            "unknown-endpoint" => meta["resolve"]["nodes"][0]["deps"][0]["pkg"] = json!("unknown"),
            "missing-edge" => {
                meta["resolve"]["nodes"][0]["deps"] = json!([]);
                meta["resolve"]["nodes"][0]["dependencies"] = json!([]);
            }
            "dev-only" => {
                meta["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]["kind"] = json!("dev")
            }
            "ambiguous-source" => {
                let mut duplicate = meta["packages"][0].clone();
                duplicate["id"] = json!("opaque-other-source");
                duplicate["source"] = json!("registry+https://example.com/index");
                meta["packages"]
                    .as_array_mut()
                    .expect("should hold packages")
                    .push(duplicate);
            }
            "dependencies-disagree" => meta["resolve"]["nodes"][0]["dependencies"] = json!([]),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&meta).expect("should encode changed metadata");
        assert!(
            reconcile(&bytes, TREE, &observed(), ROOT).is_err(),
            "should reject {case}"
        );
    }
}

#[test]
fn ordinary_compiler_units_need_reachable_host_or_target_context() {
    let mut observed = observed();
    let host = observed
        .units
        .iter_mut()
        .find(|unit| unit.package_id.contains("example-host-unix"))
        .expect("should contain host dependency");
    host.context = UnitContext::Target;
    assert!(
        reconcile(METADATA, TREE, &observed, ROOT).is_err(),
        "should reject target unit reachable only through build dependency"
    );
}

#[test]
fn tree_only_packages_never_create_compiler_membership() {
    let mut observed = observed();
    observed.packages.retain(|package| package.id == ROOT);
    observed.units.retain(|unit| unit.package_id == ROOT);
    let graph = reconcile(METADATA, TREE, &observed, ROOT)
        .expect("should retain potential graph without inventing units");
    assert_eq!(
        graph.tree_package_ids.len(),
        3,
        "should retain potential graph"
    );
    assert_eq!(
        graph.observed_package_ids,
        [ROOT],
        "should retain only actual compiler membership"
    );
}

#[test]
fn catalog_dev_only_entries_never_create_contributors() {
    let mut meta: Value = serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    let mut dev = meta["packages"][1].clone();
    dev["id"] = json!("opaque-dev-only");
    dev["name"] = json!("example-dev-only");
    meta["packages"]
        .as_array_mut()
        .expect("should hold packages")
        .push(dev);
    meta["resolve"]["nodes"]
        .as_array_mut()
        .expect("should hold nodes")
        .push(json!({"id":"opaque-dev-only", "dependencies":[], "deps":[], "features":["unused"]}));
    meta["resolve"]["nodes"][0]["dependencies"]
        .as_array_mut()
        .expect("should hold dependencies")
        .push(json!("opaque-dev-only"));
    meta["resolve"]["nodes"][0]["deps"].as_array_mut().expect("should hold edges").push(json!({"name":"example_dev_only", "pkg":"opaque-dev-only", "dep_kinds":[{"kind":"dev", "target":null}]}));
    let graph = reconcile(
        &serde_json::to_vec(&meta).expect("should encode metadata"),
        TREE,
        &observed(),
        ROOT,
    )
    .expect("should exclude uncompiled dev dependency");
    assert!(
        !graph
            .tree_package_ids
            .iter()
            .any(|id| id == "opaque-dev-only"),
        "should not treat catalog as observed graph"
    );
}

#[test]
fn disconnected_compiler_membership_is_rejected() {
    let mut observed = observed();
    observed.packages[0].id = "opaque-extra-compiler-package".to_owned();
    assert!(
        reconcile(METADATA, TREE, &observed, ROOT).is_err(),
        "should reject unjoined compiled package identity"
    );
}

#[test]
fn unused_ambiguous_catalog_names_do_not_reject_selected_tree() {
    let mut metadata: Value =
        serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    for id in ["opaque-unused-source-a", "opaque-unused-source-b"] {
        let mut package = metadata["packages"][0].clone();
        package["name"] = json!("example-unused");
        package["id"] = json!(id);
        metadata["packages"]
            .as_array_mut()
            .expect("should hold packages")
            .push(package);
    }
    reconcile(
        &serde_json::to_vec(&metadata).expect("should encode catalog"),
        TREE,
        &observed(),
        ROOT,
    )
    .expect("should reject ambiguous identities only when tree membership uses them");
}

#[test]
fn dual_role_packages_keep_compiler_features_and_both_contexts() {
    let mut meta: Value = serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    meta["resolve"]["nodes"][0]["deps"][0]["dep_kinds"]
        .as_array_mut()
        .expect("should hold dependency kinds")
        .push(json!({"kind":null,"target":null}));
    meta["resolve"]["nodes"][1]["features"] = json!(["unused-package-union"]);
    let mut messages: Vec<Value> = std::str::from_utf8(MESSAGES)
        .expect("should hold JSONL")
        .lines()
        .map(|line| serde_json::from_str(line).expect("should parse compiler message"))
        .collect();
    let mut target_unit = messages
        .iter()
        .find(|message| {
            message["reason"] == "compiler-artifact"
                && message["package_id"]
                    .as_str()
                    .is_some_and(|id| id.contains("example-host-helper"))
        })
        .expect("should contain helper compilation")
        .clone();
    target_unit["filenames"] =
        json!(["/build/target/wasm32-wasip1/release/deps/libexample_host_helper.rlib"]);
    target_unit["features"] = json!(["target-feature"]);
    messages.insert(0, target_unit);
    let stdout: Vec<u8> = messages
        .iter()
        .flat_map(|message| {
            let mut bytes = serde_json::to_vec(message).expect("should encode message");
            bytes.push(b'\n');
            bytes
        })
        .collect();
    let layout = CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept fixture layout");
    let metadata = serde_json::to_vec(&meta).expect("should encode metadata");
    let observed =
        observe_cargo(&metadata, &stdout, &layout).expect("should observe dual-role compilation");
    reconcile(&metadata, TREE, &observed, ROOT)
        .expect("should reconcile both contexts without fabricating a unit DAG");
    assert!(
        observed
            .units
            .iter()
            .any(|unit| unit.features == ["target-feature"] && unit.context == UnitContext::Target),
        "should preserve effective target features"
    );
    assert!(
        !observed.units.iter().any(|unit| unit
            .features
            .iter()
            .any(|feature| feature == "unused-package-union")),
        "should never substitute package union features"
    );
}

#[test]
fn proc_macro_normal_dependency_switches_its_dependencies_to_host() {
    let mut meta: Value = serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    meta["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]["kind"] = Value::Null;
    meta["packages"][1]["targets"][0]["kind"] = json!(["proc-macro"]);
    let mut observed = observed();
    let helper = observed
        .units
        .iter_mut()
        .find(|unit| unit.package_id.contains("example-host-helper"))
        .expect("should contain helper compilation");
    helper.target_kind = vec!["proc-macro".to_owned()];
    reconcile(
        &serde_json::to_vec(&meta).expect("should encode metadata"),
        TREE,
        &observed,
        ROOT,
    )
    .expect("should retain host context below proc macro");
}

#[test]
fn registry_deduplication_marker_is_not_a_source_presentation() {
    let mut meta: Value = serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    meta["packages"][1]["source"] = json!("registry+https://example.com/index");
    let tree = String::from_utf8(TREE.to_vec())
        .expect("should hold tree text")
        .replace("(/build/source/host-helper)", "(*)");
    assert!(
        reconcile(
            &serde_json::to_vec(&meta).expect("should encode metadata"),
            tree.as_bytes(),
            &observed(),
            ROOT
        )
        .is_err(),
        "should require complete non-deduplicated tree"
    );
}

#[test]
fn selected_tree_root_requires_a_resolve_node() {
    let mut meta: Value = serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    meta["resolve"]["nodes"] = json!([]);
    let mut observed = observed();
    observed.packages.retain(|package| package.id == ROOT);
    observed.units.retain(|unit| unit.package_id == ROOT);
    assert!(
        reconcile(
            &serde_json::to_vec(&meta).expect("should encode metadata"),
            b"0example-app v0.1.0 (/build/source/app)\n",
            &observed,
            ROOT
        )
        .is_err(),
        "should reject tree root without resolve node"
    );
}

#[test]
fn repeated_tree_occurrences_have_a_separate_bound_from_compiler_messages() {
    let mut tree = TREE.to_vec();
    for _ in 0..65_536 {
        tree.extend_from_slice(b"1example-host-helper v0.1.0 (/build/source/host-helper)\n");
    }
    let graph = reconcile(METADATA, &tree, &observed(), ROOT)
        .expect("should accept large non-deduplicated dependency trees");
    assert_eq!(
        graph.edges.len(),
        2,
        "should store unique package edges instead of repeated occurrences"
    );
}

#[test]
fn tree_occurrence_bound_is_enforced_independently_of_byte_bound() {
    let mut metadata: Value =
        serde_json::from_slice(METADATA).expect("should parse fixture metadata");
    for (package, name) in metadata["packages"]
        .as_array_mut()
        .expect("should hold packages")
        .iter_mut()
        .zip(["a", "b", "c"])
    {
        package["name"] = json!(name);
        package["version"] = json!("1");
        package["source"] = json!("registry+https://example.com/index");
    }
    let mut tree = b"0a v1\n1b v1\n2c v1\n".to_vec();
    for _ in 0..1_048_576 {
        tree.extend_from_slice(b"1b v1\n");
    }
    assert!(
        tree.len() < 16 * 1024 * 1024,
        "should exercise occurrence bound before byte bound"
    );
    assert!(
        reconcile(
            &serde_json::to_vec(&metadata).expect("should encode compact metadata"),
            &tree,
            &observed(),
            ROOT
        )
        .is_err(),
        "should reject excessive tree occurrences"
    );
}
