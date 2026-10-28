use super::*;
use crate::composition::platform::tests::projection_fixture;
use sha2::{Digest as _, Sha256};

#[test]
fn libtapi_projects_only_present_measured_bytes_as_a_build_tool() {
    for present in [false, true] {
        let platform =
            crate::composition::platform::tests::projection_toolchain_with_libtapi_fixture(present);
        let components = project(&platform, &Owner::fixture(&platform))
            .expect("should project optional TAPI measurements");
        let tapi = components.iter().find(|component| {
            matches!(
                &component.origin,
                Some(Origin::PlatformMeasuredFile { role, .. }) if role == "apple-libtapi"
            )
        });
        assert_eq!(
            tapi.is_some(),
            present,
            "should project only present libtapi"
        );
        if let Some(component) = tapi {
            assert!(
                component.declared_license.is_none(),
                "should not invent a libtapi license"
            );
            let digest = hex::encode(Sha256::digest(b"fictional libtapi"));
            assert!(
                component.roles == [ComponentRole::BuildTool],
                "should assert only a build tool role"
            );
            let (location, comment, checksum) =
                file_origin_fields(component, "apple-libtapi", &digest)
                    .expect("should render measured libtapi bytes");
            assert_eq!(location, "NOASSERTION", "should not claim acquisition");
            assert_eq!(
                checksum,
                Some(("SHA256", digest.clone())),
                "should preserve raw-file checksum"
            );
            assert!(
                comment.contains("no original acquisition")
                    && comment.contains("executed helper claim"),
                "should retain conservative measurement scope"
            );
            assert!(
                matches!(&component.origin, Some(Origin::PlatformMeasuredFile { sha256, .. }) if sha256 == &digest),
                "should bind measured libtapi bytes"
            );
        }
        assert!(
            coverage_gaps(&platform)
                .contains(&"apple-coverage-excluded:toolchain_libraries".to_owned()),
            "should retain unresolved library dependency/load closure coverage"
        );
        let receipt = serde_json::to_value(&platform).expect("should serialize platform receipt");
        assert_eq!(
            receipt["native_tools"]["full_closure_measured"], false,
            "should retain incomplete closure"
        );
    }
}

impl Owner {
    fn fixture(platform: &crate::composition::platform::PlatformEvidence) -> Self {
        Self {
            target: platform.external_requirements().target(),
            artifact: platform.artifact_sha256().to_owned(),
            rust_tree: platform.rust().tree_sha256().to_owned(),
            source: "a".repeat(40),
            receipt_source: "a".repeat(40),
            boundary: [1; 32],
            plan: "b".repeat(64),
        }
    }
}

#[test]
fn projects_measured_trees_without_raw_checksums_and_runtime_without_byte_claims() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Wasm32Wasip1);
    let owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project measured platform facts");
    let rust = components
        .iter()
        .find(|component| component.name == "Measured Rust tool tree")
        .expect("should project Rust tool tree");
    assert!(
        rust.roles.contains(&ComponentRole::BuildTool)
            && rust.roles.contains(&ComponentRole::BuildMaterial),
        "should preserve both installed Rust roles"
    );
    assert!(
        matches!(&rust.origin, Some(Origin::StructuredTree { .. })),
        "should not treat a structured tree digest as file bytes"
    );
    assert!(
        components
            .iter()
            .any(|component| component.roles == [ComponentRole::ExternalRuntime]),
        "should project actual Wasm logical imports"
    );
    for component in &components {
        assert!(
            !component.identity.contains("/boundary/")
                && !component.identity.contains("/private/")
                && !component.identity.contains("/var/"),
            "should use stable identities independent of temporary roots"
        );
        assert!(
            component.declared_license.is_none(),
            "should not invent licenses"
        );
    }
}

#[test]
fn rejects_another_artifact_target_rust_tree_or_source_owner() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Wasm32Wasip1);
    for field in ["artifact", "target", "rust", "source"] {
        let mut owner = Owner::fixture(&platform);
        match field {
            "artifact" => owner.artifact = "b".repeat(64),
            "target" => owner.target = crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu,
            "rust" => owner.rust_tree = "c".repeat(64),
            "source" => owner.receipt_source = "d".repeat(40),
            _ => unreachable!("should select a fixture field"),
        }
        assert!(
            project(&platform, &owner).is_err(),
            "should reject mismatched {field} owner"
        );
    }
}

#[test]
fn gnu_projection_requires_the_owning_boundary_and_plan_and_calls_candidates_honestly() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_gnu(crate::recipe::gnu::GnuEvidence::projection_fixture())
        .expect("should attach fictional typed GNU observations");
    let mut owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should join GNU boundary and plan");
    assert!(
        components
            .iter()
            .any(|component| component.name.contains("gcc_linker_candidate")
                && component.roles == [ComponentRole::BuildTool]),
        "should retain configured candidate meaning without claiming execution"
    );
    owner.boundary = [2; 32];
    assert!(
        project(&platform, &owner).is_err(),
        "should reject another GNU boundary"
    );
    owner.boundary = [1; 32];
    owner.plan = "f".repeat(64);
    assert!(
        project(&platform, &owner).is_err(),
        "should reject another GNU invocation"
    );
}

#[test]
fn apple_projection_preserves_raw_file_identity_and_logical_runtime_without_private_paths() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Aarch64AppleDarwin);
    let owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project Apple platform");
    assert_eq!(
        components
            .iter()
            .filter(|component| matches!(
                &component.origin,
                Some(Origin::PlatformMeasuredFile { .. })
            ))
            .count(),
        4,
        "should project three tools and SDK settings as raw files"
    );
    assert!(
        components.iter().any(|component| component.name
            == format!(
                "Logical Mach-O SDK: {}",
                platform
                    .external_requirements()
                    .macho_sdk()
                    .expect("should preserve accepted SDK")
            )),
        "should preserve accepted SDK requirement"
    );
    for component in &components {
        assert!(
            !component.identity.contains("MacOSX.platform")
                && !component.identity.contains("/private/"),
            "should exclude private SDK and tool paths from identity"
        );
    }
}

#[test]
fn measured_file_renderer_asserts_only_bytes_and_rejects_runtime_roles() {
    let mut component = measured_file(
        crate::policy::ReleaseTarget::Wasm32Wasip1,
        "fictional-tool",
        &"a".repeat(64),
        ComponentRole::BuildTool,
    )
    .expect("should describe installed file");
    let (location, information, checksum) =
        file_origin_fields(&component, "fictional-tool", &"a".repeat(64))
            .expect("should render measured bytes");
    assert_eq!(
        location, "NOASSERTION",
        "should not invent original acquisition location"
    );
    assert_eq!(
        checksum,
        Some(("SHA256", "a".repeat(64))),
        "should checksum measured raw bytes"
    );
    assert!(
        information.contains("no original acquisition") && information.contains("executed helper"),
        "should preserve limits of measured facts"
    );
    component.roles = vec![ComponentRole::ExternalRuntime];
    assert!(
        file_origin_fields(&component, "fictional-tool", &"a".repeat(64)).is_err(),
        "should never assign installed bytes to logical runtime requirements"
    );
}

#[test]
fn detached_sdk_is_structured_material_without_raw_file_checksum() {
    let platform = crate::composition::platform::tests::projection_detached_sdk_fixture();
    let owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project detached SDK");
    let sdk = components
        .iter()
        .find(|component| component.name == "Measured detached Apple SDK tree")
        .expect("should retain SDK tree");
    assert!(
        sdk.roles == [ComponentRole::BuildMaterial]
            && matches!(&sdk.origin, Some(Origin::StructuredTree { .. })),
        "should preserve SDK tree evidence as structured material"
    );
    assert!(
        platform
            .apple()
            .expect("should retain Apple receipt")
            .sdk_tree_sha256()
            .is_some(),
        "should borrow original SDK tree receipt"
    );
}

#[test]
fn measured_toolchain_projects_library_resource_and_helper_trees_and_retains_closure_gaps() {
    let platform = crate::composition::platform::tests::projection_measured_toolchain_fixture();
    let owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project measured toolchain");
    let apple = platform.apple().expect("should retain Apple receipt");
    for (name, sha256, role) in [
        (
            "Measured Apple toolchain library directory tree",
            apple.library_tree_sha256(),
            ComponentRole::BuildMaterial,
        ),
        (
            "Measured Apple clang resource directory tree",
            apple.clang_resource_tree_sha256(),
            ComponentRole::BuildMaterial,
        ),
        (
            "Measured Apple toolchain helper directory tree",
            apple.helper_tree_sha256(),
            ComponentRole::BuildTool,
        ),
    ] {
        let sha256 = sha256.expect("should hold the measured tree digest");
        let tree = components
            .iter()
            .find(|component| component.name == name)
            .unwrap_or_else(|| panic!("should project {name}"));
        assert!(
            tree.roles == [role]
                && matches!(&tree.origin, Some(Origin::StructuredTree { sha256: digest }) if digest == sha256),
            "should project {name} as a structured tree"
        );
        assert!(
            tree.declared_license.is_none(),
            "should not invent a tree license"
        );
        if name == "Measured Apple toolchain library directory tree" {
            let inputs = super::super::SpdxInputs {
                executable: b"",
                expected_artifact_sha256: owner.artifact.clone(),
                name: "fictional-artifact".to_owned(),
                version: "1.0.0".to_owned(),
                source_commit: owner.source.clone(),
                target: owner.target,
                created: "2026-01-01T00:00:00Z".to_owned(),
                document_comment: None,
                components: vec![],
                dependencies: vec![],
            };
            let (location, comment, checksum) = super::super::origin_fields(tree, &inputs)
                .expect("should render structured library material");
            assert_eq!(location, "NOASSERTION", "should not invent acquisition");
            assert!(
                checksum.is_none(),
                "should not render a raw-file checksum for a tree"
            );
            assert!(
                comment.contains("Structured installed-tree evidence"),
                "should state only installed-tree evidence"
            );
            assert_eq!(
                tree.identity,
                identity(owner.target, "apple-toolchain-library-tree", sha256)
                    .expect("should construct stable library material identity"),
                "should retain stable library tree role"
            );
        }
    }
    assert!(
        components
            .iter()
            .any(|component| component.name == "Measured platform file (apple-liblto)"),
        "should project libLTO as a measured file"
    );
    let gaps = coverage_gaps(&platform);
    for measured in ["tool_helpers", "liblto", "clang_resource_directory"] {
        assert!(
            !gaps.contains(&format!("apple-coverage-excluded:{measured}")),
            "should not state measured {measured} as excluded"
        );
    }
    for retained in [
        "apple-coverage-excluded:toolchain_libraries",
        "apple-tool-closure-unmeasured",
    ] {
        assert!(
            gaps.iter().any(|gap| gap == retained),
            "should retain {retained}"
        );
    }
}

#[test]
fn rendered_projection_preserves_runtime_constraints_without_runtime_byte_checksums() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Wasm32Wasip1);
    let owner = Owner::fixture(&platform);
    let executable =
        crate::inspect::tests::builders::wasm_module(&[("fastly_http_req", "downstream_get")]);
    let components = project(&platform, &owner).expect("should project platform");
    let inputs = super::super::SpdxInputs {
        executable: &executable,
        expected_artifact_sha256: owner.artifact,
        name: "fictional-artifact".to_owned(),
        version: "1.0.0".to_owned(),
        source_commit: owner.source,
        target: owner.target,
        created: "2026-01-01T00:00:00Z".to_owned(),
        document_comment: None,
        components,
        dependencies: vec![],
    };
    let rendered =
        super::super::render(&inputs).expect("should render schema-valid fictional projection");
    let json: serde_json::Value =
        serde_json::from_slice(rendered.bytes()).expect("should inspect rendered document");
    let runtime = json["packages"]
        .as_array()
        .expect("should retain packages")
        .iter()
        .find(|package| package["name"] == "Logical Wasm host import: fastly_http_req")
        .expect("should retain exact runtime requirement");
    assert!(
        runtime.get("checksums").is_none(),
        "should never checksum logical runtime bytes"
    );
    assert_eq!(
        runtime["downloadLocation"], "NOASSERTION",
        "should not invent runtime acquisition"
    );
}

#[test]
fn compiler_runtime_archive_is_a_build_dependency_with_no_tool_or_retention_claim() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_gnu(crate::recipe::gnu::GnuEvidence::projection_fixture())
        .expect("should attach fictional GNU receipt");
    let owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project installed GNU facts");
    let runtime = components
        .iter()
        .find(|component| component.name.contains("compiler_runtime"))
        .expect("should project measured compiler runtime archive");
    assert!(
        runtime.roles == [ComponentRole::BuildMaterial],
        "should classify libgcc.a as build material rather than an executed tool"
    );
    let id = super::super::component_id(&runtime.identity);
    let executable = crate::inspect::tests::builders::elf(
        &crate::inspect::tests::builders::ElfSpec::conforming(),
    );
    let inputs = super::super::SpdxInputs {
        executable: &executable,
        expected_artifact_sha256: owner.artifact,
        name: "fictional-artifact".to_owned(),
        version: "1.0.0".to_owned(),
        source_commit: owner.source,
        target: owner.target,
        created: "2026-01-01T00:00:00Z".to_owned(),
        document_comment: None,
        components,
        dependencies: vec![],
    };
    let rendered = super::super::render(&inputs).expect("should render measured GNU projection");
    let json: serde_json::Value =
        serde_json::from_slice(rendered.bytes()).expect("should inspect rendered relationships");
    let relationships = json["relationships"]
        .as_array()
        .expect("should retain relationships");
    assert!(
        relationships
            .iter()
            .any(|relation| relation["spdxElementId"] == id
                && relation["relationshipType"] == "BUILD_DEPENDENCY_OF"
                && relation["comment"]
                    .as_str()
                    .is_some_and(|comment| comment.contains("retention is not asserted"))),
        "should preserve archive as a measured build dependency without retention inference"
    );
    assert!(
        !relationships
            .iter()
            .any(|relation| relation["spdxElementId"] == id
                && matches!(
                    relation["relationshipType"].as_str(),
                    Some("BUILD_TOOL_OF" | "RUNTIME_DEPENDENCY_OF" | "CONTAINS")
                )),
        "should not classify compiler runtime archive as tool, logical runtime, or retained bytes"
    );
    let package = json["packages"]
        .as_array()
        .expect("should retain packages")
        .iter()
        .find(|package| package["SPDXID"] == id)
        .expect("should retain measured archive package");
    assert_eq!(
        package["checksums"][0]["checksumValue"],
        "c".repeat(64),
        "should retain measured archive raw-byte identity"
    );
    assert_eq!(
        package["downloadLocation"], "NOASSERTION",
        "should not invent original archive acquisition"
    );
}

#[test]
fn new_gnu_file_roles_require_review_before_projection() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_gnu(crate::recipe::gnu::GnuEvidence::projection_fixture_with_unknown_file_role())
        .expect("should attach fictional unknown resource");
    assert!(
        project(&platform, &Owner::fixture(&platform)).is_err(),
        "should reject new resources until their component role is reviewed"
    );
}

#[test]
fn coverage_gaps_name_unresolved_fastly_tools_and_incomplete_native_materials() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Wasm32Wasip1);
    let gaps = coverage_gaps(&platform);
    for expected in [
        "native-material-coverage-incomplete",
        "native-tool-host-path-unresolved:compiler",
        "native-tool-host-path-unresolved:linker",
        "native-tool-host-path-unresolved:build_tools",
    ] {
        assert!(
            gaps.iter().any(|gap| gap == expected),
            "should state gap {expected}"
        );
    }
    assert!(
        gaps.windows(2).all(|pair| pair[0] < pair[1]),
        "should list gaps sorted and unique"
    );
    assert!(
        !gaps.iter().any(|gap| gap.starts_with("apple-")),
        "should not invent Apple gaps for a Wasm target"
    );
}

#[test]
fn coverage_gaps_retain_gnu_tool_closure_without_unreadable_helpers() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_gnu(crate::recipe::gnu::GnuEvidence::projection_fixture())
        .expect("should attach fictional GNU measurements");
    assert!(
        platform
            .gnu()
            .expect("should hold GNU evidence")
            .content_unmeasured_files()
            .is_empty(),
        "should exercise fully readable installed files"
    );
    let gaps = coverage_gaps(&platform);
    assert!(
        gaps.iter().any(|gap| gap == "gnu-tool-closure-unmeasured"),
        "should retain the unmeasured GNU tool closure"
    );
    assert!(
        gaps.windows(2).all(|pair| pair[0] < pair[1]),
        "should list gaps sorted and unique"
    );
}

#[test]
fn coverage_gaps_retain_gnu_unverified_acquisition_without_unreadable_helpers() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::X86_64UnknownLinuxGnu)
        .with_gnu(crate::recipe::gnu::GnuEvidence::projection_fixture())
        .expect("should attach fictional GNU measurements");
    let gaps = coverage_gaps(&platform);
    assert!(
        gaps.iter()
            .any(|gap| gap == "gnu-original-acquisition-unverified"),
        "should retain unverified GNU original acquisition"
    );
    assert!(
        !gaps
            .iter()
            .any(|gap| gap == "gnu-privileged-helper-content-unmeasured"),
        "should not invent unreadable helper content"
    );
}

#[test]
fn coverage_gaps_do_not_invent_gnu_measurements_for_other_platforms() {
    for target in [
        crate::policy::ReleaseTarget::Wasm32Wasip1,
        crate::policy::ReleaseTarget::Aarch64AppleDarwin,
        crate::policy::ReleaseTarget::X86_64AppleDarwin,
    ] {
        let platform = projection_fixture(target);
        assert!(
            !coverage_gaps(&platform)
                .iter()
                .any(|gap| gap.starts_with("gnu-")),
            "should not invent GNU gaps for {}",
            target.triple()
        );
    }
}

#[test]
fn coverage_gaps_retain_apple_exclusions_and_unmeasured_tool_closure() {
    let platform = projection_fixture(crate::policy::ReleaseTarget::Aarch64AppleDarwin);
    let gaps = coverage_gaps(&platform);
    assert!(
        gaps.iter()
            .any(|gap| gap == "apple-tool-closure-unmeasured"),
        "should state the unmeasured Apple tool closure"
    );
    let apple = platform.apple().expect("should hold Apple evidence");
    assert!(
        !apple.coverage_exclusions().is_empty(),
        "should record fixture exclusions"
    );
    for exclusion in apple.coverage_exclusions() {
        let expected = format!("apple-coverage-excluded:{exclusion}");
        assert!(
            gaps.contains(&expected),
            "should state Apple exclusion {exclusion}"
        );
    }
    assert!(
        !gaps
            .iter()
            .any(|gap| gap.starts_with("native-tool-host-path-unresolved:")),
        "should not report unresolved host paths for explicit Apple files"
    );
}

#[test]
fn fastly_selected_tools_project_with_owner_binding_and_conservative_gaps() {
    let platform = projection_fixture(ReleaseTarget::Wasm32Wasip1)
        .with_fastly_tools(crate::recipe::fastly_tools::FastlyToolEvidence::projection_fixture())
        .expect("should attach fictional Fastly selections");
    let mut owner = Owner::fixture(&platform);
    let components = project(&platform, &owner).expect("should project fixed host files");
    let files = components
        .iter()
        .filter(|component| matches!(component.origin, Some(Origin::PlatformMeasuredFile { .. })))
        .collect::<Vec<_>>();
    assert_eq!(
        files.len(),
        4,
        "should project four selected or configured files"
    );
    for component in files {
        assert!(
            component.roles == [ComponentRole::BuildTool],
            "should retain installed tool scope"
        );
        assert!(
            component.declared_license.is_none(),
            "should not invent acquisition or license"
        );
        assert!(
            !component.identity.contains("/boundary"),
            "should exclude private paths from identity"
        );
    }
    let gaps = coverage_gaps(&platform);
    for gap in [
        "native-tool-host-path-unresolved:assembler",
        "native-tool-host-path-unresolved:build_tools",
        "fastly-tool-closure-unmeasured",
        "native-material-coverage-incomplete",
    ] {
        assert!(
            gaps.contains(&gap.to_owned()),
            "should retain honest gap {gap}"
        );
    }
    for role in ["compiler", "archiver", "linker"] {
        assert!(
            !gaps.contains(&format!("native-tool-host-path-unresolved:{role}")),
            "should resolve selected host role {role}"
        );
    }
    owner.boundary = [2; 32];
    assert!(
        project(&platform, &owner).is_err(),
        "should reject another boundary"
    );
    owner.boundary = [1; 32];
    owner.plan = "f".repeat(64);
    assert!(
        project(&platform, &owner).is_err(),
        "should reject another plan"
    );
}
