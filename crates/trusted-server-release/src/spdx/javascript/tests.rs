use super::*;
use crate::composition::javascript::{JsInputs, JsRetentionPolicy, collect_js};
use crate::javascript::JsBuildCompleted;
use sha2::{Digest as _, Sha256};

#[test]
fn retained_and_absent_modules_have_different_contributor_roles() {
    let completed = JsBuildCompleted::fixture_for_projection(&"a".repeat(40));
    let originals = completed
        .retained_contributions()
        .expect("should export originals");
    for (bytes, retained) in [
        (b"prefix fictional module suffix".as_slice(), true),
        (b"operator binary".as_slice(), false),
    ] {
        let modules = completed.retained_modules().expect("should export modules");
        let root =
            std::fs::canonicalize(completed.rehash_for_handoff().expect("should rehash owner"))
                .expect("should canonicalize");
        let digest = hex::encode(Sha256::digest(bytes));
        let membership = collect_js(
            bytes,
            &digest,
            &JsInputs {
                retained_root: &root,
                modules: &modules,
                inventory_sha256: originals.inventory_sha256(),
                policy: JsRetentionPolicy::ObserveCli,
            },
        )
        .expect("should inspect final bytes");
        let components = project(&membership, &originals, &"a".repeat(40), &digest)
            .expect("should project checked joins");
        assert!(
            components
                .iter()
                .any(|component| component.roles.contains(&ComponentRole::RetainedJavascript))
                == retained,
            "should claim retention only for complete final bytes"
        );
        assert!(
            components.iter().any(|component| component
                .roles
                .contains(&ComponentRole::ConservativeJavascript))
                == retained,
            "should preserve conservative origin role only for retained module"
        );
        assert!(
            project(&membership, &originals, &"b".repeat(40), &digest).is_err(),
            "should reject another source"
        );
        assert!(
            project(&membership, &originals, &"a".repeat(40), &"f".repeat(64)).is_err(),
            "should reject another artifact"
        );
        if !retained {
            let document = super::super::render(&super::super::SpdxInputs {
                executable: bytes,
                expected_artifact_sha256: digest,
                name: "example-cli".to_owned(),
                version: "1.0.0".to_owned(),
                source_commit: "a".repeat(40),
                target: crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu,
                created: "2026-01-01T00:00:00Z".to_owned(),
                document_comment: None,
                components,
                dependencies: Vec::new(),
            })
            .expect("should render absent CLI modules as build evidence");
            let value: serde_json::Value = serde_json::from_slice(document.bytes())
                .expect("should parse rendered CLI evidence");
            let relationships = value["relationships"]
                .as_array()
                .expect("should list relationships");
            assert!(
                relationships.iter().all(|edge| {
                    edge["relationshipType"] != "CONTAINS"
                        && edge["relationshipType"] != "GENERATED_FROM"
                }),
                "should claim no retained or conservative JS contributors in an absent CLI artifact"
            );
            assert!(
                relationships
                    .iter()
                    .any(|edge| edge["relationshipType"] == "BUILD_DEPENDENCY_OF"),
                "should preserve discarded JS as build evidence"
            );
        }
    }
}

#[test]
fn checked_originals_render_distinct_archive_tree_and_generated_evidence() {
    let completed = JsBuildCompleted::fixture_for_projection(&"a".repeat(40));
    let originals = completed
        .retained_contributions()
        .expect("should export originals");
    let bytes = b"prefix fictional module suffix";
    let digest = hex::encode(Sha256::digest(bytes));
    let modules = completed.retained_modules().expect("should export modules");
    let root = std::fs::canonicalize(completed.rehash_for_handoff().expect("should rehash owner"))
        .expect("should canonicalize");
    let membership = collect_js(
        bytes,
        &digest,
        &JsInputs {
            retained_root: &root,
            modules: &modules,
            inventory_sha256: originals.inventory_sha256(),
            policy: JsRetentionPolicy::FastlyAll,
        },
    )
    .expect("should inspect final bytes");
    let components = project(&membership, &originals, &"a".repeat(40), &digest)
        .expect("should project originals");
    let inputs = super::super::SpdxInputs {
        executable: bytes,
        expected_artifact_sha256: digest,
        name: "example-artifact".to_owned(),
        version: "1.0.0".to_owned(),
        source_commit: "a".repeat(40),
        target: crate::policy::ReleaseTarget::Wasm32Wasip1,
        created: "2026-01-01T00:00:00Z".to_owned(),
        document_comment: None,
        components,
        dependencies: Vec::new(),
    };
    let generated = super::super::render(&inputs).expect("should render checked original facts");
    let value: serde_json::Value =
        serde_json::from_slice(generated.bytes()).expect("should parse document");
    let packages = value["packages"]
        .as_array()
        .expect("should contain packages");
    let npm = packages
        .iter()
        .find(|package| package["name"] == "vite")
        .expect("should retain npm package");
    assert_eq!(
        npm["downloadLocation"], "NOASSERTION",
        "should not invent original archive URL"
    );
    assert_eq!(
        npm["checksums"][0]["algorithm"], "SHA512",
        "should identify original npm archive algorithm"
    );
    assert_eq!(
        npm["checksums"][0]["checksumValue"],
        "0".repeat(128),
        "should decode original archive integrity"
    );
    assert!(
        npm["sourceInfo"]
            .as_str()
            .expect("should describe original")
            .contains(&"d".repeat(64)),
        "should retain installed tree separately"
    );
    assert!(
        packages
            .iter()
            .all(|package| package["licenseDeclared"] == "NOASSERTION"),
        "should not infer licenses"
    );
    assert!(
        value["relationships"]
            .as_array()
            .expect("should contain relationships")
            .iter()
            .any(|edge| edge["relationshipType"] == "GENERATES"),
        "should join checked generated owner"
    );
    assert!(
        !String::from_utf8(generated.bytes().to_vec())
            .expect("should encode UTF8")
            .contains(root.to_str().expect("should encode root")),
        "should exclude private root"
    );
}

#[test]
fn relocated_owners_preserve_stable_component_identities() {
    let mut identities = Vec::new();
    for _ in 0..2 {
        let completed = JsBuildCompleted::fixture_for_projection(&"a".repeat(40));
        let originals = completed
            .retained_contributions()
            .expect("should export originals");
        let bytes = b"operator binary";
        let digest = hex::encode(Sha256::digest(bytes));
        let modules = completed.retained_modules().expect("should export modules");
        let root =
            std::fs::canonicalize(completed.rehash_for_handoff().expect("should rehash owner"))
                .expect("should canonicalize");
        let membership = collect_js(
            bytes,
            &digest,
            &JsInputs {
                retained_root: &root,
                modules: &modules,
                inventory_sha256: originals.inventory_sha256(),
                policy: JsRetentionPolicy::ObserveCli,
            },
        )
        .expect("should inspect final bytes");
        identities.push(
            project(&membership, &originals, &"a".repeat(40), &digest)
                .expect("should project")
                .into_iter()
                .map(|component| component.identity)
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        identities[0], identities[1],
        "should ignore private owner relocation"
    );
}

#[test]
fn equal_module_bytes_do_not_substitute_for_exact_original_inventory() {
    let owner = JsBuildCompleted::fixture_for_tests(&"a".repeat(40));
    let other = JsBuildCompleted::fixture_for_projection(&"a".repeat(40));
    let originals = other
        .retained_contributions()
        .expect("should export different originals");
    let bytes = b"fictional module";
    let digest = hex::encode(Sha256::digest(bytes));
    let modules = owner.retained_modules().expect("should export modules");
    let root = std::fs::canonicalize(owner.rehash_for_handoff().expect("should rehash owner"))
        .expect("should canonicalize");
    let membership = collect_js(
        bytes,
        &digest,
        &JsInputs {
            retained_root: &root,
            modules: &modules,
            inventory_sha256: owner.inventory_sha256(),
            policy: JsRetentionPolicy::FastlyAll,
        },
    )
    .expect("should inspect same complete module bytes");
    assert!(
        project(&membership, &originals, &"a".repeat(40), &digest).is_err(),
        "should reject different raw inventory despite equal complete module bytes"
    );
}
