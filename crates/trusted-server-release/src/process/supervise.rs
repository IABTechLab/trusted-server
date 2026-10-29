//! Leader observation, process-group kill and bounded reaping.

use std::io;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use error_stack::Report;

use super::{Cancellation, ProcessFailure, ProcessOutcome, Stream, process_error};
use crate::error::ReleaseError;

/// Bounded wait for reaping, group disappearance and pipe closure after a kill.
///
/// This is a supervisor liveness bound, not a release policy value: a killed
/// group closes its pipes promptly, so anything still holding them after this
/// grace has escaped the group.
pub(super) const KILL_GRACE: Duration = Duration::from_secs(2);
/// Supervisor polling interval for leader status, deadlines and cancellation.
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Stop conditions observed while the leader runs.
pub(super) struct Watch<'a> {
    pub(super) overflow: &'a Receiver<Stream>,
    pub(super) deadline: Instant,
    pub(super) cancellation: Option<&'a Cancellation>,
}

/// Returns whether the leader has exited, leaving it unreaped.
///
/// `WNOWAIT` keeps the zombie, so the process-group ID stays pinned until
/// [`settle`] has signalled the group.
fn leader_exited(pid: libc::pid_t) -> io::Result<bool> {
    let id = libc::id_t::try_from(pid)
        .ok()
        .filter(|_| pid > 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unusable child pid"))?;
    // SAFETY: `siginfo_t` is plain integer and pointer data; all-zero is a
    // valid value and lets `si_pid == 0` signal "no state change".
    let mut info: libc::siginfo_t = unsafe { core::mem::zeroed() };
    // SAFETY: `info` is a valid, writable `siginfo_t` for the call's duration.
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
    // SAFETY: `waitid` filled `info` (or left it zeroed under `WNOHANG`).
    Ok(unsafe { info.si_pid() } != 0)
}

/// Polls until the leader exits (`None`) or a stop condition fires.
fn wait_for_leader(pid: libc::pid_t, watch: &Watch<'_>) -> io::Result<Option<ProcessOutcome>> {
    loop {
        if let Ok(stream) = watch.overflow.try_recv() {
            return Ok(Some(ProcessOutcome::OutputLimit { stream }));
        }
        if leader_exited(pid)? {
            return Ok(None);
        }
        if watch.cancellation.is_some_and(Cancellation::is_cancelled) {
            return Ok(Some(ProcessOutcome::Cancelled));
        }
        if Instant::now() >= watch.deadline {
            return Ok(Some(ProcessOutcome::TimedOut));
        }
        match watch.overflow.recv_timeout(POLL_INTERVAL) {
            Ok(stream) => return Ok(Some(ProcessOutcome::OutputLimit { stream })),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL_INTERVAL),
        }
    }
}

pub(super) fn signal_group(process_group: libc::pid_t) -> io::Result<()> {
    group_signal(process_group, libc::SIGKILL)
}

fn group_signal(process_group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // 0, -1 and 1 would address this supervisor's group, every process or
    // init; refuse them without inventing an errno.
    if process_group <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to signal a non-child process group",
        ));
    }
    // SAFETY: `killpg` only reads its two integer arguments; it touches no
    // memory owned by this process.
    let result = unsafe { libc::killpg(process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn observation_failure(source: &io::Error) -> Report<ReleaseError> {
    process_error(
        ProcessFailure::Signal {
            errno: source.raw_os_error(),
        },
        format!("cannot observe process group: {source}"),
    )
}

/// Returns whether `source` may be a transient XNU group-operation refusal.
fn is_transient_refusal(source: &io::Error) -> bool {
    source.raw_os_error() == Some(libc::EPERM)
}

/// Polls for the leader's exit status for at most `grace`; never blocks.
///
/// Returns `Ok(None)` when the leader is still unreaped after the grace.
pub(super) fn reap_within(child: &mut Child, grace: Duration) -> io::Result<Option<ExitStatus>> {
    let until = Instant::now() + grace;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Classifies a leader that stayed unreaped after `SIGKILL` and the grace,
/// keeping the real errno of a refused group signal.
pub(super) fn unreaped_failure(refusal: Option<&io::Error>) -> ProcessFailure {
    match refusal {
        Some(source) => ProcessFailure::Signal {
            errno: source.raw_os_error(),
        },
        None => ProcessFailure::GroupSurvived,
    }
}

/// Kills the group while the unreaped leader pins its ID, reaps the leader
/// within the grace, then confirms the group is gone.
pub(super) fn settle(
    child: &mut Child,
    process_group: libc::pid_t,
) -> Result<ExitStatus, Report<ReleaseError>> {
    let refusal = signal_group(process_group)
        .err()
        .filter(|source| source.raw_os_error() != Some(libc::ESRCH));
    // A refused group signal may be XNU's zombie-only EPERM; signal the
    // leader directly as well so a live leader still dies. A failure here
    // matters only if the leader then stays unreaped, where it is reported.
    let direct = refusal.as_ref().and_then(|_| child.kill().err());
    let status = reap_within(child, KILL_GRACE).map_err(|source| {
        process_error(
            ProcessFailure::Wait {
                errno: source.raw_os_error(),
            },
            format!("cannot reap child: {source}"),
        )
    })?;
    let Some(status) = status else {
        let message = match &refusal {
            Some(source) => format!("child stayed unreaped after killpg failed: {source}"),
            None => "child stayed unreaped after SIGKILL".to_owned(),
        };
        let report = process_error(unreaped_failure(refusal.as_ref()), message);
        return Err(match direct {
            Some(source) => report.attach(format!("direct leader kill also failed: {source}")),
            None => report,
        });
    };
    confirm_group_gone(process_group)?;
    Ok(status)
}

/// Observes group disappearance without delivering another signal.
///
/// This runs after the leader is reaped, so the group ID is no longer pinned:
/// a recycled ID can cause a bounded false cleanup failure, but signal zero
/// cannot terminate the unrelated group. The initial kill stays in [`settle`].
fn confirm_group_gone(process_group: libc::pid_t) -> Result<(), Report<ReleaseError>> {
    let until = Instant::now() + KILL_GRACE;
    confirm_group_with(
        process_group,
        group_signal,
        || Instant::now() >= until,
        || thread::sleep(POLL_INTERVAL),
    )
}

fn confirm_group_with(
    process_group: libc::pid_t,
    mut probe: impl FnMut(libc::pid_t, libc::c_int) -> io::Result<()>,
    mut expired: impl FnMut() -> bool,
    mut pause: impl FnMut(),
) -> Result<(), Report<ReleaseError>> {
    loop {
        let expired = expired();
        match probe(process_group, 0) {
            Err(source) if source.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
            Err(source) if expired || !is_transient_refusal(&source) => {
                return Err(observation_failure(&source));
            }
            Ok(()) if expired => {
                return Err(process_error(
                    ProcessFailure::GroupSurvived,
                    format!("process group {process_group} remained observable after cleanup"),
                ));
            }
            Ok(()) | Err(_) => pause(),
        }
    }
}

#[cfg(test)]
mod tests;

/// Waits for the leader's exit or a stop condition.
///
/// When observation itself fails, the group is still settled before the
/// error is returned, and a cleanup failure is attached to it.
pub(super) fn observe(
    child: &mut Child,
    process_group: libc::pid_t,
    watch: &Watch<'_>,
) -> Result<Option<ProcessOutcome>, Report<ReleaseError>> {
    match wait_for_leader(process_group, watch) {
        Ok(stop) => Ok(stop),
        Err(source) => {
            let report = process_error(
                ProcessFailure::Wait {
                    errno: source.raw_os_error(),
                },
                format!("cannot observe child: {source}"),
            );
            Err(match settle(child, process_group) {
                Ok(_) => report,
                Err(cleanup) => report.attach(format!("cleanup also failed: {cleanup:?}")),
            })
        }
    }
}
