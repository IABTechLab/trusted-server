//! The command description a [`Boundary`](super::Boundary) runs confined.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use error_stack::Report;

use crate::error::ReleaseError;
pub(crate) use crate::process::Umask;
use crate::process::{BoundedCommand, Cancellation, ProcessLimits};

/// A child to run inside a [`Boundary`](super::Boundary), described by host
/// paths.
///
/// The program must lie inside a boundary root and the working directory
/// inside an immutable or writable root; on Linux both are translated to the
/// container namespace. Both must be absolute with only normal components
/// (no `.` or `..`), so the root they are located in is the root they reach.
/// Arguments are passed verbatim. Environment values are validated by the
/// same rules as [`BoundedCommand::environment`]; secret values are not
/// supported because Docker would carry them in argv.
pub(crate) struct ConfinedCommand {
    pub(super) program: PathBuf,
    pub(super) arguments: Vec<OsString>,
    pub(super) working_directory: PathBuf,
    pub(super) environment: Vec<(OsString, OsString)>,
    /// Names in `environment` added through [`Self::tool_environment`].
    tool_selectors: Vec<OsString>,
    pub(super) stdin: Option<Vec<u8>>,
    pub(super) limits: ProcessLimits,
    pub(super) cancellation: Option<Cancellation>,
    pub(super) umask: Option<Umask>,
}

impl fmt::Debug for ConfinedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfinedCommand")
            .field("program", &self.program)
            .field("arguments", &self.arguments)
            .field("working_directory", &self.working_directory)
            .field(
                "environment_names",
                &self
                    .environment
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .field("limits", &self.limits)
            .field("cancellation", &self.cancellation)
            .field("umask", &self.umask)
            .finish()
    }
}

fn invalid_path(path: &Path) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(format!(
        "confined path {} must be absolute with only normal components",
        path.display()
    ))
}

/// Rejects relative paths and `.`/`..` segments.
///
/// The raw bytes are split because [`Path::components`] silently drops
/// interior `.` segments.
fn require_normal(path: &Path) -> Result<(), Report<ReleaseError>> {
    let dotted = path
        .as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|segment| segment == b"." || segment == b"..");
    if !path.has_root() || dotted {
        return Err(invalid_path(path));
    }
    Ok(())
}

impl ConfinedCommand {
    /// Describes `program` running in `working_directory` under `limits`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when either path is relative or has
    /// a `.` or `..` component.
    pub(crate) fn new(
        program: &Path,
        working_directory: &Path,
        limits: ProcessLimits,
    ) -> Result<Self, Report<ReleaseError>> {
        require_normal(program)?;
        require_normal(working_directory)?;
        Ok(Self {
            program: program.to_path_buf(),
            arguments: Vec::new(),
            working_directory: working_directory.to_path_buf(),
            environment: Vec::new(),
            tool_selectors: Vec::new(),
            stdin: None,
            limits,
            cancellation: None,
            umask: None,
        })
    }

    /// Adds an explicitly selected tool path such as `RUSTC`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] for any name or value
    /// [`BoundedCommand::tool_selector_environment`] rejects, including a
    /// duplicate.
    pub(crate) fn tool_environment(
        mut self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<Self, Report<ReleaseError>> {
        self.validated(None)?
            .tool_selector_environment(name.as_ref(), value.as_ref())?;
        self.environment
            .push((name.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self.tool_selectors.push(name.as_ref().to_os_string());
        Ok(self)
    }

    /// Appends fixed arguments.
    #[must_use]
    pub(crate) fn arguments<I>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = OsString>,
    {
        self.arguments.extend(arguments);
        self
    }

    /// Adds a non-secret environment variable.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] for any name or value
    /// [`BoundedCommand::environment`] rejects, including a duplicate.
    pub(crate) fn environment(
        mut self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<Self, Report<ReleaseError>> {
        self.validated(Some((name.as_ref(), value.as_ref())))?;
        self.environment
            .push((name.as_ref().to_os_string(), value.as_ref().to_os_string()));
        Ok(self)
    }

    /// Returns whether `name` was added through [`Self::tool_environment`].
    pub(super) fn is_tool_selector(&self, name: &OsStr) -> bool {
        self.tool_selectors.iter().any(|selected| selected == name)
    }

    /// Returns the tool-selector entries, for the boundary's placement check.
    pub(super) fn tool_selector_entries(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.environment
            .iter()
            .filter(|(name, _)| self.is_tool_selector(name))
            .map(|(name, value)| (name, value))
    }

    /// Validates the environment plus `extra` through a scratch
    /// [`BoundedCommand`].
    pub(super) fn validated(
        &self,
        extra: Option<(&OsStr, &OsStr)>,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        let mut validator =
            BoundedCommand::new(&self.program, &self.working_directory, self.limits)?;
        for (name, value) in self.environment.iter() {
            validator = if self.tool_selectors.contains(name) {
                validator.tool_selector_environment(name, value)?
            } else {
                validator.environment(name, value)?
            };
        }
        if let Some((name, value)) = extra {
            validator = validator.environment(name, value)?;
        }
        Ok(validator)
    }

    /// Supplies fixed stdin bytes.
    #[must_use]
    pub(crate) fn stdin_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Observes `cancellation`.
    #[must_use]
    pub(crate) fn cancellation(mut self, cancellation: Cancellation) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Runs the child under `umask`.
    ///
    /// macOS sets it in the child just before `sandbox-exec` executes; Linux
    /// runs the program through `/bin/sh` of the selected image, which must
    /// exist there. The orchestrator's own umask is never changed.
    #[must_use]
    pub(crate) fn umask(mut self, umask: Umask) -> Self {
        self.umask = Some(umask);
        self
    }

    /// Returns the requested umask, for tests of command construction.
    #[cfg(test)]
    pub(crate) fn requested_umask(&self) -> Option<Umask> {
        self.umask
    }

    /// Applies stdin and cancellation to the wrapping bounded command.
    pub(super) fn finish(&self, mut bounded: BoundedCommand) -> BoundedCommand {
        if let Some(bytes) = &self.stdin {
            bounded = bounded.stdin_bytes(bytes.clone());
        }
        if let Some(cancellation) = &self.cancellation {
            bounded = bounded.cancellation(cancellation.clone());
        }
        bounded
    }
}

#[cfg(test)]
mod selector_tests;
