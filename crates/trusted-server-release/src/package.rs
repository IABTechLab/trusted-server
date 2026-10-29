//! Read-only inspection of one manual Fastly transport envelope.
//!
//! The initial deployment profile accepts only the canonical two-line manifest
//! (`manifest_version = 3` and a portable package name). It intentionally excludes
//! every other field, table and comment. Service selection and observed resource
//! bindings belong to the independently approved manual deployment procedure.

use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufReader, Read, Seek as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use flate2::bufread::GzDecoder;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, read_bounded};
use crate::error::ReleaseError;
use crate::inspect::inspect_wasm;

const ARCHIVE_LIMIT: u64 = 768 * 1024 * 1024;
const MODULE_LIMIT: u64 = 512 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 1024 * 1024;
const EXPANDED_LIMIT: u64 = MODULE_LIMIT + MANIFEST_LIMIT + 16 * 1024;

fn rejected(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Inspect).attach(message.to_string())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn approved_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn diagnostic_path(path: &Path) -> Result<Value, Report<ReleaseError>> {
    let path = path
        .to_str()
        .ok_or_else(|| rejected("selected package path must be UTF-8 for diagnostics"))?;
    Ok(json!(path))
}

fn manifest_name(bytes: &[u8]) -> Result<String, Report<ReleaseError>> {
    let text = std::str::from_utf8(bytes).map_err(rejected)?;
    let table: toml::Table = toml::from_str(text).map_err(rejected)?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| rejected("deployment manifest requires a package name"))?;
    let valid = !name.is_empty()
        && name.len() <= 64
        && name != "package"
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if !valid || bytes != format!("manifest_version = 3\nname = \"{name}\"\n").as_bytes() {
        return Err(rejected(
            "deployment manifest must match the canonical two-line version-3 profile and portable nonreserved name",
        ));
    }
    Ok(name.to_owned())
}

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    links: u64,
    size: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl From<&Metadata> for Stamp {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            links: metadata.nlink(),
            size: metadata.len(),
            mode: metadata.mode(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

fn check_path(path: &Path, expected: &Stamp) -> Result<(), Report<ReleaseError>> {
    let metadata = fs::symlink_metadata(path).map_err(rejected)?;
    if !metadata.is_file() || Stamp::from(&metadata) != *expected {
        return Err(rejected(
            "approved manifest inode, path or metadata changes",
        ));
    }
    Ok(())
}

struct RetainedArchive {
    path: PathBuf,
    file: File,
    stamp: Stamp,
}

impl RetainedArchive {
    fn open(path: PathBuf) -> Result<Self, Report<ReleaseError>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(rejected)?;
        let metadata = file.metadata().map_err(rejected)?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > ARCHIVE_LIMIT {
            return Err(rejected(
                "archive must be a bounded regular file with one hard link",
            ));
        }
        let retained = Self {
            path,
            file,
            stamp: Stamp::from(&metadata),
        };
        retained.check()?;
        Ok(retained)
    }

    fn check(&self) -> Result<(), Report<ReleaseError>> {
        let path = fs::symlink_metadata(&self.path).map_err(rejected)?;
        let held = self.file.metadata().map_err(rejected)?;
        if !path.is_file() || Stamp::from(&path) != self.stamp || Stamp::from(&held) != self.stamp {
            return Err(rejected("retained archive inode, path or metadata changes"));
        }
        Ok(())
    }

    fn hash(&mut self) -> Result<String, Report<ReleaseError>> {
        self.check()?;
        self.file.rewind().map_err(rejected)?;
        let mut hash = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = self.file.read(&mut buffer).map_err(rejected)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > self.stamp.size || count > ARCHIVE_LIMIT {
                return Err(rejected("archive grows while hashing"));
            }
            hash.update(&buffer[..read]);
        }
        if count != self.stamp.size {
            return Err(rejected("archive size changes while hashing"));
        }
        self.check()?;
        Ok(hex::encode(hash.finalize()))
    }
}

struct Expanded<R> {
    reader: R,
    count: u64,
}

impl<R: Read> Read for Expanded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let maximum = buffer.len().min(64 * 1024);
        let count = self.reader.read(&mut buffer[..maximum])?;
        self.count += count as u64;
        if self.count > EXPANDED_LIMIT {
            return Err(io::Error::other("package exceeds expanded byte bound"));
        }
        Ok(count)
    }
}

fn raw_field(bytes: &[u8]) -> Result<&str, Report<ReleaseError>> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(rejected("TAR field hides bytes after its terminator"));
    }
    std::str::from_utf8(&bytes[..end]).map_err(rejected)
}

fn module_member(
    retained: &mut RetainedArchive,
    name: &str,
    manifest: &[u8],
) -> Result<Vec<u8>, Report<ReleaseError>> {
    retained.check()?;
    retained.file.rewind().map_err(rejected)?;
    let decoded = Expanded {
        reader: GzDecoder::new(BufReader::new(&mut retained.file)),
        count: 0,
    };
    let mut archive = tar::Archive::new(decoded);
    let root = format!("{name}/");
    let bin = format!("{name}/bin/");
    let bare_bin = format!("{name}/bin");
    let module_name = format!("{name}/bin/main.wasm");
    let manifest_name = format!("{name}/fastly.toml");
    let mut seen = BTreeSet::new();
    let mut module = Vec::new();
    for entry in archive.entries().map_err(rejected)?.raw(true) {
        let mut entry = entry.map_err(rejected)?;
        if seen.len() >= 4 {
            return Err(rejected("package contains extra entries"));
        }
        let header = entry.header();
        let path = raw_field(&header.as_old().name)?.to_owned();
        // Only the two known directory entries may omit their trailing slash.
        // Canonical logical IDs reject duplicates across those raw spellings;
        // file paths, links, prefixes and metadata remain exact.
        let directory = if path == name || path == root {
            Some(root.as_str())
        } else if path == bare_bin || path == bin {
            Some(bin.as_str())
        } else {
            None
        };
        if header
            .as_ustar()
            .is_some_and(|ustar| ustar.prefix.iter().any(|byte| *byte != 0))
            || !raw_field(&header.as_old().linkname)?.is_empty()
            || !seen.insert(directory.unwrap_or(&path).to_owned())
        {
            return Err(rejected(
                "package has prefixed paths, links or duplicate entries",
            ));
        }
        let kind = header.entry_type().as_byte();
        let size = entry.size();
        if directory.is_some() {
            if kind != b'5' || size != 0 {
                return Err(rejected(
                    "package directory has the wrong type or nonzero size",
                ));
            }
        } else if path == module_name || path == manifest_name {
            if !matches!(kind, b'0' | 0) {
                return Err(rejected("package member is not a plain regular file"));
            }
            let limit = if path == module_name {
                MODULE_LIMIT
            } else {
                MANIFEST_LIMIT
            };
            if size > limit {
                return Err(rejected("package member exceeds its byte bound"));
            }
            let mut body = Vec::new();
            body.try_reserve_exact(usize::try_from(size).map_err(rejected)?)
                .map_err(rejected)?;
            entry.read_to_end(&mut body).map_err(rejected)?;
            if body.len() as u64 != size {
                return Err(rejected("package member is truncated"));
            }
            if path == module_name {
                module = body;
            } else if body != manifest {
                return Err(rejected(
                    "package manifest differs from the approved manifest bytes",
                ));
            }
        } else {
            return Err(rejected(
                "package contains an unexpected raw path or metadata header",
            ));
        }
    }
    if seen.len() != 4 {
        return Err(rejected("package must contain all four exact members"));
    }
    let mut decoded = archive.into_inner();
    let mut tail = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = decoded.read(&mut buffer).map_err(rejected)?;
        if count == 0 {
            break;
        }
        tail += count as u64;
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(rejected("TAR hides data after its terminator"));
        }
    }
    if tail < 512 || !tail.is_multiple_of(512) {
        return Err(rejected("TAR lacks complete terminator blocks"));
    }
    let mut compressed = decoded.reader.into_inner();
    if compressed.read(&mut buffer[..1]).map_err(rejected)? != 0 {
        return Err(rejected("gzip has additional members or trailing bytes"));
    }
    drop(compressed);
    retained.check()?;
    Ok(module)
}

/// Inspects a named manual package against independently approved identities.
///
/// This diagnostic supplies no release-verification, runtime or deployment
/// authority. Call it again immediately before the separate submission.
///
/// # Errors
///
/// Rejects unsafe files, changed bytes, invalid expected hashes, noncanonical
/// manifests, invalid core Wasm, or any archive outside the bounded four-member
/// profile. Expected hashes must come from the successful independent release
/// verification and context procedure, never from this package.
pub(crate) fn inspect_package(
    package_dir: &Path,
    manifest: &Path,
    expected_manifest_sha256: &str,
    expected_wasm_sha256: &str,
) -> Result<Value, Report<ReleaseError>> {
    if !approved_digest(expected_manifest_sha256) || !approved_digest(expected_wasm_sha256) {
        return Err(rejected(
            "expected SHA-256 identities must be lowercase 64-digit hex",
        ));
    }
    let manifest_stamp = Stamp::from(&fs::symlink_metadata(manifest).map_err(rejected)?);
    let approved = read_bounded(manifest, Role::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Inspect))?;
    check_path(manifest, &manifest_stamp)?;
    if approved.sha256 != expected_manifest_sha256 {
        return Err(rejected(
            "deployment manifest differs from its independently approved digest",
        ));
    }
    let name = manifest_name(&approved.bytes)?;
    let directory = fs::canonicalize(package_dir).map_err(rejected)?;
    let mut retained = RetainedArchive::open(directory.join(format!("{name}.tar.gz")))?;
    let archive_sha256 = retained.hash()?;
    let module = module_member(&mut retained, &name, &approved.bytes)?;
    let facts = inspect_wasm(&module)?;
    if facts.sha256() != expected_wasm_sha256 {
        return Err(rejected(
            "actual package module differs from independently verified executable",
        ));
    }
    drop(module);
    let rechecked_module = module_member(&mut retained, &name, &approved.bytes)?;
    if digest(&rechecked_module) != expected_wasm_sha256 || retained.hash()? != archive_sha256 {
        return Err(rejected(
            "retained archive or actual module changes before acceptance",
        ));
    }
    let rechecked_manifest = read_bounded(manifest, Role::Metadata)
        .map_err(|error| error.change_context(ReleaseError::Inspect))?;
    if rechecked_manifest.bytes != approved.bytes {
        return Err(rejected("approved manifest changes before acceptance"));
    }
    check_path(manifest, &manifest_stamp)?;
    retained.check()?;
    let package_path = diagnostic_path(&retained.path)?;
    Ok(
        json!({ "check": "manual-fastly-package", "accepted": true, "package_path": package_path,
        "archive_sha256": archive_sha256, "module_sha256": expected_wasm_sha256, "manifest_sha256": approved.sha256,
        "limits": { "runtime_validated": false, "official_release_verified": false, "deployment_authorized": false } }),
    )
}

#[cfg(test)]
mod tests;
