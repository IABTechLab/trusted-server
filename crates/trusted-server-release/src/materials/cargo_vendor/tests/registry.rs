use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::support::*;
use crate::materials::{LockedMaterials, PreparedCargoMaterials, VendorInputs};

struct Prepared {
    _archives: tempfile::TempDir,
    _output: tempfile::TempDir,
    outcome: Result<PreparedCargoMaterials, String>,
    root: std::path::PathBuf,
}

fn prepare(locked: &LockedMaterials, entries: &[(&str, &str, &[u8])]) -> Prepared {
    prepare_with(locked, entries, |_| {})
}

/// Prepares from `entries`, letting `tweak` change the output root first.
fn prepare_with(
    locked: &LockedMaterials,
    entries: &[(&str, &str, &[u8])],
    tweak: impl FnOnce(&Path),
) -> Prepared {
    let archive_directory = temporary();
    let output = temporary();
    let registry = archives(archive_directory.path(), entries);
    let root = output.path().to_owned();
    tweak(&root);
    let outcome = locked
        .prepare_cargo_vendor(
            &VendorInputs {
                registry_archives: &registry,
                git: &[],
            },
            &root,
            deadline(),
            &cancellation(),
        )
        .map_err(|error| format!("{error:?}"));
    Prepared {
        _archives: archive_directory,
        _output: output,
        outcome,
        root,
    }
}

fn two_crates() -> (Vec<u8>, Vec<u8>) {
    (
        plain_crate("example-one", "1.0.0"),
        plain_crate("example-two", "0.3.1"),
    )
}

#[test]
fn registry_crates_become_directory_source_packages_with_cargo_checksums() {
    let (one, two) = two_crates();
    let entries: [(&str, &str, &[u8]); 2] = [
        ("example-one", "1.0.0", &one),
        ("example-two", "0.3.1", &two),
    ];
    let locked = locked(&entries, Vec::new());
    let prepared = prepare(&locked, &entries);
    let materials = prepared.outcome.expect("should prepare the vendor tree");
    let vendor = materials.vendor_dir();
    let index = serde_json::to_value(materials.packages()).expect("should encode verified index");
    let identity = &index["example-one-1.0.0"];
    assert_eq!(
        identity["name"], "example-one",
        "should retain locked package name"
    );
    assert_eq!(
        identity["version"], "1.0.0",
        "should retain locked package version"
    );
    assert_eq!(
        identity["origin"]["kind"], "registry",
        "should identify original source class"
    );
    assert_eq!(
        identity["origin"]["archive_sha256"],
        hex::encode(Sha256::digest(&one)),
        "should bind the exact verified original archive"
    );
    assert_eq!(
        identity["prepared_manifest_sha256"],
        hex::encode(Sha256::digest(manifest("example-one", "1.0.0"))),
        "should bind the manifest Cargo actually reads"
    );
    assert_eq!(
        materials.packages().len(),
        2,
        "should index exactly the prepared packages"
    );
    assert_eq!(
        vendor,
        prepared.root.join("vendor"),
        "should place packages under vendor"
    );
    let mut names: Vec<String> = fs::read_dir(vendor)
        .expect("should list vendor")
        .map(|entry| {
            entry
                .expect("should read entry")
                .file_name()
                .into_string()
                .expect("should be UTF-8")
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["example-one-1.0.0", "example-two-0.3.1"],
        "should create one directory per crate"
    );

    let package = vendor.join("example-one-1.0.0");
    let manifest = manifest("example-one", "1.0.0");
    let expected = json!({
        "files": {
            "Cargo.toml": hex::encode(Sha256::digest(&manifest)),
            "build.sh": hex::encode(Sha256::digest(b"#!/bin/sh\n")),
            "src/lib.rs": hex::encode(Sha256::digest(b"pub fn fictional() {}\n")),
        },
        "package": hex::encode(Sha256::digest(&one)),
    });
    let raw = fs::read(package.join(".cargo-checksum.json")).expect("should write checksum file");
    let parsed: Value = serde_json::from_slice(&raw).expect("should write JSON");
    assert_eq!(
        parsed, expected,
        "should list every file and the lock checksum"
    );
    assert!(
        String::from_utf8(raw)
            .expect("should be UTF-8")
            .starts_with("{\"files\":{"),
        "should write files before package like cargo vendor"
    );
    for (relative, mode) in [
        ("Cargo.toml", 0o644),
        ("src/lib.rs", 0o644),
        ("build.sh", 0o755),
    ] {
        let metadata = fs::symlink_metadata(package.join(relative)).expect("should stat file");
        assert_eq!(
            metadata.permissions().mode() & 0o7777,
            mode,
            "should set mode of {relative}"
        );
        assert_eq!(metadata.nlink(), 1, "should write unlinked {relative}");
    }
    assert_eq!(materials.digest().len(), 64, "should digest the whole tree");
    assert_eq!(
        materials.source_commit(),
        SOURCE_COMMIT,
        "should bind the approved source"
    );
    assert_eq!(
        materials.cargo_lock_sha256(),
        LOCK_SHA256,
        "should bind the lock digest"
    );
    assert!(
        materials.git_sources().is_empty(),
        "should list no Git sources"
    );
    materials
        .rehash()
        .expect("should rehash the untouched tree");
}

#[test]
fn preparation_is_deterministic_and_content_sensitive() {
    let (one, two) = two_crates();
    let entries: [(&str, &str, &[u8]); 2] = [
        ("example-one", "1.0.0", &one),
        ("example-two", "0.3.1", &two),
    ];
    let locked_a = locked(&entries, Vec::new());
    let first = prepare(&locked_a, &entries)
        .outcome
        .expect("should prepare");
    let second = prepare(&locked_a, &entries)
        .outcome
        .expect("should prepare again");
    assert_eq!(
        first.digest(),
        second.digest(),
        "should reproduce the digest across roots"
    );
    assert_eq!(
        serde_json::to_value(first.packages()).expect("should encode first index"),
        serde_json::to_value(second.packages()).expect("should encode second index"),
        "should keep original identities independent of staging paths"
    );

    let changed = plain_crate_with("example-one", "1.0.0", b"pub fn other() {}\n");
    let changed_entries: [(&str, &str, &[u8]); 2] = [
        ("example-one", "1.0.0", &changed),
        ("example-two", "0.3.1", &two),
    ];
    let changed_locked = locked(&changed_entries, Vec::new());
    let third = prepare(&changed_locked, &changed_entries)
        .outcome
        .expect("should prepare changed");
    assert_ne!(
        first.digest(),
        third.digest(),
        "should change the digest with content"
    );
    assert_ne!(
        serde_json::to_value(first.packages()).expect("should encode original index"),
        serde_json::to_value(third.packages()).expect("should encode changed index"),
        "should bind changed original archives even when package names and versions match"
    );
}

fn plain_crate_with(name: &str, version: &str, source: &[u8]) -> Vec<u8> {
    crate_archive(
        name,
        version,
        &[
            ("Cargo.toml", &manifest(name, version), 0o644),
            ("src/lib.rs", source, 0o644),
        ],
    )
}

#[test]
fn rehash_detects_edits_additions_removals_and_extra_root_entries() {
    let one = plain_crate("example-one", "1.0.0");
    let entries: [(&str, &str, &[u8]); 1] = [("example-one", "1.0.0", &one)];
    let locked = locked(&entries, Vec::new());
    let package = |root: &Path| root.join("vendor/example-one-1.0.0");
    type Tamper = fn(&Path);
    let tampers: [(&str, Tamper); 6] = [
        ("edit", |root| {
            let path = root.join("vendor/example-one-1.0.0/src/lib.rs");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("should chmod");
            fs::write(path, b"tampered").expect("should edit");
        }),
        ("add", |root| {
            fs::write(root.join("vendor/example-one-1.0.0/extra.rs"), b"x").expect("should add");
        }),
        ("remove", |root| {
            fs::remove_file(root.join("vendor/example-one-1.0.0/src/lib.rs"))
                .expect("should remove");
        }),
        ("chmod", |root| {
            fs::set_permissions(
                root.join("vendor/example-one-1.0.0/build.sh"),
                fs::Permissions::from_mode(0o644),
            )
            .expect("should chmod");
        }),
        ("extra root entry", |root| {
            fs::create_dir(root.join("cargo-home")).expect("should add root entry");
        }),
        ("hard link", |root| {
            fs::hard_link(
                root.join("vendor/example-one-1.0.0/src/lib.rs"),
                root.join("vendor/example-one-1.0.0/linked.rs"),
            )
            .expect("should link");
        }),
    ];
    for (label, tamper) in tampers {
        let prepared = prepare(&locked, &entries);
        let materials = prepared.outcome.expect("should prepare");
        assert!(
            package(&prepared.root).is_dir(),
            "should create the package directory"
        );
        tamper(&prepared.root);
        assert!(
            materials.rehash().is_err(),
            "should reject {label} after preparation"
        );
    }
}

#[test]
fn registry_inputs_must_be_exactly_the_locked_crates() {
    let (one, two) = two_crates();
    let locked_both = locked(
        &[
            ("example-one", "1.0.0", &one),
            ("example-two", "0.3.1", &two),
        ],
        Vec::new(),
    );
    let missing = prepare(&locked_both, &[("example-one", "1.0.0", &one)]);
    assert!(
        missing
            .outcome
            .expect_err("should reject a missing original")
            .contains("exactly the locked"),
        "should reject a missing archive"
    );
    let locked_one = locked(&[("example-one", "1.0.0", &one)], Vec::new());
    let extra = prepare(
        &locked_one,
        &[
            ("example-one", "1.0.0", &one),
            ("example-two", "0.3.1", &two),
        ],
    );
    assert!(
        extra
            .outcome
            .expect_err("should reject an extra original")
            .contains("exactly the locked"),
        "should reject an unlocked archive"
    );
    let wrong_version = prepare(&locked_one, &[("example-one", "1.0.1", &one)]);
    assert!(
        wrong_version.outcome.is_err(),
        "should reject an archive for another version"
    );
}

#[test]
fn archive_bytes_must_match_the_locked_checksum() {
    let one = plain_crate("example-one", "1.0.0");
    let entries: [(&str, &str, &[u8]); 1] = [("example-one", "1.0.0", &one)];
    let locked = locked(&entries, Vec::new());
    let other = plain_crate_with("example-one", "1.0.0", b"different\n");
    let prepared = prepare(&locked, &[("example-one", "1.0.0", &other)]);
    assert!(
        prepared.outcome.is_err(),
        "should reject bytes that do not match the lock checksum"
    );
}

#[test]
fn manifests_must_declare_the_locked_identity_and_nothing_reserved() {
    let cases: [(&str, Vec<u8>); 5] = [
        (
            "other name",
            crate_archive(
                "example-one",
                "1.0.0",
                &[("Cargo.toml", &manifest("example-other", "1.0.0"), 0o644)],
            ),
        ),
        (
            "other version",
            crate_archive(
                "example-one",
                "1.0.0",
                &[("Cargo.toml", &manifest("example-one", "1.0.1"), 0o644)],
            ),
        ),
        (
            "missing manifest",
            crate_archive("example-one", "1.0.0", &[("src/lib.rs", b"x", 0o644)]),
        ),
        (
            "unparsable manifest",
            crate_archive(
                "example-one",
                "1.0.0",
                &[("Cargo.toml", b"[package", 0o644)],
            ),
        ),
        (
            "reserved checksum file",
            crate_archive(
                "example-one",
                "1.0.0",
                &[
                    ("Cargo.toml", &manifest("example-one", "1.0.0"), 0o644),
                    (".cargo-checksum.json", b"{}", 0o644),
                ],
            ),
        ),
    ];
    for (label, bytes) in cases {
        let entries: [(&str, &str, &[u8]); 1] = [("example-one", "1.0.0", &bytes)];
        let locked = locked(&entries, Vec::new());
        assert!(
            prepare(&locked, &entries).outcome.is_err(),
            "should reject a crate with {label}"
        );
    }
}

#[test]
fn output_root_must_be_a_fresh_real_directory() {
    let one = plain_crate("example-one", "1.0.0");
    let entries: [(&str, &str, &[u8]); 1] = [("example-one", "1.0.0", &one)];
    let locked = locked(&entries, Vec::new());
    let populated = prepare_with(&locked, &entries, |root| {
        fs::write(root.join("leftover"), b"x").expect("should populate root");
    });
    assert!(
        populated
            .outcome
            .expect_err("should reject a populated root")
            .contains("already has entries"),
        "should reject a root with prior entries"
    );
    let archive_directory = temporary();
    let registry = archives(archive_directory.path(), &entries);
    let real = archive_directory.path().join("real-root");
    fs::create_dir(&real).expect("should create real root");
    let alias = archive_directory.path().join("alias-root");
    std::os::unix::fs::symlink(&real, &alias).expect("should alias root");
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &registry,
                    git: &[]
                },
                &alias,
                deadline(),
                &cancellation(),
            )
            .expect_err("should reject a symlinked root")
            .to_string()
            .contains("Original release material"),
        "should reject a symlinked root as a material error"
    );
    let missing = archive_directory.path().join("missing-root");
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &registry,
                    git: &[]
                },
                &missing,
                deadline(),
                &cancellation(),
            )
            .is_err(),
        "should reject a root that does not exist"
    );
    let file = archive_directory.path().join("a-file");
    fs::write(&file, b"x").expect("should write file");
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &registry,
                    git: &[]
                },
                &file,
                deadline(),
                &cancellation(),
            )
            .is_err(),
        "should reject a root that is a file"
    );
}

#[test]
fn cancelled_or_expired_attempt_prepares_nothing() {
    let one = plain_crate("example-one", "1.0.0");
    let entries: [(&str, &str, &[u8]); 1] = [("example-one", "1.0.0", &one)];
    let locked = locked(&entries, Vec::new());
    let archive_directory = temporary();
    let registry = archives(archive_directory.path(), &entries);
    let output = temporary();
    let token = cancellation();
    token.cancel();
    assert!(
        locked
            .prepare_cargo_vendor(
                &VendorInputs {
                    registry_archives: &registry,
                    git: &[]
                },
                output.path(),
                deadline(),
                &token,
            )
            .is_err(),
        "should reject a cancelled attempt"
    );
    assert!(
        fs::read_dir(output.path())
            .expect("should list root")
            .next()
            .is_none(),
        "should write nothing for a cancelled attempt"
    );
}
