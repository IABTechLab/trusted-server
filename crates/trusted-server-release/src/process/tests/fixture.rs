//! Host-independent child fixtures and test-side utilities.
//!
//! Helper modes run inside a re-executed test binary (see
//! [`super::helper_entry`]) using std and libc only. Forked helper children
//! call only async-signal-safe functions.

use std::io::Read as _;
use std::process::Command as StdCommand;
use std::sync::atomic::AtomicU64;
use std::thread;

use tempfile::TempDir;

use super::*;

/// Environment selector `<mode>:<nonce>`; absent in normal test runs.
pub(super) const HELPER_SELECTOR: &str = "TS_RELEASE_PROCESS_HELPER";
/// Environment variable carrying the nonce the selector must match.
pub(super) const HELPER_NONCE: &str = "TS_RELEASE_PROCESS_NONCE";
/// Exact libtest name of [`super::helper_entry`].
pub(super) const HELPER_TEST: &str = "process::tests::helper_entry";
/// Exit status of a helper whose fork, pipe or pid-file setup failed.
pub(super) const HELPER_SETUP_FAILED: i32 = 90;
/// Exit status of an unknown helper mode.
const HELPER_UNKNOWN_MODE: i32 = 97;
/// Environment variable naming the directory where helpers record pids.
pub(super) const HELPER_PIDS: &str = "TS_RELEASE_PROCESS_PIDS";
/// How long sleeping helpers wait; every supervision bound must stay below it.
pub(super) const HELPER_SLEEP: Duration = Duration::from_secs(30);
/// Marks the start of helper stdout after the test harness preamble.
pub(super) const SENTINEL: &[u8] = b"\x1eTS-PROCESS-HELPER\x1e";
pub(super) const SECRET: &str = "fixture-secret-value-0451";
pub(super) const CUT_SECRET: &str = "Qz7-secret-Wq9";
pub(super) const CUT_PADDING: usize = 100;
pub(super) const CUT_KEPT: usize = 6;
pub(super) const OVERLAP_FIRST: &str = "Kq1Vv8Mm";
pub(super) const OVERLAP_SECOND: &str = "Vv8MmJx3";
const OVERLAP_TEXT: &str = "Kq1Vv8MmJx3";
/// Stderr written before any stdout; far above a pipe buffer.
pub(super) const LARGE_STDERR: usize = 1024 * 1024 + 17;
/// Headroom for harness startup and scheduling in wall-time assertions.
pub(super) const SLACK: Duration = Duration::from_secs(10);
/// Wall deadline for timeout tests; long enough for helpers to record pids.
pub(super) const TIMEOUT_WALL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Helper authorization and modes (run inside the re-executed child)
// ---------------------------------------------------------------------------

/// Returns the helper mode when the selector, nonce and argv all match a
/// fixture-launched helper run.
pub(super) fn authorized_mode(
    selector: &str,
    nonce: Option<&str>,
    arguments: &[String],
) -> Option<String> {
    let (mode, selector_nonce) = selector.rsplit_once(':')?;
    let exact = arguments.iter().any(|argument| argument == "--exact");
    let filtered = arguments.iter().any(|argument| argument == HELPER_TEST);
    let matched = nonce.is_some_and(|nonce| !nonce.is_empty() && nonce == selector_nonce);
    (exact && filtered && matched).then(|| mode.to_owned())
}

/// Runs helper `mode` and returns its exit status.
pub(super) fn run_helper(mode: &str) -> i32 {
    output_mode(mode)
        .or_else(|| group_mode(mode))
        .or_else(|| secret_mode(mode))
        .unwrap_or(HELPER_UNKNOWN_MODE)
}

/// Modes that only write fixed output and exit.
fn output_mode(mode: &str) -> Option<i32> {
    match mode {
        "print" => {
            write_stream(1, &[SENTINEL, b"out\nline"].concat());
            write_stream(2, b"err text");
            Some(0)
        }
        "stderr-then-stdout" => {
            write_stream(2, &[b'e'; LARGE_STDERR]);
            write_stream(1, &[SENTINEL, b"small"].concat());
            Some(0)
        }
        "bulk-stderr" => {
            write_stream(2, &b"abcdefghij".repeat(2000));
            write_stream(2, b"TAIL");
            Some(1)
        }
        "env" => {
            let mut lines: Vec<Vec<u8>> = std::env::vars_os()
                .map(|(name, value)| [name.as_bytes(), b"=", value.as_bytes(), b"\n"].concat())
                .collect();
            lines.sort();
            write_stream(1, &[SENTINEL, &lines.concat()].concat());
            Some(0)
        }
        "cat" => {
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            write_stream(1, &[SENTINEL, &input, b"|done"].concat());
            Some(0)
        }
        other => other
            .strip_prefix("exit:")
            .and_then(|code| code.parse().ok())
            .inspect(|_| write_stream(2, b"diagnostic detail")),
    }
}

/// Modes that shape the process group: sleepers, descendants, floods,
/// escapes and signals. Pids are recorded before anything else happens.
fn group_mode(mode: &str) -> Option<i32> {
    match mode {
        "raise" => {
            // SAFETY: raising SIGTERM with its default disposition ends the helper.
            unsafe {
                libc::raise(libc::SIGTERM);
            }
            Some(0)
        }
        "sleep" => {
            record_pid("leader.pid", own_pid());
            thread::sleep(HELPER_SLEEP);
            Some(0)
        }
        "linger" => {
            record_pid("grandchild.pid", fork_paused());
            Some(0)
        }
        "group-sleep" => {
            record_pid("leader.pid", own_pid());
            record_pid("grandchild.pid", fork_paused());
            thread::sleep(HELPER_SLEEP);
            Some(0)
        }
        "flood-both" => {
            record_pid("grandchild.pid", fork_flood(2));
            Some(flood(1))
        }
        "flood-stderr" => Some(flood(2)),
        "escape" => {
            record_pid("escaped.pid", fork_escaped(false));
            Some(0)
        }
        "daemon" => {
            record_pid("escaped.pid", fork_escaped(true));
            Some(0)
        }
        _ => None,
    }
}

/// Modes that echo secret-classified environment values.
fn secret_mode(mode: &str) -> Option<i32> {
    match mode {
        "secret" => {
            let secret = read_secret();
            write_stream(2, &[b"leak:", secret.as_slice(), b":"].concat());
            write_stream(1, &secret);
            Some(3)
        }
        "secret-cut" => {
            write_stream(
                2,
                &[&b"x".repeat(CUT_PADDING), read_secret().as_slice()].concat(),
            );
            Some(1)
        }
        "overlap" => {
            write_stream(2, OVERLAP_TEXT.as_bytes());
            Some(1)
        }
        _ => None,
    }
}

fn read_secret() -> Vec<u8> {
    std::env::var_os("TOKEN")
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_default()
}

fn write_stream(fd: libc::c_int, bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: `rest` is a valid initialized buffer of `rest.len()` bytes.
        let written = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        let Ok(written) = usize::try_from(written) else {
            return;
        };
        rest = rest.get(written..).unwrap_or_default();
    }
}

/// Writes to `fd` until the write fails, then returns exit status zero.
fn flood(fd: libc::c_int) -> i32 {
    let chunk = [b'o'; 4096];
    loop {
        // SAFETY: `chunk` is a valid initialized buffer.
        if unsafe { libc::write(fd, chunk.as_ptr().cast(), chunk.len()) } < 0 {
            return 0;
        }
    }
}

fn setup_failed() -> ! {
    // SAFETY: `_exit` is async-signal-safe and ends the helper immediately.
    unsafe { libc::_exit(HELPER_SETUP_FAILED) }
}

/// Records `pid`; a pid that could address a process group or every process
/// (`<= 1`) or an unwritable pid file ends the helper instead.
fn record_pid(name: &str, pid: libc::pid_t) {
    if pid <= 1 {
        setup_failed();
    }
    let Some(directory) = std::env::var_os(HELPER_PIDS) else {
        setup_failed();
    };
    if std::fs::write(Path::new(&directory).join(name), pid.to_string()).is_err() {
        setup_failed();
    }
}

fn own_pid() -> libc::pid_t {
    // SAFETY: `getpid` has no preconditions.
    unsafe { libc::getpid() }
}

fn pause_forever() -> ! {
    loop {
        // SAFETY: `pause` is async-signal-safe and has no preconditions.
        unsafe {
            libc::pause();
        }
    }
}

/// Forks a same-group child that waits until killed.
fn fork_paused() -> libc::pid_t {
    // SAFETY: the forked child only calls the async-signal-safe `pause`.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        pause_forever();
    }
    if pid < 0 {
        setup_failed();
    }
    pid
}

/// Forks a same-group child that floods `fd` until the write fails.
///
/// Each 4 KiB write is followed by a 1 ms pause so the flood stays far below
/// a 64 MiB cap even when the leader is descheduled under parallel tests.
fn fork_flood(fd: libc::c_int) -> libc::pid_t {
    // SAFETY: the forked child only calls async-signal-safe `write`,
    // `nanosleep` and `_exit`.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let chunk = [b'e'; 4096];
        let pause = libc::timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        loop {
            // SAFETY: `chunk` and `pause` are valid for the calls' duration.
            unsafe {
                if libc::write(fd, chunk.as_ptr().cast(), chunk.len()) < 0 {
                    libc::_exit(0);
                }
                libc::nanosleep(&raw const pause, core::ptr::null_mut());
            }
        }
    }
    if pid < 0 {
        setup_failed();
    }
    pid
}

/// Waits for the one-byte readiness signal, retrying `EINTR`.
fn await_ready(read_end: libc::c_int) {
    let mut byte = [0_u8; 1];
    loop {
        // SAFETY: `read_end` comes from `pipe` and `byte` is writable.
        let count = unsafe { libc::read(read_end, byte.as_mut_ptr().cast(), 1) };
        if count == 1 {
            return;
        }
        if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        setup_failed();
    }
}

/// Forks a child that calls `setsid()` (leaving the process group) and
/// optionally closes its stdio, then reports readiness and waits forever.
fn fork_escaped(close_stdio: bool) -> libc::pid_t {
    let mut ready = [0 as libc::c_int; 2];
    // SAFETY: `ready` provides space for the two pipe descriptors.
    if unsafe { libc::pipe(ready.as_mut_ptr()) } != 0 {
        setup_failed();
    }
    let [read_end, write_end] = ready;
    // SAFETY: the forked child only calls async-signal-safe functions.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: setsid/close/write are async-signal-safe; descriptors are valid.
        unsafe {
            libc::setsid();
            if close_stdio {
                libc::close(0);
                libc::close(1);
                libc::close(2);
            }
            libc::write(write_end, b"r".as_ptr().cast(), 1);
            libc::close(write_end);
        }
        pause_forever();
    }
    if pid < 0 {
        setup_failed();
    }
    // SAFETY: `write_end` comes from `pipe` above and is no longer needed here.
    unsafe {
        libc::close(write_end);
    }
    await_ready(read_end);
    // SAFETY: `read_end` comes from `pipe` above.
    unsafe {
        libc::close(read_end);
    }
    pid
}

// ---------------------------------------------------------------------------
// Test-side utilities (run in the supervising test)
// ---------------------------------------------------------------------------

pub(super) fn limits(wall: Duration) -> ProcessLimits {
    ProcessLimits {
        wall,
        stdout_max: 1024 * 1024,
        stderr_max: 1024 * 1024,
    }
}

/// Upper bound for a supervised run: its wall deadline, the reap, group and
/// pipe graces, plus [`SLACK`] for harness startup.
pub(super) fn supervision_bound(wall: Duration) -> Duration {
    let bound = wall + KILL_GRACE * 3 + SLACK;
    assert!(
        bound < HELPER_SLEEP,
        "should keep supervision bounds below the helper sleep"
    );
    bound
}

pub(super) fn workspace() -> TempDir {
    tempfile::tempdir().expect("should create a private test workspace")
}

pub(super) fn test_binary() -> PathBuf {
    std::env::current_exe().expect("should locate the test binary")
}

pub(super) fn harness_arguments() -> Vec<OsString> {
    ["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// Returns a fresh per-launch nonce and the matching `<mode>:<nonce>` selector.
pub(super) fn helper_selector(mode: &str) -> (String, String) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let nonce = format!("{}-{sequence}-{nanos}", std::process::id());
    let selector = format!("{mode}:{nonce}");
    (nonce, selector)
}

/// Builds a bounded re-exec of this test binary in helper `mode`.
pub(super) fn helper(
    workspace: &TempDir,
    mode: &str,
    process_limits: ProcessLimits,
) -> BoundedCommand {
    let (nonce, selector) = helper_selector(mode);
    BoundedCommand::new(&test_binary(), workspace.path(), process_limits)
        .expect("should accept an absolute program")
        .arguments(harness_arguments())
        .environment(HELPER_SELECTOR, selector)
        .expect("should accept the helper selector")
        .environment(HELPER_NONCE, nonce)
        .expect("should accept the helper nonce")
        .environment(HELPER_PIDS, workspace.path())
        .expect("should accept the helper pid directory")
}

/// Runs this test binary with a helper selector but `arguments` and `nonce`
/// that a fixture would not use; returns whether the run succeeded.
pub(super) fn unauthorized_helper_run(arguments: &[&str], nonce: Option<&str>) -> bool {
    let dir = workspace();
    let mut command = StdCommand::new(test_binary());
    command
        .args(arguments)
        .env_clear()
        .env(HELPER_SELECTOR, "print:fixture-nonce")
        .env(HELPER_PIDS, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(nonce) = nonce {
        command.env(HELPER_NONCE, nonce);
    }
    command
        .status()
        .expect("should run the test binary")
        .success()
}

/// Returns helper stdout after the harness preamble.
pub(super) fn payload(stdout: &[u8]) -> &[u8] {
    stdout
        .windows(SENTINEL.len())
        .position(|window| window == SENTINEL)
        .and_then(|start| stdout.get(start + SENTINEL.len()..))
        .unwrap_or_default()
}

/// Refuses pids that would address a process group or every process.
fn assert_single_process(pid: libc::pid_t) {
    assert!(
        pid > 1,
        "should only signal a single fixture pid, got {pid}"
    );
}

pub(super) fn read_pid(path: &Path) -> libc::pid_t {
    let deadline = Instant::now() + SLACK;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if let Ok(pid) = text.trim().parse::<libc::pid_t>() {
            assert_single_process(pid);
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "should observe a pid written to {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Returns whether Linux reports `pid` as a zombie (`Z` in `/proc/<pid>/stat`).
///
/// Elsewhere there is no `/proc`, the read fails and this returns `false`.
fn is_zombie(pid: libc::pid_t) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rfind(')')
                .and_then(|end| stat.get(end + 1..))
                .map(|rest| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Returns whether `pid` is a live (non-zombie) process.
pub(super) fn is_alive(pid: libc::pid_t) -> bool {
    assert_single_process(pid);
    // SAFETY: `kill` with signal 0 only performs a permission and existence check.
    let result = unsafe { libc::kill(pid, 0) };
    let exists = result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    exists && !is_zombie(pid)
}

/// Waits briefly for an already-killed, reparented pid to disappear.
pub(super) fn is_dead_soon(pid: libc::pid_t) -> bool {
    assert_single_process(pid);
    let deadline = Instant::now() + KILL_GRACE;
    while is_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

pub(super) fn force_kill(pid: libc::pid_t) {
    assert_single_process(pid);
    // SAFETY: `kill` only reads its integer arguments; `pid` is one fixture process.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

pub(super) fn failure(report: &Report<ReleaseError>) -> Option<&ProcessFailure> {
    report.downcast_ref::<ProcessFailure>()
}
