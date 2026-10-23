//! Supervision of one bounded child process group.
//!
//! The child leads its own process group. Its exit is observed with
//! `waitid(WNOWAIT)`, which leaves the leader an unreaped zombie: while that
//! zombie exists its PID, and therefore the group ID, cannot be reused, so
//! signalling the group can never hit an unrelated process. The group is
//! killed only in that window and the leader is reaped last.
//!
//! After the group `SIGKILL`, closure of both output pipes (EOF in the drain
//! threads) is the liveness signal, not repeated `killpg` results: killed
//! descendants become zombies that a non-reaping PID 1 (for example a
//! container without an init) may never collect, yet zombies hold no pipes.
//! A descendant that left the group and keeps the pipes open is reported as
//! escaped after a bounded grace. Limitation: a descendant that escaped the
//! group *and* closed its copies of the pipes is not observable here and may
//! outlive verification; it never contributes output.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use error_stack::Report;

use super::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use super::error::{ReleaseError, report};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const READ_CHUNK: usize = 64 * 1024;

/// Exit status and raw output of a child whose group was fully drained.
pub(super) struct Completed {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

struct Drained {
    bytes: Vec<u8>,
    overflowed: bool,
}

/// Both drained pipes, or why draining could not complete.
/// A drain thread collecting one pipe.
type Reader = JoinHandle<io::Result<Drained>>;

type DrainResult = Result<(Drained, Drained), Report<ReleaseError>>;

/// A drain thread reached its byte cap.
struct Overflow;

enum Stop {
    Timeout,
    Cancelled,
    Overflow,
    Wait(io::Error),
}

impl Stop {
    fn into_report(self) -> Report<ReleaseError> {
        match self {
            Self::Timeout => report(
                ReleaseError::Timeout,
                "gh child exceeded its child or attempt deadline",
            ),
            Self::Cancelled => report(ReleaseError::Cancelled, "gh child was cancelled"),
            Self::Overflow => report(
                ReleaseError::OutputLimit,
                "gh stdout or stderr exceeded its cap",
            ),
            Self::Wait(source) => report(ReleaseError::Process, source),
        }
    }
}

/// Sends one `SIGKILL` to the whole group.
///
/// `ESRCH` (no members) and `EPERM` (macOS reports it when only the zombie
/// leader remains) both mean nothing signalable is left.
fn kill_group(process_group: libc::pid_t) -> Result<(), Report<ReleaseError>> {
    // SAFETY: `killpg` only reads its two integer arguments; failure is
    // reported through errno.
    let result = unsafe { libc::killpg(process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    match source.raw_os_error() {
        Some(libc::ESRCH | libc::EPERM) => Ok(()),
        _ => Err(report(
            ReleaseError::Process,
            format!("killpg failed: {source}"),
        )),
    }
}

/// Returns whether the leader has exited, without reaping it.
fn leader_exited(pid: libc::pid_t) -> io::Result<bool> {
    let id = libc::id_t::try_from(pid).map_err(io::Error::other)?;
    // SAFETY: an all-zero `siginfo_t` is a valid value of this plain C struct.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, writable `siginfo_t` for the duration of the
    // call; `WNOWAIT` leaves the child waitable so it stays unreaped.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(source);
    }
    // SAFETY: `waitid` succeeded, so `info` is initialized; with `WNOHANG` and
    // no state change `si_pid` stays zero.
    Ok(unsafe { info.si_pid() } != 0)
}

fn drain<R: Read + Send + 'static>(
    mut reader: R,
    limit: u64,
    overflow: Sender<Overflow>,
) -> Reader {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = vec![0; READ_CHUNK];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => return Err(source),
            };
            if count == 0 {
                return Ok(Drained {
                    bytes,
                    overflowed: false,
                });
            }
            if count as u64 > limit.saturating_sub(bytes.len() as u64) {
                // Dropping the reader closes the pipe; the supervisor kills the group.
                let _ = overflow.send(Overflow);
                return Ok(Drained {
                    bytes,
                    overflowed: true,
                });
            }
            bytes.extend_from_slice(buffer.get(..count).unwrap_or_default());
        }
    })
}

/// Waits until the leader exits (left unreaped) or a stop condition occurs.
fn wait_for_leader(
    pid: libc::pid_t,
    overflow: &Receiver<Overflow>,
    child_deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(), Stop> {
    loop {
        match leader_exited(pid) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(source) => return Err(Stop::Wait(source)),
        }
        if cancellation.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        if Instant::now() >= child_deadline {
            return Err(Stop::Timeout);
        }
        match overflow.recv_timeout(POLL_INTERVAL) {
            Ok(Overflow) => return Err(Stop::Overflow),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Waits, bounded, for a killed leader to become a zombie.
fn await_leader_exit(pid: libc::pid_t, grace: Duration) -> Result<(), Report<ReleaseError>> {
    let until = Instant::now() + grace;
    loop {
        if leader_exited(pid).map_err(|source| report(ReleaseError::Process, source))? {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(report(
                ReleaseError::Process,
                "gh leader did not exit after SIGKILL",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn join_readers(
    stdout: Reader,
    stderr: Reader,
    grace: Duration,
) -> Result<(Drained, Drained), Report<ReleaseError>> {
    let until = Instant::now() + grace;
    while !(stdout.is_finished() && stderr.is_finished()) {
        if Instant::now() >= until {
            // The reader threads stay blocked until the escaped process
            // closes its pipe ends; they hold no other resources.
            return Err(report(
                ReleaseError::DescendantEscaped,
                "a gh descendant left the process group and still holds its output pipes",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
    let join = |handle: Reader| {
        handle
            .join()
            .map_err(|_| report(ReleaseError::Process, "gh output reader panicked"))?
            .map_err(|source| report(ReleaseError::Process, source))
    };
    Ok((join(stdout)?, join(stderr)?))
}

/// Kills the group while the unreaped leader pins its ID, waits for pipe
/// closure, then reaps the leader.
fn settle(
    child: &mut Child,
    process_group: libc::pid_t,
    leader_done: bool,
    readers: (Reader, Reader),
    grace: Duration,
) -> (DrainResult, io::Result<ExitStatus>) {
    let mut cleanup = kill_group(process_group);
    if !leader_done && cleanup.is_ok() {
        // A second group signal while the zombie still pins the ID catches
        // anything forked between the first signal and the leader's death.
        cleanup = await_leader_exit(process_group, grace).and_then(|()| kill_group(process_group));
    }
    let drained = cleanup.and_then(|()| join_readers(readers.0, readers.1, grace));
    if !leader_done {
        let _ = child.kill();
    }
    (drained, child.wait())
}

/// Runs `command` (already configured with piped output and its own process
/// group) under the child deadline, attempt deadline, cancellation and byte
/// caps.
///
/// # Errors
///
/// Returns [`ReleaseError::Timeout`], [`ReleaseError::Cancelled`],
/// [`ReleaseError::OutputLimit`], [`ReleaseError::DescendantEscaped`] or
/// [`ReleaseError::Process`]. The child is reaped on every path.
pub(super) fn supervise(
    command: &mut Command,
    process_bounds: &ProcessBounds,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<Completed, Report<ReleaseError>> {
    let child_deadline = (Instant::now() + process_bounds.child_timeout).min(deadline.instant());
    let mut child = command.spawn().map_err(|source| {
        report(
            ReleaseError::Process,
            format!("cannot spawn gh ({:?}): {source}", source.kind()),
        )
    })?;
    let process_group = libc::pid_t::try_from(child.id())
        .map_err(|_| report(ReleaseError::Process, "gh process ID is out of range"));
    let pipes = child.stdout.take().zip(child.stderr.take());
    let (Ok(process_group), Some((stdout, stderr))) = (process_group, pipes) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(report(
            ReleaseError::Process,
            "gh process ID or pipes are unavailable",
        ));
    };
    let (sender, overflow) = mpsc::channel();
    let readers = (
        drain(stdout, process_bounds.stdout_bytes, sender.clone()),
        drain(stderr, process_bounds.stderr_bytes, sender),
    );
    let stopped = wait_for_leader(process_group, &overflow, child_deadline, cancellation);
    let settled = settle(
        &mut child,
        process_group,
        stopped.is_ok(),
        readers,
        process_bounds.kill_grace,
    );
    complete(stopped, settled)
}

/// Turns a stop condition and cleanup outcome into the supervision result.
fn complete(
    stopped: Result<(), Stop>,
    (drained, status): (DrainResult, io::Result<ExitStatus>),
) -> Result<Completed, Report<ReleaseError>> {
    if let Err(stop) = stopped {
        let stop = stop.into_report();
        return Err(match drained {
            Err(failure) => stop.attach(format!(
                "cleanup also failed: {}",
                failure.current_context()
            )),
            Ok(_) => stop,
        });
    }
    let (stdout, stderr) = drained?;
    let status = status.map_err(|source| report(ReleaseError::Process, source))?;
    if stdout.overflowed || stderr.overflowed {
        return Err(Stop::Overflow.into_report());
    }
    Ok(Completed {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}
