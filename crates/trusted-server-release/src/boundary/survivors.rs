//! Fail-closed detection of confined descendants that outlived a macOS run.
//!
//! macOS has no subreaper, and `posix_spawn` with `POSIX_SPAWN_SETPGROUP` or
//! `POSIX_SPAWN_SETSID` leaves the supervised process group even though the
//! profile denies the `setsid`/`setpgid` system calls. After every run, each
//! other process is checked two ways, and every match is sent `SIGKILL` and
//! the run rejected:
//!
//! - Sandbox instance: the process is sandboxed, may read the run's allow
//!   marker and may not read its deny marker, according to `sandbox_check`.
//!   A confined process cannot leave or re-apply its sandbox (nested
//!   `sandbox_apply` fails with `EPERM`), so scrubbing its environment or
//!   changing its process group does not hide it.
//! - Environment: its exec-time arguments and environment (`KERN_PROCARGS2`,
//!   readable only for same-user processes) hold `NAME=<run nonce>`. macOS
//!   hides this area for Apple platform binaries such as `/bin/sleep`, so
//!   this signal is secondary to the sandbox-instance check.
//!
//! Only processes that started after the run began, less a one-second clock
//! margin, are eligible, so already-running applications are never
//! signalled even if a check misbehaves.
//!
//! `sandbox_check` is exported by libSystem's `libsystem_sandbox` but is not
//! declared in the public SDK header. A self-check against this unsandboxed
//! orchestrator must give the expected answers before any scan is trusted;
//! the orchestrator must therefore not itself run inside a sandbox.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use error_stack::Report;

use crate::error::ReleaseError;

/// Bound for killing survivors and waiting for them to disappear.
const REAP_GRACE: Duration = Duration::from_secs(2);
/// Polling interval while waiting for killed survivors to vanish.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Extra slots when listing pids, for processes started between calls.
const PID_SLACK: usize = 256;
/// `SANDBOX_FILTER_NONE` in the private `sandbox_filter_type` enumeration.
const SANDBOX_FILTER_NONE: libc::c_int = 0;
/// `SANDBOX_FILTER_PATH` in the same enumeration; observed with
/// `sandbox-exec` profiles on macOS 26.6.2.
const SANDBOX_FILTER_PATH: libc::c_int = 1;
/// The checked file operation.
const FILE_READ_DATA: &CStr = c"file-read-data";

unsafe extern "C" {
    /// Returns 0 when `operation` is allowed for `pid`, 1 when denied and -1
    /// on error; with no operation, 1 means `pid` is sandboxed.
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter: libc::c_int,
        ...
    ) -> libc::c_int;
    /// Flag suppressing sandbox violation reports for a check.
    static SANDBOX_CHECK_NO_REPORT: libc::c_int;
}

/// What identifies one run's processes.
pub(crate) struct RunSignature<'a> {
    /// Environment variable name carrying the run nonce.
    pub(crate) variable: &'a str,
    /// The run nonce.
    pub(crate) nonce: &'a str,
    /// Marker file the run's sandbox instance may read.
    pub(crate) allow: &'a Path,
    /// Marker file the run's sandbox instance may not read.
    pub(crate) deny: &'a Path,
    /// Earliest eligible process start, in microseconds since the epoch.
    /// Processes that started before the run, such as already-open
    /// applications, are never matched or signalled.
    pub(crate) started_after_micros: u64,
}

/// Prepared queries for one signature.
struct Matcher {
    started_after_micros: u64,
    entry: Vec<u8>,
    allow: CString,
    deny: CString,
}

fn survivor_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

fn no_report() -> libc::c_int {
    // SAFETY: the exported constant is an initialized, immutable `int`.
    unsafe { SANDBOX_CHECK_NO_REPORT }
}

/// Returns whether `pid` is sandboxed (1), not (0), or the check failed (-1).
fn sandboxed(pid: libc::pid_t) -> libc::c_int {
    // SAFETY: a null operation with the none filter takes no variadic value.
    unsafe { sandbox_check(pid, core::ptr::null(), SANDBOX_FILTER_NONE | no_report()) }
}

/// Returns 0 when `pid` may read `path`, 1 when denied, -1 on error.
fn read_check(pid: libc::pid_t, path: &CStr) -> libc::c_int {
    // SAFETY: the path filter takes exactly one NUL-terminated string, and
    // both strings outlive the call.
    unsafe {
        sandbox_check(
            pid,
            FILE_READ_DATA.as_ptr(),
            SANDBOX_FILTER_PATH | no_report(),
            path.as_ptr(),
        )
    }
}

impl Matcher {
    fn new(signature: &RunSignature<'_>) -> Result<Self, Report<ReleaseError>> {
        let path = |path: &Path| {
            CString::new(path.as_os_str().as_bytes())
                .map_err(|_| survivor_error("run marker path holds a NUL byte"))
        };
        Ok(Self {
            started_after_micros: signature.started_after_micros,
            entry: format!("{}={}", signature.variable, signature.nonce).into_bytes(),
            allow: path(signature.allow)?,
            deny: path(signature.deny)?,
        })
    }

    /// Requires the expected answers for the unsandboxed orchestrator.
    fn self_check(&self, own: libc::pid_t) -> Result<(), Report<ReleaseError>> {
        let answers = (
            sandboxed(own),
            read_check(own, &self.allow),
            read_check(own, &self.deny),
        );
        if answers == (0, 0, 0) {
            Ok(())
        } else {
            Err(survivor_error(format!(
                "sandbox state query self-check failed with {answers:?}; no process was signalled and the orchestrator must run unsandboxed"
            )))
        }
    }

    /// Returns whether `pid` belongs to the run's sandbox instance.
    fn in_instance(&self, pid: libc::pid_t) -> bool {
        sandboxed(pid) == 1 && read_check(pid, &self.allow) == 0 && read_check(pid, &self.deny) == 1
    }
}

/// Returns `pid`'s start time in microseconds since the epoch, when readable.
fn start_micros(pid: libc::pid_t) -> Option<u64> {
    // SAFETY: `proc_bsdinfo` is plain old data; all-zero bytes are valid.
    let mut info: libc::proc_bsdinfo = unsafe { core::mem::zeroed() };
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let filled =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (filled == size).then(|| {
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec)
    })
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

/// Returns `pid`'s start time when it is another eligible process of the
/// run's instance or carries its nonce.
fn identify(
    matcher: &Matcher,
    pid: libc::pid_t,
    own: libc::pid_t,
    buffer: &mut [u8],
) -> Option<u64> {
    if pid <= 1 || pid == own {
        return None;
    }
    let started = start_micros(pid).filter(|started| *started >= matcher.started_after_micros)?;
    let carries = matcher.in_instance(pid)
        || process_arguments(pid, buffer).is_some_and(|area| holds_entry(area, &matcher.entry));
    carries.then_some(started)
}

/// Returns every other matching process with its start time.
fn carriers(
    matcher: &Matcher,
    own: libc::pid_t,
    buffer: &mut [u8],
) -> Result<Vec<(libc::pid_t, u64)>, Report<ReleaseError>> {
    matcher.self_check(own)?;
    Ok(all_pids()?
        .into_iter()
        .filter_map(|pid| identify(matcher, pid, own, buffer).map(|started| (pid, started)))
        .collect())
}

fn own_pid() -> libc::pid_t {
    // SAFETY: `getpid` has no preconditions.
    unsafe { libc::getpid() }
}

/// Lists, without signalling them, every other process matching `signature`.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when the markers are not C strings,
/// the sandbox state query fails its self-check or processes cannot be listed.
#[cfg(test)]
pub(crate) fn matching(
    signature: &RunSignature<'_>,
) -> Result<Vec<libc::pid_t>, Report<ReleaseError>> {
    let mut buffer = vec![0_u8; argument_area_size()?];
    let found = carriers(&Matcher::new(signature)?, own_pid(), &mut buffer)?;
    Ok(found.into_iter().map(|(pid, _)| pid).collect())
}

/// Kills every surviving process of the run and rejects the run if any
/// existed.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] naming the survivors when any were
/// found (after killing them), when they did not vanish within the grace,
/// or when the sandbox state query fails its self-check.
pub(crate) fn reap(signature: &RunSignature<'_>) -> Result<(), Report<ReleaseError>> {
    let matcher = Matcher::new(signature)?;
    let own = own_pid();
    let mut buffer = vec![0_u8; argument_area_size()?];
    let deadline = Instant::now() + REAP_GRACE;
    let mut killed = Vec::new();
    loop {
        let found = carriers(&matcher, own, &mut buffer)?;
        if found.is_empty() {
            break;
        }
        for (pid, started) in found {
            // Re-identify immediately before signalling so that a reused pid
            // with a different start time is never killed.
            if identify(&matcher, pid, own, &mut buffer) == Some(started) {
                // SAFETY: `kill` has no memory-safety preconditions;
                // `identify` only returns pids above 1 other than our own.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                killed.push(pid);
            }
        }
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
