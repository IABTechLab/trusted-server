use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write as _};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use error_stack::{Report, ResultExt as _};

#[derive(Debug, derive_more::Display)]
pub(crate) enum RepositoryError {
    #[display("Git repository discovery failed")]
    Discovery,
    #[display("Git tracked-path enumeration failed")]
    TrackedPaths,
    #[display("repository path is not a normalized relative path")]
    UnsafeRelativePath,
    #[display("repository path escapes through a symlink")]
    SymlinkEscape,
    #[display("repository entry has an unsafe type or mode")]
    UnsafeEntry,
    #[display("repository file operation failed")]
    FileOperation,
}

impl core::error::Error for RepositoryError {}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct NormalizedRelativePath(PathBuf);

impl NormalizedRelativePath {
    pub(crate) fn new(path: &Path) -> Result<Self, Report<RepositoryError>> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(Report::new(RepositoryError::UnsafeRelativePath)
                .attach(format!("path: {}", path.display())));
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) if safe_component(value) => normalized.push(value),
                _ => {
                    return Err(Report::new(RepositoryError::UnsafeRelativePath)
                        .attach(format!("path: {}", path.display())));
                }
            }
        }

        if normalized.as_os_str() != path.as_os_str() {
            return Err(Report::new(RepositoryError::UnsafeRelativePath)
                .attach(format!("path: {}", path.display())));
        }

        Ok(Self(normalized))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn as_utf8(&self) -> Result<&str, Report<RepositoryError>> {
        self.0.to_str().ok_or_else(|| {
            Report::new(RepositoryError::UnsafeRelativePath)
                .attach(format!("non-UTF-8 path: {}", self.0.display()))
        })
    }
}

fn safe_component(value: &OsStr) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    !value.eq_ignore_ascii_case(".git")
        && !value.ends_with(['.', ' '])
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')
        })
        && !is_windows_device_name(value)
}

fn is_windows_device_name(value: &str) -> bool {
    let stem = value
        .split_once('.')
        .map_or(value, |(stem, _extension)| stem)
        .trim_end_matches(['.', ' ']);
    if ["CON", "PRN", "AUX", "NUL", "CLOCK$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return true;
    }

    let Some(prefix) = stem.get(..3) else {
        return false;
    };
    let Some(suffix) = stem.get(3..) else {
        return false;
    };
    (prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
        && matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
}

pub(crate) struct Repository {
    root: PathBuf,
}

impl Repository {
    pub(crate) fn discover(start: &Path) -> Result<Self, Report<RepositoryError>> {
        let start = fs::canonicalize(start)
            .change_context(RepositoryError::Discovery)
            .attach_with(|| format!("start path: {}", start.display()))?;
        let output = Command::new("git")
            .args([
                "-C",
                start.to_str().ok_or_else(|| {
                    Report::new(RepositoryError::Discovery).attach("start path is not UTF-8")
                })?,
                "rev-parse",
                "--show-toplevel",
            ])
            .output()
            .change_context(RepositoryError::Discovery)?;

        if !output.status.success() {
            return Err(Report::new(RepositoryError::Discovery)
                .attach(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
        }

        let root_bytes = output.stdout.strip_suffix(b"\n").ok_or_else(|| {
            Report::new(RepositoryError::Discovery)
                .attach("Git repository root output has no line terminator")
        })?;
        let root_text =
            core::str::from_utf8(root_bytes).change_context(RepositoryError::Discovery)?;
        let root = fs::canonicalize(root_text)
            .change_context(RepositoryError::Discovery)
            .attach_with(|| format!("repository root: {root_text}"))?;
        if !start.starts_with(&root) {
            return Err(Report::new(RepositoryError::SymlinkEscape)
                .attach(format!("start path: {}", start.display()))
                .attach(format!("repository root: {}", root.display())));
        }

        Ok(Self { root })
    }

    pub(crate) fn tracked_paths_record(&self) -> Result<Vec<u8>, Report<RepositoryError>> {
        let paths = self.tracked_paths()?;
        let mut record = Vec::new();
        for path in paths {
            record.extend_from_slice(path.as_utf8()?.as_bytes());
            record.push(b'\n');
        }
        Ok(record)
    }

    pub(crate) fn tracked_paths(
        &self,
    ) -> Result<Vec<NormalizedRelativePath>, Report<RepositoryError>> {
        let output = Command::new("git")
            .args(["-C", self.root_text()?, "ls-files", "-z"])
            .output()
            .change_context(RepositoryError::TrackedPaths)?;
        if !output.status.success() {
            return Err(Report::new(RepositoryError::TrackedPaths)
                .attach(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
        }

        let mut paths = Vec::new();
        for raw_path in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path_text = core::str::from_utf8(raw_path)
                .change_context(RepositoryError::TrackedPaths)
                .attach("tracked path is not UTF-8")?;
            let path = NormalizedRelativePath::new(Path::new(path_text))?;
            self.validate_existing(&path, true)?;
            paths.push(path);
        }
        paths.sort_unstable();
        Ok(paths)
    }

    pub(crate) fn read_tracked(
        &self,
        path: &NormalizedRelativePath,
    ) -> Result<Vec<u8>, Report<RepositoryError>> {
        self.validate_existing(path, true)?;
        let absolute = self.root.join(path.as_path());
        fs::read(&absolute)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("read tracked path: {}", absolute.display()))
    }

    pub(crate) fn read_optional(
        &self,
        path: &NormalizedRelativePath,
    ) -> Result<Option<Vec<u8>>, Report<RepositoryError>> {
        let absolute = self.root.join(path.as_path());
        if final_entry_metadata(&absolute)?.is_none() {
            self.validate_parent_chain(path)?;
            return Ok(None);
        }

        self.validate_existing(path, false)?;
        fs::read(&absolute)
            .map(Some)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("read path: {}", absolute.display()))
    }

    pub(crate) fn validate_regular_file(
        &self,
        path: &NormalizedRelativePath,
    ) -> Result<(), Report<RepositoryError>> {
        self.validate_existing(path, false)
    }

    pub(crate) fn write_atomically(
        &self,
        path: &NormalizedRelativePath,
        expected_original: Option<&[u8]>,
        contents: &[u8],
    ) -> Result<(), Report<RepositoryError>> {
        self.write_atomically_after_stage(path, expected_original, contents, || Ok(()))
    }

    fn write_atomically_after_stage<F>(
        &self,
        path: &NormalizedRelativePath,
        expected_original: Option<&[u8]>,
        contents: &[u8],
        after_stage: F,
    ) -> Result<(), Report<RepositoryError>>
    where
        F: FnOnce() -> Result<(), std::io::Error>,
    {
        self.write_atomically_with_commit(
            path,
            expected_original,
            contents,
            after_stage,
            |staged, target| fs::rename(staged, target),
        )
    }

    fn write_atomically_with_commit<F, C>(
        &self,
        path: &NormalizedRelativePath,
        expected_original: Option<&[u8]>,
        contents: &[u8],
        after_stage: F,
        commit: C,
    ) -> Result<(), Report<RepositoryError>>
    where
        F: FnOnce() -> Result<(), std::io::Error>,
        C: FnOnce(&Path, &Path) -> Result<(), std::io::Error>,
    {
        let absolute = self.root.join(path.as_path());
        let identity = self.validate_expected_target(path, expected_original)?;
        let parent = absolute.parent().ok_or_else(|| {
            Report::new(RepositoryError::UnsafeRelativePath)
                .attach(format!("path: {}", path.as_path().display()))
        })?;
        self.create_safe_parent(path, parent)?;

        let temporary = temporary_path(&absolute)?;
        if final_entry_metadata(&temporary)?.is_some() {
            fs::remove_file(&temporary)
                .change_context(RepositoryError::FileOperation)
                .attach_with(|| format!("remove stale atomic stage: {}", temporary.display()))?;
            return Err(Report::new(RepositoryError::FileOperation).attach(format!(
                "removed stale atomic stage: {}",
                temporary.display()
            )));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        options.mode(identity.as_ref().map_or(0o644, |value| value.mode & 0o777));
        let mut file = options
            .open(&temporary)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("atomic stage: {}", temporary.display()))?;
        let mut stage = AtomicStage::new(temporary.clone());

        let write_result = file
            .write_all(contents)
            .and_then(|()| file.sync_all())
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("atomic stage: {}", temporary.display()));
        write_result?;
        drop(file);

        after_stage()
            .change_context(RepositoryError::FileOperation)
            .attach("atomic update interrupted after staging")?;
        self.verify_expected_target(path, expected_original, identity.as_ref())?;

        commit(&temporary, &absolute)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("atomic target: {}", absolute.display()))?;
        stage.disarm();

        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("sync directory: {}", parent.display()))?;
        Ok(())
    }

    fn validate_expected_target(
        &self,
        path: &NormalizedRelativePath,
        expected_original: Option<&[u8]>,
    ) -> Result<Option<FileIdentity>, Report<RepositoryError>> {
        let absolute = self.root.join(path.as_path());
        match expected_original {
            Some(expected) => {
                self.validate_existing(path, false)?;
                let before = FileIdentity::read(&absolute)?;
                let actual = fs::read(&absolute)
                    .change_context(RepositoryError::FileOperation)
                    .attach_with(|| {
                        format!("read expected atomic target: {}", absolute.display())
                    })?;
                let after = FileIdentity::read(&absolute)?;
                if before != after || actual != expected {
                    return Err(Report::new(RepositoryError::FileOperation).attach(format!(
                        "atomic target changed before staging: {}",
                        absolute.display()
                    )));
                }
                Ok(Some(after))
            }
            None => {
                if final_entry_metadata(&absolute)?.is_some() {
                    return Err(Report::new(RepositoryError::FileOperation).attach(format!(
                        "atomic target was created concurrently: {}",
                        absolute.display()
                    )));
                }
                self.validate_parent_chain(path)?;
                Ok(None)
            }
        }
    }

    fn verify_expected_target(
        &self,
        path: &NormalizedRelativePath,
        expected_original: Option<&[u8]>,
        expected_identity: Option<&FileIdentity>,
    ) -> Result<(), Report<RepositoryError>> {
        let absolute = self.root.join(path.as_path());
        match (expected_original, expected_identity) {
            (Some(expected), Some(identity)) => {
                self.validate_existing(path, false)?;
                let before = FileIdentity::read(&absolute)?;
                let actual = fs::read(&absolute)
                    .change_context(RepositoryError::FileOperation)
                    .attach_with(|| format!("re-read atomic target: {}", absolute.display()))?;
                let after = FileIdentity::read(&absolute)?;
                if &before != identity || before != after || actual != expected {
                    return Err(Report::new(RepositoryError::FileOperation).attach(format!(
                        "atomic target changed while staging: {}",
                        absolute.display()
                    )));
                }
            }
            (None, None) => {
                if final_entry_metadata(&absolute)?.is_some() {
                    return Err(Report::new(RepositoryError::FileOperation).attach(format!(
                        "atomic target was created while staging: {}",
                        absolute.display()
                    )));
                }
                self.validate_parent_chain(path)?;
            }
            _ => {
                return Err(Report::new(RepositoryError::FileOperation)
                    .attach("atomic target expectation is inconsistent"));
            }
        }
        Ok(())
    }

    fn root_text(&self) -> Result<&str, Report<RepositoryError>> {
        self.root.to_str().ok_or_else(|| {
            Report::new(RepositoryError::Discovery).attach("repository root is not UTF-8")
        })
    }

    fn validate_existing(
        &self,
        path: &NormalizedRelativePath,
        allow_internal_symlink: bool,
    ) -> Result<(), Report<RepositoryError>> {
        self.validate_parent_chain(path)?;
        let absolute = self.root.join(path.as_path());
        let link_metadata = fs::symlink_metadata(&absolute)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("inspect path: {}", absolute.display()))?;
        if link_metadata.file_type().is_symlink() && !allow_internal_symlink {
            return Err(Report::new(RepositoryError::UnsafeEntry)
                .attach(format!("symlink path: {}", absolute.display())));
        }
        let canonical = fs::canonicalize(&absolute)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("canonicalize path: {}", absolute.display()))?;
        if !canonical.starts_with(&self.root) {
            return Err(Report::new(RepositoryError::SymlinkEscape)
                .attach(format!("path: {}", absolute.display()))
                .attach(format!("target: {}", canonical.display())));
        }

        let metadata = fs::metadata(&canonical)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("inspect target: {}", canonical.display()))?;
        if !metadata.is_file() || unsafe_mode(&metadata) {
            return Err(Report::new(RepositoryError::UnsafeEntry)
                .attach(format!("path: {}", canonical.display())));
        }
        if link_metadata.file_type().is_symlink() {
            let target_relative = canonical.strip_prefix(&self.root).map_err(|_error| {
                Report::new(RepositoryError::SymlinkEscape)
                    .attach(format!("target: {}", canonical.display()))
            })?;
            let target_relative = NormalizedRelativePath::new(target_relative)?;
            self.validate_parent_chain(&target_relative)?;
        }
        Ok(())
    }

    fn validate_parent_chain(
        &self,
        path: &NormalizedRelativePath,
    ) -> Result<(), Report<RepositoryError>> {
        self.validate_directory(&self.root)?;
        let Some(parent) = path.as_path().parent() else {
            return Ok(());
        };
        let mut ancestor = self.root.clone();
        for component in parent.components() {
            let Component::Normal(component) = component else {
                return Err(Report::new(RepositoryError::UnsafeRelativePath)
                    .attach(format!("path: {}", path.as_path().display())));
            };
            ancestor.push(component);
            if !ancestor
                .try_exists()
                .change_context(RepositoryError::FileOperation)?
            {
                break;
            }
            self.validate_directory(&ancestor)?;
        }
        Ok(())
    }

    fn validate_directory(&self, path: &Path) -> Result<(), Report<RepositoryError>> {
        let canonical = fs::canonicalize(path)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("canonicalize directory: {}", path.display()))?;
        if !canonical.starts_with(&self.root) {
            return Err(Report::new(RepositoryError::SymlinkEscape)
                .attach(format!("directory: {}", path.display()))
                .attach(format!("target: {}", canonical.display())));
        }
        let metadata = fs::metadata(&canonical).change_context(RepositoryError::FileOperation)?;
        if !metadata.is_dir() || unsafe_mode(&metadata) {
            return Err(Report::new(RepositoryError::UnsafeEntry)
                .attach(format!("directory: {}", canonical.display())));
        }
        Ok(())
    }

    fn create_safe_parent(
        &self,
        path: &NormalizedRelativePath,
        parent: &Path,
    ) -> Result<(), Report<RepositoryError>> {
        self.validate_parent_chain(path)?;
        fs::create_dir_all(parent)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("create directory: {}", parent.display()))?;
        self.validate_parent_chain(path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}

impl FileIdentity {
    fn read(path: &Path) -> Result<Self, Report<RepositoryError>> {
        let metadata = fs::symlink_metadata(path)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("inspect atomic target identity: {}", path.display()))?;
        if !metadata.is_file() || unsafe_mode(&metadata) {
            return Err(Report::new(RepositoryError::UnsafeEntry)
                .attach(format!("unsafe atomic target: {}", path.display())));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
        })
    }
}

struct AtomicStage {
    path: Option<PathBuf>,
}

impl AtomicStage {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for AtomicStage {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn final_entry_metadata(path: &Path) -> Result<Option<fs::Metadata>, Report<RepositoryError>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Report::new(error)
            .change_context(RepositoryError::FileOperation)
            .attach(format!("inspect final entry: {}", path.display()))),
    }
}

fn temporary_path(target: &Path) -> Result<PathBuf, Report<RepositoryError>> {
    let file_name = target.file_name().ok_or_else(|| {
        Report::new(RepositoryError::UnsafeRelativePath)
            .attach(format!("target: {}", target.display()))
    })?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(".docs-parity.tmp");
    Ok(target.with_file_name(temporary_name))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unsafe_mode(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o022 != 0
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsafe_mode(_metadata: &fs::Metadata) -> bool {
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (tempfile::TempDir, Repository, NormalizedRelativePath) {
        let directory = tempfile::tempdir().expect("should create repository fixture");
        let root = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
        let repository = Repository { root };
        let path = NormalizedRelativePath::new(Path::new("record.txt"))
            .expect("should create normalized fixture path");
        (directory, repository, path)
    }

    #[test]
    fn atomic_writer_rejects_stale_content_and_cleans_the_stage() {
        let (directory, repository, path) = repository();
        let target = directory.path().join("record.txt");
        fs::write(&target, b"original").expect("should write original");

        let result =
            repository.write_atomically_after_stage(&path, Some(b"original"), b"generated", || {
                fs::write(&target, b"concurrent")
            });

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).expect("should read concurrent edit"),
            b"concurrent"
        );
        assert!(
            !temporary_path(&target)
                .expect("should derive stage")
                .exists()
        );
    }

    #[test]
    fn atomic_writer_rejects_replacement_identity_even_with_equal_bytes() {
        let (directory, repository, path) = repository();
        let target = directory.path().join("record.txt");
        let replacement = directory.path().join("replacement.txt");
        fs::write(&target, b"original").expect("should write original");

        let result =
            repository.write_atomically_after_stage(&path, Some(b"original"), b"generated", || {
                fs::write(&replacement, b"original")?;
                fs::rename(&replacement, &target)
            });

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).expect("should read replacement"),
            b"original"
        );
        assert!(
            !temporary_path(&target)
                .expect("should derive stage")
                .exists()
        );
    }

    #[test]
    fn interrupted_atomic_writer_reaps_its_stage_without_touching_target() {
        let (directory, repository, path) = repository();
        let target = directory.path().join("record.txt");
        fs::write(&target, b"original").expect("should write original");

        let result =
            repository.write_atomically_after_stage(&path, Some(b"original"), b"generated", || {
                Err(std::io::Error::other("injected interruption"))
            });

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).expect("should read original"),
            b"original"
        );
        assert!(
            !temporary_path(&target)
                .expect("should derive stage")
                .exists()
        );
    }

    #[test]
    fn failed_atomic_rename_cleans_the_stage_and_preserves_the_target() {
        let (directory, repository, path) = repository();
        let target = directory.path().join("record.txt");
        fs::write(&target, b"original").expect("should write original");

        let result = repository.write_atomically_with_commit(
            &path,
            Some(b"original"),
            b"generated",
            || Ok(()),
            |_stage, _target| Err(std::io::Error::other("injected rename failure")),
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).expect("should read original"),
            b"original"
        );
        assert!(
            !temporary_path(&target)
                .expect("should derive stage")
                .exists()
        );
    }

    #[test]
    fn atomic_writer_rejects_a_concurrently_created_target() {
        let (directory, repository, path) = repository();
        let target = directory.path().join("record.txt");

        let result = repository.write_atomically_after_stage(&path, None, b"generated", || {
            fs::write(&target, b"concurrent")
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read(&target).expect("should read concurrent creation"),
            b"concurrent"
        );
        assert!(
            !temporary_path(&target)
                .expect("should derive stage")
                .exists()
        );
    }
}
