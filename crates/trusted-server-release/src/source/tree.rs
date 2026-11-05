//! Verified walking of Git tree objects in their canonical binary form.

use std::collections::BTreeSet;

use error_stack::Report;

use super::git::Git;
use super::{Mode, REVIEWED_SYMLINKS, source_error};
use crate::error::ReleaseError;

const TREE_MODE: &[u8] = b"40000";

/// One tracked non-tree entry at the approved commit.
#[derive(Debug)]
pub(super) struct TreeEntry {
    pub(super) path: String,
    pub(super) mode: Mode,
    pub(super) oid: String,
}

/// Bounds for walking the approved commit's trees.
#[derive(Clone, Copy)]
pub(super) struct TreeLimits {
    /// Maximum tree levels, counting the root tree as level one.
    pub(super) depth: usize,
    /// Maximum blob and subtree entries across all trees.
    pub(super) entries: usize,
    /// Maximum tree objects read.
    pub(super) objects: usize,
}

/// Production tree bounds, far above the current repository's size.
pub(super) const TREE_LIMITS: TreeLimits = TreeLimits {
    depth: 256,
    entries: 1_000_000,
    objects: 100_000,
};

/// Walks every tree from `root_tree`, rehashing each tree object and parsing
/// Git's binary tree format directly instead of trusting `ls-tree` output.
pub(super) fn walk_tree(
    git: &Git,
    root_tree: &str,
    limits: &TreeLimits,
) -> Result<Vec<TreeEntry>, Report<ReleaseError>> {
    let mut entries = Vec::new();
    let mut level = vec![(String::new(), root_tree.to_owned())];
    let mut depth = 0;
    let mut objects = 0;
    let mut subtrees_seen = 0;
    while !level.is_empty() {
        depth += 1;
        objects += level.len();
        if depth > limits.depth {
            return Err(source_error("tracked tree exceeds the depth limit"));
        }
        if objects > limits.objects {
            return Err(source_error("tracked tree exceeds the tree object limit"));
        }
        let oids: Vec<&str> = level.iter().map(|(_, oid)| oid.as_str()).collect();
        let mut next = Vec::new();
        git.read_objects("tree", &oids, |index, bytes| {
            parse_tree_object(&level[index].0, bytes, &mut entries, &mut next)?;
            if entries.len() + subtrees_seen + next.len() > limits.entries {
                return Err(source_error("tracked tree exceeds the entry limit"));
            }
            Ok(())
        })?;
        subtrees_seen += next.len();
        level = next;
    }
    Ok(entries)
}

/// One raw entry decoded from a tree object.
struct RawEntry<'a> {
    mode: &'a [u8],
    name: &'a str,
    oid: String,
}

/// Parses one binary tree object in Git's canonical form.
///
/// Entries must use canonical modes and names and appear in strictly
/// increasing order, comparing tree names as if suffixed by `/`; this also
/// rejects duplicate names, including a file and tree sharing one name.
pub(super) fn parse_tree_object(
    prefix: &str,
    mut bytes: &[u8],
    entries: &mut Vec<TreeEntry>,
    subtrees: &mut Vec<(String, String)>,
) -> Result<(), Report<ReleaseError>> {
    let mut order = TreeOrder::default();
    while !bytes.is_empty() {
        let entry = next_entry(&mut bytes, prefix)?;
        order.admit(&entry, prefix)?;
        let path = if prefix.is_empty() {
            entry.name.to_owned()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        check_tracked_path(&path)?;
        match classify_mode(entry.mode, &path)? {
            None => subtrees.push((path, entry.oid)),
            Some(mode) => entries.push(TreeEntry {
                path,
                mode,
                oid: entry.oid,
            }),
        }
    }
    Ok(())
}

fn next_entry<'a>(
    bytes: &mut &'a [u8],
    prefix: &str,
) -> Result<RawEntry<'a>, Report<ReleaseError>> {
    let malformed = || source_error(format!("tree object for {prefix:?} is malformed"));
    let data: &'a [u8] = bytes;
    let space = data
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or_else(malformed)?;
    let rest = &data[space + 1..];
    let nul = rest
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(malformed)?;
    let raw_oid = rest.get(nul + 1..nul + 21).ok_or_else(malformed)?;
    let name = std::str::from_utf8(&rest[..nul])
        .map_err(|_| source_error("tracked paths must be UTF-8"))?;
    if name.is_empty() || name.contains('/') {
        return Err(malformed());
    }
    *bytes = &rest[nul + 21..];
    Ok(RawEntry {
        mode: &data[..space],
        name,
        oid: hex::encode(raw_oid),
    })
}

/// Enforces Git's canonical strictly increasing tree entry order.
#[derive(Default)]
struct TreeOrder<'a> {
    previous: Option<Vec<u8>>,
    names: BTreeSet<&'a str>,
}

impl<'a> TreeOrder<'a> {
    fn admit(&mut self, entry: &RawEntry<'a>, prefix: &str) -> Result<(), Report<ReleaseError>> {
        if !self.names.insert(entry.name) {
            return Err(source_error(format!(
                "tree object for {prefix:?} repeats the name {}",
                entry.name
            )));
        }
        let mut key = entry.name.as_bytes().to_vec();
        if entry.mode == TREE_MODE {
            key.push(b'/');
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| *previous >= key)
        {
            return Err(source_error(format!(
                "tree object for {prefix:?} is not in canonical order"
            )));
        }
        self.previous = Some(key);
        Ok(())
    }
}

/// Maps a canonical mode to a tracked file mode, or `None` for a subtree.
fn classify_mode(mode: &[u8], path: &str) -> Result<Option<Mode>, Report<ReleaseError>> {
    let mode = match mode {
        TREE_MODE => return Ok(None),
        b"100644" => Mode::Regular,
        b"100755" => Mode::Executable,
        b"120000" => Mode::Symlink,
        b"160000" => {
            return Err(source_error(format!(
                "tracked submodule {path} is not reviewed"
            )));
        }
        _ => {
            return Err(source_error(format!(
                "tracked path {path} has an unsupported mode"
            )));
        }
    };
    if mode == Mode::Symlink && !REVIEWED_SYMLINKS.iter().any(|(link, _)| *link == path) {
        return Err(source_error(format!(
            "tracked symlink {path} is not reviewed"
        )));
    }
    Ok(Some(mode))
}

/// Rejects unsafe tracked path components.
///
/// Components must be ASCII: case- and normalization-folding filesystems
/// (HFS+/APFS Unicode normalization, NTFS and FAT aliasing) can map distinct
/// non-ASCII tree names onto one worktree path, and no tracked path currently
/// needs them.
pub(super) fn check_tracked_path(path: &str) -> Result<(), Report<ReleaseError>> {
    if !path.is_ascii() {
        return Err(source_error(
            "tracked path components must be ASCII to avoid filesystem name aliasing",
        ));
    }
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
