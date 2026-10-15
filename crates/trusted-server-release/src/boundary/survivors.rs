//! Fail-closed detection of confined descendants that outlived a macOS run.
//!
//! macOS has no subreaper, and `posix_spawn` with `POSIX_SPAWN_SETPGROUP` or
//! `POSIX_SPAWN_SETSID` leaves the supervised process group even though the
//! profile denies the `setsid`/`setpgid` system calls. Each confined run
//! therefore carries `NAME=<random nonce>` in its environment. After the run,
//! every process whose exec-time arguments and environment
//! (`KERN_PROCARGS2`, readable only for same-user processes) contain that
//! exact entry is sent `SIGKILL`, and the run is rejected.
//!
//! Residual: a descendant that execs with the variable removed is not found;
//! it stays in the same sandbox instance and remains network-denied.

use std::time::{Duration, Instant};

use error_stack::Report;

use crate::error::ReleaseError;

/// Bound for killing survivors and waiting for them to disappear.
const REAP_GRACE: Duration = Duration::from_secs(2);
/// Polling interval while waiting for killed survivors to vanish.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Extra slots when listing pids, for processes started between calls.
const PID_SLACK: usize = 256;

fn survivor_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Lists every process ID on the system.
fn all_pids() -> Result<Vec<libc::pid_t>, Report<ReleaseError>> {
    // SAFETY: a null buffer of size zero asks only for the current count.
    let count = unsafe { libc::proc_listallpids(core::ptr::null_mut(), 0) };
    let count =
        usize::try_from(count).map_err(|_| survivor_error("cannot count processes"))? + PID_SLACK;
    let mut pids: Vec<libc::pid_t> = vec![0; count];
    let bytes = libc::c_int::try_from(count * size_of::<libc::pid_t>())
        .map_err(|_| survivor_error("process list is too large"))?;
    // SAFETY: `pids` has room for exactly `bytes` bytes of pid entries.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let listed = usize::try_from(listed).map_err(|_| survivor_error("cannot list processes"))?;
    pids.truncate(listed.min(count));
    Ok(pids)
}

/// Returns the kernel's maximum argument-area size (`KERN_ARGMAX`).
fn argument_area_size() -> Result<usize, Report<ReleaseError>> {
    let mut maximum: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: `maximum` and `size` describe a valid c_int output buffer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut maximum).cast(),
            &raw mut size,
            core::ptr::null_mut(),
            0,
        )
    };
    usize::try_from(maximum)
        .ok()
        .filter(|maximum| rc == 0 && *maximum > 0)
        .ok_or_else(|| survivor_error("cannot read KERN_ARGMAX"))
}

/// Reads `pid`'s exec-time argument and environment area into `buffer`,
/// returning the filled prefix when readable (same-user processes only).
fn process_arguments(pid: libc::pid_t, buffer: &mut [u8]) -> Option<&[u8]> {
    let mut size = buffer.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buffer` and `size` describe a valid output buffer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            core::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 { buffer.get(..size) } else { None }
}

/// Returns whether `area` holds `entry` as a whole NUL-delimited string.
fn holds_entry(area: &[u8], entry: &[u8]) -> bool {
    area.split(|byte| *byte == 0).any(|item| item == entry)
}

/// Returns every other process carrying `entry`.
fn carriers(entry: &[u8]) -> Result<Vec<libc::pid_t>, Report<ReleaseError>> {
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let mut buffer = vec![0_u8; argument_area_size()?];
    let mut found = Vec::new();
    for pid in all_pids()? {
        let carries = pid > 1
            && pid != own
            && process_arguments(pid, &mut buffer).is_some_and(|area| holds_entry(area, entry));
        if carries {
            found.push(pid);
        }
    }
    Ok(found)
}

/// Kills every surviving carrier of `name=nonce` and rejects the run if any
/// existed.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] naming the survivors when any were
/// found (after killing them), or when they did not vanish within the grace.
pub(crate) fn reap(name: &str, nonce: &str) -> Result<(), Report<ReleaseError>> {
    let entry = format!("{name}={nonce}").into_bytes();
    let deadline = Instant::now() + REAP_GRACE;
    let mut killed = Vec::new();
    loop {
        let found = carriers(&entry)?;
        if found.is_empty() {
            break;
        }
        for pid in &found {
            // SAFETY: `kill` has no memory-safety preconditions; `pid > 1`.
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        killed.extend(found);
        if Instant::now() >= deadline {
            return Err(survivor_error(format!(
                "confined descendants {killed:?} still exist after SIGKILL"
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if killed.is_empty() {
        return Ok(());
    }
    killed.sort_unstable();
    killed.dedup();
    Err(survivor_error(format!(
        "confined descendants {killed:?} escaped the process group and were killed"
    )))
}
