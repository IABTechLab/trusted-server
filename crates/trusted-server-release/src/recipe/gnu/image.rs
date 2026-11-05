//! Bounded installed-image file identity, separate from owned tool-tree policy.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::super::recipe_error;
use crate::error::ReleaseError;

/// Image paths conservatively covering installed native tools and platform files.
///
/// Alias targets are recorded, never traversed. This is not execution tracing
/// and does not imply that files outside these trees are unavailable.
pub(super) const ROOTS: [&str; 10] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc/alternatives",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/ld.so.cache",
    "/etc/gcc",
];
const MAX_ENTRIES: usize = 250_000;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Reviewed root-only privileged helpers in the ordinary UBI 9 builder image.
///
/// Observed in `registry.access.redhat.com/ubi9/ubi:9.6` with gcc, glibc-devel
/// and git-core: the setgid `utempter`, the PAM `unix_update` and the setuid
/// `userhelper`. None is a compiler, linker or build helper. Their content is
/// not hashed; it is bound only through the image identity itself.
pub(super) const REVIEWED_UNREADABLE: [&str; 3] = [
    "/usr/libexec/utempter/utempter",
    "/usr/sbin/unix_update",
    "/usr/sbin/userhelper",
];

/// Which unreadable image files may be recorded by metadata only.
pub(super) struct UnreadablePolicy<'a> {
    /// Exact absolute image paths, sorted.
    pub(super) paths: &'a [&'a str],
    /// The owner every such file must have.
    pub(super) owner_uid: u32,
}

/// The production policy: the reviewed paths, owned by root.
pub(super) const PRODUCTION_UNREADABLE: UnreadablePolicy<'static> = UnreadablePolicy {
    paths: &REVIEWED_UNREADABLE,
    owner_uid: 0,
};

/// The installed-image identity and the files whose content it could not read.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct InstalledSnapshot {
    /// Digest of every recorded name and fact.
    pub(super) sha256: String,
    /// Sorted absolute image paths recorded by metadata only, never by content.
    pub(super) content_unmeasured: Vec<String>,
}

/// Opens one image file without following a final alias.
fn open_image_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Hashes one held no-follow regular file from the read-only image namespace.
///
/// Unlike host document handling, image hardlinks are permitted. Only the
/// confined GNU and Fastly helpers use this policy to measure candidate files.
pub(super) fn image_file_hash(path: &Path) -> Result<(String, u64), Report<ReleaseError>> {
    let file = open_image_file(path).map_err(|error| {
        recipe_error(format!(
            "cannot open image file {}: {error}",
            path.display()
        ))
    })?;
    hash_open_file(file)
}

/// Hashes an opened image file, requiring unchanged identity while reading.
fn hash_open_file(mut file: fs::File) -> Result<(String, u64), Report<ReleaseError>> {
    let before = file
        .metadata()
        .map_err(|error| recipe_error(error.to_string()))?;
    if !before.is_file() || before.len() > MAX_FILE {
        return Err(recipe_error("image input is not a bounded regular file"));
    }
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|error| recipe_error(error.to_string()))?;
        if length == 0 {
            break;
        }
        bytes += length as u64;
        if bytes > MAX_FILE {
            return Err(recipe_error("image file exceeds byte limit"));
        }
        digest.update(&buffer[..length]);
    }
    let after = file
        .metadata()
        .map_err(|error| recipe_error(error.to_string()))?;
    let facts = |metadata: &fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    if facts(&before) != facts(&after) || bytes != before.len() {
        return Err(recipe_error("image file changed during measurement"));
    }
    Ok((hex::encode(digest.finalize()), bytes))
}

/// Requires an absolute normal selected path within the measured image trees.
pub(super) fn image_path(path: &Path) -> Result<(), Report<ReleaseError>> {
    let text = path
        .to_str()
        .ok_or_else(|| recipe_error("GNU path is not UTF-8"))?;
    if !path.is_absolute()
        || text.split('/').any(|part| matches!(part, "." | ".."))
        || !ROOTS.iter().any(|root| path.starts_with(root))
    {
        return Err(recipe_error(
            "GNU tool or resource escapes measured image paths",
        ));
    }
    Ok(())
}

/// Measures installed file names, kinds, modes, content and alias targets.
///
/// Hard links are acceptable here because the owning Docker boundary supplies
/// a read-only image filesystem; this function must never authorize host tools.
/// Missing optional roots are explicit facts. Docker-generated host/network
/// files and pseudo filesystems are outside the listed scope.
///
/// Only the exact reviewed paths in [`PRODUCTION_UNREADABLE`] may be recorded
/// by mode, owner and size instead of content when the unprivileged helper
/// cannot open them; they are listed in
/// [`InstalledSnapshot::content_unmeasured`]. Any other unreadable file, or a
/// reviewed path with another owner, a group/other write bit or other-read
/// permission, is rejected. Execute-only files outside that list therefore
/// fail closed. Selected build tools are hashed separately and never take
/// this path.
pub(super) fn installed_snapshot(root: &Path) -> Result<InstalledSnapshot, Report<ReleaseError>> {
    installed_snapshot_with(root, &PRODUCTION_UNREADABLE)
}

/// [`installed_snapshot`] with an explicit unreadable-file policy.
pub(super) fn installed_snapshot_with(
    root: &Path,
    unreadable: &UnreadablePolicy<'_>,
) -> Result<InstalledSnapshot, Report<ReleaseError>> {
    let mut pending: Vec<PathBuf> = ROOTS.iter().map(|path| root.join(&path[1..])).collect();
    let mut facts = BTreeMap::new();
    let mut content_unmeasured = Vec::new();
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        let name = path
            .strip_prefix(root)
            .map_err(|error| recipe_error(error.to_string()))?
            .to_str()
            .ok_or_else(|| recipe_error("image path is not UTF-8"))?
            .to_owned();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(recipe_error(format!(
                    "cannot inspect image file {name}: {error}"
                )));
            }
        };
        let fact = if let Some(metadata) = metadata {
            let mode = metadata.permissions().mode() & 0o7777;
            if metadata.is_dir() {
                for entry in fs::read_dir(&path).map_err(|error| recipe_error(error.to_string()))? {
                    pending.push(
                        entry
                            .map_err(|error| recipe_error(error.to_string()))?
                            .path(),
                    );
                    if pending.len() + facts.len() > MAX_ENTRIES {
                        return Err(recipe_error("GNU image inventory exceeds entry limit"));
                    }
                }
                format!("directory:{mode}")
            } else if metadata.is_file() {
                match open_image_file(&path) {
                    Ok(file) => {
                        let (sha256, size) = hash_open_file(file)?;
                        bytes = bytes
                            .checked_add(size)
                            .filter(|bytes| *bytes <= MAX_BYTES)
                            .ok_or_else(|| {
                                recipe_error("GNU image inventory exceeds byte limit")
                            })?;
                        format!("file:{mode}:{size}:{sha256}")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        let absolute = format!("/{name}");
                        check_reviewed_unreadable(&absolute, &path, &metadata, unreadable)?;
                        content_unmeasured.push(absolute);
                        format!(
                            "unmeasured-file:{mode}:{}:{}:{}",
                            metadata.uid(),
                            metadata.gid(),
                            metadata.len()
                        )
                    }
                    Err(error) => {
                        return Err(recipe_error(format!(
                            "cannot open image file /{name}: {error}"
                        )));
                    }
                }
            } else if metadata.file_type().is_symlink() {
                let target =
                    fs::read_link(&path).map_err(|error| recipe_error(error.to_string()))?;
                format!(
                    "alias:{mode}:{}",
                    target
                        .to_str()
                        .ok_or_else(|| recipe_error("image alias is not UTF-8"))?
                )
            } else {
                return Err(recipe_error("GNU image inventory contains a special file"));
            }
        } else {
            "missing".to_owned()
        };
        facts.insert(name, fact);
        if facts.len() > MAX_ENTRIES {
            return Err(recipe_error("GNU image inventory exceeds entry limit"));
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"trusted-server-gnu-installed-image-v2\n");
    for (name, fact) in facts {
        for value in [name, fact] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
    }
    content_unmeasured.sort();
    Ok(InstalledSnapshot {
        sha256: hex::encode(digest.finalize()),
        content_unmeasured,
    })
}

/// Accepts a permission-denied file only at a reviewed path with expected
/// ownership and permissions, unchanged since it was first inspected.
fn check_reviewed_unreadable(
    absolute: &str,
    path: &Path,
    metadata: &fs::Metadata,
    unreadable: &UnreadablePolicy<'_>,
) -> Result<(), Report<ReleaseError>> {
    let mode = metadata.mode();
    if !unreadable.paths.contains(&absolute)
        || metadata.uid() != unreadable.owner_uid
        || mode & 0o022 != 0
        || mode & 0o004 != 0
    {
        return Err(recipe_error(format!(
            "image file {absolute} cannot be read and is not a reviewed privileged helper"
        )));
    }
    let after = fs::symlink_metadata(path).map_err(|error| recipe_error(error.to_string()))?;
    let facts = |metadata: &fs::Metadata| {
        (
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            metadata.len(),
            metadata.is_file(),
        )
    };
    if facts(metadata) != facts(&after) {
        return Err(recipe_error(format!(
            "image file {absolute} changed during measurement"
        )));
    }
    Ok(())
}
