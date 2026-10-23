//! Fictional originals for vendoring tests; nothing here spawns a process.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use sha2::{Digest as _, Sha256};

use crate::git_objects::checked_sha1;
use crate::materials::LockedMaterials;
use crate::materials::git::VerifiedGitMaterial;
use crate::materials::locked::{CargoMaterials, GitPackage, RegistryMaterial};
use crate::process::Cancellation;

pub(super) const SOURCE_COMMIT: &str = "1111111111111111111111111111111111111111";
pub(super) const LOCK_SHA256: &str =
    "2222222222222222222222222222222222222222222222222222222222222222";
pub(super) const SELECTOR: &str = "git+https://example.com/org/workspace?tag=v1";

/// A canonical temporary directory.
pub(super) fn temporary() -> tempfile::TempDir {
    let root = fs::canonicalize(std::env::temp_dir()).expect("should resolve temporary parent");
    tempfile::tempdir_in(root).expect("should create canonical temporary directory")
}

pub(super) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// Builds a `.crate` archive whose members sit under `<name>-<version>/`.
pub(super) fn crate_archive(name: &str, version: &str, files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, bytes, mode) in files {
        let mut header = tar::Header::new_ustar();
        header.set_mode(*mode);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{name}-{version}/{path}"), *bytes)
            .expect("should append fictional member");
    }
    builder
        .into_inner()
        .expect("should finish fictional TAR")
        .finish()
        .expect("should finish fictional gzip")
}

pub(super) fn manifest(name: &str, version: &str) -> Vec<u8> {
    format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n")
        .into_bytes()
}

/// A standard two-file crate.
pub(super) fn plain_crate(name: &str, version: &str) -> Vec<u8> {
    crate_archive(
        name,
        version,
        &[
            ("Cargo.toml", &manifest(name, version), 0o644),
            ("src/lib.rs", b"pub fn fictional() {}\n", 0o644),
            ("build.sh", b"#!/bin/sh\n", 0o755),
        ],
    )
}

/// Lock state for `registry` archives and `git` packages.
pub(super) fn locked(registry: &[(&str, &str, &[u8])], git: Vec<GitPackage>) -> LockedMaterials {
    LockedMaterials {
        source_commit: SOURCE_COMMIT.to_owned(),
        cargo_lock_sha256: LOCK_SHA256.to_owned(),
        npm_lock_sha256: "c".repeat(64),
        cargo: CargoMaterials {
            registry: registry
                .iter()
                .map(|(name, version, bytes)| {
                    (
                        ((*name).to_owned(), (*version).to_owned()),
                        RegistryMaterial {
                            name: (*name).to_owned(),
                            version: (*version).to_owned(),
                            sha256: hex::encode(Sha256::digest(bytes)),
                        },
                    )
                })
                .collect(),
            git: git
                .iter()
                .map(|package| (package.selector.clone(), package.revision.clone()))
                .collect(),
            git_packages: git,
        },
        npm: BTreeMap::new(),
    }
}

/// Writes registry archives into `directory` and returns them by identity.
pub(super) fn archives(
    directory: &Path,
    entries: &[(&str, &str, &[u8])],
) -> BTreeMap<(String, String), PathBuf> {
    entries
        .iter()
        .map(|(name, version, bytes)| {
            let path = directory.join(format!("{name}-{version}.crate"));
            fs::write(&path, bytes).expect("should write fictional archive");
            (((*name).to_owned(), (*version).to_owned()), path)
        })
        .collect()
}

pub(super) fn cancellation() -> Cancellation {
    Cancellation::default()
}

/// Builds loose Git objects by hand.
pub(super) struct Objects {
    pub(super) store: tempfile::TempDir,
}

impl Objects {
    pub(super) fn new() -> Self {
        Self { store: temporary() }
    }

    pub(super) fn object(&self, kind: &str, bytes: &[u8]) -> String {
        let header = format!("{kind} {}\0", bytes.len());
        let oid = checked_sha1(&[header.as_bytes(), bytes]).expect("should hash fixture object");
        let path = self.store.path().join(&oid[..2]).join(&oid[2..]);
        fs::create_dir_all(path.parent().expect("should have parent"))
            .expect("should create object directory");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(header.as_bytes())
            .expect("should encode header");
        encoder.write_all(bytes).expect("should encode body");
        fs::write(path, encoder.finish().expect("should finish object"))
            .expect("should write fixture object");
        oid
    }

    pub(super) fn blob(&self, bytes: &[u8]) -> String {
        self.object("blob", bytes)
    }

    /// Entries are `(mode, name, oid)`; callers list them in Git order.
    pub(super) fn tree(&self, entries: &[(&str, &str, &str)]) -> String {
        let mut body = Vec::new();
        for (mode, name, oid) in entries {
            body.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            body.extend(hex::decode(oid).expect("should decode oid"));
        }
        self.object("tree", &body)
    }

    /// Wraps the store as verified material for `selector` at `revision`.
    pub(super) fn material(
        self,
        selector: &str,
        revision: &str,
        root_tree: &str,
    ) -> VerifiedGitMaterial {
        VerifiedGitMaterial::for_tests(
            SOURCE_COMMIT,
            LOCK_SHA256,
            selector,
            revision,
            root_tree,
            self.store,
        )
    }
}

pub(super) const REVISION: &str = "3333333333333333333333333333333333333333";

pub(super) fn git_package(name: &str, version: &str) -> GitPackage {
    GitPackage {
        name: name.to_owned(),
        version: version.to_owned(),
        selector: SELECTOR.to_owned(),
        revision: REVISION.to_owned(),
    }
}
