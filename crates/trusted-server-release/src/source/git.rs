//! Bounded read-only Git plumbing under a cleared, hardened environment.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use error_stack::Report;

use super::source_error;
use crate::error::ReleaseError;
use crate::git_objects::ObjectReader;
use crate::preflight::AuditedEnvironment;
use crate::process::{BoundedCommand, ProcessLimits, ProcessResult};

const STDERR_LIMIT: u64 = 64 * 1024;
/// Read-only Git plumbing under a cleared, hardened environment.
pub(super) struct Git {
    program: PathBuf,
    /// Child `PATH`: only the resolved git executable's own directory.
    child_path: String,
    root: PathBuf,
    home: tempfile::TempDir,
    /// Remaining bytes of the aggregate object budget.
    objects: ObjectReader,
    wall_limit: Duration,
    attempt_deadline: Instant,
}

impl Git {
    pub(super) fn locate(
        root: &Path,
        audited: &AuditedEnvironment,
    ) -> Result<Self, Report<ReleaseError>> {
        let search_path = audited
            .search_path()
            .ok_or_else(|| source_error("PATH is required to locate git"))?;
        let program = search_path
            .split(':')
            .filter(|directory| directory.starts_with('/'))
            .map(|directory| Path::new(directory).join("git"))
            .find(|candidate| {
                fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| source_error("git was not found on an absolute PATH entry"))?;
        let child_path = program
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| source_error("git directory must be UTF-8"))?
            .to_owned();
        let home = tempfile::Builder::new()
            .prefix("release-git-home-")
            .tempdir()
            .map_err(|error| source_error(format!("cannot create private git HOME: {error}")))?;
        Ok(Self {
            program,
            child_path,
            root: root.to_path_buf(),
            home,
            objects: ObjectReader::source(),
            wall_limit: Duration::from_secs(120),
            attempt_deadline: Instant::now() + Duration::from_secs(900),
        })
    }

    #[cfg(test)]
    pub(super) fn with_wall_limit(mut self, limit: Duration) -> Self {
        self.wall_limit = limit;
        self
    }

    fn command(
        &self,
        args: &[&str],
        stdout_limit: u64,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(self.root.join(".git"));
        let mut work_tree = OsString::from("--work-tree=");
        work_tree.push(&self.root);
        let remaining = self
            .attempt_deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| source_error("Git authentication attempt exceeded its deadline"))?;
        let limits = ProcessLimits {
            wall: self.wall_limit.min(remaining),
            stdout_max: stdout_limit,
            stderr_max: STDERR_LIMIT,
        };
        let mut arguments = vec![git_dir, work_tree, OsString::from("--no-replace-objects")];
        arguments.extend(
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "protocol.allow=never",
            ]
            .into_iter()
            .chain(args.iter().copied())
            .map(OsString::from),
        );
        // HOME is fresh and private and XDG_CONFIG_HOME is absent. This
        // disables user configuration without an arbitrary Git config path.
        BoundedCommand::new(&self.program, &self.root, limits)?
            .arguments(arguments)
            .environment("PATH", &self.child_path)?
            .environment("HOME", self.home.path())?
            .environment("GIT_CONFIG_NOSYSTEM", "1")?
            .environment("GIT_NO_REPLACE_OBJECTS", "1")?
            .environment("GIT_TERMINAL_PROMPT", "0")?
            .environment("GIT_OPTIONAL_LOCKS", "0")?
            .environment("LC_ALL", "C")
    }

    fn execute(
        &self,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        stdout_limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        let mut command = self.command(args, stdout_limit)?;
        if let Some(bytes) = stdin {
            command = command.stdin_bytes(bytes);
        }
        command
            .run()
            .and_then(ProcessResult::require_success)
            .map_err(|report| {
                report
                    .change_context(ReleaseError::Source)
                    .attach(format!("git {}", args.join(" ")))
            })
    }

    /// Replaces the aggregate object budget so tests can exercise exhaustion.
    #[cfg(test)]
    pub(super) fn with_object_budget(self, bytes: u64) -> Self {
        self.objects.set_budget_for_tests(bytes);
        self
    }

    /// Runs one plumbing command whose stdout must fit `stdout_limit` bytes.
    pub(super) fn run(
        &self,
        args: &[&str],
        stdout_limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        self.execute(args, None, stdout_limit)
    }

    /// Reads and collision-checks source objects under the source byte policy.
    ///
    /// # Errors
    ///
    /// Returns an error for failed Git commands or invalid object bytes.
    pub(super) fn read_objects(
        &self,
        kind: &'static str,
        oids: &[&str],
        visit: impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
    ) -> Result<(), Report<ReleaseError>> {
        self.objects
            .read_objects(
                kind,
                oids,
                &mut |arguments, input, limit| self.execute(arguments, Some(input), limit),
                visit,
            )
            .map_err(|report| report.change_context(ReleaseError::Source))
    }
}
