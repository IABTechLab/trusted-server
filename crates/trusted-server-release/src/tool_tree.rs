//! Content digest of a selected tool tree, shared by the JS and Rust recipes.
//!
//! The digest covers every entry's relative path, kind, permission bits and,
//! for regular files, size and SHA-256. It rejects anything a later swap could
//! hide behind: hard-linked files (a second path can rewrite the content),
//! absolute or root-escaping symlinks, dangling symlinks, and special files.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;

/// Per-file byte limit while hashing a tool tree.
const FILE_BYTE_LIMIT: u64 = 512 * 1024 * 1024;
/// Total byte limit across one tool tree.
const TREE_BYTE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// Entry limit across one tool tree.
const ENTRY_LIMIT: usize = 100_000;

fn tree_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Material).attach(detail.to_string())
}

/// Hashes the whole tree below `root`, calling `check` between entries and
/// file chunks so the owning attempt can cancel or expire it.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] for a hard-linked or special file, a
/// non-UTF-8 name, an absolute, dangling or root-escaping symlink, an
/// exceeded byte or entry limit, or when `check` fails.
pub(crate) fn tool_tree_digest(
    root: &Path,
    check: &dyn Fn() -> Result<(), Report<ReleaseError>>,
) -> Result<String, Report<ReleaseError>> {
    let mut digest = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut pending = vec![root.to_owned()];
    let mut facts = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(tree_error)? {
            check()?;
            let entry = entry.map_err(tree_error)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(tree_error)?
                .to_str()
                .ok_or_else(|| tree_error("tool path is not UTF-8"))?
                .to_owned();
            let metadata = fs::symlink_metadata(&path).map_err(tree_error)?;
            let fact = if metadata.is_dir() {
                pending.push(path.clone());
                format!("directory:{}", metadata.permissions().mode() & 0o7777)
            } else if metadata.is_file() && metadata.nlink() == 1 {
                let raw = hash_bounded_with_check(&path, FILE_BYTE_LIMIT, check)?;
                total_bytes += raw.size;
                if total_bytes > TREE_BYTE_LIMIT {
                    return Err(tree_error("selected tool tree exceeds total byte limit"));
                }
                format!(
                    "file:{}:{}:{}",
                    metadata.permissions().mode() & 0o7777,
                    raw.size,
                    raw.sha256
                )
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(tree_error)?;
                if target.is_absolute()
                    || !fs::canonicalize(&path)
                        .map_err(tree_error)?
                        .starts_with(root)
                {
                    return Err(tree_error("selected tool alias escapes its owned root"));
                }
                format!(
                    "alias:{}",
                    target
                        .to_str()
                        .ok_or_else(|| tree_error("tool alias is not UTF-8"))?
                )
            } else {
                return Err(tree_error(
                    "selected tool contains an unsafe file type or hardlink",
                ));
            };
            facts.insert(relative, fact);
            if facts.len() > ENTRY_LIMIT {
                return Err(tree_error("selected tool tree exceeds file limit"));
            }
        }
    }
    for (relative, fact) in facts {
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(fact.as_bytes());
        digest.update([0]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests;
