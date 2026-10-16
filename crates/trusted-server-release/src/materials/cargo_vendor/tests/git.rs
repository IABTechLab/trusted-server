use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::support::*;
use crate::materials::locked::GitPackage;
use crate::materials::{
    GitVendorInput, LockedMaterials, NormalizedManifest, PreparedCargoMaterials, VendorInputs,
};

const INHERITING: &str = "[package]\nname = \"example-core\"\nversion.workspace = true\nedition.workspace = true\n\n[dependencies]\nexample-dep = { workspace = true }\n";
const NORMALIZED: &str =
    "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2021\"\n";
const PLAIN: &str = "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2021\"\n";

/// A fictional workspace: root manifest plus `crates/core` and a nested package.
struct Workspace {
    objects: Objects,
    root_tree: String,
    manifest_blob: String,
}

fn workspace(core_manifest: &str, extra_core_entries: &[(&str, &str, &str)]) -> Workspace {
    let objects = Objects::new();
    let manifest_blob = objects.blob(core_manifest.as_bytes());
    let lib = objects.blob(b"pub fn core() {}\n");
    let script = objects.blob(b"#!/bin/sh\n");
    let src = objects.tree(&[("100644", "lib.rs", &lib)]);
    let nested_manifest = objects.blob(&manifest("example-nested", "0.1.0"));
    let nested_lib = objects.blob(b"pub fn nested() {}\n");
    let nested_src = objects.tree(&[("100644", "lib.rs", &nested_lib)]);
    let nested = objects.tree(&[
        ("100644", "Cargo.toml", &nested_manifest),
        ("40000", "src", &nested_src),
    ]);
    let mut entries: Vec<(&str, &str, &str)> = vec![
        ("100644", "Cargo.toml", &manifest_blob),
        ("100755", "run.sh", &script),
        ("40000", "nested", &nested),
        ("40000", "src", &src),
    ];
    entries.extend_from_slice(extra_core_entries);
    entries.sort_by_key(|entry| entry.1);
    let core = objects.tree(&entries);
    let crates = objects.tree(&[("40000", "core", &core)]);
    let workspace_manifest = objects.blob(b"[workspace]\nmembers = [\"crates/core\"]\n");
    let root_tree = objects.tree(&[
        ("40000", "crates", &crates),
        ("100644", "Cargo.toml", &workspace_manifest),
    ]);
    Workspace {
        objects,
        root_tree,
        manifest_blob,
    }
}

struct Run {
    _output: tempfile::TempDir,
    root: std::path::PathBuf,
    outcome: Result<PreparedCargoMaterials, String>,
}

fn run(
    locked: &LockedMaterials,
    material: &crate::materials::git::VerifiedGitMaterial,
    normalized: Option<NormalizedManifest>,
) -> Run {
    let output = temporary();
    let root = output.path().to_owned();
    let package = &locked.cargo.git_packages[0];
    let inputs = [GitVendorInput {
        material,
        name: &package.name,
        version: &package.version,
        normalized,
    }];
    let outcome = locked
        .prepare_cargo_vendor(
            &VendorInputs {
                registry_archives: &Default::default(),
                git: &inputs,
            },
            &root,
            deadline(),
            &cancellation(),
        )
        .map_err(|error| format!("{error:?}"));
    Run {
        _output: output,
        root,
        outcome,
    }
}

fn locked_core() -> LockedMaterials {
    locked(&[], vec![git_package("example-core", "0.4.0")])
}

fn normalized(cargo_toml: &str, orig: &str) -> Option<NormalizedManifest> {
    Some(NormalizedManifest {
        cargo_toml: cargo_toml.as_bytes().to_vec(),
        cargo_toml_orig: orig.as_bytes().to_vec(),
    })
}

fn rejection(run: Run) -> String {
    run.outcome.expect_err("should reject the Git package")
}

#[test]
fn plain_manifest_package_is_vendored_without_derivation() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let result = run(&locked_core(), &material, None);
    let materials = result.outcome.expect("should vendor the plain package");
    let package = result.root.join("vendor/example-core");
    assert_eq!(
        fs::read(package.join("Cargo.toml")).expect("should read manifest"),
        PLAIN.as_bytes(),
        "should keep the committed manifest"
    );
    assert_eq!(
        fs::read(package.join("src/lib.rs")).expect("should read source"),
        b"pub fn core() {}\n",
        "should copy the tree"
    );
    assert_eq!(
        fs::metadata(package.join("run.sh"))
            .expect("should stat script")
            .permissions()
            .mode()
            & 0o7777,
        0o755,
        "should keep the executable bit"
    );
    assert!(
        !package.join("nested").exists(),
        "should skip nested packages"
    );
    assert!(
        !package.join("Cargo.toml.orig").exists(),
        "should not invent an original"
    );
    let checksum: Value = serde_json::from_slice(
        &fs::read(package.join(".cargo-checksum.json")).expect("should read checksums"),
    )
    .expect("should be JSON");
    assert_eq!(
        checksum,
        json!({
            "files": {
                "Cargo.toml": hex::encode(Sha256::digest(PLAIN.as_bytes())),
                "run.sh": hex::encode(Sha256::digest(b"#!/bin/sh\n")),
                "src/lib.rs": hex::encode(Sha256::digest(b"pub fn core() {}\n")),
            },
            "package": null,
        }),
        "should list files and a null package checksum like cargo vendor"
    );
    assert!(
        materials.derived_manifest_digests().is_empty(),
        "should record no derived manifest"
    );
    assert_eq!(
        materials.git_sources().len(),
        1,
        "should list the locked Git source"
    );
    assert_eq!(
        materials.git_sources()[0].key(),
        SELECTOR,
        "should key the source by its selector"
    );
    materials.rehash().expect("should rehash the vendor tree");
}

#[test]
fn workspace_inheritance_requires_a_matching_normalized_manifest() {
    let built = workspace(INHERITING, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let missing = rejection(run(&locked_core(), &material, None));
    assert!(
        missing.contains("normalized manifest is required"),
        "should require normalization: {missing}"
    );

    let accepted = run(
        &locked_core(),
        &material,
        normalized(NORMALIZED, INHERITING),
    );
    let materials = accepted.outcome.expect("should accept the normalized pair");
    let package = accepted.root.join("vendor/example-core");
    assert_eq!(
        fs::read(package.join("Cargo.toml")).expect("should read manifest"),
        NORMALIZED.as_bytes(),
        "should vendor the normalized manifest"
    );
    assert_eq!(
        fs::read(package.join("Cargo.toml.orig")).expect("should read original"),
        INHERITING.as_bytes(),
        "should keep the committed manifest as the original"
    );
    let digest = hex::encode(Sha256::digest(NORMALIZED.as_bytes()));
    assert_eq!(
        materials.derived_manifest_digests().get("example-core"),
        Some(&digest),
        "should record the normalized manifest digest as derived"
    );
    let checksum: Value = serde_json::from_slice(
        &fs::read(package.join(".cargo-checksum.json")).expect("should read checksums"),
    )
    .expect("should be JSON");
    assert_eq!(
        checksum["files"]["Cargo.toml"],
        json!(digest),
        "should checksum the normalized manifest"
    );
    assert_eq!(
        checksum["files"]["Cargo.toml.orig"],
        json!(hex::encode(Sha256::digest(INHERITING.as_bytes()))),
        "should checksum the original"
    );
}

#[test]
fn normalized_manifest_must_derive_from_the_verified_blob_and_resolve_inheritance() {
    let built = workspace(INHERITING, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let cases: [(&str, Option<NormalizedManifest>, &str); 5] = [
        (
            "forged original",
            normalized(NORMALIZED, "[package]\nname = \"example-core\"\n"),
            "differs from the verified tree",
        ),
        (
            "other name",
            normalized(
                &NORMALIZED.replace("example-core", "example-other"),
                INHERITING,
            ),
            "does not declare package",
        ),
        (
            "other version",
            normalized(&NORMALIZED.replace("0.4.0", "0.4.1"), INHERITING),
            "does not declare package",
        ),
        (
            "still inheriting",
            normalized(INHERITING, INHERITING),
            "does not declare package",
        ),
        (
            "inherited dependency",
            normalized(
                &format!("{NORMALIZED}[dependencies]\nexample-dep = {{ workspace = true }}\n"),
                INHERITING,
            ),
            "still inherits",
        ),
    ];
    for (label, pair, expected) in cases {
        let message = rejection(run(&locked_core(), &material, pair));
        assert!(
            message.contains(expected),
            "should reject {label}: {message}"
        );
    }
}

#[test]
fn package_identity_and_root_must_be_unambiguous() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let other_version = locked(&[], vec![git_package("example-core", "0.5.0")]);
    assert!(
        rejection(run(&other_version, &material, None)).contains("declares example-core 0.4.0"),
        "should reject a manifest whose version differs from the lock"
    );
    let unknown = locked(&[], vec![git_package("example-missing", "0.4.0")]);
    assert!(
        rejection(run(&unknown, &material, None)).contains("no Git directory declares"),
        "should reject a package no directory declares"
    );

    let objects = Objects::new();
    let blob = objects.blob(PLAIN.as_bytes());
    let first = objects.tree(&[("100644", "Cargo.toml", &blob)]);
    let second = objects.tree(&[("100644", "Cargo.toml", &blob)]);
    let root = objects.tree(&[("40000", "a", &first), ("40000", "b", &second)]);
    let material = objects.material(SELECTOR, REVISION, &root);
    assert!(
        rejection(run(&locked_core(), &material, None)).contains("ambiguous Git package roots"),
        "should reject two directories declaring the package"
    );
}

#[test]
fn tree_entries_that_would_alias_the_manifest_or_escape_are_rejected() {
    let objects = Objects::new();
    let blob = objects.blob(PLAIN.as_bytes());
    let orig = objects.blob(b"committed orig\n");
    let tree = objects.tree(&[
        ("100644", "Cargo.toml", &blob),
        ("100644", "Cargo.toml.orig", &orig),
    ]);
    let material = objects.material(SELECTOR, REVISION, &tree);
    let conflict = rejection(run(
        &locked_core(),
        &material,
        normalized(NORMALIZED, PLAIN),
    ));
    assert!(
        conflict.contains("repeated"),
        "should reject a committed Cargo.toml.orig beside a normalized pair: {conflict}"
    );

    for (label, mode, name) in [
        ("symlink", "120000", "link"),
        ("submodule", "160000", "module"),
        ("group-writable file", "100664", "odd"),
        ("dotdot name", "100644", ".."),
        ("backslash name", "100644", "a\\b"),
        ("git name", "100644", ".GIT"),
    ] {
        let objects = Objects::new();
        let manifest = objects.blob(PLAIN.as_bytes());
        let extra = objects.blob(b"x");
        let tree = objects.tree(&[("100644", "Cargo.toml", &manifest), (mode, name, &extra)]);
        let material = objects.material(SELECTOR, REVISION, &tree);
        assert!(
            run(&locked_core(), &material, None).outcome.is_err(),
            "should reject a {label} entry"
        );
    }
}

#[test]
fn git_inputs_must_be_exactly_the_locked_bound_closures() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let cases: [(&str, GitPackage); 3] = [
        (
            "selector",
            GitPackage {
                selector: "git+https://example.com/org/other?tag=v1".to_owned(),
                ..git_package("example-core", "0.4.0")
            },
        ),
        (
            "revision",
            GitPackage {
                revision: "4".repeat(40),
                ..git_package("example-core", "0.4.0")
            },
        ),
        (
            "tag",
            GitPackage {
                selector: "git+https://example.com/org/workspace?tag=v2".to_owned(),
                ..git_package("example-core", "0.4.0")
            },
        ),
    ];
    for (label, package) in cases {
        let locked = locked(&[], vec![package]);
        assert!(
            rejection(run(&locked, &material, None)).contains("not bound to its locked source"),
            "should reject a closure with another {label}"
        );
    }

    let output = temporary();
    let locked = locked_core();
    let none: [GitVendorInput<'_>; 0] = [];
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &Default::default(),
                    git: &none
                },
                output.path(),
                deadline(),
                &cancellation()
            )
            .expect_err("should reject a missing closure")
            .to_string()
            .contains("Original release material"),
        "should reject a locked Git package without a closure"
    );
    let twice = [
        GitVendorInput {
            material: &material,
            name: "example-core",
            version: "0.4.0",
            normalized: None,
        },
        GitVendorInput {
            material: &material,
            name: "example-core",
            version: "0.4.0",
            normalized: None,
        },
    ];
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &Default::default(),
                    git: &twice
                },
                output.path(),
                deadline(),
                &cancellation()
            )
            .is_err(),
        "should reject an extra closure"
    );
}

#[test]
fn store_objects_are_rehashed_before_use() {
    let built = workspace(PLAIN, &[]);
    let store = built.objects.store.path().to_owned();
    let blob = &built.manifest_blob;
    let object = store.join(&blob[..2]).join(&blob[2..]);
    let forged_store = Objects::new();
    let forged = forged_store
        .blob(b"[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2018\"\n");
    let other = forged_store
        .store
        .path()
        .join(&forged[..2])
        .join(&forged[2..]);
    fs::copy(&other, &object).expect("should swap the committed manifest object for forged bytes");
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let message = rejection(run(&locked_core(), &material, None));
    assert!(
        message.contains("do not match their identity"),
        "should reject an edited store object: {message}"
    );

    let built = workspace(PLAIN, &[]);
    let lib = built
        .objects
        .store
        .path()
        .read_dir()
        .expect("should list store")
        .count();
    assert!(lib > 3, "should have written several objects");
    let missing_root = built.objects.material(SELECTOR, REVISION, &"5".repeat(40));
    assert!(
        rejection(run(&locked_core(), &missing_root, None)).contains("cannot be opened"),
        "should reject a root tree absent from the store"
    );
}

#[test]
fn git_package_directory_is_its_bare_name_and_never_shared() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let accepted = run(&locked_core(), &material, None);
    accepted.outcome.expect("should vendor the Git package");
    assert!(
        accepted
            .root
            .join("vendor/example-core/Cargo.toml")
            .is_file()
            && !accepted.root.join("vendor/example-core-0.4.0").exists(),
        "should name the Git package directory after the package alone"
    );

    let registry_crate = plain_crate("example", "core");
    let locked = locked(
        &[("example", "core", &registry_crate)],
        vec![git_package("example-core", "0.4.0")],
    );
    let archive_directory = temporary();
    let registry = archives(
        archive_directory.path(),
        &[("example", "core", &registry_crate)],
    );
    let output = temporary();
    let inputs = [GitVendorInput {
        material: &material,
        name: "example-core",
        version: "0.4.0",
        normalized: None,
    }];
    let error = locked
        .prepare_cargo_vendor(
            &VendorInputs {
                registry_archives: &registry,
                git: &inputs,
            },
            output.path(),
            deadline(),
            &cancellation(),
        )
        .expect_err("should refuse a Git directory that a registry crate already holds");
    assert!(
        format!("{error:?}").contains("cannot be created exclusively"),
        "should fail closed on the shared directory: {error:?}"
    );
}

#[test]
fn git_packages_sharing_a_name_fail_closed() {
    let built = workspace(INHERITING, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let locked = locked(
        &[],
        vec![
            git_package("example-core", "0.4.0"),
            git_package("example-core", "0.5.0"),
        ],
    );
    let inputs = ["0.4.0", "0.5.0"].map(|version| GitVendorInput {
        material: &material,
        name: "example-core",
        version,
        normalized: normalized(&NORMALIZED.replace("0.4.0", version), INHERITING),
    });
    let output = temporary();
    let error = locked
        .prepare_cargo_vendor(
            &VendorInputs {
                registry_archives: &Default::default(),
                git: &inputs,
            },
            output.path(),
            deadline(),
            &cancellation(),
        )
        .expect_err("should refuse a second Git package in the same bare-name directory");
    assert!(
        format!("{error:?}").contains("cannot be created exclusively"),
        "should fail closed on the shared directory: {error:?}"
    );
}
