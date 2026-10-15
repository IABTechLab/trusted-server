//! Local authentication of the approved release source checkout.
//!
//! The repository layout and configuration are inspected with bounded direct
//! reads before Git runs, because Git itself would execute or redirect through
//! several configuration channels. Only read-only plumbing then runs under a
//! cleared environment. Remote branch-tip ancestry and ruleset validation are
//! hosted operations (Task 14) and are not established here.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::documents::{Role, lower_hex, parse_json, read_bounded};
use crate::error::ReleaseError;
use crate::preflight::AuditedEnvironment;
use git::Git;

mod git;

const PROBLEMS_SHOWN: usize = 20;
const RELEASE_CRATE_PREFIX: &str = "crates/trusted-server-release/";

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

/// Untracked coordination paths reviewed for release sources. Empty for now.
const REVIEWED_UNTRACKED: [&str; 0] = [];

/// Root files always included in the helper-source closure.
const CLOSURE_FILES: [&str; 6] = [
    ".cargo/config.toml",
    ".tool-versions",
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "scripts/release/bootstrap.sh",
];

/// Path-dependency manifests that must be listed as workspace members.
const REQUIRED_MEMBER_MANIFESTS: [&str; 1] = ["crates/trusted-server-openrtb/Cargo.toml"];

fn source_error(message: impl Into<String>) -> Report<ReleaseError> {
    Report::new(ReleaseError::Source).attach(message.into())
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
    commit: String,
    epoch: u64,
    snapshot: SourceSnapshot,
    closure_digest: String,
    closure_paths: Vec<String>,
}

impl ApprovedSource {
    /// The approved 40-hex source commit.
    pub(crate) fn commit(&self) -> &str {
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

    /// SHA-256 of a tracked path's content at the approved commit.
    pub(crate) fn tracked_sha256(&self, path: &str) -> Option<&str> {
        self.snapshot
            .records
            .get(path)
            .map(|(_, sha256)| sha256.as_str())
    }
}

/// Authenticates the checkout at `root` as exactly the approved commit.
///
/// # Errors
///
/// Fails on an extensionless `rust-toolchain`, malformed commit, unsupported
/// repository layout or configuration, a HEAD other than the commit, any
/// tracked-path difference, untracked path, or incomplete helper closure.
pub(crate) fn authenticate(
    root: &Path,
    commit: &str,
    audited: &AuditedEnvironment,
) -> Result<ApprovedSource, Report<ReleaseError>> {
    match fs::symlink_metadata(root.join("rust-toolchain")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(source_error(
                "extensionless rust-toolchain is rejected before toolchain selection",
            ));
        }
    }
    if !lower_hex(commit, 40) {
        return Err(source_error(
            "approved source commit must be exactly 40 lowercase hex digits",
        ));
    }
    check_layout(root)?;
    let git = Git::locate(root, audited)?;
    let object_type = git.run(&["cat-file", "-t", commit])?;
    if object_type != b"commit\n" {
        return Err(source_error(format!("{commit} is not a commit object")));
    }
    let head = git.run(&["rev-parse", "--verify", "HEAD"])?;
    if head.strip_suffix(b"\n") != Some(commit.as_bytes()) {
        return Err(source_error("HEAD does not resolve to the approved commit"));
    }
    let epoch = committer_epoch(&git.run(&["cat-file", "commit", commit])?)?;
    let entries = parse_tree(&git.run(&["ls-tree", "-r", "-z", "--full-tree", commit])?)?;
    let (records, manifest) = tracked_records(&git, &entries)?;
    compare_worktree(root, &records)?;
    let (closure_digest, closure_paths) = helper_closure(&records, &manifest)?;
    let fingerprint = digest_records(records.iter());
    Ok(ApprovedSource {
        commit: commit.to_owned(),
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
        match fs::symlink_metadata(git_dir.join(relative)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(source_error(format!(
                    ".git/{relative} is present; repository indirection or rewriting is unsupported"
                )));
            }
            Err(error) => {
                return Err(source_error(format!(
                    ".git/{relative} cannot be inspected: {error}"
                )));
            }
        }
    }
    let packed = git_dir.join("packed-refs");
    if fs::symlink_metadata(&packed).is_ok() {
        let bytes = read_bounded(&packed, Role::Metadata)?.bytes;
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
    }
    let config = read_bounded(&git_dir.join("config"), Role::Metadata)?;
    let text =
        std::str::from_utf8(&config.bytes).map_err(|_| source_error("git config must be UTF-8"))?;
    check_git_config(text)
}

/// Accepts only allowlisted repository configuration keys and values.
///
/// # Errors
///
/// Fails on any key outside the allowlist, escapes or continuations, unknown
/// section syntax, or unreviewed values. Values are never echoed.
fn check_git_config(text: &str) -> Result<(), Report<ReleaseError>> {
    let mut section: Option<(String, bool)> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let number = index + 1;
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.contains('\\') {
            return Err(source_error(format!(
                "git config line {number} uses escapes or continuations"
            )));
        }
        if let Some(header) = line.strip_prefix('[') {
            section = Some(config_section(header, number)?);
            continue;
        }
        let (name, subsection) = section.as_ref().ok_or_else(|| {
            source_error(format!("git config line {number} is outside a section"))
        })?;
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim().to_ascii_lowercase(), Some(value.trim())),
            None => (line.to_ascii_lowercase(), None),
        };
        if !config_value_allowed(name, *subsection, &key, value) {
            return Err(source_error(format!(
                "git config key {name}.{key} on line {number} is not allowlisted or has an unreviewed value"
            )));
        }
    }
    Ok(())
}

fn config_section(header: &str, number: usize) -> Result<(String, bool), Report<ReleaseError>> {
    let invalid = || {
        source_error(format!(
            "git config line {number} has an unsupported section"
        ))
    };
    let header = header.strip_suffix(']').ok_or_else(invalid)?;
    let (name, subsection) = match header.split_once(' ') {
        Some((name, quoted)) => {
            let inner = quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .ok_or_else(invalid)?;
            if inner.contains('"') {
                return Err(invalid());
            }
            (name, true)
        }
        None => (header, false),
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid());
    }
    Ok((name.to_ascii_lowercase(), subsection))
}

fn config_value_allowed(section: &str, subsection: bool, key: &str, value: Option<&str>) -> bool {
    let boolean = matches!(value, Some("true" | "false"));
    let plain = value.is_some_and(|value| !value.is_empty() && !value.contains(['"', '#', ';']));
    match (section, subsection, key) {
        ("core", false, "repositoryformatversion") => value == Some("0"),
        ("core", false, "bare") => value == Some("false"),
        (
            "core",
            false,
            "filemode" | "logallrefupdates" | "ignorecase" | "precomposeunicode" | "symlinks",
        ) => boolean,
        ("remote", true, "url" | "fetch") | ("branch", true, "remote" | "merge") => plain,
        _ => false,
    }
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
        && zone[1..].bytes().all(|byte| byte.is_ascii_digit());
    if epoch.is_empty() || !epoch.bytes().all(|byte| byte.is_ascii_digit()) || !zone_valid {
        return Err(invalid());
    }
    epoch.parse().map_err(|_| invalid())
}

struct TreeEntry {
    path: String,
    mode: Mode,
    oid: String,
}

fn parse_tree(output: &[u8]) -> Result<Vec<TreeEntry>, Report<ReleaseError>> {
    let mut entries = Vec::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let text =
            std::str::from_utf8(record).map_err(|_| source_error("tracked paths must be UTF-8"))?;
        let (meta, path) = text
            .split_once('\t')
            .ok_or_else(|| source_error("git ls-tree record is malformed"))?;
        check_tracked_path(path)?;
        let fields: Vec<&str> = meta.split(' ').collect();
        let [mode, kind, oid] = fields.as_slice() else {
            return Err(source_error("git ls-tree record is malformed"));
        };
        let mode = match (*mode, *kind) {
            ("100644", "blob") => Mode::Regular,
            ("100755", "blob") => Mode::Executable,
            ("120000", "blob") => Mode::Symlink,
            ("160000", _) => {
                return Err(source_error(format!(
                    "tracked submodule {path} is not reviewed"
                )));
            }
            _ => {
                return Err(source_error(format!(
                    "tracked path {path} has unsupported mode {mode}"
                )));
            }
        };
        if !lower_hex(oid, 40) {
            return Err(source_error("git ls-tree object id is malformed"));
        }
        if mode == Mode::Symlink && !REVIEWED_SYMLINKS.iter().any(|(link, _)| *link == path) {
            return Err(source_error(format!(
                "tracked symlink {path} is not reviewed"
            )));
        }
        entries.push(TreeEntry {
            path: path.to_owned(),
            mode,
            oid: (*oid).to_owned(),
        });
    }
    Ok(entries)
}

fn check_tracked_path(path: &str) -> Result<(), Report<ReleaseError>> {
    let unsafe_component = path.split('/').any(|component| {
        component.is_empty()
            || component == "."
            || component == ".."
            || component.eq_ignore_ascii_case(".git")
    });
    if unsafe_component {
        return Err(source_error("tracked path has an unsafe component"));
    }
    Ok(())
}

fn tracked_records(
    git: &Git,
    entries: &[TreeEntry],
) -> Result<(Records, Vec<u8>), Report<ReleaseError>> {
    let mut records = Records::new();
    let mut manifest = None;
    git.read_blobs(entries, |entry, bytes| {
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

fn compare_worktree(root: &Path, expected: &Records) -> Result<(), Report<ReleaseError>> {
    let mut ancestors = BTreeSet::new();
    for path in expected.keys() {
        let mut prefix = path.as_str();
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            ancestors.insert(parent);
            prefix = parent;
        }
    }
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let mut directories = vec![String::new()];
    while let Some(directory) = directories.pop() {
        let absolute = root.join(&directory);
        let mut names = fs::read_dir(&absolute)
            .and_then(|entries| {
                entries
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<io::Result<Vec<OsString>>>()
            })
            .map_err(|error| source_error(format!("cannot list {directory:?}: {error}")))?;
        names.sort();
        for name in names {
            let Some(name) = name.to_str() else {
                problems.push(format!(
                    "untracked non-UTF-8 path {}",
                    name.to_string_lossy()
                ));
                continue;
            };
            let relative = if directory.is_empty() {
                name.to_owned()
            } else {
                format!("{directory}/{name}")
            };
            if relative == ".git" {
                continue;
            }
            observe_path(
                root,
                &relative,
                expected,
                &ancestors,
                &mut directories,
                &mut problems,
            );
            seen.insert(relative);
        }
    }
    for path in expected.keys().filter(|path| !seen.contains(*path)) {
        problems.push(format!("tracked path {path} is missing"));
    }
    if problems.is_empty() {
        return Ok(());
    }
    let count = problems.len();
    problems.truncate(PROBLEMS_SHOWN);
    Err(source_error(format!(
        "worktree differs from the approved commit ({count} problems): {}",
        problems.join("; ")
    )))
}

fn observe_path(
    root: &Path,
    relative: &str,
    expected: &Records,
    ancestors: &BTreeSet<&str>,
    directories: &mut Vec<String>,
    problems: &mut Vec<String>,
) {
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            problems.push(format!("{relative} cannot be inspected: {error}"));
            return;
        }
    };
    if metadata.is_dir() && ancestors.contains(relative) {
        directories.push(relative.to_owned());
        return;
    }
    let Some(record) = expected.get(relative) else {
        if !REVIEWED_UNTRACKED.contains(&relative) {
            problems.push(format!("untracked path {relative}"));
        }
        return;
    };
    let observed = if metadata.file_type().is_symlink() {
        fs::read_link(&path)
            .map(|target| {
                (
                    Mode::Symlink,
                    hex::encode(Sha256::digest(target.as_os_str().as_bytes())),
                )
            })
            .map_err(|error| error.to_string())
    } else if metadata.is_file() && metadata.nlink() != 1 {
        Err(format!("has {} hard links", metadata.nlink()))
    } else if metadata.is_file() {
        let mode = if metadata.permissions().mode() & 0o100 == 0 {
            Mode::Regular
        } else {
            Mode::Executable
        };
        read_bounded(&path, Role::Executable)
            .map(|raw| (mode, raw.sha256))
            .map_err(|report| format!("{report:?}"))
    } else {
        Err("has an unsupported file type".to_owned())
    };
    match observed {
        Ok(observed) if observed == *record => {}
        Ok(_) => problems.push(format!(
            "{relative} differs from the approved commit in mode, type or content"
        )),
        Err(reason) => problems.push(format!("{relative} {reason}")),
    }
}

fn helper_closure(
    records: &Records,
    manifest: &[u8],
) -> Result<(String, Vec<String>), Report<ReleaseError>> {
    let text =
        std::str::from_utf8(manifest).map_err(|_| source_error("root Cargo.toml must be UTF-8"))?;
    let table: toml::Table = toml::from_str(text)
        .map_err(|error| source_error(format!("root Cargo.toml is invalid: {error}")))?;
    let members = table
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| source_error("root Cargo.toml must list [workspace] members"))?;
    let mut paths = BTreeSet::new();
    let mut member_manifests = BTreeSet::new();
    for member in members {
        let member = member
            .as_str()
            .ok_or_else(|| source_error("workspace members must be strings"))?;
        if member.contains(['*', '?', '[', ']']) {
            return Err(source_error(format!(
                "workspace member {member} uses a glob; the closure requires explicit members"
            )));
        }
        let manifest = format!("{member}/Cargo.toml");
        check_tracked_path(&manifest)?;
        if !records.contains_key(&manifest) {
            return Err(source_error(format!(
                "workspace member manifest {manifest} is not tracked"
            )));
        }
        member_manifests.insert(manifest);
    }
    for required in REQUIRED_MEMBER_MANIFESTS {
        if !member_manifests.contains(required) {
            return Err(source_error(format!(
                "{required} must be a listed workspace member manifest"
            )));
        }
    }
    for path in CLOSURE_FILES {
        if !records.contains_key(path) {
            return Err(source_error(format!(
                "helper closure file {path} is not tracked"
            )));
        }
        paths.insert(path.to_owned());
    }
    paths.extend(member_manifests);
    paths.extend(
        records
            .keys()
            .filter(|path| path.starts_with(RELEASE_CRATE_PREFIX))
            .cloned(),
    );
    let digest = digest_records(paths.iter().filter_map(|path| records.get_key_value(path)));
    Ok((digest, paths.into_iter().collect()))
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
    if keys
        != BTreeSet::from([
            "materials",
            "modules",
            "schema_version",
            "source_commit",
            "tools",
        ])
    {
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
        Some(commit) if commit == source.commit => Ok(()),
        _ => Err(source_error(
            "inventory source_commit does not match the approved commit",
        )),
    }
}

#[cfg(test)]
mod tests;
