use crate::process::Cancellation;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::Sha512;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

use super::*;

fn compatibility_original(root: &str, local_pax: bool) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    if local_pax {
        let mut body = String::new();
        for assignment in ["path=package/file", "size=1", "NODETAR.type=File"] {
            let mut length = assignment.len() + 3;
            loop {
                let record = format!("{length} {assignment}\n");
                if record.len() == length {
                    body.push_str(&record);
                    break;
                }
                length = record.len();
            }
        }
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::XHeader);
        header.set_size(body.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "PaxHeader/package/file", body.as_bytes())
            .expect("should append fictional local metadata");
    }
    let mut header = tar::Header::new_ustar();
    header.set_mode(0o644);
    header.set_size(1);
    header.set_cksum();
    builder
        .append_data(&mut header, format!("{root}/file"), b"x".as_slice())
        .expect("should append fictional regular member");
    builder
        .into_inner()
        .expect("should finish fictional TAR")
        .finish()
        .expect("should finish fictional gzip")
}

#[test]
fn legacy_root_and_pax_compatibility_require_exact_locked_versions() {
    for (name, accepted, rejected, root, pax) in [
        ("@types/chai", "5.2.3", "5.2.4", "chai", false),
        ("@types/node", "24.10.13", "24.10.14", "node v24.10", false),
        ("gulp-wrap", "0.15.0", "0.15.1", "package", true),
    ] {
        let bytes = compatibility_original(root, pax);
        let directory = tempfile::tempdir().expect("should own fictional distribution");
        let path = directory.path().join("original.tgz");
        std::fs::write(&path, &bytes).expect("should retain fictional original");
        for (version, allowed) in [(accepted, true), (rejected, false)] {
            let location = format!("node_modules/{name}");
            let locked = LockedMaterials {
                source_commit: "a".repeat(40),
                cargo_lock_sha256: "b".repeat(64),
                npm_lock_sha256: "c".repeat(64),
                cargo: CargoMaterials {
                    registry: BTreeMap::new(),
                    git: Default::default(),
                    git_packages: Vec::new(),
                },
                npm: BTreeMap::from([(
                    location.clone(),
                    NpmMaterial {
                        name: name.to_owned(),
                        version: version.to_owned(),
                        resolved: "https://example.com/fixture.tgz".to_owned(),
                        sha512: Sha512::digest(&bytes).into(),
                    },
                )]),
            };
            let original = locked
                .verify_npm(
                    &location,
                    &path,
                    Instant::now() + Duration::from_secs(10),
                    &Cancellation::default(),
                )
                .expect("should authenticate exact fictional bytes before selection");
            assert_eq!(
                locked.extract_npm(&location, original).is_ok(),
                allowed,
                "should restrict legacy extraction to reviewed package/version: {name}@{version}"
            );
        }
    }
}

fn npm_lock(integrity: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "lockfileVersion": 3,
        "packages": {
            "": {"name": "fictional-project"},
            "node_modules/fictional-package": {
                "version": "1.0.0",
                "resolved": format!("{}fictional-package/-/fictional-package-1.0.0.tgz", locked::NPM_REGISTRY),
                "integrity": integrity,
                "optional": true
            }
        }
    })).expect("should encode fictional lockfile")
}

#[test]
fn npm_integrity_requires_one_canonical_sha512_identity() {
    for integrity in [
        "",
        "sha256-AA==",
        "sha512-AA==",
        "sha512-???",
        "sha512-AA== sha512-AA==",
    ] {
        assert!(
            parse_npm(&npm_lock(integrity)).is_err(),
            "should reject missing, ambiguous or unsupported archive integrity"
        );
    }
}

#[test]
fn cargo_git_revision_cannot_be_only_a_tag() {
    let lock = b"version = 4\n[[package]]\nname = \"fictional\"\nversion = \"1.0.0\"\nsource = \"git+https://example.com/fictional?tag=v1\"\n";
    assert!(
        parse_cargo(lock).is_err(),
        "should require a full locked Git revision"
    );
}

#[test]
fn cargo_material_source_cannot_be_mistyped_as_a_local_package() {
    let lock =
        b"version = 4\n[[package]]\nname = \"fictional\"\nversion = \"1.0.0\"\nsource = 42\n";
    assert!(
        parse_cargo(lock).is_err(),
        "should reject malformed acquisition identity"
    );
}

#[test]
fn npm_names_cannot_change_the_registry_path() {
    let integrity = format!("sha512-{}", STANDARD.encode([0_u8; 64]));
    let mut lock: serde_json::Value =
        serde_json::from_slice(&npm_lock(&integrity)).expect("should parse fictional lockfile");
    lock["packages"]["node_modules/fictional-package"]["name"] = json!("../fictional-package");
    lock["packages"]["node_modules/fictional-package"]["resolved"] = json!(format!(
        "{}../fictional-package/-/fictional-package-1.0.0.tgz",
        locked::NPM_REGISTRY
    ));
    assert!(
        parse_npm(&serde_json::to_vec(&lock).expect("should encode fixture")).is_err(),
        "should reject traversal in the declared package identity"
    );
}

#[test]
fn source_lockfiles_account_for_each_supported_acquisition_class() {
    let cargo = parse_cargo(include_bytes!("../../../../Cargo.lock"))
        .expect("should parse the source-controlled Cargo lock");
    assert!(
        !cargo.registry.is_empty(),
        "should retain registry originals"
    );
    assert!(
        !cargo.git.is_empty(),
        "should retain exact Git revisions as unverified expectations"
    );
    let npm = parse_npm(include_bytes!(
        "../../../trusted-server-js/lib/package-lock.json"
    ))
    .expect("should parse the source-controlled npm lock");
    for package in [
        "node_modules/@esbuild/darwin-arm64",
        "node_modules/@esbuild/darwin-x64",
        "node_modules/@esbuild/linux-x64",
    ] {
        assert!(
            npm.contains_key(package),
            "should retain selected native esbuild originals despite optional flags"
        );
    }
}

#[test]
fn optional_npm_original_still_requires_its_exact_bytes() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let original = directory.path().join("original.tgz");
    let bytes = b"fictional original archive bytes";
    std::fs::write(&original, bytes).expect("should create fictional original");
    let packages = parse_npm(&npm_lock(&format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(bytes))
    )))
    .expect("should parse locked archive");
    let deadline = Instant::now() + Duration::from_secs(5);
    let expected = &packages["node_modules/fictional-package"].sha512;
    let mut retained = archive::verify(
        &original,
        archive::Expected::Sha512(expected),
        "fictional-source",
        "npm:node_modules/fictional-package",
        deadline,
        &Cancellation::default(),
    )
    .expect("should verify the exact original");
    let mut changed = bytes.to_vec();
    changed[0] ^= 1;
    std::fs::write(&original, changed).expect("should substitute one byte at the same path");
    assert!(
        archive::verify(
            &original,
            archive::Expected::Sha512(expected),
            "fictional-source",
            "npm:node_modules/fictional-package",
            deadline,
            &Cancellation::default()
        )
        .is_err(),
        "should reject a changed original even when its filename is unchanged"
    );
    let mut copy = Vec::new();
    retained
        .copy_verified(&mut copy)
        .expect("should retain the original verified bytes");
    assert_eq!(
        copy, bytes,
        "should consume the held private bytes rather than reopening the replaced path"
    );
}

#[test]
fn npm_selectors_and_nested_locations_are_not_ambiguous() {
    let integrity = format!("sha512-{}", STANDARD.encode([0_u8; 64]));
    for (field, value) in [
        ("name", json!(42)),
        ("name", json!("another-package")),
        ("link", json!("false")),
    ] {
        let mut lock: serde_json::Value =
            serde_json::from_slice(&npm_lock(&integrity)).expect("should parse fixture");
        lock["packages"]["node_modules/fictional-package"][field] = value;
        assert!(
            parse_npm(&serde_json::to_vec(&lock).expect("should encode fixture")).is_err(),
            "should reject mistyped or conflicting package selectors"
        );
    }
    let mut lock: serde_json::Value =
        serde_json::from_slice(&npm_lock(&integrity)).expect("should parse fixture");
    let package = lock["packages"]
        .as_object_mut()
        .expect("should have packages")
        .remove("node_modules/fictional-package")
        .expect("should have fictional package");
    lock["packages"]["node_modules/not/a/package/node_modules/fictional-package"] = package;
    assert!(
        parse_npm(&serde_json::to_vec(&lock).expect("should encode fixture")).is_err(),
        "should validate every nested installation component"
    );
}

#[test]
fn cancellation_after_verification_prevents_material_consumption() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let original = directory.path().join("original.crate");
    let bytes = b"fictional original";
    std::fs::write(&original, bytes).expect("should create fixture original");
    let cancellation = Cancellation::default();
    let expected = hex::encode(sha2::Sha256::digest(bytes));
    let mut retained = archive::verify(
        &original,
        archive::Expected::Sha256(&expected),
        "fictional-source",
        "crate:fictional-package@1.0.0",
        Instant::now() + Duration::from_secs(5),
        &cancellation,
    )
    .expect("should verify original before cancellation");
    cancellation.cancel();
    let mut destination = Vec::new();
    assert!(
        retained.copy_verified(&mut destination).is_err(),
        "should retain the shared cancellation boundary through later use"
    );
    assert!(
        destination.is_empty(),
        "should not emit cancelled material bytes"
    );
}

#[test]
fn archive_inputs_reject_aliases_special_files_and_oversize() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let regular = directory.path().join("original");
    let alias = directory.path().join("alias");
    let hard = directory.path().join("hard-link");
    let fifo = directory.path().join("fifo");
    let huge = directory.path().join("huge");
    std::fs::write(&regular, b"fictional").expect("should write original");
    symlink(&regular, &alias).expect("should create symlink");
    std::fs::hard_link(&regular, &hard).expect("should create hard link");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("should encode FIFO path");
    // SAFETY: The path is a live NUL-terminated string and mode is valid.
    assert_eq!(
        unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) },
        0,
        "should create FIFO"
    );
    std::fs::File::create(&huge)
        .expect("should create sparse fixture")
        .set_len(64 * 1024 * 1024 + 1)
        .expect("should exceed archive limit");
    let digest = hex::encode(sha2::Sha256::digest(b"fictional"));
    for path in [
        &regular,
        &alias,
        &hard,
        &fifo,
        &huge,
        &directory.path().to_path_buf(),
    ] {
        assert!(
            archive::verify(
                path,
                archive::Expected::Sha256(&digest),
                "fictional-source",
                "crate:fictional@1.0.0",
                Instant::now() + Duration::from_secs(5),
                &Cancellation::default()
            )
            .is_err(),
            "should reject unsafe original before any blocking read"
        );
    }
}

#[test]
fn registry_original_and_attempt_bounds_are_checked_before_use() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let path = directory.path().join("fictional.crate");
    let bytes = b"fictional original";
    std::fs::write(&path, bytes).expect("should write original");
    let digest = hex::encode(sha2::Sha256::digest(bytes));
    let cancellation = Cancellation::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    archive::verify(
        &path,
        archive::Expected::Sha256(&digest),
        "fictional-source",
        "crate:fictional@1.0.0",
        deadline,
        &cancellation,
    )
    .expect("should verify registry SHA-256");
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("should form expired deadline");
    assert!(
        archive::verify(
            &path,
            archive::Expected::Sha256(&digest),
            "fictional-source",
            "crate:fictional@1.0.0",
            expired,
            &cancellation
        )
        .is_err(),
        "should reject an expired attempt"
    );
    cancellation.cancel();
    assert!(
        archive::verify(
            &path,
            archive::Expected::Sha256(&digest),
            "fictional-source",
            "crate:fictional@1.0.0",
            deadline,
            &cancellation
        )
        .is_err(),
        "should reject cancellation before reading"
    );
    let mut changed = bytes.to_vec();
    changed[0] ^= 1;
    std::fs::write(&path, changed).expect("should alter one original byte");
    assert!(
        archive::verify(
            &path,
            archive::Expected::Sha256(&digest),
            "fictional-source",
            "crate:fictional@1.0.0",
            deadline,
            &Cancellation::default()
        )
        .is_err(),
        "should reject changed registry bytes"
    );
}

#[test]
fn npm_lockfile_duplicate_keys_are_rejected_before_identity_selection() {
    assert!(
        parse_npm(br#"{"lockfileVersion":3,"packages":{},"packages":{}}"#).is_err(),
        "should reject ambiguous archive selection before parsing package identities"
    );
}
