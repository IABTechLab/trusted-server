use super::*;
use crate::composition::{CargoLayout, observe_cargo};
use crate::policy::ReleaseTarget;
use std::path::PathBuf;

fn git() -> CargoOrigin {
    CargoOrigin::Git {
        revision: "a".repeat(40),
        repository_tree: "b".repeat(40),
        package_tree: "c".repeat(40),
        package_path: "packages/example".to_owned(),
        committed_manifest_blob: "d".repeat(40),
        committed_manifest_sha256: "e".repeat(64),
        normalized_manifest_sha256: None,
    }
}

#[test]
fn original_identity_distinguishes_complete_registry_and_git_facts() {
    let registry = CargoOrigin::Registry {
        archive_sha256: "f".repeat(64),
    };
    let identity = |source: &str, original: &CargoOrigin| {
        material_identity(source, original, "example", "1.0.0")
            .expect("should accept verified original")
    };
    assert_ne!(
        identity("registry+https://example.com/first", &registry),
        identity("registry+https://example.com/second", &registry),
        "should distinguish registry source identities"
    );
    let source = format!("git+https://example.com/repo?tag=v1#{}", "a".repeat(40));
    let original = git();
    let first = identity(&source, &original);
    assert_ne!(
        first,
        identity(&source.replace("tag=v1", "branch=v1"), &original),
        "should preserve Git selector"
    );
    let mut changed = git();
    if let CargoOrigin::Git { package_path, .. } = &mut changed {
        *package_path = "other".to_owned();
    }
    assert_ne!(
        first,
        identity(&source, &changed),
        "should preserve package path"
    );
    let mut changed = git();
    if let CargoOrigin::Git { revision, .. } = &mut changed {
        *revision = "f".repeat(40);
    }
    assert_ne!(
        first,
        identity(&source.replace(&"a".repeat(40), &"f".repeat(40)), &changed),
        "should preserve locked revision"
    );
    assert!(
        material_identity(&source, &changed, "example", "1.0.0").is_err(),
        "should reject conflicting locked revision"
    );
}

#[test]
fn malformed_originals_and_noncanonical_paths_are_rejected() {
    for source in [
        "https://example.com/index",
        "registry+file:///tmp/index",
        "registry+https://",
        "registry+https://example.com/index#extra",
    ] {
        assert!(
            material_identity(
                source,
                &CargoOrigin::Registry {
                    archive_sha256: "f".repeat(64)
                },
                "example",
                "1"
            )
            .is_err(),
            "should reject malformed registry identity"
        );
    }
    for path in ["../outside", "/absolute", "a//b", "a/./b", "a\\b"] {
        let mut original = git();
        if let CargoOrigin::Git { package_path, .. } = &mut original {
            *package_path = path.to_owned();
        }
        assert!(
            material_identity(
                &format!("git+https://example.com/repo#{}", "a".repeat(40)),
                &original,
                "example",
                "1"
            )
            .is_err(),
            "should reject invalid package path"
        );
    }
    assert!(
        material_identity(
            "registry+https://example.com/index",
            &CargoOrigin::Registry {
                archive_sha256: "a".repeat(40)
            },
            "example",
            "1"
        )
        .is_err(),
        "should reject non-SHA256 archive identity"
    );
}

#[test]
fn compiler_roles_fail_closed_and_preserve_host_target_distinction() {
    for (context, kind, crate_type, expected) in [
        (
            UnitContext::Host,
            "proc-macro",
            "proc-macro",
            ComponentRole::BuildTool,
        ),
        (
            UnitContext::Host,
            "custom-build",
            "bin",
            ComponentRole::BuildTool,
        ),
        (
            UnitContext::Host,
            "lib",
            "lib",
            ComponentRole::BuildMaterial,
        ),
        (
            UnitContext::Target,
            "lib",
            "lib",
            ComponentRole::ObservedRust,
        ),
        (
            UnitContext::Target,
            "bin",
            "bin",
            ComponentRole::ObservedRust,
        ),
    ] {
        assert!(
            classify(context, &[kind.to_owned()], &[crate_type.to_owned()])
                .expect("should classify known compiler unit")
                == expected,
            "should retain observed compiler role"
        );
    }
    for (context, kind, crate_type) in [
        (UnitContext::Target, "proc-macro", "proc-macro"),
        (UnitContext::Target, "custom-build", "bin"),
        (UnitContext::Host, "bin", "bin"),
        (UnitContext::Target, "example", "bin"),
        (UnitContext::Target, "lib", "unknown"),
    ] {
        assert!(
            classify(context, &[kind.to_owned()], &[crate_type.to_owned()]).is_err(),
            "should reject impossible or unknown unit"
        );
    }
}

#[test]
fn workspace_logical_identity_ignores_relocated_opaque_ids() {
    let metadata =
        include_str!("../../../tests/fixtures/composition/cargo-1.95-cross-host-metadata.json");
    let messages =
        include_str!("../../../tests/fixtures/composition/cargo-1.95-cross-messages.jsonl");
    let capture = |root: &str| {
        let layout = CargoLayout::new(
            ReleaseTarget::Wasm32Wasip1,
            PathBuf::from(format!("{root}/target")),
            PathBuf::from(format!("{root}/source")),
        )
        .expect("should accept layout");
        observe_cargo(
            metadata.replace("/build", root).as_bytes(),
            messages.replace("/build", root).as_bytes(),
            &layout,
        )
        .expect("should observe relocated fixture")
    };
    let first = capture("/first");
    let second = capture("/second");
    assert_ne!(
        first.packages()[0].id(),
        second.packages()[0].id(),
        "should actually relocate opaque Cargo identities"
    );
    let keys = |observed: &CargoObservations| {
        observed
            .packages()
            .iter()
            .map(|package| {
                workspace_identity(
                    &"a".repeat(40),
                    &format!("{}/Cargo.toml", package.name()),
                    package.name(),
                    package.version(),
                )
                .expect("should accept tracked manifest")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&first),
        keys(&second),
        "should retain stable source identities after relocation"
    );
}

#[test]
fn registry_locked_source_requires_an_actual_https_authority() {
    let original = CargoOrigin::Registry {
        archive_sha256: "a".repeat(64),
    };
    for source in [
        "registry+https:///index",
        "registry+https://@/index",
        "sparse+https:///index",
    ] {
        assert!(
            material_identity(source, &original, "example", "1").is_err(),
            "should reject registry identity without a valid authority"
        );
    }
}

#[test]
fn per_unit_feature_receipts_remain_separate_for_multiple_roles() {
    let metadata =
        include_bytes!("../../../tests/fixtures/composition/cargo-1.95-cross-host-metadata.json");
    let messages =
        include_str!("../../../tests/fixtures/composition/cargo-1.95-cross-messages.jsonl");
    let mut stream = String::new();
    for line in messages.lines() {
        let mut message: serde_json::Value =
            serde_json::from_str(line).expect("should parse observed compiler line");
        if message["reason"] == "compiler-artifact"
            && message["package_id"]
                .as_str()
                .is_some_and(|id| id.contains("example-app@"))
        {
            message["features"] = if message["target"]["kind"][0] == "custom-build" {
                json!(["host-only"])
            } else {
                json!(["target-only"])
            };
        }
        stream.push_str(&serde_json::to_string(&message).expect("should serialize compiler line"));
        stream.push('\n');
    }
    let layout = CargoLayout::new(
        ReleaseTarget::Wasm32Wasip1,
        PathBuf::from("/build/target"),
        PathBuf::from("/build/source"),
    )
    .expect("should accept fixture layout");
    let observed = observe_cargo(metadata, stream.as_bytes(), &layout)
        .expect("should observe distinct compiler units");
    let units = observed
        .units()
        .iter()
        .filter(|unit| unit.package_id().contains("example-app@"))
        .collect::<Vec<_>>();
    let roles = package_roles(&observed, units[0].package_id())
        .expect("should classify all observed roles");
    assert!(
        roles.contains(&ComponentRole::BuildTool) && roles.contains(&ComponentRole::ObservedRust),
        "should retain multiple package roles"
    );
    assert!(
        units
            .iter()
            .any(|unit| unit.context() == UnitContext::Host && unit.features() == ["host-only"]),
        "should retain host features separately"
    );
    assert!(units.iter().any(|unit| unit.context() == UnitContext::Target && unit.features() == ["target-only"]),"should retain target features separately");
    assert!(
        units.iter().all(|unit| unit.features().len() == 1),
        "should never synthesize package feature unions"
    );
}

#[test]
fn package_edges_require_both_observed_endpoints_and_skip_dev_kinds() {
    let identities = BTreeMap::from([
        ("parent", "stable-parent".to_owned()),
        ("child", "stable-child".to_owned()),
    ]);
    assert!(
        project_edge(
            "parent",
            "potential-only",
            &["normal".to_owned()],
            &identities
        )
        .expect("should exclude potential-only endpoint")
        .is_empty(),
        "should exclude potential graph membership"
    );
    assert!(
        project_edge("parent", "child", &["dev".to_owned()], &identities)
            .expect("should skip dev-only relation")
            .is_empty(),
        "should omit dev-only dependencies"
    );
    let edges = project_edge(
        "parent",
        "child",
        &["normal".to_owned(), "build".to_owned()],
        &identities,
    )
    .expect("should preserve normal and build declarations");
    assert_eq!(edges.len(), 2, "should retain both relation kinds");
    assert!(
        !edges[0].build && edges[1].build,
        "should preserve normal versus build semantics"
    );
}

#[test]
fn multiple_library_outputs_preserve_host_and_target_roles() {
    let kinds = ["cdylib".to_owned(), "rlib".to_owned()];
    for (context, expected) in [
        (UnitContext::Host, ComponentRole::BuildMaterial),
        (UnitContext::Target, ComponentRole::ObservedRust),
    ] {
        assert!(
            classify(context, &kinds, &kinds)
                .expect("should accept multiple compatible library outputs")
                == expected,
            "should preserve library compiler context"
        );
    }
}

#[test]
fn multiple_library_outputs_reject_mixed_duplicate_and_incompatible_kinds() {
    for kinds in [
        vec!["lib", "bin"],
        vec!["rlib", "proc-macro"],
        vec!["rlib", "custom-build"],
        vec!["rlib", "test"],
        vec!["rlib", "rlib"],
        vec![],
    ] {
        let kinds = kinds.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            classify(UnitContext::Target, &kinds, &["rlib".to_owned()]).is_err(),
            "should reject non-library or duplicate target kinds"
        );
    }
    assert!(
        classify(
            UnitContext::Target,
            &["cdylib".to_owned(), "rlib".to_owned()],
            &["bin".to_owned()]
        )
        .is_err(),
        "should reject incompatible library crate types"
    );
}
