//! Deadlines, cancellation, byte caps, termination, reaping and output
//! acceptance with real OS fake children.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// Bounds, termination and reaping.

#[test]
fn child_timeout_kills_and_reaps_group() {
    let fake = Fake::empty();
    let script = format!(
        "sleep 30 &\necho $! > '{}'\nsleep 30",
        fake.file("grandchild.pid").display()
    );
    fake.install(&script);
    let bounds = ProcessBounds {
        child_timeout: Duration::from_millis(300),
        ..test_bounds()
    };
    let runner = fake.runner(bounds).expect("should resolve fake gh");
    let started = Instant::now();

    let (error, _) = context(run(
        &runner,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    assert_eq!(
        error,
        ReleaseError::Timeout,
        "should report timeout outcome"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should bound wall time"
    );
    assert_dead(
        read_pid(&fake.file("grandchild.pid")),
        "same-group grandchild",
    );
}

#[test]
fn attempt_deadline_bounds_child_before_child_timeout() {
    let fake = Fake::script("sleep 30");
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let started = Instant::now();

    let (error, _) = context(run(
        &runner,
        AttemptDeadline::at(Instant::now() + Duration::from_millis(300)),
        &Cancellation::default(),
    ));

    assert_eq!(
        error,
        ReleaseError::Timeout,
        "should honor attempt deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should bound wall time"
    );
}

#[test]
fn cancellation_kills_child() {
    let fake = Fake::empty();
    let script = format!(
        "echo $$ > '{}'\nsleep 30",
        fake.file("leader.pid").display()
    );
    fake.install(&script);
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let cancellation = Cancellation::default();
    let trigger = cancellation.clone();
    let leader_pid = fake.file("leader.pid");
    let canceller = thread::spawn(move || {
        let pid = read_pid(&leader_pid);
        trigger.cancel();
        pid
    });
    let started = Instant::now();

    let (error, _) = context(run(&runner, AttemptDeadline::start(), &cancellation));

    let leader = canceller.join().expect("should join canceller");
    assert_eq!(error, ReleaseError::Cancelled, "should report cancellation");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should bound wall time"
    );
    assert_dead(leader, "cancelled leader");
}

#[test]
fn simultaneous_stdout_and_stderr_saturation_is_bounded() {
    let fake = Fake::empty();
    let script = format!(
        "sh -c 'echo $$ > \"$0\"; exec yes stderr-line' '{pid}' >&2 &\nwhile [ ! -s '{pid}' ]; do :; done\nexec yes stdout-line",
        pid = fake.file("writer.pid").display()
    );
    fake.install(&script);
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let started = Instant::now();

    let (error, _) = context(run(
        &runner,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    assert_eq!(
        error,
        ReleaseError::OutputLimit,
        "should stop at byte bound"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should bound wall time"
    );
    assert_dead(read_pid(&fake.file("writer.pid")), "stderr writer");
}

#[test]
fn stderr_saturation_alone_is_bounded() {
    let fake = Fake::script("exec yes stderr-line >&2");
    let bounds = ProcessBounds {
        stdout_bytes: 1024 * 1024,
        ..test_bounds()
    };
    let runner = fake.runner(bounds).expect("should resolve fake gh");

    let (error, _) = context(run(
        &runner,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    assert_eq!(error, ReleaseError::OutputLimit, "should bound stderr");
}

#[test]
fn truncated_json_is_rejected() {
    let fake = Fake::script("printf '[{\"verificationResult\":'");
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let subject = fake.file("subject.wasm");

    let (error, _) = context(runner.verify_attestation(
        &subject,
        &fake.file("subject.wasm.provenance.sigstore.json"),
        &release(),
        PredicateKind::Provenance,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    assert_eq!(
        error,
        ReleaseError::Document,
        "should reject incomplete JSON"
    );
}

#[test]
fn failed_exit_yields_no_claims_and_redacts_token() {
    let fake = Fake::script(
        "echo '[{\"verificationResult\":{}}]'\necho \"denied for $GH_TOKEN\" >&2\nexit 1",
    );
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let subject = fake.file("subject.wasm");

    let (error, text) = context(runner.verify_attestation(
        &subject,
        &fake.file("subject.wasm.provenance.sigstore.json"),
        &release(),
        PredicateKind::Provenance,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    assert_eq!(
        error,
        ReleaseError::ExitFailure,
        "should fail closed on exit 1"
    );
    assert!(
        text.contains("denied for [redacted]"),
        "should keep redacted stderr: {text}"
    );
    assert!(
        !text.contains(TOKEN),
        "should never print the token: {text}"
    );
}

#[test]
fn requires_exactly_one_attestation_result() {
    for (output, accepted) in [
        ("[]", false),
        (
            "[{\"verificationResult\":{}},{\"verificationResult\":{}}]",
            false,
        ),
        ("{\"verificationResult\":{}}", false),
        ("[{\"verificationResult\":{}}]", true),
    ] {
        let fake = Fake::script(&format!("echo '{output}'"));
        let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
        let subject = fake.file("subject.wasm");

        let result = runner.verify_attestation(
            &subject,
            &fake.file("subject.wasm.sbom.sigstore.json"),
            &release(),
            PredicateKind::Spdx,
            AttemptDeadline::start(),
            &Cancellation::default(),
        );

        if accepted {
            assert!(result.is_ok(), "should accept exactly one result");
        } else {
            assert_eq!(
                context(result).0,
                ReleaseError::Output,
                "should reject {output}"
            );
        }
    }
}

#[test]
fn successful_exit_kills_lingering_same_group_descendant() {
    let fake = Fake::empty();
    let script = format!(
        "echo $$ > '{leader}'\nsleep 30 &\necho $! > '{grandchild}'\necho '[{{}}]'\nexit 0",
        leader = fake.file("leader.pid").display(),
        grandchild = fake.file("grandchild.pid").display()
    );
    fake.install(&script);
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let started = Instant::now();

    let stdout = run(&runner, AttemptDeadline::start(), &Cancellation::default())
        .expect("should complete after killing lingering group member");

    assert_eq!(stdout, b"[{}]\n", "should retain exact stdout");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should not wait for sleep"
    );
    assert_dead(
        read_pid(&fake.file("grandchild.pid")),
        "lingering grandchild",
    );
    assert!(
        !process_exists(read_pid(&fake.file("leader.pid"))),
        "should have reaped the leader before returning"
    );
}

#[test]
fn escaped_descendant_is_reported_not_awaited() {
    let fake = Fake::empty();
    let pid_file = fake.file("escaped.pid");
    fake.install(&helper_exec("escaped", fake.directory.path()));
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let started = Instant::now();

    let result = run(&runner, AttemptDeadline::start(), &Cancellation::default());
    let elapsed = started.elapsed();
    let escaped = read_pid(&pid_file);
    terminate_process(escaped);

    assert_eq!(
        context(result).0,
        ReleaseError::DescendantEscaped,
        "should report escaped descendant"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "should not hang on open pipes"
    );
}

// Host-independent helper children.
//
// `/bin/sh` cannot leave its process group portably (dash ignores `set -m`
// without a terminal and macOS has no `setsid` binary), so the fake gh execs
// this test binary in a hidden helper mode that uses libc directly.

/// Environment selector `<mode>:<nonce>`; absent in normal test runs.
const HELPER_SELECTOR: &str = "TS_CLI_GH_HELPER";
/// Environment variable carrying the nonce the selector must match.
const HELPER_NONCE: &str = "TS_CLI_GH_HELPER_NONCE";
/// Environment variable naming the directory where helpers record pids.
const HELPER_PIDS: &str = "TS_CLI_GH_HELPER_PIDS";
/// Exact libtest name of [`helper_entry`].
const HELPER_TEST: &str = "release::tests::gh::lifecycle::helper_entry";
/// Exit status of a helper whose fork or pipe setup failed.
const HELPER_SETUP_FAILED: i32 = 90;

/// Returns a fake gh body that execs this test binary in helper `mode`.
fn helper_exec(mode: &str, pids: &Path) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let nonce = format!("{}-{sequence}-{nanos}", std::process::id());
    let binary = std::env::current_exe().expect("should locate the test binary");
    let (binary, pids) = (binary.display().to_string(), pids.display().to_string());
    assert!(
        !binary.contains('\'') && !pids.contains('\''),
        "should quote helper paths safely"
    );
    format!(
        "{HELPER_SELECTOR}='{mode}:{nonce}' {HELPER_NONCE}='{nonce}' {HELPER_PIDS}='{pids}' \
         exec '{binary}' --exact {HELPER_TEST} --nocapture --test-threads=1"
    )
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
    let code = match mode.as_deref() {
        Some("escaped") => {
            record_helper_pid("escaped.pid", fork_escaped());
            0
        }
        _ => HELPER_SETUP_FAILED,
    };
    // SAFETY: `_exit` ends the helper without returning to the harness.
    unsafe { libc::_exit(code) }
}

#[test]
fn helper_selector_fails_closed_without_exact_filter_and_nonce() {
    let exact = [
        "binary".to_owned(),
        "--exact".to_owned(),
        HELPER_TEST.to_owned(),
    ];

    assert_eq!(
        authorized_mode("escaped:n1", Some("n1"), &exact),
        Some("escaped".to_owned()),
        "should authorize the exact fixture invocation"
    );
    assert_eq!(
        authorized_mode("escaped:n1", Some("n2"), &exact),
        None,
        "should reject a mismatched nonce"
    );
    assert_eq!(
        authorized_mode("escaped:n1", None, &exact),
        None,
        "should reject a missing nonce"
    );
    assert_eq!(
        authorized_mode("escaped:n1", Some("n1"), &exact[..2]),
        None,
        "should reject a run without the exact helper filter"
    );
}

fn helper_setup_failed() -> ! {
    // SAFETY: `_exit` is async-signal-safe and ends the helper immediately.
    unsafe { libc::_exit(HELPER_SETUP_FAILED) }
}

/// Records `pid`; a pid that could address a group or every process ends the
/// helper instead.
fn record_helper_pid(name: &str, pid: libc::pid_t) {
    if pid <= 1 {
        helper_setup_failed();
    }
    if let Some(directory) = std::env::var_os(HELPER_PIDS) {
        let _ = fs::write(Path::new(&directory).join(name), pid.to_string());
    }
}

/// Forks a child that calls `setsid()` (leaving the process group) while
/// keeping the inherited stdout/stderr open, reports readiness and waits
/// until killed.
fn fork_escaped() -> libc::pid_t {
    let mut ready = [0 as libc::c_int; 2];
    // SAFETY: `ready` provides space for the two pipe descriptors.
    if unsafe { libc::pipe(ready.as_mut_ptr()) } != 0 {
        helper_setup_failed();
    }
    let [read_end, write_end] = ready;
    // SAFETY: the forked child only calls async-signal-safe functions.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: setsid/write/close/pause are async-signal-safe and the
        // descriptors are valid.
        unsafe {
            libc::setsid();
            libc::write(write_end, b"r".as_ptr().cast(), 1);
            libc::close(write_end);
            loop {
                libc::pause();
            }
        }
    }
    if pid < 0 {
        helper_setup_failed();
    }
    let mut byte = [0_u8; 1];
    // SAFETY: the descriptors come from `pipe` above and `byte` is writable.
    unsafe {
        libc::close(write_end);
        libc::read(read_end, byte.as_mut_ptr().cast(), 1);
        libc::close(read_end);
    }
    pid
}
