use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::git_tree::{PackageRoot, Store, ancestor_manifests};
use super::support::*;
use crate::materials::locked::GitPackage;
use crate::materials::{GitVendorInput, LockedMaterials, PreparedCargoMaterials, VendorInputs};

const INHERITING: &str = "[package]\nname = \"example-core\"\nversion.workspace = true\nedition.workspace = true\n\n[dependencies]\nexample-dep = { workspace = true, features = [\"extra\"] }\n";
const PLAIN: &str = "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2021\"\n";
const WORKSPACE_ROOT: &str = "[workspace]\nmembers = [\"crates/core\"]\n\n[workspace.package]\nversion = \"0.4.0\"\nedition = \"2021\"\n\n[workspace.dependencies]\nexample-dep = { version = \"1\", features = [\"base\"] }\n";

/// A fictional workspace: root manifest plus `crates/core` and a nested package.
struct Workspace {
    objects: Objects,
    root_tree: String,
    manifest_blob: String,
}

fn workspace(core_manifest: &str, extra_core_files: &[(&str, &[u8])]) -> Workspace {
    workspace_with_root(core_manifest, Some(WORKSPACE_ROOT), extra_core_files)
}

fn workspace_with_root(
    core_manifest: &str,
    workspace_manifest: Option<&str>,
    extra_core_files: &[(&str, &[u8])],
) -> Workspace {
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
    let extra_blobs: Vec<(&str, String)> = extra_core_files
        .iter()
        .map(|(name, bytes)| (*name, objects.blob(bytes)))
        .collect();
    let mut entries: Vec<(&str, &str, &str)> = vec![
        ("100644", "Cargo.toml", &manifest_blob),
        ("100755", "run.sh", &script),
        ("40000", "nested", &nested),
        ("40000", "src", &src),
    ];
    entries.extend(
        extra_blobs
            .iter()
            .map(|(name, oid)| ("100644", *name, oid.as_str())),
    );
    entries.sort_by_key(|entry| entry.1);
    let core = objects.tree(&entries);
    let crates = objects.tree(&[("40000", "core", &core)]);
    let root_blob = workspace_manifest.map(|text| objects.blob(text.as_bytes()));
    let mut root_entries = vec![("40000", "crates", crates.as_str())];
    if let Some(blob) = &root_blob {
        root_entries.insert(0, ("100644", "Cargo.toml", blob.as_str()));
    }
    let root_tree = objects.tree(&root_entries);
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

fn run(locked: &LockedMaterials, material: &crate::materials::git::VerifiedGitMaterial) -> Run {
    let output = temporary();
    let root = output.path().to_owned();
    let package = &locked.cargo.git_packages[0];
    let inputs = [GitVendorInput {
        material,
        name: &package.name,
        version: &package.version,
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

fn documentation_links(readme_target: &[u8]) -> Workspace {
    let objects = Objects::new();
    let manifest_blob = objects.blob(PLAIN.as_bytes());
    let license = objects.blob(b"example license bytes\n");
    let readme = objects.blob(b"example readme bytes\n");
    let license_link = objects.blob(b"../LICENSE");
    let readme_link = objects.blob(readme_target);
    let indirect = objects.blob(b"README.md");
    let directory = objects.tree(&[]);
    let package = objects.tree(&[
        ("100644", "Cargo.toml", &manifest_blob),
        ("120000", "LICENSE", &license_link),
        ("120000", "README.md", &readme_link),
    ]);
    let root_tree = objects.tree(&[
        ("100644", "LICENSE", &license),
        ("100755", "README.md", &readme),
        ("120000", "alias", &indirect),
        ("40000", "directory", &directory),
        ("40000", "package", &package),
    ]);
    Workspace {
        objects,
        root_tree,
        manifest_blob,
    }
}

#[test]
fn git_documentation_links_copy_verified_parent_files_as_regular_files() {
    let built = documentation_links(b"../README.md");
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let result = run(&locked_core(), &material);
    result
        .outcome
        .expect("should vendor direct documentation file links");
    let package = result.root.join("vendor/example-core");
    for (name, expected, mode) in [
        ("LICENSE", &b"example license bytes\n"[..], 0o644),
        ("README.md", &b"example readme bytes\n"[..], 0o755),
    ] {
        let path = package.join(name);
        assert!(
            fs::symlink_metadata(&path)
                .expect("should inspect copied file")
                .is_file(),
            "should write a regular file"
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("should inspect copied permissions")
                .permissions()
                .mode()
                & 0o7777,
            mode,
            "should preserve the target executable mode"
        );
        assert_eq!(
            fs::read(path).expect("should read copied file"),
            expected,
            "should copy the authenticated target bytes"
        );
    }
}

#[test]
fn git_documentation_links_reject_unsafe_or_indirect_targets() {
    for target in [
        &b"../../outside"[..],
        b"/outside",
        b"../missing",
        b"../alias",
        b"../directory",
        b"../README.md\0",
        b"..\\README.md",
        b"../README.md\n",
        b"../missing/../README.md",
        b"../alias/README.md",
        b"../README.md/.",
        b"../directory/..",
    ] {
        let built = documentation_links(target);
        let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
        assert!(
            run(&locked_core(), &material).outcome.is_err(),
            "should reject an unsafe or indirect file link"
        );
    }
}

#[test]
fn git_manifest_links_are_rejected_during_package_discovery() {
    let objects = Objects::new();
    let manifest = objects.blob(PLAIN.as_bytes());
    let target = objects.blob(b"manifest.toml");
    let root = objects.tree(&[
        ("120000", "Cargo.toml", &target),
        ("100644", "manifest.toml", &manifest),
    ]);
    let material = objects.material(SELECTOR, REVISION, &root);
    assert!(
        rejection(run(&locked_core(), &material)).contains("manifest aliases"),
        "should reject an aliased manifest before using its bytes"
    );
}

#[test]
fn git_file_link_targets_are_rehashed_before_copying() {
    let built = documentation_links(b"../README.md");
    let target = built.objects.blob(b"example readme bytes\n");
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    fs::write(
        material
            .object_store()
            .join(&target[..2])
            .join(&target[2..]),
        b"changed object bytes",
    )
    .expect("should corrupt the target object");
    assert!(
        run(&locked_core(), &material).outcome.is_err(),
        "should reject a target changed after object verification"
    );
}

fn rejection(run: Run) -> String {
    run.outcome.expect_err("should reject the Git package")
}

#[test]
fn plain_manifest_package_is_vendored_without_derivation() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let result = run(&locked_core(), &material);
    let materials = result.outcome.expect("should vendor the plain package");
    let index = serde_json::to_value(materials.packages()).expect("should encode Git index");
    let identity = &index["example-core"];
    assert_eq!(
        identity["source"],
        format!("{SELECTOR}#{REVISION}"),
        "should preserve the complete locked source"
    );
    assert_eq!(
        identity["origin"]["kind"], "git",
        "should identify verified Git originals"
    );
    assert_eq!(
        identity["origin"]["repository_tree"], built.root_tree,
        "should bind verified repository tree"
    );
    assert_eq!(
        identity["origin"]["committed_manifest_blob"], built.manifest_blob,
        "should retain the exact committed manifest blob"
    );
    assert_eq!(
        identity["origin"]["committed_manifest_sha256"],
        hex::encode(Sha256::digest(PLAIN.as_bytes())),
        "should bind original manifest bytes"
    );
    assert_eq!(
        identity["origin"]["package_path"], "crates/core",
        "should bind package location within repository"
    );
    assert!(
        identity["origin"]["normalized_manifest_sha256"].is_null(),
        "should not invent normalization for a plain manifest"
    );
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
        materials.normalized_manifest_digests().is_empty()
            && materials.derived_manifest_digests().is_empty(),
        "should record no normalized or derived manifest"
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
fn workspace_inheritance_is_normalized_by_the_producer_from_the_verified_tree() {
    let built = workspace(INHERITING, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let accepted = run(&locked_core(), &material);
    let materials = accepted
        .outcome
        .expect("should normalize the inheriting manifest itself");
    let package = accepted.root.join("vendor/example-core");
    let written = fs::read(package.join("Cargo.toml")).expect("should read manifest");
    let parsed: toml::Table =
        toml::from_str(std::str::from_utf8(&written).expect("should write a UTF-8 manifest"))
            .expect("should write TOML");
    let expected: toml::Table = toml::from_str(
        "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2021\"\n\n[dependencies.example-dep]\nversion = \"1\"\nfeatures = [\"base\", \"extra\"]\n",
    )
    .expect("should parse the expected manifest");
    assert_eq!(
        parsed, expected,
        "should resolve inherited fields and dependencies from the workspace root"
    );
    assert_eq!(
        fs::read(package.join("Cargo.toml.orig")).expect("should read original"),
        INHERITING.as_bytes(),
        "should keep the committed manifest as the original"
    );
    let digest = hex::encode(Sha256::digest(&written));
    let index = serde_json::to_value(materials.packages()).expect("should encode normalized index");
    let identity = &index["example-core"];
    assert_eq!(
        identity["prepared_manifest_sha256"], digest,
        "should identify the prepared normalized manifest"
    );
    assert_eq!(
        identity["origin"]["normalized_manifest_sha256"], digest,
        "should retain reviewed normalization identity"
    );
    assert_eq!(
        identity["origin"]["committed_manifest_sha256"],
        hex::encode(Sha256::digest(INHERITING.as_bytes())),
        "should retain distinct original bytes"
    );
    assert_eq!(
        materials.normalized_manifest_digests().get("example-core"),
        Some(&digest),
        "should record the producer-normalized manifest digest"
    );
    assert!(
        materials.derived_manifest_digests().is_empty(),
        "should record no caller-derived manifest"
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

    let again = run(&locked_core(), &material);
    again.outcome.expect("should normalize again");
    assert_eq!(
        fs::read(again.root.join("vendor/example-core/Cargo.toml"))
            .expect("should read the second manifest"),
        written,
        "should normalize to identical bytes every time"
    );
}

#[test]
fn unresolvable_workspace_inheritance_fails_closed() {
    let cases: [(&str, Workspace, &str, &str); 4] = [
        (
            "no workspace root in the tree",
            workspace_with_root(INHERITING, None, &[]),
            "0.4.0",
            "no workspace root encloses it",
        ),
        (
            "root without the inherited dependency",
            workspace_with_root(
                INHERITING,
                Some(
                    "[workspace]\n\n[workspace.package]\nversion = \"0.4.0\"\nedition = \"2021\"\n",
                ),
                &[],
            ),
            "0.4.0",
            "which is not defined",
        ),
        (
            "inherited version other than the lock",
            workspace(INHERITING, &[]),
            "0.5.0",
            "does not declare package example-core 0.5.0",
        ),
        (
            "member patch section",
            workspace(
                &format!("{INHERITING}\n[patch.crates-io]\nexample-dep = {{ path = \"x\" }}\n"),
                &[],
            ),
            "0.4.0",
            "declares [patch]",
        ),
    ];
    for (label, built, version, expected) in cases {
        let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
        let locked = locked(&[], vec![git_package("example-core", version)]);
        let message = rejection(run(&locked, &material));
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
        rejection(run(&other_version, &material)).contains("declares example-core 0.4.0"),
        "should reject a manifest whose version differs from the lock"
    );
    let unknown = locked(&[], vec![git_package("example-missing", "0.4.0")]);
    assert!(
        rejection(run(&unknown, &material)).contains("no Git directory declares"),
        "should reject a package no directory declares"
    );

    let objects = Objects::new();
    let blob = objects.blob(PLAIN.as_bytes());
    let first = objects.tree(&[("100644", "Cargo.toml", &blob)]);
    let second = objects.tree(&[("100644", "Cargo.toml", &blob)]);
    let root = objects.tree(&[("40000", "a", &first), ("40000", "b", &second)]);
    let material = objects.material(SELECTOR, REVISION, &root);
    assert!(
        rejection(run(&locked_core(), &material)).contains("ambiguous Git package roots"),
        "should reject two directories declaring the package"
    );
}

#[test]
fn tree_entries_that_would_alias_the_manifest_or_escape_are_rejected() {
    let built = workspace(INHERITING, &[("Cargo.toml.orig", b"committed orig\n")]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let conflict = rejection(run(&locked_core(), &material));
    assert!(
        conflict.contains("repeated"),
        "should reject a committed Cargo.toml.orig beside a normalized manifest: {conflict}"
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
            run(&locked_core(), &material).outcome.is_err(),
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
            rejection(run(&locked, &material)).contains("not bound to its locked source"),
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
        },
        GitVendorInput {
            material: &material,
            name: "example-core",
            version: "0.4.0",
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
    let message = rejection(run(&locked_core(), &material));
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
        rejection(run(&locked_core(), &missing_root)).contains("cannot be opened"),
        "should reject a root tree absent from the store"
    );
}

#[test]
fn git_package_directory_is_its_bare_name_and_never_shared() {
    let built = workspace(PLAIN, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let accepted = run(&locked_core(), &material);
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
    let versions = ["0.4.0", "0.5.0"];
    let materials = versions.map(|version| {
        let root = WORKSPACE_ROOT.replace("0.4.0", version);
        let built = workspace_with_root(INHERITING, Some(&root), &[]);
        built.objects.material(SELECTOR, REVISION, &built.root_tree)
    });
    let locked = locked(
        &[],
        versions
            .iter()
            .map(|version| git_package("example-core", version))
            .collect(),
    );
    let inputs: Vec<GitVendorInput<'_>> = versions
        .iter()
        .zip(&materials)
        .map(|(version, material)| GitVendorInput {
            material,
            name: "example-core",
            version,
        })
        .collect();
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

#[test]
fn ancestor_walk_reads_every_manifest_above_the_package_nearest_first() {
    let objects = Objects::new();
    let core_manifest = objects.blob(INHERITING.as_bytes());
    let core = objects.tree(&[("100644", "Cargo.toml", &core_manifest)]);
    let umbrella = b"[package]\nname = \"example-umbrella\"\nversion = \"9.9.9\"\n";
    let umbrella_manifest = objects.blob(umbrella);
    let crates = objects.tree(&[
        ("100644", "Cargo.toml", &umbrella_manifest),
        ("40000", "core", &core),
    ]);
    let root_manifest = objects.blob(WORKSPACE_ROOT.as_bytes());
    let root_tree = objects.tree(&[
        ("100644", "Cargo.toml", &root_manifest),
        ("40000", "crates", &crates),
    ]);
    let store = Store::new(objects.store.path());
    let package = PackageRoot {
        tree: core.clone(),
        manifest: core_manifest.clone(),
        path: "crates/core".to_owned(),
    };
    let found = ancestor_manifests(&store, &root_tree, &package)
        .expect("should walk the verified package path");
    let seen: Vec<(&str, &[u8])> = found
        .iter()
        .map(|ancestor| (ancestor.directory.as_str(), ancestor.bytes.as_slice()))
        .collect();
    assert_eq!(
        seen,
        [
            ("crates", umbrella.as_slice()),
            ("", WORKSPACE_ROOT.as_bytes()),
        ],
        "should list the intermediate package manifest before the workspace root"
    );

    let elsewhere = PackageRoot {
        tree: crates.clone(),
        ..package.clone()
    };
    let wrong_tree = ancestor_manifests(&store, &root_tree, &elsewhere)
        .expect_err("should refuse a path that leads to another tree");
    assert!(
        format!("{wrong_tree:?}").contains("does not lead to the package tree"),
        "should report the mismatched package tree: {wrong_tree:?}"
    );
    let absent = PackageRoot {
        path: "crates/absent".to_owned(),
        ..package
    };
    let missing = ancestor_manifests(&store, &root_tree, &absent)
        .expect_err("should refuse a path absent from the tree");
    assert!(
        format!("{missing:?}").contains("is not a tree in the verified closure"),
        "should report the missing directory: {missing:?}"
    );
}

#[test]
fn git_targets_that_leave_the_package_are_refused() {
    let escaping = format!("{PLAIN}build = \"../shared/build.rs\"\n");
    let built = workspace(&escaping, &[]);
    let material = built.objects.material(SELECTOR, REVISION, &built.root_tree);
    let message = rejection(run(&locked_core(), &material));
    assert!(
        message.contains("leaves the vendored package directory"),
        "should refuse a build script outside the package: {message}"
    );
}
