//! Group kill, reaping, deadlines, cancellation, caps and escapes.

use super::*;

#[test]
fn timeout_kills_and_reaps_leader() {
    let dir = workspace();
    let started = Instant::now();

    let result = helper(&dir, "sleep", limits(TIMEOUT_WALL))
        .run()
        .expect("should supervise a timed-out child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::TimedOut,
        "should report a timeout outcome rather than an errno"
    );
    assert!(
        started.elapsed() < supervision_bound(TIMEOUT_WALL),
        "should stop within the wall deadline plus the kill graces"
    );
    let leader = read_pid(&dir.path().join("leader.pid"));
    assert!(!is_alive(leader), "should reap the leader");
    let report = result
        .require_success()
        .expect_err("should reject a timeout");
    assert_eq!(
        failure(&report),
        Some(&ProcessFailure::Unsuccessful),
        "should classify a timeout as an errno-free unsuccessful outcome"
    );
}

#[test]
fn normal_exit_kills_lingering_group_descendant() {
    let dir = workspace();

    let result = helper(&dir, "linger", limits(SLACK))
        .run()
        .expect("should run a child with a descendant");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should report the leader exit"
    );
    let grandchild = read_pid(&dir.path().join("grandchild.pid"));
    assert!(
        is_dead_soon(grandchild),
        "should kill the same-group grandchild"
    );
}

#[test]
fn timeout_kills_leader_and_grandchild() {
    let dir = workspace();

    let result = helper(&dir, "group-sleep", limits(TIMEOUT_WALL))
        .run()
        .expect("should supervise a timed-out group");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::TimedOut,
        "should report a timeout"
    );
    assert!(
        !is_alive(read_pid(&dir.path().join("leader.pid"))),
        "should reap the leader"
    );
    assert!(
        is_dead_soon(read_pid(&dir.path().join("grandchild.pid"))),
        "should kill the grandchild"
    );
}

#[test]
fn stdout_cap_with_stderr_flood_does_not_deadlock() {
    let dir = workspace();
    let process_limits = ProcessLimits {
        wall: SLACK * 3,
        stdout_max: 64 * 1024,
        stderr_max: 64 * 1024 * 1024,
    };
    let started = Instant::now();

    let result = helper(&dir, "flood-both", process_limits)
        .run()
        .expect("should supervise a flooding child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::OutputLimit {
            stream: Stream::Stdout
        },
        "should report the stdout cap"
    );
    assert!(
        result.stdout().len() <= 64 * 1024,
        "should keep stdout within its cap"
    );
    assert!(
        started.elapsed() < supervision_bound(Duration::ZERO),
        "should stop at the cap, well before the wall deadline"
    );
    assert!(
        is_dead_soon(read_pid(&dir.path().join("grandchild.pid"))),
        "should kill the flooding grandchild"
    );
}

#[test]
fn stderr_cap_reports_stderr() {
    let dir = workspace();
    let process_limits = ProcessLimits {
        wall: SLACK,
        stdout_max: 1024 * 1024,
        stderr_max: 4096,
    };

    let result = helper(&dir, "flood-stderr", process_limits)
        .run()
        .expect("should supervise a flooding child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::OutputLimit {
            stream: Stream::Stderr
        },
        "should report the stderr cap"
    );
    assert!(
        result.stderr().len() <= 4096,
        "should keep stderr within its cap"
    );
}

#[test]
fn cancellation_from_another_thread_stops_group() {
    let dir = workspace();
    let cancellation = Cancellation::default();
    let command = helper(&dir, "group-sleep", limits(SLACK * 3)).cancellation(cancellation.clone());
    let grandchild_file = dir.path().join("grandchild.pid");
    let canceller = thread::spawn(move || {
        let grandchild = read_pid(&grandchild_file);
        cancellation.cancel();
        grandchild
    });
    let started = Instant::now();

    let result = command.run().expect("should supervise a cancelled child");
    let grandchild = canceller.join().expect("should join the cancelling thread");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Cancelled,
        "should report cancellation"
    );
    assert!(
        started.elapsed() < supervision_bound(Duration::ZERO),
        "should stop promptly after cancellation"
    );
    assert!(
        is_dead_soon(grandchild),
        "should kill the grandchild on cancellation"
    );
}

#[test]
fn cancellation_before_spawn_starts_nothing() {
    let dir = workspace();
    let cancellation = Cancellation::default();
    cancellation.cancel();

    let result = helper(&dir, "sleep", limits(SLACK))
        .cancellation(cancellation)
        .run()
        .expect("should report a pre-cancelled run");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Cancelled,
        "should report cancellation"
    );
    assert!(
        !dir.path().join("leader.pid").exists(),
        "should not spawn a child after cancellation"
    );
}

#[test]
fn escaped_descendant_holding_pipes_is_reported() {
    let dir = workspace();
    let started = Instant::now();

    let report = helper(&dir, "escape", limits(SLACK))
        .run()
        .expect_err("should reject output held by an escaped descendant");
    let elapsed = started.elapsed();
    let escaped = read_pid(&dir.path().join("escaped.pid"));
    force_kill(escaped);

    assert!(
        matches!(failure(&report), Some(ProcessFailure::DescendantEscaped)),
        "should classify the escape: {report:?}"
    );
    assert!(
        elapsed < supervision_bound(Duration::ZERO),
        "should give up within the bounded drain grace"
    );
}

/// Documents a residual this module cannot close: a `setsid()` daemon with
/// closed stdio leaves no observable trace for the supervisor. The Task 7 OS
/// boundary (container PID namespace, macOS sandbox) must contain it.
#[test]
fn closed_stdio_daemon_escape_is_not_detected_here() {
    let dir = workspace();

    let result = helper(&dir, "daemon", limits(SLACK))
        .run()
        .expect("should not observe a daemon that closed its stdio");
    let daemon = read_pid(&dir.path().join("escaped.pid"));
    let survived = is_alive(daemon);
    force_kill(daemon);

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should report only the leader exit"
    );
    assert!(
        survived,
        "should leave the escaped daemon to the OS boundary (documented residual)"
    );
}

#[test]
fn reaping_is_bounded_and_never_blocks() {
    let dir = workspace();
    let (nonce, selector) = helper_selector("sleep");
    let mut child = StdCommand::new(test_binary())
        .args(harness_arguments())
        .env_clear()
        .env(HELPER_SELECTOR, selector)
        .env(HELPER_NONCE, nonce)
        .env(HELPER_PIDS, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("should spawn an unsignalled helper");
    let started = Instant::now();

    let reaped = reap_within(&mut child, Duration::from_millis(200));
    let elapsed = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        matches!(reaped, Ok(None)),
        "should report an unreaped leader instead of waiting"
    );
    assert!(
        elapsed < KILL_GRACE,
        "should return after the bounded grace"
    );
}

#[test]
fn unreaped_leader_failure_keeps_real_errno() {
    let refused = io::Error::from_raw_os_error(libc::EPERM);

    assert_eq!(
        unreaped_failure(Some(&refused)),
        ProcessFailure::Signal {
            errno: Some(libc::EPERM)
        },
        "should carry the real killpg errno"
    );
    assert_eq!(
        unreaped_failure(None),
        ProcessFailure::GroupSurvived,
        "should report survival when signalling succeeded"
    );
}

#[test]
fn group_signal_refuses_init_and_broadcast_ids() {
    // Only pgid 1 is probed: 0 and -1 would address this runner or every process.
    let refused = signal_group(1).expect_err("should refuse process group 1");

    assert_eq!(
        refused.raw_os_error(),
        None,
        "should refuse before calling killpg, without a synthesized errno"
    );
}
