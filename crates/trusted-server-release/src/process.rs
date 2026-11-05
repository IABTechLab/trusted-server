//! Bounded child-process execution.
//!
//! Every child runs from an absolute program path with a fixed argument
//! vector, an explicit working directory and an explicit environment applied
//! after clearing the inherited one. Each child leads a new process group.
//!
//! # Group kill and reaping
//!
//! The supervisor observes the leader's exit with
//! `waitid(WEXITED | WNOHANG | WNOWAIT)`, which leaves the leader as an
//! unreaped zombie. While that zombie pins the process-group ID, the group is
//! sent `SIGKILL` — after a normal exit as well as on deadline, cancellation
//! or output cap — so the signal cannot reach a recycled group. Only then is
//! the leader reaped (polling, bounded by the kill grace, never a blocking
//! wait), and read-only `killpg(pgid, 0)` probes confirm disappearance.
//!
//! A residual pgid-reuse window remains after reaping: if every member is
//! already gone and the ID is recycled before confirmation, a probe can
//! observe the unrelated group and cause a bounded false cleanup failure.
//! Signal zero cannot kill that group. Destructive signalling occurs only
//! before reaping, while the leader's identity remains pinned.
//!
//! Pipes are drained concurrently under byte caps and every wait is bounded,
//! so a descendant that leaves the group (`setsid`, job control) while still
//! holding the output pipes is reported as
//! [`ProcessFailure::DescendantEscaped`] instead of hanging the producer.
//!
//! # Residuals delegated to the Task 7 OS boundary
//!
//! This module does **not** contain a descendant that calls `setsid()` and
//! closes its stdio (a daemon): it leaves no group membership or pipe for the
//! supervisor to observe, and the run reports only the leader's outcome. The
//! Task 7 boundary must provide that containment — on Linux a container
//! private PID namespace with an init process (`docker run --init`), whose
//! teardown kills every namespace member; on macOS the sandbox profile.
//! macOS has no subreaper, and this module deliberately does not change
//! process-wide state such as `PR_SET_CHILD_SUBREAPER`.
//!
//! # Platform notes
//!
//! - On Linux, signalling a zombie succeeds, so `killpg` keeps returning `0`
//!   until init reaps reparented zombie descendants. Containers therefore need
//!   a reaping init (`--init`); without one, runs fail with
//!   [`ProcessFailure::GroupSurvived`] instead of passing silently.
//!   Process-group members cannot be enumerated portably, so the supervisor
//!   does not scan `/proc` to treat a zombie-only group as gone; it relies on
//!   that reaping init. The Linux test fixtures read `/proc/<pid>/stat` only
//!   to treat an already-killed `Z`-state fixture pid as dead.
//! - XNU's `killpg` reports `EPERM` while a group holds only zombies, so
//!   `EPERM` is retried within the kill grace and recorded with its real errno
//!   only if it persists.
//! - A persistent genuine `EPERM` and a group that survives `SIGKILL` need
//!   privileges or kernel states tests cannot arrange; those paths are
//!   unit-tested through their decision functions only.
//! - Process and group IDs `<= 1` are never signalled: `0`, `-1` and `1`
//!   would address the supervisor's own group, every process, or init.
//!
//! # Environment
//!
//! The environment denylist is a backstop, not the policy: the Task 7
//! boundary and preparation callers move to per-tool environment allowlists.
//! Compiler and tool selection (`RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`,
//! `CARGO_BUILD_RUSTC`, `CARGO_BUILD_RUSTDOC`, `CARGO_BUILD_RUSTFLAGS` and
//! the wrappers) is denied here; recipes may set compiler flags only through a
//! future reviewed API.
//!
//! Configuration redirectors need the same pinning. `HOME`, `CARGO_HOME` and
//! `XDG_CONFIG_HOME` are accepted here but choose where tools read
//! configuration; `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_RUSTC`
//! and `CARGO_BUILD_RUSTDOC` select compiler flags and tools and are denied
//! here until that reviewed API exists. Callers' per-tool allowlists (Task 7
//! boundary and preparation) must pin every such redirector to authenticated
//! private paths.
//!
//! A timeout is the [`ProcessOutcome::TimedOut`] outcome; errnos are recorded
//! only from the operating system and never synthesized.

mod environment;
mod pipes;
mod redact;
mod supervise;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use error_stack::Report;

use self::environment::{EnvironmentValue, is_credential};
use self::pipes::{DrainedOutput, Pipes};
use self::redact::{REDACTED, redact, redact_cut_tail, redacted_text};
use self::supervise::{KILL_GRACE, Watch, observe, reap_within, settle};
use crate::error::ReleaseError;

/// Maximum stderr bytes shown in a failure excerpt (display bound only).
const STDERR_EXCERPT_BYTES: usize = 4 * 1024;

/// Required per-child limits; there are deliberately no defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessLimits {
    /// Wall-clock deadline measured from spawn.
    pub(crate) wall: Duration,
    /// Maximum captured stdout bytes; exceeding it kills the group.
    pub(crate) stdout_max: u64,
    /// Maximum captured stderr bytes; exceeding it kills the group.
    pub(crate) stderr_max: u64,
}

/// A cancellation flag shared between the caller and running children.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Requests cancellation of every child observing this flag.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns whether cancellation was requested.
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A captured child output stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum Stream {
    #[display("stdout")]
    Stdout,
    #[display("stderr")]
    Stderr,
}

/// How a supervised child run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum ProcessOutcome {
    /// The leader exited normally with `code`.
    #[display("exited with status {code}")]
    Exited { code: i32 },
    /// The leader was terminated by `signal`.
    #[display("was terminated by signal {signal}")]
    Signaled { signal: i32 },
    /// The wall-clock deadline expired and the group was killed.
    #[display("exceeded its wall-clock deadline")]
    TimedOut,
    /// Cancellation was requested and the group was killed (or never started).
    #[display("was cancelled")]
    Cancelled,
    /// A stream exceeded its byte cap and the group was killed.
    #[display("exceeded the {stream} byte cap")]
    OutputLimit { stream: Stream },
}

/// Typed classification attached to every [`ReleaseError::Process`] report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum ProcessFailure {
    /// The command description was rejected before spawning.
    #[display("invalid bounded command")]
    InvalidCommand,
    /// The operating system refused to start the child.
    #[display("spawn failed (errno {errno:?})")]
    Spawn { errno: Option<i32> },
    /// Observing or reaping the leader failed.
    #[display("wait failed (errno {errno:?})")]
    Wait { errno: Option<i32> },
    /// Group signalling or read-only observation failed outside `ESRCH`.
    #[display("process-group signal failed (errno {errno:?})")]
    Signal { errno: Option<i32> },
    /// The leader or group still existed after `SIGKILL` and the kill grace.
    #[display("process group survived SIGKILL past the grace period")]
    GroupSurvived,
    /// A descendant left the group and still holds the child pipes.
    #[display("a descendant escaped the process group while holding its pipes")]
    DescendantEscaped,
    /// Reading or writing a child pipe failed.
    #[display("child pipe failed (errno {errno:?})")]
    Pipe { errno: Option<i32> },
    /// The child ran but did not exit with status zero.
    #[display("child did not succeed")]
    Unsuccessful,
}

/// A file-mode creation mask a child runs under, restricted to permission bits.
///
/// Rendered as three octal digits such as `022` or `027`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
#[display("{_0:03o}")]
pub(crate) struct Umask(u16);

impl Umask {
    /// The largest valid mask, `0o777`.
    const MAX: u32 = 0o777;

    /// Accepts a numeric mask of at most `0o777`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] with [`ProcessFailure::InvalidCommand`]
    /// for any value above `0o777`.
    pub(crate) fn new(mask: u32) -> Result<Self, Report<ReleaseError>> {
        match u16::try_from(mask) {
            Ok(mask) if u32::from(mask) <= Self::MAX => Ok(Self(mask)),
            _ => Err(invalid(format!("umask {mask:#o} is not within 0..=0o777"))),
        }
    }

    /// Returns the numeric mask.
    #[must_use]
    pub(crate) fn mask(self) -> u32 {
        u32::from(self.0)
    }
}

/// A fully specified child command executed under [`ProcessLimits`].
pub(crate) struct BoundedCommand {
    program: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: BTreeMap<OsString, EnvironmentValue>,
    stdin: Option<Vec<u8>>,
    limits: ProcessLimits,
    umask: Option<Umask>,
    cancellation: Option<Cancellation>,
}

fn process_error(failure: ProcessFailure, message: impl fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Process)
        .attach(failure)
        .attach(message.to_string())
}

fn invalid(message: impl fmt::Display) -> Report<ReleaseError> {
    process_error(ProcessFailure::InvalidCommand, message)
}

impl BoundedCommand {
    /// Describes a child running `program` in `working_directory`.
    ///
    /// The command starts with no arguments, an empty environment and a null
    /// stdin.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] with [`ProcessFailure::InvalidCommand`]
    /// when `program` or `working_directory` is not absolute.
    pub(crate) fn new(
        program: &Path,
        working_directory: &Path,
        limits: ProcessLimits,
    ) -> Result<Self, Report<ReleaseError>> {
        if !program.is_absolute() {
            return Err(invalid("program path is not absolute"));
        }
        if !working_directory.is_absolute() {
            return Err(invalid("working directory is not absolute"));
        }
        Ok(Self {
            program: program.to_path_buf(),
            arguments: Vec::new(),
            working_directory: working_directory.to_path_buf(),
            environment: BTreeMap::new(),
            stdin: None,
            limits,
            cancellation: None,
            umask: None,
        })
    }

    /// Appends fixed arguments to the argument vector.
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
    /// Returns [`ProcessFailure::InvalidCommand`] for an empty or malformed
    /// name, a value containing NUL, a duplicate name, a code or configuration
    /// injection variable (`LD_*`, `DYLD_*`, `NODE_OPTIONS`, `npm_config_*`,
    /// Git configuration and interpreter search paths), or a known credential
    /// name, which only [`Self::secret_environment`] may carry.
    pub(crate) fn environment(
        self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<Self, Report<ReleaseError>> {
        if is_credential(name.as_ref()) {
            return Err(invalid(format!(
                "credential variable {:?} must be classified as secret",
                name.as_ref()
            )));
        }
        self.insert_environment(name.as_ref(), value.as_ref(), false)
    }

    /// Adds an environment variable whose value is redacted from all
    /// diagnostics, including captured stderr excerpts.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessFailure::InvalidCommand`] for the same reasons as
    /// [`Self::environment`] except the credential rule, or for an empty
    /// secret value.
    pub(crate) fn secret_environment(
        self,
        name: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> Result<Self, Report<ReleaseError>> {
        if value.as_ref().is_empty() {
            return Err(invalid(format!(
                "secret environment variable {:?} is empty",
                name.as_ref()
            )));
        }
        self.insert_environment(name.as_ref(), value.as_ref(), true)
    }

    /// Supplies fixed in-memory bytes on stdin instead of `/dev/null`.
    #[must_use]
    pub(crate) fn stdin_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Observes `cancellation`; setting it kills and reaps the child group,
    /// and a flag set before [`Self::run`] prevents spawning at all.
    #[must_use]
    pub(crate) fn cancellation(mut self, cancellation: Cancellation) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Sets the child's umask before it executes; the parent's is untouched.
    #[must_use]
    pub(crate) fn umask(mut self, umask: Umask) -> Self {
        self.umask = Some(umask);
        self
    }

    fn secrets(&self) -> Vec<Vec<u8>> {
        self.environment
            .values()
            .filter(|entry| entry.secret)
            .map(|entry| entry.value.as_bytes().to_vec())
            .collect()
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .current_dir(&self.working_directory)
            .env_clear()
            .envs(
                self.environment
                    .iter()
                    .map(|(name, entry)| (name, &entry.value)),
            )
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(umask) = self.umask {
            let mode = libc::mode_t::from(umask.0);
            // SAFETY: the closure runs in the forked child before exec and
            // only calls `umask`, which is async-signal-safe, touches no
            // memory and cannot fail; it captures one `Copy` integer.
            unsafe {
                command.pre_exec(move || {
                    libc::umask(mode);
                    Ok(())
                });
            }
        }
        command
    }

    fn result(
        &self,
        outcome: ProcessOutcome,
        output: DrainedOutput,
        started: Instant,
    ) -> ProcessResult {
        ProcessResult {
            program: self.program.clone(),
            outcome,
            stdout: output.stdout,
            stderr: output.stderr,
            stderr_truncated: output.stderr_truncated,
            elapsed: started.elapsed(),
            secrets: self.secrets(),
        }
    }

    /// Spawns the child and returns it with its usable process-group ID.
    fn spawn_group(&self) -> Result<(Child, libc::pid_t), Report<ReleaseError>> {
        let mut child = self.command().spawn().map_err(|source| {
            process_error(
                ProcessFailure::Spawn {
                    errno: source.raw_os_error(),
                },
                format!("cannot spawn child: {source}"),
            )
        })?;
        if let Some(process_group) = libc::pid_t::try_from(child.id())
            .ok()
            .filter(|pid| *pid > 1)
        {
            return Ok((child, process_group));
        }
        // The ID is unusable for killpg, so only the leader can be stopped.
        let mut report = process_error(
            ProcessFailure::Spawn { errno: None },
            "child process ID is unusable as a process group",
        );
        if let Err(source) = child.kill() {
            report = report.attach(format!("cannot kill the child: {source}"));
        }
        match reap_within(&mut child, KILL_GRACE) {
            Ok(Some(_)) => {}
            Ok(None) => report = report.attach("child stayed unreaped after the kill grace"),
            Err(source) => report = report.attach(format!("cannot reap the child: {source}")),
        }
        Err(report)
    }

    /// Runs the child to an outcome under the configured limits.
    ///
    /// Deadlines, cancellation and output caps are outcomes, not errors: the
    /// group is killed while the leader pins its ID, the leader is reaped and
    /// the group confirmed gone before returning. After a normal leader exit
    /// the group is killed as well so no same-group descendant outlives the
    /// run.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] carrying a [`ProcessFailure`] when the
    /// child cannot be spawned (with the real errno), the group cannot be
    /// signalled, reaped or does not vanish within the kill grace, a pipe
    /// fails, or a descendant escaped the group while holding the child
    /// pipes.
    pub(crate) fn run(&self) -> Result<ProcessResult, Report<ReleaseError>> {
        let started = Instant::now();
        if self.is_cancelled() {
            return Ok(self.result(ProcessOutcome::Cancelled, DrainedOutput::default(), started));
        }
        let deadline = started + self.limits.wall;
        let (mut child, process_group) = self.spawn_group()?;
        let Some((pipes, overflow)) = Pipes::start(&mut child, self.stdin.clone(), &self.limits)
        else {
            settle(&mut child, process_group)?;
            return Err(process_error(
                ProcessFailure::Pipe { errno: None },
                "child pipes are unavailable",
            ));
        };
        let watch = Watch {
            overflow: &overflow,
            deadline,
            cancellation: self.cancellation.as_ref(),
        };
        let stop = observe(&mut child, process_group, &watch)?;
        let status = settle(&mut child, process_group)?;
        let drained = pipes.join()?;
        let outcome = final_outcome(stop, &overflow, &drained, status)?;
        Ok(self.result(outcome, drained, started))
    }
}

/// Chooses the reported outcome once the group is gone and pipes are drained.
///
/// A stop condition wins; otherwise a leader that died of `SIGPIPE` after a
/// capped drain closed its pipe still reports the cap, not its own exit.
fn final_outcome(
    stop: Option<ProcessOutcome>,
    overflow: &Receiver<Stream>,
    drained: &DrainedOutput,
    status: ExitStatus,
) -> Result<ProcessOutcome, Report<ReleaseError>> {
    if let Some(outcome) = stop {
        return Ok(outcome);
    }
    match overflow.try_recv().ok().or(drained.overflowed) {
        Some(stream) => Ok(ProcessOutcome::OutputLimit { stream }),
        None => status_outcome(status),
    }
}

impl fmt::Debug for BoundedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secrets = self.secrets();
        let text = |bytes: &[u8]| redacted_text(bytes, &secrets);
        let arguments: Vec<String> = self
            .arguments
            .iter()
            .map(|argument| text(argument.as_bytes()))
            .collect();
        let environment: BTreeMap<String, String> = self
            .environment
            .iter()
            .map(|(name, entry)| {
                let value = if entry.secret {
                    String::from_utf8_lossy(REDACTED).into_owned()
                } else {
                    text(entry.value.as_bytes())
                };
                (text(name.as_bytes()), value)
            })
            .collect();
        formatter
            .debug_struct("BoundedCommand")
            .field("program", &text(self.program.as_os_str().as_bytes()))
            .field("arguments", &arguments)
            .field(
                "working_directory",
                &text(self.working_directory.as_os_str().as_bytes()),
            )
            .field("environment", &environment)
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .field("limits", &self.limits)
            .field("cancellable", &self.cancellation.is_some())
            .finish()
    }
}

/// The bounded result of one supervised child run.
pub(crate) struct ProcessResult {
    program: PathBuf,
    outcome: ProcessOutcome,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Whether the stderr drain stopped at its cap, whatever the reported outcome.
    stderr_truncated: bool,
    elapsed: Duration,
    secrets: Vec<Vec<u8>>,
}

impl ProcessResult {
    /// Returns how the run ended.
    #[must_use]
    pub(crate) fn outcome(&self) -> ProcessOutcome {
        self.outcome
    }

    /// Returns captured stdout, at most the configured cap.
    #[must_use]
    pub(crate) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Returns captured stderr, at most the configured cap.
    #[must_use]
    pub(crate) fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Returns wall-clock time from spawn to a confirmed-gone group.
    #[must_use]
    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns stdout only when the child exited with status zero.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] with [`ProcessFailure::Unsuccessful`]
    /// for any other outcome, carrying a bounded, lossy stderr tail with every
    /// secret environment value redacted, including a secret prefix cut off by
    /// the stderr cap.
    pub(crate) fn require_success(self) -> Result<Vec<u8>, Report<ReleaseError>> {
        if self.outcome == (ProcessOutcome::Exited { code: 0 }) {
            return Ok(self.stdout);
        }
        let redacted = redact(&self.stderr, &self.secrets);
        // Only a stderr cut by its cap can end inside a secret; the cut is
        // tracked per stream because another stream may have overflowed first.
        let redacted = if self.stderr_truncated {
            redact_cut_tail(redacted, &self.secrets)
        } else {
            redacted
        };
        let start = redacted.len().saturating_sub(STDERR_EXCERPT_BYTES);
        let excerpt = String::from_utf8_lossy(redacted.get(start..).unwrap_or_default());
        Err(process_error(
            ProcessFailure::Unsuccessful,
            format!(
                "{} {}; stderr tail ({} of {} bytes): {excerpt}",
                redacted_text(self.program.as_os_str().as_bytes(), &self.secrets),
                self.outcome,
                redacted.len() - start,
                redacted.len()
            ),
        ))
    }
}

impl fmt::Debug for ProcessResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessResult")
            .field(
                "program",
                &redacted_text(self.program.as_os_str().as_bytes(), &self.secrets),
            )
            .field("outcome", &self.outcome)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("elapsed", &self.elapsed)
            .finish_non_exhaustive()
    }
}

fn status_outcome(status: ExitStatus) -> Result<ProcessOutcome, Report<ReleaseError>> {
    match (status.code(), status.signal()) {
        (Some(code), _) => Ok(ProcessOutcome::Exited { code }),
        (None, Some(signal)) => Ok(ProcessOutcome::Signaled { signal }),
        (None, None) => Err(process_error(
            ProcessFailure::Wait { errno: None },
            format!("unrecognized child status {status:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;
