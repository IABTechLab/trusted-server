use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
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

    fn as_path(&self) -> &Path {
        &self.0
    }

    fn as_utf8(&self) -> Result<&str, Report<RepositoryError>> {
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
    let bytes = value.as_bytes();
    let has_portable_drive_prefix =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    !value.contains('\\') && !has_portable_drive_prefix
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

        let root_text = core::str::from_utf8(&output.stdout)
            .change_context(RepositoryError::Discovery)?
            .trim_end();
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

        let mut record = Vec::new();
        for path in paths {
            record.extend_from_slice(path.as_utf8()?.as_bytes());
            record.push(b'\n');
        }
        Ok(record)
    }

    pub(crate) fn read_optional(
        &self,
        path: &NormalizedRelativePath,
    ) -> Result<Option<Vec<u8>>, Report<RepositoryError>> {
        let absolute = self.root.join(path.as_path());
        if !absolute
            .try_exists()
            .change_context(RepositoryError::FileOperation)?
        {
            self.validate_parent_chain(path)?;
            return Ok(None);
        }

        self.validate_existing(path, false)?;
        fs::read(&absolute)
            .map(Some)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("read path: {}", absolute.display()))
    }

    pub(crate) fn write_atomically(
        &self,
        path: &NormalizedRelativePath,
        contents: &[u8],
    ) -> Result<(), Report<RepositoryError>> {
        let absolute = self.root.join(path.as_path());
        if absolute
            .try_exists()
            .change_context(RepositoryError::FileOperation)?
        {
            self.validate_existing(path, false)?;
        }
        let parent = absolute.parent().ok_or_else(|| {
            Report::new(RepositoryError::UnsafeRelativePath)
                .attach(format!("path: {}", path.as_path().display()))
        })?;
        self.create_safe_parent(path, parent)?;

        let temporary = temporary_path(&absolute)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary)
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("atomic stage: {}", temporary.display()))?;

        let write_result = file
            .write_all(contents)
            .and_then(|()| file.sync_all())
            .inspect_err(|_error| {
                let _ = fs::remove_file(&temporary);
            })
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("atomic stage: {}", temporary.display()));
        write_result?;
        drop(file);

        if let Err(error) = fs::rename(&temporary, &absolute) {
            let _ = fs::remove_file(&temporary);
            return Err(Report::new(error)
                .change_context(RepositoryError::FileOperation)
                .attach(format!("atomic target: {}", absolute.display())));
        }

        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .change_context(RepositoryError::FileOperation)
            .attach_with(|| format!("sync directory: {}", parent.display()))?;
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

#[cfg(unix)]
fn unsafe_mode(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o022 != 0
}

#[cfg(not(unix))]
fn unsafe_mode(_metadata: &fs::Metadata) -> bool {
    false
}
