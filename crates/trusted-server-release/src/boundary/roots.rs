//! Boundary root validation, alias checks and immutable tree facts.
//!
//! Every root is canonicalized, must be UTF-8 without quote, backslash or
//! control characters (so it can travel as an SBPL `-D` define or a Docker
//! mount field without escaping), and must be disjoint from every other root.
//!
//! Immutable roots are scanned before and after each confined execution.
//! Every regular file must have `nlink == 1`: a pre-existing hard link outside
//! the root writes through path-based write denial, and a write followed by
//! content, mode and mtime restoration is invisible to a hash comparison, so
//! the link itself is rejected before anything runs. Symlinks must be
//! relative, resolve inside their root and not dangle, which keeps their
//! meaning identical in the host and container namespaces.

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::error::ReleaseError;

/// Maximum number of roots per role; SBPL parameter slots are fixed.
pub(crate) const MAX_ROOTS: usize = 8;
/// File read size while hashing immutable files.
const HASH_CHUNK: usize = 64 * 1024;

fn boundary_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// How a root is exposed inside the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum RootRole {
    /// Read-only source or object store, scanned and rehashed.
    #[display("immutable")]
    Immutable,
    /// Read-only authenticated tool file or directory.
    #[display("tool")]
    Tool,
    /// Private writable installation or output root.
    #[display("writable")]
    Writable,
    /// Read-and-execute-only host system tool directory, such as the Apple
    /// developer directory.
    ///
    /// It is never scanned: its identity is an explicit record of versions
    /// and tool hashes held by the caller, not a tree snapshot.
    #[display("system")]
    System,
}

/// Validated, canonical and pairwise-disjoint boundary roots.
///
/// System roots take part in equality, so a boundary configured without the
/// exact system roots never matches a recipe that requires them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundaryRoots {
    immutable: Vec<PathBuf>,
    tools: Vec<PathBuf>,
    writable: Vec<PathBuf>,
    system: Vec<PathBuf>,
}

/// Rejects path text that cannot travel unescaped as a profile define.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] for a relative or non-UTF-8 path, the
/// filesystem root, or one containing `"`, `\` or any control character
/// (including newline and NUL).
pub(crate) fn validate_path_text(path: &Path) -> Result<&str, Report<ReleaseError>> {
    let text = path
        .to_str()
        .ok_or_else(|| boundary_error("boundary path is not UTF-8"))?;
    if !path.is_absolute() || text == "/" {
        return Err(boundary_error(format!(
            "boundary path {text:?} must be absolute and not the filesystem root"
        )));
    }
    if text
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return Err(boundary_error(format!(
            "boundary path {text:?} contains a quote, backslash or control character"
        )));
    }
    Ok(text)
}

fn canonical_root(path: &Path, role: RootRole) -> Result<PathBuf, Report<ReleaseError>> {
    validate_path_text(path)?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| boundary_error(format!("cannot resolve {role} root: {error}")))?;
    validate_path_text(&canonical)?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| boundary_error(format!("cannot inspect {role} root: {error}")))?;
    let accepted = metadata.is_dir() || (role == RootRole::Tool && metadata.is_file());
    if !accepted {
        return Err(boundary_error(format!(
            "{role} root {} has an unsupported file type",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn canonical_roots(
    paths: &[PathBuf],
    role: RootRole,
) -> Result<Vec<PathBuf>, Report<ReleaseError>> {
    if paths.len() > MAX_ROOTS {
        return Err(boundary_error(format!(
            "at most {MAX_ROOTS} {role} roots are supported"
        )));
    }
    paths
        .iter()
        .map(|path| canonical_root(path, role))
        .collect()
}

/// Returns whether either path contains the other.
#[must_use]
pub(crate) fn overlaps(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

impl BoundaryRoots {
    /// Canonicalizes and validates the roots.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for invalid path text, a missing
    /// root, a non-directory immutable or writable root, more than
    /// [`MAX_ROOTS`] roots of one role, no writable root, or any two roots
    /// that are equal or nested.
    pub(crate) fn new(
        immutable: &[PathBuf],
        tools: &[PathBuf],
        writable: &[PathBuf],
    ) -> Result<Self, Report<ReleaseError>> {
        let roots = Self {
            immutable: canonical_roots(immutable, RootRole::Immutable)?,
            tools: canonical_roots(tools, RootRole::Tool)?,
            writable: canonical_roots(writable, RootRole::Writable)?,
            system: Vec::new(),
        };
        if roots.writable.is_empty() {
            return Err(boundary_error("a boundary needs a private writable root"));
        }
        roots.check_disjoint()?;
        Ok(roots)
    }

    /// Adds read-and-execute-only system roots, replacing any set before.
    ///
    /// System roots are never scanned or written; the macOS profile allows
    /// only reading and executing below them and the Linux container
    /// boundary refuses them.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for invalid path text, a missing or
    /// non-directory root, more than [`MAX_ROOTS`] system roots, or a system
    /// root equal to or nested with any other root.
    pub(crate) fn with_system(mut self, system: &[PathBuf]) -> Result<Self, Report<ReleaseError>> {
        self.system = canonical_roots(system, RootRole::System)?;
        self.check_disjoint()?;
        Ok(self)
    }

    /// Requires every pair of roots to be neither equal nor nested.
    fn check_disjoint(&self) -> Result<(), Report<ReleaseError>> {
        let all: Vec<(RootRole, usize, &Path)> = self.entries().collect();
        for (index, (_, _, first)) in all.iter().enumerate() {
            for (_, _, second) in all.iter().skip(index + 1) {
                if overlaps(first, second) {
                    return Err(boundary_error(format!(
                        "boundary roots {} and {} are not disjoint",
                        first.display(),
                        second.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Returns these roots without the last writable root, which must be
    /// `excluded`.
    ///
    /// Only the last writable root may be dropped, so every remaining root
    /// keeps its role, per-role index and therefore its execution path.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when `excluded` cannot be resolved,
    /// is not the last writable root, or is the only writable root.
    pub(crate) fn without_last_writable(
        &self,
        excluded: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        let excluded = fs::canonicalize(excluded).map_err(|error| {
            boundary_error(format!(
                "cannot resolve the excluded writable root: {error}"
            ))
        })?;
        let Some((last, remaining)) = self.writable.split_last() else {
            return Err(boundary_error("a boundary needs a private writable root"));
        };
        if *last != excluded {
            return Err(boundary_error(
                "only the last writable root may be excluded without shifting root indices",
            ));
        }
        if remaining.is_empty() {
            return Err(boundary_error(
                "excluding the only writable root leaves no private writable root",
            ));
        }
        Ok(Self {
            immutable: self.immutable.clone(),
            tools: self.tools.clone(),
            writable: remaining.to_vec(),
            system: self.system.clone(),
        })
    }

    /// Returns every root with its role and per-role index.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (RootRole, usize, &Path)> {
        fn tag(
            role: RootRole,
            paths: &[PathBuf],
        ) -> impl Iterator<Item = (RootRole, usize, &Path)> {
            paths
                .iter()
                .enumerate()
                .map(move |(index, path)| (role, index, path.as_path()))
        }
        tag(RootRole::Immutable, &self.immutable)
            .chain(tag(RootRole::Tool, &self.tools))
            .chain(tag(RootRole::Writable, &self.writable))
            .chain(tag(RootRole::System, &self.system))
    }

    /// Returns the read-and-execute-only system roots.
    #[must_use]
    pub(crate) fn system(&self) -> &[PathBuf] {
        &self.system
    }

    /// Returns the immutable roots.
    #[must_use]
    pub(crate) fn immutable(&self) -> &[PathBuf] {
        &self.immutable
    }

    /// Returns the tool roots.
    #[must_use]
    pub(crate) fn tools(&self) -> &[PathBuf] {
        &self.tools
    }

    /// Returns the writable roots.
    #[must_use]
    pub(crate) fn writable(&self) -> &[PathBuf] {
        &self.writable
    }

    /// Returns the root containing `path` and the path relative to it.
    #[must_use]
    pub(crate) fn locate<'a>(&self, path: &'a Path) -> Option<(RootRole, usize, &'a Path)> {
        self.entries().find_map(|(role, index, root)| {
            path.strip_prefix(root)
                .ok()
                .map(|relative| (role, index, relative))
        })
    }
}

/// The observed identity of one entry under an immutable root.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File { size: u64, sha256: [u8; 32] },
    Symlink { target: PathBuf },
}

/// Metadata compared before and after each confined execution.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryFacts {
    kind: EntryKind,
    mode: u32,
    nlink: u64,
    device: u64,
    inode: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

/// A complete snapshot of one immutable root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeFacts {
    root: PathBuf,
    entries: BTreeMap<PathBuf, EntryFacts>,
}

/// What a tree scan records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanPolicy {
    /// Hash files, require `nlink == 1` and check aliases.
    Immutable,
    /// Require `nlink == 1` and check aliases without hashing (writable
    /// roots before a run).
    SingleLinks,
    /// Check aliases and file types only (writable roots after a run).
    AliasesOnly,
}

fn nanos(seconds: i64, nanoseconds: i64) -> i128 {
    i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds)
}

fn facts(kind: EntryKind, metadata: &Metadata) -> EntryFacts {
    EntryFacts {
        kind,
        mode: metadata.mode(),
        nlink: metadata.nlink(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mtime_ns: nanos(metadata.mtime(), metadata.mtime_nsec()),
        ctime_ns: nanos(metadata.ctime(), metadata.ctime_nsec()),
    }
}

/// Hashes a regular file opened without following links, rejecting hard
/// links and a file swapped between `lstat` and `open`.
fn hash_file(path: &Path, listed: &Metadata) -> Result<[u8; 32], Report<ReleaseError>> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| boundary_error(format!("cannot open {}: {error}", path.display())))?;
    let opened = file
        .metadata()
        .map_err(|error| boundary_error(format!("cannot stat {}: {error}", path.display())))?;
    if opened.dev() != listed.dev() || opened.ino() != listed.ino() {
        return Err(boundary_error(format!(
            "{} changed while it was scanned",
            path.display()
        )));
    }
    if opened.nlink() != 1 {
        return Err(boundary_error(format!(
            "file {} has {} hard links; a link outside the root would bypass write denial",
            path.display(),
            opened.nlink()
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| boundary_error(format!("cannot read {}: {error}", path.display())))?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            return Ok(hasher.finalize().into());
        };
        hasher.update(chunk);
    }
}

/// Lexically resolves a relative link target from the link's directory,
/// returning `None` when it climbs above the root.
fn lexical_target(link_relative: &Path, target: &Path) -> Option<PathBuf> {
    let mut resolved: Vec<Component<'_>> = link_relative.parent()?.components().collect();
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop()?;
            }
            Component::Normal(_) => resolved.push(component),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved.iter().collect())
}

/// Rejects an alias that is absolute, escapes the root or dangles.
fn check_alias(root: &Path, relative: &Path) -> Result<PathBuf, Report<ReleaseError>> {
    let path = root.join(relative);
    let target = fs::read_link(&path)
        .map_err(|error| boundary_error(format!("cannot read link {}: {error}", path.display())))?;
    if target.is_absolute() {
        return Err(boundary_error(format!(
            "link {} is absolute and resolves differently inside the container namespace",
            path.display()
        )));
    }
    if lexical_target(relative, &target).is_none() {
        return Err(boundary_error(format!(
            "link {} escapes its root",
            path.display()
        )));
    }
    let resolved = fs::canonicalize(&path)
        .map_err(|error| boundary_error(format!("link {} dangles: {error}", path.display())))?;
    if !resolved.starts_with(root) {
        return Err(boundary_error(format!(
            "link {} resolves outside its root",
            path.display()
        )));
    }
    Ok(target)
}

fn entry_kind(
    root: &Path,
    relative: &Path,
    metadata: &Metadata,
    policy: ScanPolicy,
) -> Result<EntryKind, Report<ReleaseError>> {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Ok(EntryKind::Directory);
    }
    if file_type.is_symlink() {
        return Ok(EntryKind::Symlink {
            target: check_alias(root, relative)?,
        });
    }
    if !file_type.is_file() {
        return Err(boundary_error(format!(
            "{} is a socket, FIFO or device; only files, directories and links are allowed",
            entry_path(root, relative).display()
        )));
    }
    let path = entry_path(root, relative);
    let sha256 = match policy {
        ScanPolicy::Immutable => hash_file(&path, metadata)?,
        ScanPolicy::SingleLinks if metadata.nlink() != 1 => {
            return Err(boundary_error(format!(
                "file {} has {} hard links; a link outside the root would bypass write denial",
                path.display(),
                metadata.nlink()
            )));
        }
        ScanPolicy::SingleLinks | ScanPolicy::AliasesOnly => [0; 32],
    };
    Ok(EntryKind::File {
        size: metadata.len(),
        sha256,
    })
}

/// Returns `root` itself for the empty relative path (a file root) and the
/// joined path otherwise.
fn entry_path(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

fn child_names(directory: &Path) -> Result<Vec<PathBuf>, Report<ReleaseError>> {
    let read_error = |error: std::io::Error| {
        boundary_error(format!("cannot list {}: {error}", directory.display()))
    };
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).map_err(read_error)? {
        names.push(PathBuf::from(entry.map_err(read_error)?.file_name()));
    }
    Ok(names)
}

impl TreeFacts {
    /// Scans `root` (canonical) under `policy`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for an unreadable entry, a
    /// non-regular special file, a hard-linked regular file (immutable and
    /// single-link policies), or an absolute, escaping or dangling symlink.
    pub(crate) fn scan(root: &Path, policy: ScanPolicy) -> Result<Self, Report<ReleaseError>> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(relative) = pending.pop() {
            let path = entry_path(root, &relative);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                boundary_error(format!("cannot inspect {}: {error}", path.display()))
            })?;
            let kind = entry_kind(root, &relative, &metadata, policy)?;
            if kind == EntryKind::Directory {
                pending.extend(
                    child_names(&path)?
                        .into_iter()
                        .map(|name| relative.join(name)),
                );
            }
            entries.insert(relative, facts(kind, &metadata));
        }
        Ok(Self {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// Requires `after` to match this snapshot exactly.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] naming the first added, removed or
    /// changed entry.
    pub(crate) fn ensure_unchanged(&self, after: &Self) -> Result<(), Report<ReleaseError>> {
        if self.root != after.root {
            return Err(boundary_error("tree snapshots describe different roots"));
        }
        let changed = self
            .entries
            .iter()
            .find(|(path, facts)| after.entries.get(*path) != Some(facts))
            .map(|(path, _)| path)
            .or_else(|| {
                after
                    .entries
                    .keys()
                    .find(|path| !self.entries.contains_key(*path))
            });
        match changed {
            None => Ok(()),
            Some(path) => Err(boundary_error(format!(
                "immutable root {} changed at {:?}",
                self.root.display(),
                path
            ))),
        }
    }
}
