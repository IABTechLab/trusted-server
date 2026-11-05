//! Boundary tests.
//!
//! Fixtures are host-independent: confined children re-execute this test
//! binary in a hidden, nonce-gated helper mode (see [`helper_entry`] and
//! [`modes`]) built from std and libc only — no shells, interpreters or
//! coreutils. The helper speaks the same probe protocol as
//! `boundary-probe connect`, sharing
//! [`connect_probe`](super::probe::connect_probe), after a sentinel that
//! skips the libtest preamble.
//!
//! Tool roots require `nlink == 1`, and Cargo hard-links its test binaries,
//! so every boundary fixture runs a private staged copy of this binary.

mod container;
mod modes;
mod unit;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::*;

/// Environment selector `<mode>:<nonce>`; absent in normal test runs.
const HELPER_SELECTOR: &str = "TS_RELEASE_BOUNDARY_HELPER";
/// Environment variable carrying the nonce the selector must match.
const HELPER_NONCE: &str = "TS_RELEASE_BOUNDARY_NONCE";
/// Exact libtest name of [`helper_entry`].
const HELPER_TEST: &str = "boundary::tests::helper_entry";
/// Marks the helper's report after the libtest preamble.
const SENTINEL: &[u8] = b"\x1eTS-BOUNDARY-HELPER\x1e";
/// libtest arguments selecting only [`helper_entry`]; mode arguments follow.
const HELPER_ARGUMENTS: [&str; 4] = ["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"];
/// Exit status of a helper whose setup failed.
const HELPER_SETUP_FAILED: i32 = 90;
/// Generous per-run bound for every fixture child.
const SLACK: Duration = Duration::from_secs(60);

/// Returns limits generous enough for any fixture child.
fn limits() -> ProcessLimits {
    ProcessLimits {
        wall: SLACK,
        stdout_max: 1024 * 1024,
        stderr_max: 1024 * 1024,
    }
}

/// Returns this run's helper nonce.
fn nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| {
        hex::encode(super::random_bytes::<16>().expect("should read nonce entropy"))
    })
}

/// Returns the canonical (Cargo hard-linked) test binary path.
fn test_binary() -> PathBuf {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .expect("should locate the test binary")
}

/// Returns the helper environment selecting `mode`.
fn helper_environment(mode: &str) -> Vec<(OsString, OsString)> {
    vec![
        (
            OsString::from(HELPER_SELECTOR),
            OsString::from(format!("{mode}:{}", nonce())),
        ),
        (OsString::from(HELPER_NONCE), OsString::from(nonce())),
    ]
}

/// Returns the full helper argument vector for `mode_arguments`.
fn helper_arguments(mode_arguments: &[&str]) -> Vec<OsString> {
    HELPER_ARGUMENTS
        .iter()
        .chain(mode_arguments)
        .map(OsString::from)
        .collect()
}

/// Returns the test-binary probe at `program`.
fn helper_probe(program: &Path) -> ProbeProgram {
    ProbeProgram::test_helper(
        program.to_path_buf(),
        helper_arguments(&[]),
        helper_environment("connect"),
        SENTINEL,
    )
}

/// Stages a private single-link copy of the test binary into `tools` and
/// returns its probe.
fn staged_probe(tools: &Path) -> ProbeProgram {
    helper_probe(&test_binary())
        .staged(tools)
        .expect("should stage the helper binary")
}

/// Builds a confined helper command running `mode` from `working_directory`.
fn helper_command(
    program: &Path,
    working_directory: &Path,
    mode: &str,
    mode_arguments: &[&str],
) -> ConfinedCommand {
    let mut command = ConfinedCommand::new(program, working_directory, limits())
        .expect("should describe helper command")
        .arguments(helper_arguments(mode_arguments));
    for (name, value) in helper_environment(mode) {
        command = command
            .environment(name, value)
            .expect("should accept helper environment");
    }
    command
}

/// Returns the helper's report text after the sentinel.
fn helper_report(stdout: &[u8]) -> String {
    let start = stdout
        .windows(SENTINEL.len())
        .rposition(|window| window == SENTINEL)
        .expect("should find helper sentinel in stdout");
    String::from_utf8_lossy(&stdout[start + SENTINEL.len()..])
        .trim_end()
        .to_owned()
}

/// Spawns a same-user sibling that lives until killed or its parent pipe closes.
fn spawn_sibling() -> std::process::Child {
    let mut command = StdCommand::new(test_binary());
    command
        .args(helper_arguments(&[]))
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null());
    for (name, value) in helper_environment("wait-for-eof") {
        command.env(name, value);
    }
    command.spawn().expect("should spawn sibling")
}

/// Returns whether `pid` still exists (zombies count until reaped).
fn process_exists(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks existence; callers pass a pid > 1.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Waits up to five seconds for `pid` to disappear.
fn wait_gone(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Reads a pid recorded by a helper.
fn read_pid(path: &Path) -> libc::pid_t {
    let pid: libc::pid_t = std::fs::read_to_string(path)
        .expect("should read recorded pid")
        .trim()
        .parse()
        .expect("should parse recorded pid");
    assert!(pid > 1, "should record a real pid");
    pid
}

/// Returns the helper mode when the selector, nonce and argv all match a
/// fixture-launched helper run.
fn authorized_mode(selector: &str, nonce: Option<&str>, arguments: &[String]) -> Option<String> {
    let (mode, selector_nonce) = selector.rsplit_once(':')?;
    let exact = arguments.iter().any(|argument| argument == "--exact");
    let filtered = arguments.iter().any(|argument| argument == HELPER_TEST);
    let matched = nonce.is_some_and(|nonce| !nonce.is_empty() && nonce == selector_nonce);
    (exact && filtered && matched).then(|| mode.to_owned())
}

/// Hidden re-exec entry point; returns immediately in normal test runs.
///
/// A selector present without the fixture's exact filter and nonce fails the
/// run rather than silently acting or passing.
#[test]
fn helper_entry() {
    let Some(selector) = std::env::var_os(HELPER_SELECTOR) else {
        return;
    };
    let nonce = std::env::var(HELPER_NONCE).ok();
    let arguments: Vec<String> = std::env::args().collect();
    let mode = authorized_mode(&selector.to_string_lossy(), nonce.as_deref(), &arguments);
    assert!(
        mode.is_some(),
        "should run helper mode only from a fixture-launched exact helper invocation"
    );
    let rest: Vec<String> = arguments
        .iter()
        .skip_while(|argument| argument.as_str() != "--test-threads=1")
        .skip(1)
        .cloned()
        .collect();
    let code = mode.map_or(HELPER_SETUP_FAILED, |mode| modes::run(&mode, &rest));
    // SAFETY: `_exit` ends the helper without returning to the harness, so no
    // harness summary is appended to the captured output.
    unsafe { libc::_exit(code) }
}

#[test]
fn helper_requires_exact_filter_and_matching_nonce() {
    let exact: Vec<String> = ["binary", "--exact", HELPER_TEST]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    assert_eq!(
        authorized_mode("connect:abc", Some("abc"), &exact),
        Some(String::from("connect")),
        "should authorize a fixture-launched helper"
    );
    assert_eq!(authorized_mode("connect:abc", Some("xyz"), &exact), None);
    assert_eq!(authorized_mode("connect:abc", None, &exact), None);
    assert_eq!(authorized_mode("connect:", Some(""), &exact), None);
    assert_eq!(
        authorized_mode("connect:abc", Some("abc"), &exact[..2]),
        None
    );
}
