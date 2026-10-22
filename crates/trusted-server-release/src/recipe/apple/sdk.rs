//! Detached, bounded copies of the selected installed Apple SDK.
//!
//! The receipt measures the destination that the recipe actually selects.
//! Copying is not an atomic observation of vendor directory membership and
//! does not authenticate an original distribution or its acquisition.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use error_stack::Report;
use serde::Serialize;

use super::super::fresh::real_directory;
use super::super::recipe_error;
use crate::error::ReleaseError;
use crate::process::Cancellation;
use crate::tool_tree::tool_tree_digest;

const FILE_LIMIT: u64 = 512 * 1024 * 1024;
const TREE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;

/// Digest of a complete detached SDK tree, constructed only by measurement.
#[derive(Clone, Debug, Serialize)]
pub(super) struct SdkReceipt {
    tree_sha256: String,
    regular_bytes: u64,
    entries: usize,
}

impl SdkReceipt {
    /// Returns the detached SDK's measured structured-tree identity.
    pub(super) fn tree_sha256(&self) -> &str {
        &self.tree_sha256
    }
}

/// Private SDK copy whose lifetime is retained by every tool selection clone.
#[derive(Debug)]
pub(super) struct OwnedSdk {
    _directory: OwnedDirectory,
    root: PathBuf,
    receipt: SdkReceipt,
    deadline: Instant,
    cancellation: Cancellation,
}

/// Owns cleanup from creation, including errors after source modes are restored.
///
/// Only created directory paths are recorded, bounded by the SDK entry limit.
/// Cleanup uses root-relative descriptors and never changes file permissions.
#[derive(Debug)]
struct OwnedDirectory {
    directory: tempfile::TempDir,
    root: Option<File>,
    directories: Vec<PathBuf>,
}

impl OwnedDirectory {
    fn new(directory: tempfile::TempDir) -> Result<Self, Report<ReleaseError>> {
        let mut owned = Self {
            directory,
            root: None,
            directories: Vec::new(),
        };
        owned.root = Some(
            File::options()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(owned.directory.path())
                .map_err(io)?,
        );
        Ok(owned)
    }
}

/// Restores owner access through checked directory components, never aliases.
fn restore_directory(root: &File, relative: &Path) -> std::io::Result<()> {
    let mut parent = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::other(
                "cleanup path is not relative normal text",
            ));
        };
        let name = CString::new(name.as_encoded_bytes())?;
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor and NUL-terminated name remain valid, and
        // fstatat initializes metadata on success without following aliases.
        let status = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the entire metadata value.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(std::io::Error::other(
                "cleanup component is not a real directory",
            ));
        }
        // SAFETY: the live parent descriptor anchors the relative name;
        // AT_SYMLINK_NOFOLLOW prevents a swapped alias changing its target.
        if unsafe {
            libc::fchmodat(
                parent.as_raw_fd(),
                name.as_ptr(),
                metadata.st_mode | 0o700,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat borrows valid arguments and returns a new descriptor.
        // Every component is opened without following a symlink.
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned an owned descriptor, transferred exactly once.
        parent = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(())
}

impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if let Some(root) = &self.root {
            if let Ok(metadata) = root.metadata() {
                let _ = root.set_permissions(fs::Permissions::from_mode(metadata.mode() | 0o700));
            }
            // Created parents precede their descendants. Cleanup deliberately
            // ignores the attempt budget so cancellation cannot retain copies.
            for relative in &self.directories {
                let _ = restore_directory(root, relative);
            }
        }
        // TempDir now removes the accessible private tree. Destruction remains
        // best effort for external I/O failures; the real SDK smoke observes
        // that the copied root is actually gone before reporting its receipt.
    }
}

fn check(deadline: Instant, cancellation: &Cancellation) -> Result<(), Report<ReleaseError>> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(recipe_error("Apple SDK snapshot cancelled or timed out"));
    }
    Ok(())
}

fn io(error: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("Apple SDK snapshot: {error}"))
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

impl OwnedSdk {
    /// Copies a selected vendor tree without retaining any vendor hardlinks.
    ///
    /// # Errors
    ///
    /// Rejects unsafe aliases, special files, regular-file mutation during
    /// copying, exhausted limits, cancellation and expiration. The destination
    /// must pass the strict owned tool tree policy before it can become selected.
    /// This installed-content copy is not an atomic vendor-source observation
    /// or an authenticated distribution snapshot.
    pub(super) fn stage(
        source: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::stage_with_limits(
            source,
            deadline,
            cancellation,
            (FILE_LIMIT, TREE_LIMIT, ENTRY_LIMIT),
        )
    }

    fn stage_with_limits(
        source: &Path,
        deadline: Instant,
        cancellation: &Cancellation,
        limits: (u64, u64, usize),
    ) -> Result<Self, Report<ReleaseError>> {
        check(deadline, cancellation)?;
        let source = fs::canonicalize(source).map_err(io)?;
        real_directory("source Apple SDK", &source)?;
        let mut directory = OwnedDirectory::new(
            tempfile::Builder::new()
                .prefix("ts-apple-sdk-")
                .tempdir()
                .map_err(io)?,
        )?;
        let root = fs::canonicalize(directory.directory.path()).map_err(io)?;
        let mut pending = vec![source.clone()];
        let mut directories = Vec::new();
        let mut entries = 0;
        let mut total = 0;
        let mut buffer = vec![0; 64 * 1024];
        while let Some(parent) = pending.pop() {
            check(deadline, cancellation)?;
            real_directory("source Apple SDK directory", &parent)?;
            for entry in fs::read_dir(parent).map_err(io)? {
                check(deadline, cancellation)?;
                let path = entry.map_err(io)?.path();
                let relative = path
                    .strip_prefix(&source)
                    .map_err(|error| recipe_error(error.to_string()))?;
                if relative.to_str().is_none() {
                    return Err(recipe_error("Apple SDK path is not UTF-8"));
                }
                entries += 1;
                if entries > limits.2 {
                    return Err(recipe_error("Apple SDK exceeds entry limit"));
                }
                let destination = root.join(relative);
                let metadata = fs::symlink_metadata(&path).map_err(io)?;
                if metadata.is_dir() {
                    fs::create_dir(&destination).map_err(io)?;
                    directory.directories.push(relative.to_owned());
                    directories.push((destination, metadata.permissions()));
                    pending.push(path);
                } else if metadata.is_file() {
                    if metadata.len() > limits.0 || total + metadata.len() > limits.1 {
                        return Err(recipe_error("Apple SDK exceeds byte limit"));
                    }
                    let mut input = File::options()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&path)
                        .map_err(io)?;
                    let opened = input.metadata().map_err(io)?;
                    if !opened.is_file() || stamp(&opened) != stamp(&metadata) {
                        return Err(recipe_error("Apple SDK source changed before copying"));
                    }
                    let mut output = File::options()
                        .write(true)
                        .create_new(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&destination)
                        .map_err(io)?;
                    let mut size = 0;
                    loop {
                        check(deadline, cancellation)?;
                        let read = input.read(&mut buffer).map_err(io)?;
                        if read == 0 {
                            break;
                        }
                        size += read as u64;
                        total += read as u64;
                        if size > limits.0 || total > limits.1 {
                            return Err(recipe_error("Apple SDK grew beyond byte limit"));
                        }
                        output.write_all(&buffer[..read]).map_err(io)?;
                    }
                    if size != metadata.len()
                        || stamp(&input.metadata().map_err(io)?) != stamp(&metadata)
                        || stamp(&fs::symlink_metadata(&path).map_err(io)?) != stamp(&metadata)
                    {
                        return Err(recipe_error("Apple SDK source changed while copying"));
                    }
                    output.set_permissions(metadata.permissions()).map_err(io)?;
                } else if metadata.file_type().is_symlink() {
                    let target = fs::read_link(&path).map_err(io)?;
                    if target.is_absolute()
                        || target.to_str().is_none()
                        || !fs::canonicalize(&path).map_err(io)?.starts_with(&source)
                    {
                        return Err(recipe_error("Apple SDK alias escapes source root"));
                    }
                    symlink(target, destination).map_err(io)?;
                } else {
                    return Err(recipe_error("Apple SDK contains a special file"));
                }
            }
        }
        for (path, permissions) in directories.into_iter().rev() {
            check(deadline, cancellation)?;
            fs::set_permissions(path, permissions).map_err(io)?;
        }
        fs::set_permissions(&root, fs::metadata(&source).map_err(io)?.permissions()).map_err(io)?;
        let tree_sha256 = tool_tree_digest(&root, &|| check(deadline, cancellation))?;
        Ok(Self {
            _directory: directory,
            root,
            receipt: SdkReceipt {
                tree_sha256,
                regular_bytes: total,
                entries,
            },
            deadline,
            cancellation: cancellation.clone(),
        })
    }

    /// The exact owned directory selected as SDKROOT.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The receipt measured from the private destination tree.
    pub(super) fn receipt(&self) -> &SdkReceipt {
        &self.receipt
    }

    /// Rehashes the owned SDK under the caller's original attempt budget.
    ///
    /// # Errors
    ///
    /// Rejects changed or unsafe entries, cancellation and expiration.
    pub(super) fn verify(&self) -> Result<(), Report<ReleaseError>> {
        check(self.deadline, &self.cancellation)?;
        real_directory("owned Apple SDK", &self.root)?;
        let digest = tool_tree_digest(&self.root, &|| check(self.deadline, &self.cancellation))?;
        if digest != self.receipt.tree_sha256 {
            return Err(recipe_error("owned Apple SDK changed after measurement"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::process::Cancellation;

    #[test]
    fn explicit_smoke_developer_selection_requires_a_canonical_plain_directory() {
        let directory = tempfile::tempdir().expect("should create selection fixture");
        let canonical =
            fs::canonicalize(directory.path()).expect("should resolve selection fixture");
        symlink(&canonical, canonical.join("alias")).expect("should create developer alias");
        for path in [PathBuf::from("relative-developer"), canonical.join("alias")] {
            let error = super::super::AppleDeveloperTools::measure_selected_developer(
                path,
                Instant::now() + Duration::from_secs(30),
                &Cancellation::default(),
            )
            .expect_err("should reject unchecked developer root before reading SDK");
            let text = format!("{error:?}");
            assert!(
                text.contains("absolute normal path") || text.contains("symlink"),
                "should reject root itself rather than missing SDK: {text}"
            );
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the failed finalizer owns the SDK to exercise cleanup on its error return"
    )]
    fn fail_after_restoring_modes(sdk: OwnedSdk) -> Result<(), Report<ReleaseError>> {
        fs::set_permissions(
            sdk.root().join("readonly/inaccessible"),
            fs::Permissions::from_mode(0o0),
        )
        .expect("should restore inaccessible child mode");
        fs::set_permissions(
            sdk.root().join("readonly"),
            fs::Permissions::from_mode(0o555),
        )
        .expect("should restore readonly child mode");
        fs::set_permissions(sdk.root(), fs::Permissions::from_mode(0o0))
            .expect("should restore inaccessible root mode");
        Err(recipe_error("finalization failed after mode restoration"))
    }

    #[test]
    fn readonly_sdk_directory_is_removed_after_last_clone_even_when_cancelled() {
        let source = tempfile::tempdir().expect("should create source");
        fs::create_dir(source.path().join("readonly")).expect("should create readonly directory");
        fs::write(source.path().join("readonly/header.h"), b"header").expect("should write header");
        fs::set_permissions(
            source.path().join("readonly"),
            fs::Permissions::from_mode(0o555),
        )
        .expect("should set readonly mode");
        let cancellation = Cancellation::default();
        let sdk = Arc::new(
            OwnedSdk::stage(
                source.path(),
                Instant::now() + Duration::from_secs(30),
                &cancellation,
            )
            .expect("should copy readonly SDK"),
        );
        fs::set_permissions(
            source.path().join("readonly"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("should restore source for fixture cleanup");
        let root = sdk.root().to_owned();
        let clone = Arc::clone(&sdk);
        drop(sdk);
        assert!(root.exists(), "should retain SDK while clone exists");
        assert_eq!(
            fs::metadata(root.join("readonly"))
                .expect("should inspect readonly directory")
                .mode()
                & 0o7777,
            0o555,
            "should preserve measured mode while alive"
        );
        cancellation.cancel();
        drop(clone);
        let leaked = root.exists();
        if leaked {
            fs::set_permissions(root.join("readonly"), fs::Permissions::from_mode(0o755))
                .expect("should clean regression failure");
            fs::remove_dir_all(&root).expect("should remove regression failure");
        }
        assert!(
            !leaked,
            "should remove readonly SDK after last clone despite cancellation"
        );
    }

    #[test]
    fn failed_stage_scope_removes_restored_readonly_directories() {
        let source = tempfile::tempdir().expect("should create source");
        fs::create_dir_all(source.path().join("readonly/inaccessible"))
            .expect("should create directory");
        fs::write(
            source.path().join("readonly/inaccessible/header.h"),
            b"header",
        )
        .expect("should write header");
        let sdk = OwnedSdk::stage(
            source.path(),
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should copy SDK before fallible finalization");
        let root = sdk.root().to_owned();
        let failed = fail_after_restoring_modes(sdk);
        assert!(failed.is_err(), "should exercise failed finalization");
        let leaked = root.exists();
        if leaked {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
                .expect("should clean inaccessible regression failure");
            fs::set_permissions(root.join("readonly"), fs::Permissions::from_mode(0o755))
                .expect("should clean readonly regression failure");
            fs::set_permissions(
                root.join("readonly/inaccessible"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("should clean inaccessible child regression failure");
            fs::remove_dir_all(&root).expect("should remove regression failure");
        }
        assert!(
            !leaked,
            "should clean failed stage after modes were restored"
        );
    }

    #[test]
    fn cleanup_never_follows_replaced_directory_aliases_or_chmods_files() {
        let source = tempfile::tempdir().expect("should create source");
        fs::create_dir(source.path().join("directory")).expect("should create directory");
        let external = tempfile::tempdir().expect("should create external fixture");
        fs::write(external.path().join("header.h"), b"external header")
            .expect("should write external file");
        fs::set_permissions(
            external.path().join("header.h"),
            fs::Permissions::from_mode(0o444),
        )
        .expect("should protect external file");
        let sdk = OwnedSdk::stage(
            source.path(),
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should stage SDK");
        let root = sdk.root().to_owned();
        fs::remove_dir(root.join("directory")).expect("should replace owned directory");
        symlink(external.path(), root.join("directory")).expect("should add external alias");
        fs::hard_link(external.path().join("header.h"), root.join("hardlink.h"))
            .expect("should add external file hardlink");
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o555))
            .expect("should protect external directory");
        drop(sdk);
        let mode = fs::metadata(external.path())
            .expect("should inspect external directory")
            .mode()
            & 0o7777;
        let file_mode = fs::metadata(external.path().join("header.h"))
            .expect("should inspect external file")
            .mode()
            & 0o7777;
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o755))
            .expect("should restore external fixture for cleanup");
        assert!(
            !root.exists(),
            "should remove aliases and hardlinks from owned tree"
        );
        assert_eq!(mode, 0o555, "should not chmod external alias target");
        assert_eq!(
            file_mode, 0o444,
            "should not chmod external hardlinked file"
        );
    }

    #[test]
    #[ignore = "requires the explicitly selected local Xcode SDK and a receipt destination; copies the SDK, never modifies Xcode"]
    fn ordinary_ci_selected_sdk_detached_snapshot_smoke() {
        let destination = std::env::var_os("TS_RELEASE_APPLE_SDK_SMOKE_RECEIPT")
            .map(PathBuf::from)
            .expect("should explicitly supply a fresh smoke receipt destination");
        let cancellation = Cancellation::default();
        let deadline = Instant::now() + Duration::from_secs(15 * 60);
        // This explicit local observation never changes CI link selection or
        // grants qualification to the measured installed developer tools.
        let apple = match std::env::var_os("TS_RELEASE_APPLE_SDK_SMOKE_DEVELOPER_ROOT") {
            Some(root) => super::super::AppleDeveloperTools::measure_selected_developer(
                PathBuf::from(root),
                deadline,
                &cancellation,
            ),
            None => super::super::AppleDeveloperTools::for_ordinary_ci(deadline, &cancellation),
        }
        .expect("should measure Xcode and stage the selected SDK");
        let root = apple.sdk_root().to_owned();
        let evidence = apple
            .candidate_measurement()
            .expect("should recheck selected SDK at evidence sealing");
        let receipt = serde_json::to_vec(
            &evidence
                .sdk_tree
                .expect("should have a complete selected SDK receipt"),
        )
        .expect("should serialize SDK receipt");
        let clone = apple.clone();
        drop(apple);
        clone
            .verify()
            .expect("should retain and recheck selected SDK through clone");
        drop(clone);
        assert!(
            !root.exists(),
            "should clean copied SDK after last selection is dropped"
        );
        let mut output = File::options()
            .write(true)
            .create_new(true)
            .open(destination)
            .expect("should create fresh smoke receipt");
        output
            .write_all(&receipt)
            .expect("should write observed SDK receipt after cleanup");
    }

    #[test]
    fn snapshot_copies_vendor_hardlinks_independently_and_preserves_internal_aliases() {
        let source = tempfile::tempdir().expect("should create source");
        fs::write(source.path().join("header.h"), b"header").expect("should write header");
        fs::hard_link(
            source.path().join("header.h"),
            source.path().join("other.h"),
        )
        .expect("should link vendor file");
        symlink("header.h", source.path().join("alias.h")).expect("should link alias");
        let sdk = OwnedSdk::stage(
            source.path(),
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should stage vendor SDK");
        for name in ["header.h", "other.h"] {
            assert_eq!(
                fs::metadata(sdk.root().join(name))
                    .expect("should inspect copy")
                    .nlink(),
                1,
                "should create independent owned files"
            );
        }
        assert_eq!(
            fs::read_link(sdk.root().join("alias.h")).expect("should inspect alias"),
            std::path::PathBuf::from("header.h"),
            "should preserve relative alias"
        );
        sdk.verify().expect("should verify owned tree");
    }

    #[test]
    fn snapshot_refuses_cancelled_or_expired_source_copy() {
        let source = tempfile::tempdir().expect("should create source");
        let cancellation = Cancellation::default();
        cancellation.cancel();
        assert!(
            OwnedSdk::stage(
                source.path(),
                Instant::now() + Duration::from_secs(30),
                &cancellation
            )
            .is_err(),
            "should stop cancelled copy"
        );
        assert!(
            OwnedSdk::stage(
                source.path(),
                Instant::now() - Duration::from_secs(1),
                &Cancellation::default()
            )
            .is_err(),
            "should stop expired copy"
        );
    }

    #[test]
    fn snapshot_preserves_source_modes_and_checks_empty_tree_cancellation() {
        let source = tempfile::tempdir().expect("should create source");
        fs::set_permissions(source.path(), fs::Permissions::from_mode(0o755))
            .expect("should set source mode");
        let cancellation = Cancellation::default();
        let sdk = OwnedSdk::stage(
            source.path(),
            Instant::now() + Duration::from_secs(30),
            &cancellation,
        )
        .expect("should stage source");
        assert_eq!(
            fs::metadata(sdk.root())
                .expect("should inspect root")
                .mode()
                & 0o7777,
            0o755,
            "should preserve source root mode"
        );
        cancellation.cancel();
        assert!(
            sdk.verify().is_err(),
            "should check cancellation even for empty tree"
        );
    }

    #[test]
    fn snapshot_rejects_unsafe_source_aliases_and_special_files() {
        for target in ["/absolute", "../outside", "missing"] {
            let source = tempfile::tempdir().expect("should create source");
            symlink(target, source.path().join("alias")).expect("should write alias");
            assert!(
                OwnedSdk::stage(
                    source.path(),
                    Instant::now() + Duration::from_secs(30),
                    &Cancellation::default()
                )
                .is_err(),
                "should reject {target}"
            );
        }
        let source = tempfile::tempdir().expect("should create source");
        let fifo = CString::new(
            source
                .path()
                .join("fifo")
                .to_str()
                .expect("should have UTF-8 path"),
        )
        .expect("should encode FIFO path");
        // SAFETY: the CString remains alive and points to a NUL-terminated path.
        assert_eq!(
            unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) },
            0,
            "should create special file"
        );
        assert!(
            OwnedSdk::stage(
                source.path(),
                Instant::now() + Duration::from_secs(30),
                &Cancellation::default()
            )
            .is_err(),
            "should reject special file"
        );
    }

    #[test]
    fn snapshot_enforces_source_file_tree_and_entry_bounds() {
        for limits in [(3, 20, 10), (20, 5, 10), (20, 20, 1)] {
            let source = tempfile::tempdir().expect("should create source");
            for name in ["first", "second"] {
                fs::write(source.path().join(name), b"1234").expect("should write input");
            }
            assert!(
                OwnedSdk::stage_with_limits(
                    source.path(),
                    Instant::now() + Duration::from_secs(30),
                    &Cancellation::default(),
                    limits
                )
                .is_err(),
                "should reject exhausted {limits:?}"
            );
        }
    }

    #[test]
    fn snapshot_rejects_destination_hardlinks_and_changed_aliases() {
        for hardlink in [true, false] {
            let source = tempfile::tempdir().expect("should create source");
            fs::write(source.path().join("header.h"), b"header").expect("should write header");
            symlink("header.h", source.path().join("alias.h")).expect("should write alias");
            let sdk = OwnedSdk::stage(
                source.path(),
                Instant::now() + Duration::from_secs(30),
                &Cancellation::default(),
            )
            .expect("should stage source");
            if hardlink {
                fs::hard_link(sdk.root().join("header.h"), source.path().join("outside.h"))
                    .expect("should add external hardlink");
            } else {
                fs::remove_file(sdk.root().join("alias.h")).expect("should remove alias");
                symlink("../outside", sdk.root().join("alias.h")).expect("should change alias");
            }
            assert!(sdk.verify().is_err(), "should reject mutated owned SDK");
        }
    }
}
