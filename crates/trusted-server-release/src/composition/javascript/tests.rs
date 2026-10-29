use std::fs;
use std::os::unix::fs::symlink;

use sha2::Sha256;

use super::*;
use crate::javascript::JsBuildCompleted;

fn collect(bytes: &[u8], policy: JsRetentionPolicy) -> Result<JsMembership, Report<ReleaseError>> {
    let completed = JsBuildCompleted::fixture_for_tests(&"a".repeat(40));
    let modules = completed
        .retained_modules()
        .expect("should export rehashed module facts");
    let root = fs::canonicalize(
        completed
            .rehash_for_handoff()
            .expect("should retain checked handoff"),
    )
    .expect("should use recipe's canonical handoff path");
    collect_js(
        bytes,
        &hex::encode(Sha256::digest(bytes)),
        &JsInputs {
            retained_root: &root,
            modules: &modules,
            inventory_sha256: completed.inventory_sha256(),
            policy,
        },
    )
}

#[test]
fn complete_module_bytes_are_conservative_embedded_contributors() {
    let found = collect(
        b"binary-prefix\0fictional module\0binary-suffix",
        JsRetentionPolicy::FastlyAll,
    )
    .expect("should find the complete final module");
    assert_eq!(
        found.modules[0].classification, "conservative_embedded_js",
        "should label retained bytes conservatively"
    );
    assert_eq!(
        found.artifact_sha256,
        hex::encode(Sha256::digest(
            b"binary-prefix\0fictional module\0binary-suffix"
        )),
        "should bind exact final executable bytes"
    );
}

#[test]
fn absent_or_partial_cli_modules_remain_build_material() {
    for bytes in [
        b"operator binary".as_slice(),
        b"fictional modul",
        b"ictional module",
        b"fictional modUle",
    ] {
        let absent = collect(bytes, JsRetentionPolicy::ObserveCli)
            .expect("should record honestly absent module");
        assert_eq!(
            absent.modules[0].classification, "js_build_material",
            "should not infer embedding from partial or changed bytes"
        );
        assert!(
            collect(bytes, JsRetentionPolicy::FastlyAll).is_err(),
            "should reject a Fastly artifact missing a complete prepared module"
        );
    }
}

#[test]
fn final_js_membership_rejects_an_unrelated_inventory_identity() {
    let completed = JsBuildCompleted::fixture_for_tests(&"a".repeat(40));
    let modules = completed
        .retained_modules()
        .expect("should export checked module facts");
    let root = fs::canonicalize(
        completed
            .rehash_for_handoff()
            .expect("should retain handoff"),
    )
    .expect("should use canonical handoff root");
    let bytes = b"fictional module";
    let digest = hex::encode(Sha256::digest(bytes));
    let mut inputs = JsInputs {
        retained_root: &root,
        modules: &modules,
        inventory_sha256: &"b".repeat(64),
        policy: JsRetentionPolicy::ObserveCli,
    };
    assert!(
        collect_js(bytes, &digest, &inputs).is_err(),
        "should reject a digest that does not identify the retained original inventory"
    );
    inputs.inventory_sha256 = completed.inventory_sha256();
    collect_js(bytes, &digest, &inputs)
        .expect("should accept the exact same bytes with their actual inventory binding");
}

#[test]
fn changed_bundle_wrong_final_digest_duplicates_and_aliases_are_rejected() {
    let completed = JsBuildCompleted::fixture_for_tests(&"a".repeat(40));
    let modules = completed
        .retained_modules()
        .expect("should export checked facts");
    let root = fs::canonicalize(
        completed
            .rehash_for_handoff()
            .expect("should retain handoff"),
    )
    .expect("should use canonical handoff root");
    let bytes = b"fictional module";
    let digest = hex::encode(Sha256::digest(bytes));
    let inputs = |modules| JsInputs {
        retained_root: &root,
        modules,
        inventory_sha256: completed.inventory_sha256(),
        policy: JsRetentionPolicy::ObserveCli,
    };
    assert!(
        collect_js(bytes, &"b".repeat(64), &inputs(&modules)).is_err(),
        "should reject a mismatched final byte identity"
    );
    assert!(
        collect_js(bytes, &digest, &inputs(&[])).is_err(),
        "should reject an omitted prepared module set"
    );
    let mut duplicates = modules.clone();
    duplicates.push(modules[0].clone());
    assert!(
        collect_js(bytes, &digest, &inputs(&duplicates)).is_err(),
        "should reject duplicated module identities"
    );
    let bundle = root.join("bundles/tsjs-core.js");
    fs::write(&bundle, b"changed").expect("should change prepared module");
    assert!(
        collect_js(bytes, &digest, &inputs(&modules)).is_err(),
        "should reject changed prepared module bytes"
    );
    fs::remove_file(&bundle).expect("should remove changed bundle");
    let other = tempfile::tempdir().expect("should own external replacement");
    fs::write(other.path().join("module.js"), bytes)
        .expect("should write identical replacement bytes");
    symlink(other.path().join("module.js"), &bundle).expect("should create final-file alias");
    assert!(
        collect_js(bytes, &digest, &inputs(&modules)).is_err(),
        "should reject aliased prepared module"
    );
}
