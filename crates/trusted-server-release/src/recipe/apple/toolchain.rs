//! In-place identity of the selected toolchain's helpers, `libLTO`, optional
//! `libtapi`, clang resources and the entire library directory.
//!
//! Everything is read below the selected `clang`'s toolchain `usr` directory,
//! which stays part of the vendor developer root: nothing is copied, executed
//! or resolved through `xcrun`. Vendor hard links are accepted because the
//! tree is not owned; every measured file is rehashed at each verification
//! point instead. The receipt states which bytes were present, not which of
//! them clang actually read or ran. Libraries outside the selected directories
//! and the dependency/load closure remain unresolved.
//!
//! # Swap window
//!
//! Only the leaf of each measured file is opened with `O_NOFOLLOW`; the
//! intermediate directories are resolved by path. Between the canonical-root
//! check and a later open, a process able to write the vendor tree could swap
//! an intermediate directory for an alias. Such a change is detected, not
//! prevented: each file's device, inode, size, mode and times must match
//! before and after hashing, every root is rechecked as a real unaliased
//! directory, and the whole receipt is remeasured at each verification
//! point. Inside the confined build the developer directory is a read and
//! execute only system root, so the build itself cannot perform the swap.
//!
//! [`vendor_tree_digest`] requires a canonical root (a real directory that
//! resolves to itself); alias containment is checked by comparing canonical
//! targets against that root, which is only meaningful when the root is
//! canonical.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::super::fresh::real_directory;
use super::super::recipe_error;
use crate::error::ReleaseError;
use crate::process::Cancellation;

/// Toolchain library directory relative to the toolchain `usr` directory.
const TOOLCHAIN_LIB: &str = "lib";
/// The LTO library `ld` loads, relative to the toolchain `usr` directory.
const LIBLTO: &str = "lib/libLTO.dylib";
/// Optional TAPI library relative to the selected toolchain `usr` directory.
const LIBTAPI: &str = "lib/libtapi.dylib";
/// Parent of clang's versioned resource directory below the toolchain `usr`.
const CLANG_RESOURCES: &str = "lib/clang";
/// Largest accepted `libLTO.dylib`.
const LIBLTO_LIMIT: u64 = 512 * 1024 * 1024;
/// Largest accepted `libtapi.dylib`.
const LIBTAPI_LIMIT: u64 = 128 * 1024 * 1024;
/// Most entries accepted in the clang resource parent directory.
const RESOURCE_PARENT_ENTRY_LIMIT: usize = 64;
/// Longest accepted clang resource version or alias name.
const MAX_RESOURCE_VERSION: usize = 32;
/// File read size while hashing a vendor file.
const HASH_CHUNK: usize = 64 * 1024;
/// Versioned domain prefix of every vendor tree digest.
const TREE_DOMAIN: &[u8] = b"ts-apple-vendor-tree-v1\0";

/// The kind label and bounds of one in-place vendor tree.
///
/// The kind is hashed into the digest, so trees of different kinds never
/// share an identity even with identical contents.
#[derive(Clone, Copy, Debug)]
struct TreePolicy {
    kind: &'static str,
    file_bytes: u64,
    tree_bytes: u64,
    entries: usize,
}

/// Policy for the clang resource directory (headers and runtime libraries).
const RESOURCE_POLICY: TreePolicy = TreePolicy {
    kind: "clang-resource-directory",
    file_bytes: 128 * 1024 * 1024,
    tree_bytes: 1024 * 1024 * 1024,
    entries: 20_000,
};

/// Policy for the toolchain helper directory beside `clang`.
const HELPER_POLICY: TreePolicy = TreePolicy {
    kind: "toolchain-helper-directory",
    file_bytes: 512 * 1024 * 1024,
    tree_bytes: 4 * 1024 * 1024 * 1024,
    entries: 4_000,
};

/// Policy for the selected toolchain's entire `usr/lib` directory.
const LIBRARY_POLICY: TreePolicy = TreePolicy {
    kind: "toolchain-library-directory",
    file_bytes: 512 * 1024 * 1024,
    tree_bytes: 4 * 1024 * 1024 * 1024,
    entries: 20_000,
};

/// Structured digest of one vendor tree measured in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct VendorTreeReceipt {
    tree_sha256: String,
    regular_bytes: u64,
    entries: usize,
}

/// Measured toolchain inputs beyond `clang`, `ar` and `ld`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ToolchainReceipt {
    clang_resource_version: String,
    clang_resource_aliases: Vec<String>,
    clang_resource_tree: VendorTreeReceipt,
    liblto_sha256: String,
    libtapi_sha256: Option<String>,
    helper_tree: VendorTreeReceipt,
    library_tree: VendorTreeReceipt,
}

impl ToolchainReceipt {
    /// Returns the clang resource directory's structured-tree digest.
    pub(super) fn clang_resource_tree_sha256(&self) -> &str {
        &self.clang_resource_tree.tree_sha256
    }
    /// Returns the raw-file digest of `libLTO.dylib`.
    pub(super) fn liblto_sha256(&self) -> &str {
        &self.liblto_sha256
    }
    /// Returns the raw-file digest of `libtapi.dylib`, when present.
    pub(super) fn libtapi_sha256(&self) -> Option<&str> {
        self.libtapi_sha256.as_deref()
    }
    /// Returns the helper directory's structured-tree digest.
    pub(super) fn helper_tree_sha256(&self) -> &str {
        &self.helper_tree.tree_sha256
    }
    /// Returns the entire library directory's structured-tree digest.
    pub(super) fn library_tree_sha256(&self) -> &str {
        &self.library_tree.tree_sha256
    }
}

/// A toolchain receipt bound to the attempt budget that rechecks it.
#[derive(Clone, Debug)]
pub(super) struct MeasuredToolchain {
    helper_dir: PathBuf,
    usr_dir: PathBuf,
    receipt: ToolchainReceipt,
    deadline: Instant,
    cancellation: Cancellation,
}

impl MeasuredToolchain {
    /// Measures helpers, resources and the entire selected `usr/lib` in place.
    ///
    /// The toolchain holds `clang` strictly below `developer_dir`.
    ///
    /// # Errors
    ///
    /// Rejects a `clang` path with a non-normal component or not at
    /// `<toolchain>/usr/bin/clang`, a toolchain outside the developer
    /// directory, anything other than exactly one clang resource version
    /// directory, a missing or non-regular `libLTO.dylib`, unsafe aliases,
    /// special files, exhausted limits, cancellation and expiration.
    pub(super) fn measure(
        clang: &Path,
        developer_dir: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        let (helper_dir, usr_dir) = toolchain_layout(clang, developer_dir)?;
        let receipt = measure_receipt(&helper_dir, &usr_dir, &|| budget(deadline, cancellation))?;
        Ok(Self {
            helper_dir,
            usr_dir,
            receipt,
            deadline,
            cancellation: cancellation.clone(),
        })
    }

    /// The receipt measured at selection.
    pub(super) fn receipt(&self) -> &ToolchainReceipt {
        &self.receipt
    }

    /// Remeasures the toolchain under the original attempt budget.
    ///
    /// # Errors
    ///
    /// Rejects any changed, added or removed entry, a changed resource
    /// selection, unsafe entries, cancellation and expiration.
    pub(super) fn verify(&self) -> Result<(), Report<ReleaseError>> {
        let current = measure_receipt(&self.helper_dir, &self.usr_dir, &|| {
            budget(self.deadline, &self.cancellation)
        })?;
        if current != self.receipt {
            return Err(recipe_error("Apple toolchain changed after measurement"));
        }
        Ok(())
    }
}

fn budget(deadline: Instant, cancellation: &Cancellation) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(recipe_error(
            "Apple toolchain measurement cancelled or timed out",
        ));
    }
    Ok(())
}

fn measurement_error(error: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("Apple toolchain measurement: {error}"))
}

/// Returns the helper (`usr/bin`) and `usr` directories of `clang`.
///
/// The path must be absolute with only normal components, `clang` must sit
/// in a directory named `bin` whose parent is named `usr`, and that `usr`
/// directory must be strictly inside `developer_dir`.
fn toolchain_layout(
    clang: &Path,
    developer_dir: &Path,
) -> Result<(PathBuf, PathBuf), Report<ReleaseError>> {
    let mut components = clang.components();
    let normal = components.next() == Some(Component::RootDir)
        && components.all(|component| matches!(component, Component::Normal(_)));
    if !normal {
        return Err(recipe_error(
            "Apple clang path must be absolute with only normal components",
        ));
    }
    let helper_dir = clang
        .parent()
        .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
        .ok_or_else(|| recipe_error("Apple clang must sit in a toolchain usr/bin directory"))?;
    let usr_dir = helper_dir
        .parent()
        .filter(|usr| usr.file_name().is_some_and(|name| name == "usr"))
        .ok_or_else(|| recipe_error("Apple clang must sit in a toolchain usr/bin directory"))?;
    if !usr_dir.starts_with(developer_dir) || usr_dir == developer_dir {
        return Err(recipe_error(
            "Apple toolchain is not inside the Apple developer directory",
        ));
    }
    Ok((helper_dir.to_owned(), usr_dir.to_owned()))
}

fn measure_receipt(
    helper_dir: &Path,
    usr_dir: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<ToolchainReceipt, Report<ReleaseError>> {
    check()?;
    real_directory("Apple toolchain helper directory", helper_dir)?;
    real_directory(
        "Apple toolchain library directory",
        &usr_dir.join(TOOLCHAIN_LIB),
    )?;
    let resource_parent = usr_dir.join(CLANG_RESOURCES);
    let (clang_resource_version, clang_resource_aliases) =
        select_resource_version(&resource_parent, check)?;
    let resource_dir = resource_parent.join(&clang_resource_version);
    real_directory("Apple clang resource directory", &resource_dir)?;
    let clang_resource_tree = vendor_tree_digest(&resource_dir, RESOURCE_POLICY, check)?;
    let (liblto_sha256, _) = hash_vendor_file(&usr_dir.join(LIBLTO), LIBLTO_LIMIT, check)?;
    let libtapi_sha256 = optional_libtapi_digest(&usr_dir.join(LIBTAPI), check)?;
    let helper_tree = vendor_tree_digest(helper_dir, HELPER_POLICY, check)?;
    let library_tree = vendor_tree_digest(&usr_dir.join(TOOLCHAIN_LIB), LIBRARY_POLICY, check)?;
    Ok(ToolchainReceipt {
        clang_resource_version,
        clang_resource_aliases,
        clang_resource_tree,
        liblto_sha256,
        libtapi_sha256,
        helper_tree,
        library_tree,
    })
}

/// Records absence only when the exact leaf does not exist; aliases and all
/// other metadata or read failures are rejected rather than treated as absent.
fn optional_libtapi_digest(
    path: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<Option<String>, Report<ReleaseError>> {
    check()?;
    match fs::symlink_metadata(path) {
        Ok(_) => hash_vendor_file(path, LIBTAPI_LIMIT, check).map(|(digest, _)| Some(digest)),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            check()?;
            let parent = path
                .parent()
                .ok_or_else(|| recipe_error("Apple libtapi path has no library directory"))?;
            real_directory("Apple toolchain library directory", parent)?;
            Ok(None)
        }
        Err(error) => Err(measurement_error(error)),
    }
}

fn resource_version_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_RESOURCE_VERSION
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
}

/// Selects the only real clang version directory and its sibling aliases.
///
/// Every alias must name exactly that directory; any other entry fails closed.
fn select_resource_version(
    parent: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<(String, Vec<String>), Report<ReleaseError>> {
    real_directory("Apple clang resource parent", parent)?;
    let mut versions = Vec::new();
    let mut aliases = Vec::new();
    for (index, entry) in fs::read_dir(parent).map_err(measurement_error)?.enumerate() {
        check()?;
        if index >= RESOURCE_PARENT_ENTRY_LIMIT {
            return Err(recipe_error(
                "Apple clang resource parent exceeds entry limit",
            ));
        }
        let entry = entry.map_err(measurement_error)?;
        let name = entry
            .file_name()
            .into_string()
            .ok()
            .filter(|name| resource_version_name(name))
            .ok_or_else(|| recipe_error("Apple clang resource entry is not a version name"))?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(measurement_error)?;
        if metadata.is_dir() {
            versions.push(name);
        } else if metadata.file_type().is_symlink() {
            aliases.push((
                name,
                fs::read_link(entry.path()).map_err(measurement_error)?,
            ));
        } else {
            return Err(recipe_error(
                "Apple clang resource parent holds a non-directory entry",
            ));
        }
    }
    let [version] = <[String; 1]>::try_from(versions).map_err(|_| {
        recipe_error("Apple clang resources must hold exactly one version directory")
    })?;
    let mut alias_names = Vec::with_capacity(aliases.len());
    for (name, target) in aliases {
        if target != Path::new(&version) {
            return Err(recipe_error(
                "Apple clang resource alias names another target than the version directory",
            ));
        }
        alias_names.push(name);
    }
    alias_names.sort();
    Ok((version, alias_names))
}

fn stamp(metadata: &fs::Metadata) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Hashes one vendor regular file in place, opened without following a leaf
/// alias. Vendor hard links are accepted; the content is rechecked instead.
fn hash_vendor_file(
    path: &Path,
    limit: u64,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<(String, u64), Report<ReleaseError>> {
    check()?;
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(measurement_error)?;
    let before = file.metadata().map_err(measurement_error)?;
    if !before.is_file() || before.len() > limit {
        return Err(recipe_error(format!(
            "Apple toolchain file {} is not a regular file within its size limit",
            path.display()
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_CHUNK];
    let mut size = 0_u64;
    loop {
        check()?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(measurement_error(error)),
        };
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        size += u64::try_from(chunk.len()).map_err(measurement_error)?;
        if size > before.len() {
            return Err(recipe_error("Apple toolchain file grew while hashing"));
        }
        hasher.update(chunk);
    }
    if size != before.len()
        || stamp(&file.metadata().map_err(measurement_error)?) != stamp(&before)
        || stamp(&fs::symlink_metadata(path).map_err(measurement_error)?) != stamp(&before)
    {
        return Err(recipe_error("Apple toolchain file changed while hashing"));
    }
    Ok((hex::encode(hasher.finalize()), size))
}

/// Digests every entry below `root` in place: relative path, kind, mode and,
/// for regular files, size and SHA-256; for aliases, their relative target.
/// The digest is seeded with [`TREE_DOMAIN`] and the policy's tree kind.
///
/// `root` must be canonical (callers check it with `real_directory`), since
/// alias containment compares canonical targets against it.
///
/// Rejects non-UTF-8 names, absolute, dangling or root-escaping aliases,
/// special files and exhausted policy limits.
fn vendor_tree_digest(
    root: &Path,
    policy: TreePolicy,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<VendorTreeReceipt, Report<ReleaseError>> {
    let mut pending = vec![root.to_owned()];
    let mut facts = BTreeMap::new();
    let mut regular_bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(measurement_error)? {
            check()?;
            let path = entry.map_err(measurement_error)?.path();
            let relative = path
                .strip_prefix(root)
                .map_err(measurement_error)?
                .to_str()
                .ok_or_else(|| recipe_error("Apple toolchain path is not UTF-8"))?
                .to_owned();
            if facts.len() >= policy.entries {
                return Err(recipe_error("Apple toolchain tree exceeds entry limit"));
            }
            let metadata = fs::symlink_metadata(&path).map_err(measurement_error)?;
            let mode = metadata.permissions().mode() & 0o7777;
            let fact = if metadata.is_dir() {
                pending.push(path);
                format!("directory:{mode}")
            } else if metadata.is_file() {
                let (sha256, size) = hash_vendor_file(&path, policy.file_bytes, check)?;
                regular_bytes += size;
                if regular_bytes > policy.tree_bytes {
                    return Err(recipe_error("Apple toolchain tree exceeds byte limit"));
                }
                format!("file:{mode}:{size}:{sha256}")
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(measurement_error)?;
                let target = target
                    .to_str()
                    .filter(|text| !target.is_absolute() && !text.is_empty())
                    .ok_or_else(|| recipe_error("Apple toolchain alias is not relative UTF-8"))?
                    .to_owned();
                if !fs::canonicalize(&path)
                    .map_err(measurement_error)?
                    .starts_with(root)
                {
                    return Err(recipe_error("Apple toolchain alias escapes its tree"));
                }
                format!("alias:{target}")
            } else {
                return Err(recipe_error("Apple toolchain tree contains a special file"));
            };
            facts.insert(relative, fact);
        }
    }
    let mut digest = Sha256::new();
    digest.update(TREE_DOMAIN);
    digest.update(policy.kind.as_bytes());
    digest.update([0]);
    for (relative, fact) in &facts {
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(fact.as_bytes());
        digest.update([0]);
    }
    Ok(VendorTreeReceipt {
        tree_sha256: hex::encode(digest.finalize()),
        regular_bytes,
        entries: facts.len(),
    })
}

#[cfg(test)]
mod tests;
