use super::*;
use crate::composition::UnitContext;
use crate::composition::native::fixture_for_projection;
use crate::composition::origin::VerifiedPackageOrigins;
use sha2::Digest as _;

#[test]
fn native_license_declarations_require_native_license_evidence() {
    for name in ["aws-lc-sys", "ring", "libsqlite3-sys"] {
        for context in [UnitContext::Target, UnitContext::Host] {
            let (inventory, observed) = fixture_for_projection(name, context);
            let origins = VerifiedPackageOrigins::fixture_native_registry(&observed);
            let mut components = project(&inventory, &observed, &origins)
                .expect("should project measured native inputs");
            components[0].declared_license = Some("MIT".to_owned());
            let executable = b"fictional native executable";
            let result = super::super::render(&super::super::SpdxInputs {
                executable,
                expected_artifact_sha256: hex::encode(sha2::Sha256::digest(executable)),
                name: "example-cli".to_owned(),
                version: "1.0.0".to_owned(),
                source_commit: "a".repeat(40),
                target: crate::policy::ReleaseTarget::Aarch64AppleDarwin,
                created: "2026-01-01T00:00:00Z".to_owned(),
                document_comment: None,
                components,
                dependencies: Vec::new(),
            });
            assert!(
                result.is_err(),
                "should reject an unsupported native license declaration for {name} in {context:?}"
            );
        }
    }
}

#[test]
fn target_and_host_native_inputs_keep_distinct_roles_and_unknown_implementation_versions() {
    for context in [UnitContext::Target, UnitContext::Host] {
        let (inventory, observed) = fixture_for_projection("ring", context);
        let origins = VerifiedPackageOrigins::fixture_native_registry(&observed);
        let components =
            project(&inventory, &observed, &origins).expect("should project checked native facts");
        assert_eq!(
            components.len(),
            1,
            "should project one native wrapper context"
        );
        assert!(
            components[0]
                .roles
                .contains(&if context == UnitContext::Target {
                    ComponentRole::ObservedNative
                } else {
                    ComponentRole::BuildMaterial
                }),
            "should distinguish target contribution from host build input"
        );
        assert_eq!(
            components[0].version, None,
            "should not reuse Cargo wrapper version as native implementation version"
        );
    }
}

#[test]
fn native_original_archive_checksum_and_measured_outputs_remain_distinct() {
    for name in ["aws-lc-sys", "ring", "libsqlite3-sys"] {
        let (inventory, observed) = fixture_for_projection(name, UnitContext::Target);
        let origins = VerifiedPackageOrigins::fixture_native_registry(&observed);
        let components =
            project(&inventory, &observed, &origins).expect("should project checked native facts");
        let executable = b"fictional native executable";
        let document = super::super::render(&super::super::SpdxInputs {
            executable,
            expected_artifact_sha256: hex::encode(sha2::Sha256::digest(executable)),
            name: "example-cli".to_owned(),
            version: "1.0.0".to_owned(),
            source_commit: "a".repeat(40),
            target: crate::policy::ReleaseTarget::Aarch64AppleDarwin,
            created: "2026-01-01T00:00:00Z".to_owned(),
            document_comment: None,
            components,
            dependencies: Vec::new(),
        })
        .expect("should render native original facts against authoritative schema");
        let value: serde_json::Value =
            serde_json::from_slice(document.bytes()).expect("should parse SPDX");
        let package = &value["packages"][1];
        assert_eq!(
            package["checksums"],
            json!([{"algorithm":"SHA256","checksumValue":"a".repeat(64)}]),
            "should checksum only the original registry source archive"
        );
        assert!(
            package.get("versionInfo").is_none(),
            "should not label native implementation with Cargo wrapper version"
        );
        assert_eq!(
            package["downloadLocation"], "NOASSERTION",
            "should not invent source archive URL"
        );
        assert_eq!(
            package["licenseDeclared"], "NOASSERTION",
            "should not infer native implementation license from wrapper"
        );
        let info = package["sourceInfo"]
            .as_str()
            .expect("should describe native provenance");
        assert!(
            info.contains(inventory.contributors()[0].archives()[0].sha256()),
            "should retain measured static output digest as provenance"
        );
        assert!(
            info.contains(inventory.contributors()[0].source_tree_sha256()),
            "should retain structured prepared-tree provenance"
        );
        assert!(
            info.contains("Cargo wrapper versions do not identify native implementation versions"),
            "should label wrapper version limitations"
        );
        assert!(
            !info.contains(observed.packages()[0].id()),
            "should omit opaque package ID"
        );
        assert!(
            !info.contains("/target/") && !info.contains("/vendor/"),
            "should omit private source/output paths"
        );
    }
}

#[test]
fn native_logical_ids_do_not_depend_on_private_source_and_output_roots() {
    let identities = (0..2)
        .map(|_| {
            let (inventory, observed) = fixture_for_projection("ring", UnitContext::Target);
            let origins = VerifiedPackageOrigins::fixture_native_registry(&observed);
            project(&inventory, &observed, &origins).expect("should project relocated facts")[0]
                .identity
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        identities[0], identities[1],
        "should preserve native logical identity across private root relocation"
    );
}
