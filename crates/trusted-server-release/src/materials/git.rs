//! Locked Git commit snapshots, distinct from acquisition and Cargo checkouts.

mod input;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use error_stack::Report;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use sha2::{Digest as _, Sha256};

use super::{LockedMaterials, material_error};
use crate::error::ReleaseError;
use crate::git_objects::ObjectReader;
use crate::process::{BoundedCommand, Cancellation, ProcessLimits, ProcessResult};

const TREE_DEPTH: usize = 256;
const TREE_ENTRIES: usize = 100_000;

/// One bounded collection shares its decoded object budget across dependencies.
///
/// A locally selected executable is not an authenticated release-tool selection.
pub(crate) struct GitCollection<'a> {
    locked: &'a LockedMaterials,
    program: PathBuf,
    program_sha256: String,
    deadline: Instant,
    cancellation: Cancellation,
    objects: ObjectReader,
}

/// A selected commit's complete verified closure in an owned private object store.
///
/// Parent history and unselected objects are deliberately absent. This value
/// proves an object snapshot, not an installed Cargo checkout or tool qualification.
pub(crate) struct VerifiedGitMaterial {
    source_commit: String,
    cargo_lock_sha256: String,
    selector: String,
    revision: String,
    root_tree: String,
    store: tempfile::TempDir,
    object_ids: BTreeSet<String>,
}

impl VerifiedGitMaterial {
    /// Wraps a hand-built loose-object store without verifying it, for tests.
    #[cfg(test)]
    pub(super) fn for_tests(
        source_commit: &str,
        cargo_lock_sha256: &str,
        selector: &str,
        revision: &str,
        root_tree: &str,
        store: tempfile::TempDir,
    ) -> Self {
        Self {
            source_commit: source_commit.to_owned(),
            cargo_lock_sha256: cargo_lock_sha256.to_owned(),
            selector: selector.to_owned(),
            revision: revision.to_owned(),
            root_tree: root_tree.to_owned(),
            store,
            object_ids: BTreeSet::new(),
        }
    }

    /// Returns the independently approved source binding.
    pub(crate) fn source_commit(&self) -> &str {
        &self.source_commit
    }
    /// Returns the approved raw Cargo lockfile digest.
    pub(crate) fn cargo_lock_sha256(&self) -> &str {
        &self.cargo_lock_sha256
    }
    /// Returns the exact locked Git source selector.
    pub(crate) fn selector(&self) -> &str {
        &self.selector
    }
    /// Returns the exact locked commit identity.
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }
    /// Returns the selected commit's collision-checked root tree identity.
    pub(crate) fn root_tree(&self) -> &str {
        &self.root_tree
    }
    /// Borrows the owned verified-only object directory, never the input store.
    pub(crate) fn object_store(&self) -> &Path {
        self.store.path()
    }
    /// Returns the verified selected closure's distinct object count.
    pub(crate) fn object_count(&self) -> usize {
        self.object_ids.len()
    }
}

fn program_digest(
    path: &Path,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<String, Report<ReleaseError>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(material_error)?;
    let metadata = file.metadata().map_err(material_error)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.len() > 64 * 1024 * 1024
    {
        return Err(material_error(
            "selected Git must be a bounded regular executable",
        ));
    }
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            return Err(material_error(
                "Git executable verification cancelled or timed out",
            ));
        }
        let count = file.read(&mut buffer).map_err(material_error)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > metadata.len() {
            return Err(material_error("selected Git executable grew"));
        }
        digest.update(&buffer[..count]);
    }
    let after = file.metadata().map_err(material_error)?;
    let stamp = |value: &std::fs::Metadata| {
        (
            value.dev(),
            value.ino(),
            value.nlink(),
            value.len(),
            value.mode(),
            value.mtime(),
            value.mtime_nsec(),
            value.ctime(),
            value.ctime_nsec(),
        )
    };
    if length != metadata.len() || stamp(&metadata) != stamp(&after) {
        return Err(material_error("selected Git executable changed"));
    }
    Ok(hex::encode(digest.finalize()))
}

impl<'a> GitCollection<'a> {
    pub(super) fn new(
        locked: &'a LockedMaterials,
        program: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        if !program.is_absolute() {
            return Err(material_error("selected Git path must be absolute"));
        }
        let program = fs::canonicalize(program).map_err(material_error)?;
        let program_sha256 = program_digest(&program, deadline, cancellation)?;
        let collection = Self {
            locked,
            program,
            program_sha256,
            deadline,
            cancellation: cancellation.clone(),
            objects: ObjectReader::cargo_materials(),
        };
        collection.check()?;
        Ok(collection)
    }

    fn check(&self) -> Result<(), Report<ReleaseError>> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(material_error(
                "Git material verification cancelled or timed out",
            ));
        }
        Ok(())
    }

    fn execute(
        &self,
        repository: &Path,
        arguments: &[&str],
        input: Vec<u8>,
        limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.check()?;
        if program_digest(&self.program, self.deadline, &self.cancellation)? != self.program_sha256
        {
            return Err(material_error("selected Git executable changed"));
        }
        let home = repository.join("home");
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(repository);
        let mut fixed = vec![git_dir, OsString::from("--no-replace-objects")];
        fixed.extend(
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "protocol.allow=never",
            ]
            .into_iter()
            .chain(arguments.iter().copied())
            .map(OsString::from),
        );
        let wall = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| material_error("Git material deadline expired"))?;
        let output = BoundedCommand::new(
            &self.program,
            repository,
            ProcessLimits {
                wall: wall.min(Duration::from_secs(120)),
                stdout_max: limit,
                stderr_max: 64 * 1024,
            },
        )?
        .arguments(fixed)
        .environment("HOME", &home)?
        .environment(
            "PATH",
            self.program
                .parent()
                .ok_or_else(|| material_error("Git has no parent"))?,
        )?
        .environment("GIT_CONFIG_NOSYSTEM", "1")?
        .environment("GIT_NO_REPLACE_OBJECTS", "1")?
        .environment("GIT_TERMINAL_PROMPT", "0")?
        .environment("GIT_OPTIONAL_LOCKS", "0")?
        .environment("LC_ALL", "C")?
        .stdin_bytes(input)
        .cancellation(self.cancellation.clone())
        .run()
        .and_then(ProcessResult::require_success)
        .map_err(|report| report.change_context(ReleaseError::Material))?;
        self.check()?;
        Ok(output)
    }

    fn read(
        &self,
        repository: &Path,
        kind: &'static str,
        oid: &str,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.check()?;
        let mut body = Vec::new();
        self.objects
            .read_objects(
                kind,
                &[oid],
                &mut |arguments, input, limit| self.execute(repository, arguments, input, limit),
                |_, bytes| {
                    body.extend_from_slice(bytes);
                    Ok(())
                },
            )
            .map_err(|report| report.change_context(ReleaseError::Material))?;
        Ok(body)
    }

    fn retain(
        &self,
        store: &Path,
        kind: &str,
        oid: &str,
        bytes: &[u8],
    ) -> Result<(), Report<ReleaseError>> {
        self.check()?;
        let directory = store.join(&oid[..2]);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(material_error(error)),
        }
        let output = File::create_new(directory.join(&oid[2..])).map_err(material_error)?;
        let mut encoded = ZlibEncoder::new(output, Compression::default());
        encoded
            .write_all(format!("{kind} {}\0", bytes.len()).as_bytes())
            .map_err(material_error)?;
        for chunk in bytes.chunks(64 * 1024) {
            self.check()?;
            encoded.write_all(chunk).map_err(material_error)?;
        }
        encoded.finish().map_err(material_error)?;
        self.check()
    }

    /// Verifies only an approved locked selector and its full commit identity.
    ///
    /// Input objects are copied as untrusted data. Their repository configuration,
    /// refs, hooks and alternates are never consulted. All selected trees and blobs
    /// are independently rehashed, including subtrees outside Cargo package roots.
    ///
    /// # Errors
    ///
    /// Returns an error for an unlocked selection, unsafe input, changed executable,
    /// missing/corrupt objects, unsafe tree entry or exceeded shared limits.
    pub(crate) fn verify(
        &self,
        selector: &str,
        revision: &str,
        objects: &Path,
    ) -> Result<VerifiedGitMaterial, Report<ReleaseError>> {
        self.check()?;
        if !lower_oid(revision.as_bytes()) {
            return Err(material_error(
                "Git revision must be a full canonical object identity",
            ));
        }
        if !self
            .locked
            .cargo
            .git
            .contains(&(selector.to_owned(), revision.to_owned()))
        {
            return Err(material_error(
                "Git selection is absent from approved Cargo lockfile",
            ));
        }
        let repository = tempfile::Builder::new()
            .prefix("release-git-input-")
            .tempdir()
            .map_err(material_error)?;
        fs::create_dir(repository.path().join("objects")).map_err(material_error)?;
        fs::create_dir(repository.path().join("refs")).map_err(material_error)?;
        fs::create_dir(repository.path().join("home")).map_err(material_error)?;
        fs::write(repository.path().join("HEAD"), b"ref: refs/heads/unused\n")
            .map_err(material_error)?;
        fs::write(
            repository.path().join("config"),
            b"[core]\nrepositoryformatversion = 0\nbare = true\n",
        )
        .map_err(material_error)?;
        input::copy(objects, &repository.path().join("objects"), self)?;
        let store = tempfile::Builder::new()
            .prefix("release-git-verified-")
            .tempdir()
            .map_err(material_error)?;
        let commit = self.read(repository.path(), "commit", revision)?;
        let root_tree = commit_tree(&commit)?;
        self.retain(store.path(), "commit", revision, &commit)?;
        let mut object_kinds = BTreeMap::from([(revision.to_owned(), "commit")]);
        let mut trees = BTreeMap::<String, Vec<Entry>>::new();
        let mut pending = vec![(root_tree.clone(), 1_usize, 0_usize)];
        let mut entry_count = 0_usize;
        while let Some((oid, depth, prefix_length)) = pending.pop() {
            self.check()?;
            if depth > TREE_DEPTH {
                return Err(material_error("Git dependency tree exceeds depth limit"));
            }
            let entries = if let Some(entries) = trees.get(&oid) {
                entries.clone()
            } else {
                let bytes = self.read(repository.path(), "tree", &oid)?;
                let entries = parse_tree(&bytes)?;
                self.retain(store.path(), "tree", &oid, &bytes)?;
                if object_kinds.insert(oid.clone(), "tree").is_some() {
                    return Err(material_error(
                        "Git object is referenced with conflicting types",
                    ));
                }
                trees.insert(oid, entries.clone());
                entries
            };
            entry_count = entry_count
                .checked_add(entries.len())
                .ok_or_else(|| material_error("tree entry count overflow"))?;
            if entry_count > TREE_ENTRIES {
                return Err(material_error("Git dependency tree exceeds entry limit"));
            }
            for entry in entries {
                let path_length = prefix_length + entry.name_length;
                if path_length > 4096 {
                    return Err(material_error("Git dependency path exceeds byte limit"));
                }
                if entry.tree {
                    pending.push((entry.oid, depth + 1, path_length + 1));
                } else if let Some(kind) = object_kinds.get(&entry.oid) {
                    if *kind != "blob" {
                        return Err(material_error(
                            "Git object is referenced with conflicting types",
                        ));
                    }
                } else {
                    let bytes = self.read(repository.path(), "blob", &entry.oid)?;
                    self.retain(store.path(), "blob", &entry.oid, &bytes)?;
                    object_kinds.insert(entry.oid, "blob");
                }
            }
        }
        self.check()?;
        Ok(VerifiedGitMaterial {
            source_commit: self.locked.source_commit.clone(),
            cargo_lock_sha256: self.locked.cargo_lock_sha256.clone(),
            selector: selector.to_owned(),
            revision: revision.to_owned(),
            root_tree,
            store,
            object_ids: object_kinds.into_keys().collect(),
        })
    }
}

fn lower_oid(bytes: &[u8]) -> bool {
    bytes.len() == 40
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn commit_tree(bytes: &[u8]) -> Result<String, Report<ReleaseError>> {
    if bytes.contains(&0) {
        return Err(material_error("Git commit contains a NUL"));
    }
    let end = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or_else(|| material_error("Git commit has no header terminator"))?;
    let mut headers = bytes[..end].split(|byte| *byte == b'\n');
    let tree = headers
        .next()
        .and_then(|line| line.strip_prefix(b"tree "))
        .filter(|oid| lower_oid(oid))
        .ok_or_else(|| material_error("Git commit has no canonical root tree"))?;
    if headers.any(|line| line.starts_with(b"tree ")) {
        return Err(material_error("Git commit repeats its root tree"));
    }
    String::from_utf8(tree.to_vec()).map_err(material_error)
}

#[derive(Clone)]
struct Entry {
    tree: bool,
    oid: String,
    name_length: usize,
}

fn parse_tree(mut bytes: &[u8]) -> Result<Vec<Entry>, Report<ReleaseError>> {
    let mut names = BTreeSet::new();
    let mut previous = None::<Vec<u8>>;
    let mut entries = Vec::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| material_error("malformed Git tree mode"))?;
        let mode = &bytes[..space];
        let tree = match mode {
            b"40000" => true,
            // Link targets remain authenticated blob data; no filesystem link is created.
            b"100644" | b"100755" | b"120000" => false,
            _ => {
                return Err(material_error(
                    "Git dependency submodules and noncanonical modes are unreviewed",
                ));
            }
        };
        let rest = &bytes[space + 1..];
        let nul = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| material_error("malformed Git tree name"))?;
        let name = &rest[..nul];
        if name.is_empty()
            || !name.is_ascii()
            || name
                .iter()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b':'))
            || matches!(name, b"." | b"..")
            || name.eq_ignore_ascii_case(b".git")
            || name.ends_with(b".")
            || name.ends_with(b" ")
            || name.len() > 255
        {
            return Err(material_error("unsafe Git dependency tree name"));
        }
        let folded: Vec<_> = name.iter().map(u8::to_ascii_lowercase).collect();
        if !names.insert(folded) {
            return Err(material_error(
                "Git dependency tree aliases or repeats a name",
            ));
        }
        let mut key = name.to_vec();
        if tree {
            key.push(b'/');
        }
        if previous.as_ref().is_some_and(|previous| *previous >= key) {
            return Err(material_error("Git dependency tree order is noncanonical"));
        }
        previous = Some(key);
        let raw_oid = rest
            .get(nul + 1..nul + 21)
            .ok_or_else(|| material_error("truncated Git tree identity"))?;
        entries.push(Entry {
            tree,
            oid: hex::encode(raw_oid),
            name_length: name.len(),
        });
        if entries.len() > TREE_ENTRIES {
            return Err(material_error("Git tree exceeds entry limit"));
        }
        bytes = &rest[nul + 21..];
    }
    Ok(entries)
}

#[cfg(test)]
mod tests;
