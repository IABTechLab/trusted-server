//! Process supervision tests.
//!
//! Fixtures are host-independent: instead of ambient shells or coreutils,
//! each child re-executes this test binary in a hidden helper mode selected
//! by [`HELPER_SELECTOR`] (see [`helper_entry`] and [`fixture`]). The helper
//! implements every process shape with std and libc only.
//!
//! The helper fails closed: a selector without the exact helper filter and a
//! matching per-run nonce fails the test run instead of acting or skipping.
//!
//! Genuine `EPERM` group-signal refusals and a group that survives
//! `SIGKILL` need privileges or kernel states a test cannot arrange, so those
//! paths are covered only by unit tests of [`reap_within`] and
//! [`unreaped_failure`], not end to end.

mod fixture;
mod outcomes;
mod redaction;
mod supervision;
mod variables;

use std::io;
use std::process::Command as StdCommand;
use std::thread;

use self::fixture::{
    CUT_KEPT, CUT_PADDING, CUT_SECRET, HELPER_NONCE, HELPER_PIDS, HELPER_SELECTOR,
    HELPER_SETUP_FAILED, HELPER_TEST, LARGE_STDERR, OVERLAP_FIRST, OVERLAP_SECOND, SECRET, SLACK,
    TIMEOUT_WALL, authorized_mode, failure, force_kill, harness_arguments, helper, helper_selector,
    is_alive, is_dead_soon, limits, payload, read_pid, run_helper, supervision_bound, test_binary,
    unauthorized_helper_run, workspace,
};
use super::supervise::{signal_group, unreaped_failure};
use super::*;

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
    let code = mode.map_or(HELPER_SETUP_FAILED, |mode| run_helper(&mode));
    // SAFETY: `_exit` ends the helper without returning to the harness, so no
    // harness summary is appended to the captured output.
    unsafe { libc::_exit(code) }
}
