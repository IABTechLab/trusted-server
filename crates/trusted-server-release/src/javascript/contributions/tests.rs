use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::JsBuildCompleted;

pub(super) fn fixture() -> JsBuildCompleted {
    let mut completed = JsBuildCompleted::fixture_for_tests(&"a".repeat(40));
    let module = &completed.modules["core"];
    let inventory = json!({
        "schema_version":1,"source_commit":completed.source_commit,
        "modules":[{"id":"core","filename":module.filename,"size":module.size,"sha256":module.sha256,
        "contributions":[{"kind":"source","path":"src/example.ts","transform":"identity","sha256":"c".repeat(64),"stage":"rollup-rendered","rendered_length":12}]}],
        "tools":[],"materials":[]
    });
    replace(&mut completed, &inventory);
    completed
}

fn replace(completed: &mut JsBuildCompleted, inventory: &Value) {
    let bytes = serde_json::to_vec(inventory).expect("should encode inventory");
    std::fs::write(
        completed.directory.path().join("tsjs-inventory.json"),
        &bytes,
    )
    .expect("should write retained inventory");
    completed.inventory_sha256 = hex::encode(Sha256::digest(&bytes));
}

#[test]
fn checked_export_preserves_original_digest_and_conservative_source_origin() {
    let completed = fixture();
    let exported = completed
        .retained_contributions()
        .expect("should export checked origins");
    let value = serde_json::to_value(exported).expect("should serialize informational export");
    assert_eq!(
        value["inventory_sha256"], completed.inventory_sha256,
        "should bind raw original inventory"
    );
    assert_eq!(
        value["inventory"]["modules"][0]["contributions"][0]["path"], "src/example.ts",
        "should preserve rendered source origin"
    );
}

#[test]
fn checked_export_rejects_changed_bundle_before_reading_origins() {
    let completed = fixture();
    std::fs::write(
        completed.directory.path().join("bundles/tsjs-core.js"),
        b"tampered",
    )
    .expect("should mutate fixture bundle");
    assert!(
        completed.retained_contributions().is_err(),
        "should reject changed final module"
    );
}

#[test]
fn checked_export_rejects_unknown_origin_source_mismatch_and_path_aliases() {
    for (pointer, replacement) in [
        ("/source_commit", json!("b".repeat(40))),
        ("/modules/0/contributions/0/kind", json!("unknown")),
        ("/modules/0/contributions/0/path", json!("src//example.ts")),
        ("/modules/0/contributions/0/stage", json!("final-function")),
        ("/modules/0/contributions/0/rendered_length", json!(0)),
    ] {
        let mut completed = fixture();
        let bytes = std::fs::read(completed.directory.path().join("tsjs-inventory.json"))
            .expect("should read fixture");
        let mut inventory: Value = serde_json::from_slice(&bytes).expect("should parse fixture");
        *inventory
            .pointer_mut(pointer)
            .expect("should select fixture field") = replacement;
        replace(&mut completed, &inventory);
        assert!(
            completed.retained_contributions().is_err(),
            "should reject invalid origin at {pointer}"
        );
    }
}

pub(super) fn rich_inventory(completed: &JsBuildCompleted) -> Value {
    let bytes = std::fs::read(completed.directory.path().join("tsjs-inventory.json"))
        .expect("should read fixture");
    let mut inventory: Value = serde_json::from_slice(&bytes).expect("should parse fixture");
    let integrity = format!("sha512-{}==", "A".repeat(86));
    inventory["materials"] = json!([
        {"kind":"npm-package","id":"npm:node_modules/vite","lock_path":"node_modules/vite","name":"vite","version":"1.0.0","integrity":integrity,"installed_tree_sha256":"d".repeat(64)},
        {"kind":"tool-distribution","id":"tool:vite","name":"vite","version":"1.0.0","sha256":"e".repeat(64)},
        {"kind":"source","id":"source:scripts/example.mjs","path":"scripts/example.mjs","sha256":"f".repeat(64)},
        {"kind":"lifecycle-event","id":"lifecycle:node_modules/vite:postinstall","package_material_id":"npm:node_modules/vite","event":"postinstall","script_sha256":"1".repeat(64)},
        {"kind":"lifecycle-output","id":"output:node_modules/vite/generated.js","package_material_id":"npm:node_modules/vite","event":"postinstall","script_sha256":"1".repeat(64),"path":"node_modules/vite/generated.js","size":10,"sha256":"2".repeat(64)}
    ]);
    inventory["tools"] =
        json!([{"name":"vite","version":"1.0.0","role":"bundler","material_id":"tool:vite"}]);
    inventory["modules"][0]["contributions"] = json!([
        {"kind":"source","path":"src/example.ts","transform":"raw","sha256":"c".repeat(64),"stage":"rollup-rendered","rendered_length":12},
        {"kind":"npm","lock_path":"node_modules/vite","package_name":"vite","package_version":"1.0.0","integrity":integrity,"path":"dist/example.js","transform":"identity","sha256":"3".repeat(64),"stage":"rollup-rendered","rendered_length":15},
        {"kind":"generated","id":"\u{0}commonjsHelpers.js","plugin":"commonjs","owner_lock_path":"node_modules/vite","generated_sha256":"4".repeat(64),"stage":"rollup-rendered","rendered_length":20},
        {"kind":"generated","id":"\u{0}node_modules/vite/dist/example.js?commonjs-es-import","plugin":"commonjs","owner_lock_path":"node_modules/vite","generated_sha256":"5".repeat(64),"input":{"kind":"npm","lock_path":"node_modules/vite","package_name":"vite","package_version":"1.0.0","integrity":integrity,"path":"dist/example.js","transform":"identity","sha256":"3".repeat(64)},"stage":"rollup-rendered","rendered_length":20}
    ]);
    inventory
}

#[test]
fn original_collector_variants_preserve_generated_nul_ids_and_distinct_tree_evidence() {
    let mut completed = fixture();
    let inventory = rich_inventory(&completed);
    replace(&mut completed, &inventory);
    let exported = completed
        .retained_contributions()
        .expect("should accept actual collector variants");
    assert_eq!(
        exported.modules().len(),
        1,
        "should export typed bundle observations"
    );
    assert_eq!(
        exported.materials().len(),
        5,
        "should retain every distinct material role"
    );
    assert_eq!(
        exported.inventory_sha256(),
        completed.inventory_sha256,
        "should preserve raw digest"
    );
    let value = serde_json::to_value(exported).expect("should encode checked origins");
    assert_eq!(
        value["inventory"], inventory,
        "should preserve all original typed facts"
    );
}

#[test]
fn mismatched_npm_generated_tool_and_lifecycle_ownership_fail_closed() {
    for (pointer, replacement) in [
        ("/modules/0/contributions/1/package_version", json!("2.0.0")),
        (
            "/modules/0/contributions/1/path",
            json!("node_modules/other/example.js"),
        ),
        ("/modules/0/contributions/2/id", json!("\u{0}unknown.js")),
        ("/modules/0/contributions/2/plugin", json!("unreviewed")),
        (
            "/modules/0/contributions/3/input/sha256",
            json!("6".repeat(64)),
        ),
        ("/tools/0/material_id", json!("npm:node_modules/vite")),
        ("/materials/4/script_sha256", json!("6".repeat(64))),
        ("/materials/4/path", json!("node_modules/other/output.js")),
    ] {
        let mut completed = fixture();
        let mut inventory = rich_inventory(&completed);
        *inventory
            .pointer_mut(pointer)
            .expect("should select ownership field") = replacement;
        replace(&mut completed, &inventory);
        assert!(
            completed.retained_contributions().is_err(),
            "should reject inconsistent ownership at {pointer}"
        );
    }
}

#[test]
fn duplicate_contribution_and_material_ownership_fail_closed() {
    for duplicate_material in [false, true] {
        let mut completed = fixture();
        let mut inventory = rich_inventory(&completed);
        let entries = if duplicate_material {
            inventory["materials"].as_array_mut()
        } else {
            inventory["modules"][0]["contributions"].as_array_mut()
        }
        .expect("should select records");
        entries.push(entries[0].clone());
        replace(&mut completed, &inventory);
        assert!(
            completed.retained_contributions().is_err(),
            "should reject duplicate ownership"
        );
    }
}

#[test]
fn checked_export_retains_exact_original_raw_bytes() {
    let completed = fixture();
    let expected = std::fs::read(completed.directory.path().join("tsjs-inventory.json"))
        .expect("should read original bytes");
    let exported = completed
        .retained_contributions()
        .expect("should export originals");
    assert_eq!(
        exported.raw_inventory(),
        expected,
        "should preserve exact raw bytes"
    );
}
