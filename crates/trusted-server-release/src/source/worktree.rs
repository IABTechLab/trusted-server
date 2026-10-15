//! Comparison of the checked-out worktree against approved tracked records.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::{Mode, Records, source_error};
use crate::documents::{Role, read_bounded};
use crate::error::ReleaseError;

const PROBLEMS_SHOWN: usize = 20;

/// Untracked coordination paths reviewed for release sources. Empty for now.
const REVIEWED_UNTRACKED: [&str; 0] = [];

/// Walks the worktree without following symlinks and fails on any difference.
pub(super) fn compare_worktree(
    root: &Path,
    expected: &Records,
) -> Result<(), Report<ReleaseError>> {
    let mut scan = Scan {
        root,
        expected,
        ancestors: ancestor_directories(expected),
        directories: vec![String::new()],
        problems: Vec::new(),
        seen: BTreeSet::new(),
    };
    while let Some(directory) = scan.directories.pop() {
        for name in list_directory(root, &directory)? {
            scan.visit(&directory, &name);
        }
    }
    let missing: Vec<String> = expected
        .keys()
        .filter(|path| !scan.seen.contains(*path))
        .map(|path| format!("tracked path {path} is missing"))
        .collect();
    scan.problems.extend(missing);
    report_problems(scan.problems)
}

/// Every proper ancestor directory of a tracked path.
fn ancestor_directories(expected: &Records) -> BTreeSet<&str> {
    let mut ancestors = BTreeSet::new();
    for path in expected.keys() {
        let mut prefix = path.as_str();
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            ancestors.insert(parent);
            prefix = parent;
        }
    }
    ancestors
}

fn list_directory(root: &Path, directory: &str) -> Result<Vec<OsString>, Report<ReleaseError>> {
    let mut names = fs::read_dir(root.join(directory))
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<Vec<OsString>>>()
        })
        .map_err(|error| source_error(format!("cannot list {directory:?}: {error}")))?;
    names.sort();
    Ok(names)
}

fn report_problems(mut problems: Vec<String>) -> Result<(), Report<ReleaseError>> {
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

struct Scan<'a> {
    root: &'a Path,
    expected: &'a Records,
    ancestors: BTreeSet<&'a str>,
    directories: Vec<String>,
    problems: Vec<String>,
    seen: BTreeSet<String>,
}

impl Scan<'_> {
    fn visit(&mut self, directory: &str, name: &OsString) {
        let Some(name) = name.to_str() else {
            self.problems.push(format!(
                "untracked non-UTF-8 path {}",
                name.to_string_lossy()
            ));
            return;
        };
        let relative = if directory.is_empty() {
            name.to_owned()
        } else {
            format!("{directory}/{name}")
        };
        if relative == ".git" {
            return;
        }
        self.observe(&relative);
        self.seen.insert(relative);
    }

    fn observe(&mut self, relative: &str) {
        let path = self.root.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.problems
                    .push(format!("{relative} cannot be inspected: {error}"));
                return;
            }
        };
        if metadata.is_dir() && self.ancestors.contains(relative) {
            self.directories.push(relative.to_owned());
            return;
        }
        let Some(record) = self.expected.get(relative) else {
            if !REVIEWED_UNTRACKED.contains(&relative) {
                self.problems.push(format!("untracked path {relative}"));
            }
            return;
        };
        match observe_record(&path, &metadata) {
            Ok(observed) if observed == *record => {}
            Ok(_) => self.problems.push(format!(
                "{relative} differs from the approved commit in mode, type or content"
            )),
            Err(reason) => self.problems.push(format!("{relative} {reason}")),
        }
    }
}

/// Observes the mode and content digest of one tracked worktree path.
fn observe_record(path: &Path, metadata: &Metadata) -> Result<(Mode, String), String> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|error| error.to_string())?;
        return Ok((
            Mode::Symlink,
            hex::encode(Sha256::digest(target.as_os_str().as_bytes())),
        ));
    }
    if !metadata.is_file() {
        return Err("has an unsupported file type".to_owned());
    }
    if metadata.nlink() != 1 {
        return Err(format!("has {} hard links", metadata.nlink()));
    }
    let mode = if metadata.permissions().mode() & 0o100 == 0 {
        Mode::Regular
    } else {
        Mode::Executable
    };
    read_bounded(path, Role::Executable)
        .map(|raw| (mode, raw.sha256))
        .map_err(|report| format!("{report:?}"))
}
