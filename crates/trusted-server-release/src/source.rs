//! Local authentication of the approved release source checkout.
//!
//! The repository layout and configuration are inspected with bounded direct
//! reads before Git runs, because Git itself would execute or redirect through
//! several configuration channels. Only read-only plumbing then runs under a
//! cleared environment, and every object Git returns is rehashed. Remote
//! branch-tip ancestry and ruleset validation are hosted operations (Task 14)
//! and are not established here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, lower_hex, parse_json, read_bounded};
use crate::error::ReleaseError;
use crate::preflight::AuditedEnvironment;
use closure::helper_closure;
use config::check_git_config;
use git::Git;
use tree::{TREE_LIMITS, TreeEntry, walk_tree};
use worktree::compare_worktree;

mod closure;
mod config;
mod git;
mod materialize;
mod tree;
mod worktree;

pub(crate) use materialize::{FreshSource, FreshSourceVerification};

/// Repository channels that rewrite history, objects or checkout attributes.
const FORBIDDEN_GIT_PATHS: [&str; 7] = [
    "commondir",
    "objects/info/alternates",
    "objects/info/http-alternates",
    "info/grafts",
    "info/attributes",
    "shallow",
    "refs/replace",
];

/// Tracked symlinks reviewed for release sources, with their exact targets.
const REVIEWED_SYMLINKS: [(&str, &str); 1] = [("CLAUDE.md", "AGENTS.md")];

fn source_error(message: impl Into<String>) -> Report<ReleaseError> {
    Report::new(ReleaseError::Source).attach(message.into())
}

/// A Git SHA-1 commit id: exactly 40 lowercase hexadecimal digits.
#[derive(Clone, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) struct CommitId(String);

impl CommitId {
    /// Parses an approved commit argument without any normalization.
    ///
    /// # Errors
    ///
    /// Fails unless `value` is exactly 40 lowercase hexadecimal digits.
    pub(crate) fn parse(value: &str) -> Result<Self, Report<ReleaseError>> {
        if !lower_hex(value, 40) {
            return Err(source_error(
                "approved source commit must be exactly 40 lowercase hex digits",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// The commit id text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Regular,
    Executable,
    Symlink,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
        }
    }
}

/// Tracked path records: mode and SHA-256 of content or symlink target.
type Records = BTreeMap<String, (Mode, String)>;

/// Returns the complete approved JS entry-point set of `records`, sorted.
fn js_module_ids(records: &Records) -> Vec<String> {
    let prefix = "crates/trusted-server-js/lib/src/";
    let mut ids: Vec<_> = records
        .keys()
        .filter_map(|path| {
            let path = path.strip_prefix(prefix)?;
            if path == "core/index.ts" {
                return Some("core".to_owned());
            }
            let integration = path
                .strip_prefix("integrations/")?
                .strip_suffix("/index.ts")?;
            (!integration.contains('/')).then(|| integration.to_owned())
        })
        .collect();
    ids.sort();
    ids
}

/// Fingerprint of every tracked worktree path at the approved commit.
pub(crate) struct SourceSnapshot {
    root: PathBuf,
    records: Records,
    fingerprint: String,
}

impl SourceSnapshot {
    /// SHA-256 over sorted path, mode and content-digest records.
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Rescans the worktree and fails on any tracked or untracked change.
    ///
    /// # Errors
    ///
    /// Fails on changed content, modes, file types or hard-link counts,
    /// missing tracked paths, or any new untracked path.
    pub(crate) fn verify_unchanged(&self) -> Result<(), Report<ReleaseError>> {
        compare_worktree(&self.root, &self.records)
    }
}

/// A locally authenticated source checkout; constructed only by [`authenticate`].
pub(crate) struct ApprovedSource {
    commit: CommitId,
    epoch: u64,
    snapshot: SourceSnapshot,
    closure_digest: String,
    closure_paths: Vec<String>,
}

impl ApprovedSource {
    /// The approved source commit.
    pub(crate) fn commit(&self) -> &CommitId {
        &self.commit
    }

    /// `SOURCE_DATE_EPOCH` derived from the approved committer timestamp.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The tracked worktree snapshot for before/after mutation checks.
    pub(crate) fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }

    /// Audit fingerprint of the helper-source closure. It is not authority.
    pub(crate) fn closure_digest(&self) -> &str {
        &self.closure_digest
    }

    /// Sorted paths covered by [`Self::closure_digest`].
    pub(crate) fn closure_paths(&self) -> &[String] {
        &self.closure_paths
    }

    /// Returns the complete approved JS entry-point set in sorted order.
    pub(crate) fn module_ids(&self) -> Vec<String> {
        js_module_ids(&self.snapshot.records)
    }

    /// Describes an approved source in memory without reading Git, for unit
    /// tests whose subject consumes only the commit and tracked path names.
    ///
    /// Every path is recorded as a regular file with a fictional digest.
    #[cfg(test)]
    pub(crate) fn in_memory_for_tests(commit: &CommitId, tracked: &[&str]) -> Self {
        let records: Records = tracked
            .iter()
            .map(|path| ((*path).to_owned(), (Mode::Regular, "0".repeat(64))))
            .collect();
        let fingerprint = digest_records(records.iter());
        Self {
            commit: commit.clone(),
            epoch: 1_700_000_000,
            snapshot: SourceSnapshot {
                root: PathBuf::from("/example/in-memory-source"),
                records,
                fingerprint,
            },
            closure_digest: "0".repeat(64),
            closure_paths: Vec::new(),
        }
    }

    /// SHA-256 of a tracked path's content at the approved commit.
    pub(crate) fn tracked_sha256(&self, path: &str) -> Option<&str> {
        self.snapshot
            .records
            .get(path)
            .map(|(_, sha256)| sha256.as_str())
    }

    /// Approved regular Cargo manifests, keyed by source-relative path.
    ///
    /// Symlink identities describe link targets rather than manifest bytes and
    /// therefore cannot authorize a workspace package's composition origin.
    pub(crate) fn cargo_manifest_digests(&self) -> BTreeMap<PathBuf, String> {
        self.snapshot
            .records
            .iter()
            .filter(|(path, (mode, _))| {
                matches!(mode, Mode::Regular | Mode::Executable)
                    && (path.as_str() == "Cargo.toml" || path.ends_with("/Cargo.toml"))
            })
            .map(|(path, (_, digest))| (PathBuf::from(path), digest.clone()))
            .collect()
    }

    /// Reads bounded tracked bytes and checks their approved object digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Source`] for an untracked name, changed bytes,
    /// or an input rejected by the held-descriptor document reader.
    pub(crate) fn read_tracked(
        &self,
        path: &str,
        role: Role,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        let expected = self
            .tracked_sha256(path)
            .ok_or_else(|| source_error("material input is not an approved tracked file"))?;
        let raw = read_bounded(&self.snapshot.root.join(path), role)?;
        if raw.sha256 != expected {
            return Err(source_error(
                "tracked material input differs from the approved source",
            ));
        }
        Ok(raw.bytes)
    }
}

/// Authenticates the checkout at `root` as exactly the approved commit.
///
/// # Errors
///
/// Fails on an extensionless `rust-toolchain`, an ancestor Cargo
/// configuration layer, unsupported repository layout or configuration, a
/// HEAD other than the commit, any object whose content does not hash to its
/// id, any tracked-path difference, untracked path, or incomplete closure.
pub(crate) fn authenticate(
    root: &Path,
    commit: &CommitId,
    audited: &AuditedEnvironment,
) -> Result<ApprovedSource, Report<ReleaseError>> {
    if !is_absent(&root.join("rust-toolchain"))? {
        return Err(source_error(
            "extensionless rust-toolchain is rejected before toolchain selection",
        ));
    }
    check_ancestor_cargo_config(root)?;
    check_layout(root)?;
    let git = Git::locate(root, audited)?;
    check_head(&git, commit)?;
    let commit_object = read_single(&git, "commit", commit.as_str())?;
    let epoch = committer_epoch(&commit_object)?;
    let entries = walk_tree(&git, &commit_tree(&commit_object)?, &TREE_LIMITS)?;
    let (records, manifest) = tracked_records(&git, &entries)?;
    compare_worktree(root, &records)?;
    let (closure_digest, closure_paths) = helper_closure(&records, &manifest)?;
    let fingerprint = digest_records(records.iter());
    Ok(ApprovedSource {
        commit: commit.clone(),
        epoch,
        snapshot: SourceSnapshot {
            root: root.to_path_buf(),
            records,
            fingerprint,
        },
        closure_digest,
        closure_paths,
    })
}

/// Returns whether `path` is absent; any inspection error other than
/// `NotFound` fails closed rather than being treated as absence.
fn is_absent(path: &Path) -> Result<bool, Report<ReleaseError>> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(source_error(format!(
            "{} cannot be inspected: {error}",
            path.display()
        ))),
    }
}

/// Cargo merges `.cargo/config(.toml)` from every ancestor of its working
/// directory, so any such layer above the source root is rejected.
fn check_ancestor_cargo_config(root: &Path) -> Result<(), Report<ReleaseError>> {
    let canonical = fs::canonicalize(root)
        .map_err(|error| source_error(format!("cannot resolve source root: {error}")))?;
    for ancestor in canonical.ancestors().skip(1) {
        for name in ["config", "config.toml"] {
            let candidate = ancestor.join(".cargo").join(name);
            if !is_absent(&candidate)? {
                return Err(source_error(format!(
                    "ancestor Cargo configuration layer {} is rejected",
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

fn check_layout(root: &Path) -> Result<(), Report<ReleaseError>> {
    let git_dir = root.join(".git");
    let metadata = fs::symlink_metadata(&git_dir)
        .map_err(|error| source_error(format!(".git cannot be inspected: {error}")))?;
    if !metadata.is_dir() {
        return Err(source_error(
            ".git must be a real directory; gitfile, symlink and linked-worktree \
             indirections are unsupported",
        ));
    }
    for relative in FORBIDDEN_GIT_PATHS {
        if !is_absent(&git_dir.join(relative))? {
            return Err(source_error(format!(
                ".git/{relative} is present; repository indirection or rewriting is unsupported"
            )));
        }
    }
    check_packed_refs(&git_dir.join("packed-refs"))?;
    let config = read_bounded(&git_dir.join("config"), Role::Metadata)?;
    let text =
        std::str::from_utf8(&config.bytes).map_err(|_| source_error("git config must be UTF-8"))?;
    check_git_config(text)
}

fn check_packed_refs(packed: &Path) -> Result<(), Report<ReleaseError>> {
    if is_absent(packed)? {
        return Ok(());
    }
    let bytes = read_bounded(packed, Role::Metadata)
        .map_err(|report| report.attach(".git/packed-refs cannot be read"))?
        .bytes;
    let replaced = bytes.split(|byte| *byte == b'\n').any(|line| {
        line.splitn(2, |byte| *byte == b' ')
            .nth(1)
            .is_some_and(|name| name.starts_with(b"refs/replace/"))
    });
    if replaced {
        return Err(source_error(
            ".git/packed-refs lists refs/replace references",
        ));
    }
    Ok(())
}

fn check_head(git: &Git, commit: &CommitId) -> Result<(), Report<ReleaseError>> {
    let object_type = git.run(&["cat-file", "-t", commit.as_str()], 64)?;
    if object_type != b"commit\n" {
        return Err(source_error(format!("{commit} is not a commit object")));
    }
    let head = git.run(&["rev-parse", "--verify", "HEAD"], 128)?;
    if head.strip_suffix(b"\n") != Some(commit.as_str().as_bytes()) {
        return Err(source_error("HEAD does not resolve to the approved commit"));
    }
    Ok(())
}

/// Parses the committer timestamp from a raw commit object header.
///
/// # Errors
///
/// Fails unless the header has exactly one well-formed committer line.
fn committer_epoch(raw: &[u8]) -> Result<u64, Report<ReleaseError>> {
    let end = raw
        .windows(2)
        .position(|window| window == b"\n\n")
        .unwrap_or(raw.len());
    let header = std::str::from_utf8(&raw[..end])
        .map_err(|_| source_error("commit header must be UTF-8"))?;
    let committers: Vec<&str> = header
        .lines()
        .filter_map(|line| line.strip_prefix("committer "))
        .collect();
    let [committer] = committers.as_slice() else {
        return Err(source_error("commit must have exactly one committer line"));
    };
    let invalid = || source_error("committer line must end with `> <epoch> <+|-HHMM>`");
    let (_, stamp) = committer.rsplit_once('>').ok_or_else(invalid)?;
    let (epoch, zone) = stamp
        .strip_prefix(' ')
        .and_then(|stamp| stamp.split_once(' '))
        .ok_or_else(invalid)?;
    let zone_valid = zone.len() == 5
        && zone.starts_with(['+', '-'])
        && zone[1..].bytes().all(|byte| byte.is_ascii_digit())
        && zone[3..] <= *"59";
    if epoch.is_empty() || !epoch.bytes().all(|byte| byte.is_ascii_digit()) || !zone_valid {
        return Err(invalid());
    }
    epoch.parse().map_err(|_| invalid())
}

/// Reads one object of `kind` whose content must hash to `oid`.
fn read_single(git: &Git, kind: &'static str, oid: &str) -> Result<Vec<u8>, Report<ReleaseError>> {
    let mut found = None;
    git.read_objects(kind, &[oid], |_, bytes| {
        found = Some(bytes.to_vec());
        Ok(())
    })?;
    found.ok_or_else(|| source_error(format!("{kind} {oid} was not returned")))
}

/// Returns the root tree id from a verified raw commit object.
fn commit_tree(raw: &[u8]) -> Result<String, Report<ReleaseError>> {
    let first = raw.split(|byte| *byte == b'\n').next().unwrap_or_default();
    std::str::from_utf8(first)
        .ok()
        .and_then(|line| line.strip_prefix("tree "))
        .filter(|oid| lower_hex(oid, 40))
        .map(str::to_owned)
        .ok_or_else(|| source_error("commit object must start with a SHA-1 tree line"))
}

/// Records the verified tracked blob identities and original root manifest.
fn tracked_records(
    git: &Git,
    entries: &[TreeEntry],
) -> Result<(Records, Vec<u8>), Report<ReleaseError>> {
    let mut records = Records::new();
    let mut manifest = None;
    let oids: Vec<&str> = entries.iter().map(|entry| entry.oid.as_str()).collect();
    git.read_objects("blob", &oids, |index, bytes| {
        let entry = &entries[index];
        if entry.mode == Mode::Symlink {
            let reviewed = REVIEWED_SYMLINKS
                .iter()
                .any(|(link, target)| *link == entry.path && target.as_bytes() == bytes);
            if !reviewed {
                return Err(source_error(format!(
                    "tracked symlink {} has an unreviewed target",
                    entry.path
                )));
            }
        }
        if entry.path == "Cargo.toml" {
            manifest = Some(bytes.to_vec());
        }
        records.insert(
            entry.path.clone(),
            (entry.mode, hex::encode(Sha256::digest(bytes))),
        );
        Ok(())
    })?;
    let manifest = manifest.ok_or_else(|| source_error("root Cargo.toml is not tracked"))?;
    Ok((records, manifest))
}

fn digest_records<'a>(records: impl Iterator<Item = (&'a String, &'a (Mode, String))>) -> String {
    let mut hasher = Sha256::new();
    for (path, (mode, sha256)) in records {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(mode.label().as_bytes());
        hasher.update(b"\0");
        hasher.update(sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// Joins a JS input inventory to the approved source commit.
///
/// # Errors
///
/// Fails unless the inventory is strict JSON with the reviewed field set,
/// schema version 1 and a `source_commit` equal to the approved commit.
// Prepared-input composition (Task 9) consumes this join.
#[allow(dead_code)]
pub(crate) fn join_inventory(
    source: &ApprovedSource,
    bytes: &[u8],
) -> Result<(), Report<ReleaseError>> {
    let value = parse_json(bytes, Role::Metadata)?;
    let object = value
        .as_object()
        .ok_or_else(|| source_error("inventory must be an object"))?;
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let reviewed = BTreeSet::from([
        "materials",
        "modules",
        "schema_version",
        "source_commit",
        "tools",
    ]);
    if keys != reviewed {
        return Err(source_error(
            "inventory must contain exactly the reviewed fields",
        ));
    }
    let arrays = ["materials", "modules", "tools"]
        .iter()
        .all(|key| object[*key].is_array());
    if object["schema_version"] != 1 || !arrays {
        return Err(source_error(
            "inventory schema version or field types are invalid",
        ));
    }
    match object["source_commit"].as_str() {
        Some(commit) if commit == source.commit.as_str() => Ok(()),
        _ => Err(source_error(
            "inventory source_commit does not match the approved commit",
        )),
    }
}

#[cfg(test)]
mod tests;
