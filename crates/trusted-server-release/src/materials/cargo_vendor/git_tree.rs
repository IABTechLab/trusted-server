//! Reads a verified loose-object Git closure and materializes one package.
//!
//! [`VerifiedGitMaterial`](crate::materials::git::VerifiedGitMaterial) owns a
//! private store of zlib-compressed loose objects whose SHA-1 identities were
//! already checked. Every object is rehashed again here, so a store edited
//! after verification is rejected instead of vendored.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use error_stack::Report;
use flate2::read::ZlibDecoder;

use super::normalize::AncestorManifest;
use super::writer::PackageWriter;
use crate::error::ReleaseError;
use crate::git_objects::checked_sha1;
use crate::materials::material_error;

/// Largest single object read from the store.
const OBJECT_LIMIT: u64 = 32 * 1024 * 1024;
/// Total object bytes read per vendoring.
const TOTAL_LIMIT: u64 = 512 * 1024 * 1024;
/// Deepest accepted tree nesting.
const DEPTH_LIMIT: usize = 256;
/// Most tree entries visited per vendoring.
const ENTRY_LIMIT: usize = 100_000;
/// Largest manifest examined while looking for a package root.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// One parsed tree entry.
#[derive(Clone, Debug)]
pub(super) struct TreeEntry {
    pub(super) name: String,
    pub(super) directory: bool,
    pub(super) executable: bool,
    symlink: bool,
    pub(super) oid: String,
}

/// A rehashing reader over one verified loose-object store.
pub(super) struct Store<'a> {
    root: &'a Path,
    remaining: Cell<u64>,
    entries: Cell<usize>,
}

fn lower_oid(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Parses inert tree entries, rejecting submodules and unsafe names.
fn parse_tree(mut bytes: &[u8]) -> Result<Vec<TreeEntry>, Report<ReleaseError>> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| material_error("malformed Git tree mode"))?;
        let (directory, executable, symlink) = match &bytes[..space] {
            b"40000" => (true, false, false),
            b"100644" => (false, false, false),
            b"100755" => (false, true, false),
            b"120000" => (false, false, true),
            _ => {
                return Err(material_error(
                    "Git package submodules and noncanonical modes are unreviewed",
                ));
            }
        };
        let rest = &bytes[space + 1..];
        let nul = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| material_error("malformed Git tree name"))?;
        let name = &rest[..nul];
        let unsafe_name = name.is_empty()
            || !name.is_ascii()
            || name
                .iter()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b':'))
            || matches!(name, b"." | b"..")
            || name.eq_ignore_ascii_case(b".git")
            || name.len() > 255;
        if unsafe_name {
            return Err(material_error("unsafe Git package tree name"));
        }
        if symlink && name == b"Cargo.toml" {
            return Err(material_error(
                "Git package manifest aliases are unreviewed",
            ));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(material_error("Git package tree aliases or repeats a name"));
        }
        let oid = rest
            .get(nul + 1..nul + 21)
            .ok_or_else(|| material_error("truncated Git tree identity"))?;
        entries.push(TreeEntry {
            name: String::from_utf8_lossy(name).into_owned(),
            directory,
            executable,
            symlink,
            oid: hex::encode(oid),
        });
        bytes = &rest[nul + 21..];
    }
    Ok(entries)
}

impl<'a> Store<'a> {
    /// Opens a store rooted at the verified material's object directory.
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            remaining: Cell::new(TOTAL_LIMIT),
            entries: Cell::new(0),
        }
    }

    /// Reads and rehashes one object of `kind`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] for a missing, linked, oversize,
    /// malformed or mis-identified object, or an exhausted total budget.
    pub(super) fn read(
        &self,
        kind: &str,
        oid: &str,
        limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        if !lower_oid(oid) {
            return Err(material_error("Git object id is not canonical"));
        }
        let path = self.root.join(&oid[..2]).join(&oid[2..]);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| {
                material_error(format!("Git object {oid} cannot be opened: {error}"))
            })?;
        let metadata = file.metadata().map_err(material_error)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(material_error("Git object is not one regular file"));
        }
        let allowed = limit.min(OBJECT_LIMIT);
        let mut inflated = Vec::new();
        ZlibDecoder::new(file)
            .take(allowed + 64)
            .read_to_end(&mut inflated)
            .map_err(|error| material_error(format!("Git object {oid} is not zlib: {error}")))?;
        let nul = inflated
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| material_error("Git object has no header terminator"))?;
        let header = std::str::from_utf8(&inflated[..nul]).map_err(material_error)?;
        let (found_kind, length) = header
            .split_once(' ')
            .ok_or_else(|| material_error("Git object header is malformed"))?;
        let length: u64 = length
            .parse()
            .ok()
            .filter(|length: &u64| length.to_string() == header[found_kind.len() + 1..])
            .ok_or_else(|| material_error("Git object length is not canonical"))?;
        let body = &inflated[nul + 1..];
        if found_kind != kind || length != body.len() as u64 || length > allowed {
            return Err(material_error(
                "Git object kind or length differs from its reference or limit",
            ));
        }
        let remaining = self
            .remaining
            .get()
            .checked_sub(length)
            .ok_or_else(|| material_error("Git package reads exceed the total byte budget"))?;
        self.remaining.set(remaining);
        if checked_sha1(&[&inflated[..=nul], body])? != oid {
            return Err(material_error(
                "Git object bytes do not match their identity",
            ));
        }
        Ok(inflated.split_off(nul + 1))
    }

    fn tree(&self, oid: &str) -> Result<Vec<TreeEntry>, Report<ReleaseError>> {
        let entries = parse_tree(&self.read("tree", oid, OBJECT_LIMIT)?)?;
        let visited = self.entries.get() + entries.len();
        if visited > ENTRY_LIMIT {
            return Err(material_error("Git package tree exceeds the entry limit"));
        }
        self.entries.set(visited);
        Ok(entries)
    }

    /// Resolves only direct relative file links through authenticated tree objects.
    fn file_link(
        &self,
        repository_tree: &str,
        path: &str,
        target: &[u8],
    ) -> Result<(Vec<u8>, bool), Report<ReleaseError>> {
        if target.is_empty()
            || target.len() > 4096
            || !target.is_ascii()
            || target.starts_with(b"/")
            || target
                .iter()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
        {
            return Err(material_error("unsafe Git file link target"));
        }
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let mut parts: Vec<&str> = parent.split('/').filter(|part| !part.is_empty()).collect();
        let target = std::str::from_utf8(target).map_err(material_error)?;
        if matches!(target.rsplit('/').next(), Some("" | "." | "..")) {
            return Err(material_error("Git file link must name a file"));
        }
        let mut descended = false;
        for part in target.split('/') {
            match part {
                "" => return Err(material_error("empty Git file link target component")),
                "." => {}
                ".." => {
                    if descended {
                        return Err(material_error(
                            "Git file link traverses a parent after descending",
                        ));
                    }
                    if parts.pop().is_none() {
                        return Err(material_error("Git file link escapes its repository"));
                    }
                }
                _ => {
                    descended = true;
                    parts.push(part);
                }
            }
        }
        if parts.is_empty() || parts.len() > DEPTH_LIMIT || parts.join("/").len() > 4096 {
            return Err(material_error("Git file link target exceeds path limits"));
        }
        let mut tree = repository_tree.to_owned();
        for (index, part) in parts.iter().enumerate() {
            let entry = self
                .tree(&tree)?
                .into_iter()
                .find(|entry| entry.name == *part)
                .ok_or_else(|| material_error("Git file link target is missing"))?;
            if index + 1 == parts.len() {
                if entry.directory || entry.symlink {
                    return Err(material_error(
                        "Git file link must target a direct regular file",
                    ));
                }
                return Ok((
                    self.read("blob", &entry.oid, OBJECT_LIMIT)?,
                    entry.executable,
                ));
            }
            if !entry.directory {
                return Err(material_error(
                    "Git file link parent is not a direct directory",
                ));
            }
            tree = entry.oid;
        }
        Err(material_error("Git file link target is missing"))
    }
}

/// The unique directory of the requested package inside a repository tree.
#[derive(Clone, Debug)]
pub(super) struct PackageRoot {
    pub(super) tree: String,
    pub(super) manifest: String,
    pub(super) path: String,
}

/// Returns the package name a manifest declares, if it parses as a manifest.
fn declared_name(manifest: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(manifest).ok()?;
    let table: toml::Table = toml::from_str(text).ok()?;
    table
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

/// Finds the only directory whose `Cargo.toml` declares package `name`.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] when no directory or more than one
/// directory declares the package, or the tree exceeds a limit. Manifests
/// that do not parse are not candidates; Cargo could not use them either.
pub(super) fn find_package(
    store: &Store<'_>,
    root_tree: &str,
    name: &str,
) -> Result<PackageRoot, Report<ReleaseError>> {
    let mut candidates = Vec::new();
    let mut pending = vec![(root_tree.to_owned(), String::new(), 1_usize)];
    while let Some((tree, path, depth)) = pending.pop() {
        if depth > DEPTH_LIMIT {
            return Err(material_error("Git package tree exceeds the depth limit"));
        }
        for entry in store.tree(&tree)? {
            let child = if path.is_empty() {
                entry.name.clone()
            } else {
                format!("{path}/{}", entry.name)
            };
            if entry.directory {
                pending.push((entry.oid, child, depth + 1));
            } else if entry.name == "Cargo.toml" {
                let manifest = store.read("blob", &entry.oid, MANIFEST_LIMIT)?;
                if declared_name(&manifest).as_deref() == Some(name) {
                    candidates.push(PackageRoot {
                        tree: tree.clone(),
                        manifest: entry.oid,
                        path: path.clone(),
                    });
                }
            }
        }
    }
    match candidates.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(material_error(format!(
            "no Git directory declares package {name}"
        ))),
        _ => Err(material_error(format!(
            "ambiguous Git package roots: several directories declare package {name}"
        ))),
    }
}

/// Reads every `Cargo.toml` in the directories above `root`, nearest first.
///
/// The walk descends from the repository root along the package path within
/// the same verified tree, so a workspace root can only come from that tree.
/// The package directory's own manifest is not included.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] when a directory on the package path
/// is missing or not a tree, or an object cannot be read.
pub(super) fn ancestor_manifests(
    store: &Store<'_>,
    root_tree: &str,
    root: &PackageRoot,
) -> Result<Vec<AncestorManifest>, Report<ReleaseError>> {
    let mut found = Vec::new();
    let mut tree = root_tree.to_owned();
    let mut directory = String::new();
    let parts: Vec<&str> = root
        .path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() >= DEPTH_LIMIT {
        return Err(material_error("Git package tree exceeds the depth limit"));
    }
    for part in parts {
        let entries = store.tree(&tree)?;
        if let Some(manifest) = entries
            .iter()
            .find(|entry| !entry.directory && entry.name == "Cargo.toml")
        {
            found.push(AncestorManifest {
                directory: directory.clone(),
                bytes: store.read("blob", &manifest.oid, MANIFEST_LIMIT)?,
            });
        }
        tree = entries
            .into_iter()
            .find(|entry| entry.directory && entry.name == part)
            .map(|entry| entry.oid)
            .ok_or_else(|| {
                material_error("Git package path is not a tree in the verified closure")
            })?;
        directory = if directory.is_empty() {
            part.to_owned()
        } else {
            format!("{directory}/{part}")
        };
    }
    if tree != root.tree {
        return Err(material_error(
            "Git package path does not lead to the package tree",
        ));
    }
    found.reverse();
    Ok(found)
}

/// Writes the package tree below `root.tree` through `writer`.
///
/// Subdirectories with their own `Cargo.toml` are separate packages and are
/// skipped, as Cargo's own file listing does. The root `Cargo.toml` is not
/// written here: the caller decides which manifest bytes represent it.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] for an unreadable object or write.
pub(super) fn materialize(
    store: &Store<'_>,
    repository_tree: &str,
    root: &PackageRoot,
    writer: &mut PackageWriter,
) -> Result<(), Report<ReleaseError>> {
    let mut pending = vec![(store.tree(&root.tree)?, String::new(), 1_usize)];
    while let Some((entries, prefix, depth)) = pending.pop() {
        if depth > DEPTH_LIMIT {
            return Err(material_error("Git package tree exceeds the depth limit"));
        }
        for entry in &entries {
            let relative = format!("{prefix}{}", entry.name);
            if entry.directory {
                let child = store.tree(&entry.oid)?;
                let nested_package = child
                    .iter()
                    .any(|inner| !inner.directory && inner.name == "Cargo.toml");
                if !nested_package {
                    pending.push((child, format!("{relative}/"), depth + 1));
                }
            } else if relative != "Cargo.toml" {
                let (bytes, executable) = if entry.symlink {
                    let target = store.read("blob", &entry.oid, 4096)?;
                    let path = if root.path.is_empty() {
                        relative.clone()
                    } else {
                        format!("{}/{relative}", root.path)
                    };
                    store.file_link(repository_tree, &path, &target)?
                } else {
                    (
                        store.read("blob", &entry.oid, OBJECT_LIMIT)?,
                        entry.executable,
                    )
                };
                writer.write(&relative, &bytes, executable)?;
            }
        }
    }
    Ok(())
}
