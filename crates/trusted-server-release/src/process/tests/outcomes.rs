//! Outcome classification, output capture, stdin and helper authorization.

use super::*;

#[test]
fn success_captures_stdout_and_stderr_exactly() {
    let dir = workspace();

    let result = helper(&dir, "print", limits(SLACK))
        .run()
        .expect("should run a successful child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should exit zero"
    );
    assert_eq!(
        payload(result.stdout()),
        b"out\nline",
        "should capture stdout exactly"
    );
    assert_eq!(
        result.stderr(),
        b"err text",
        "should capture stderr exactly"
    );
    assert!(
        result.elapsed() < SLACK,
        "should report a bounded elapsed time"
    );
    let stdout = result.require_success().expect("should accept exit zero");
    assert_eq!(payload(&stdout), b"out\nline", "should return stdout");
}

#[test]
fn nonzero_exit_reports_code_and_bounded_stderr() {
    let dir = workspace();

    let result = helper(&dir, "exit:7", limits(SLACK))
        .run()
        .expect("should run a failing child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 7 },
        "should report the exit code"
    );
    let report = result
        .require_success()
        .expect_err("should reject a nonzero exit");
    let text = format!("{report:?}");
    assert!(
        text.contains("diagnostic detail"),
        "should include the stderr excerpt: {text}"
    );
    assert!(
        matches!(failure(&report), Some(ProcessFailure::Unsuccessful)),
        "should classify the failure as unsuccessful"
    );
}

#[test]
fn stderr_excerpt_is_bounded() {
    let dir = workspace();

    let report = helper(&dir, "bulk-stderr", limits(SLACK))
        .run()
        .expect("should run a failing child")
        .require_success()
        .expect_err("should reject a nonzero exit");
    let text = format!("{report:?}");

    assert!(text.contains("TAIL"), "should keep the stderr tail: {text}");
    assert!(
        text.len() < 20_000 / 2,
        "should bound the excerpt well below the captured stderr"
    );
}

#[test]
fn large_stderr_before_stdout_drains_concurrently() {
    let dir = workspace();
    let process_limits = ProcessLimits {
        wall: SLACK,
        stdout_max: 1024 * 1024,
        stderr_max: 4 * 1024 * 1024,
    };

    let result = helper(&dir, "stderr-then-stdout", process_limits)
        .run()
        .expect("should run a child that fills stderr first");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should exit zero without blocking on a full stderr pipe"
    );
    assert_eq!(
        result.stderr().len(),
        LARGE_STDERR,
        "should capture every stderr byte"
    );
    assert_eq!(payload(result.stdout()), b"small", "should capture stdout");
}

#[test]
fn signaled_child_reports_signal() {
    let dir = workspace();

    let result = helper(&dir, "raise", limits(SLACK))
        .run()
        .expect("should run a signaled child");

    assert_eq!(
        result.outcome(),
        ProcessOutcome::Signaled {
            signal: libc::SIGTERM
        },
        "should report the terminating signal"
    );
    assert!(
        result.require_success().is_err(),
        "should reject a signaled child"
    );
}

#[test]
fn nonexistent_program_reports_real_errno() {
    let dir = workspace();
    let missing = PathBuf::from("/nonexistent-trusted-server-release/fixture-tool");
    let command = BoundedCommand::new(&missing, dir.path(), limits(SLACK))
        .expect("should accept an absolute program");

    let report = command.run().expect_err("should fail to spawn");

    assert!(
        matches!(
            failure(&report),
            Some(ProcessFailure::Spawn {
                errno: Some(libc::ENOENT)
            })
        ),
        "should record ENOENT: {report:?}"
    );
    assert!(
        format!("{report:?}").contains(&format!("os error {}", libc::ENOENT)),
        "should carry the operating system error text"
    );
}

#[test]
fn bounded_stdin_reaches_child() {
    let dir = workspace();
    let command = helper(&dir, "cat", limits(SLACK)).stdin_bytes(b"from stdin".to_vec());

    let stdout = command
        .run()
        .expect("should run the cat helper")
        .require_success()
        .expect("should exit zero");

    assert_eq!(
        payload(&stdout),
        b"from stdin|done",
        "should deliver stdin bytes"
    );
}

#[test]
fn default_stdin_is_null() {
    let dir = workspace();

    let stdout = helper(&dir, "cat", limits(SLACK))
        .run()
        .expect("should run the cat helper")
        .require_success()
        .expect("should exit zero");

    assert_eq!(
        payload(&stdout),
        b"|done",
        "should read EOF from null stdin"
    );
}

#[test]
fn helper_selector_without_fixture_invocation_fails_the_run() {
    assert!(
        !unauthorized_helper_run(&[HELPER_TEST, "--test-threads=1"], Some("fixture-nonce")),
        "should fail a non-exact filter run that carries a selector"
    );
    assert!(
        !unauthorized_helper_run(
            &["--exact", HELPER_TEST, "--test-threads=1"],
            Some("other-nonce")
        ),
        "should fail an exact run whose nonce does not match"
    );
    assert!(
        !unauthorized_helper_run(&["--exact", HELPER_TEST, "--test-threads=1"], None),
        "should fail an exact run without a nonce"
    );
}

#[test]
fn helper_authorization_requires_exact_filter_and_nonce() {
    let fixture: Vec<String> = ["binary", "--exact", HELPER_TEST]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let loose: Vec<String> = ["binary", HELPER_TEST]
        .into_iter()
        .map(str::to_owned)
        .collect();

    assert_eq!(
        authorized_mode("print:n1", Some("n1"), &fixture),
        Some("print".to_owned()),
        "should authorize a fixture invocation"
    );
    assert_eq!(
        authorized_mode("print", Some("n1"), &fixture),
        None,
        "should reject a selector without a nonce"
    );
    assert_eq!(
        authorized_mode("print:n1", Some(""), &fixture),
        None,
        "should reject an empty nonce"
    );
    assert_eq!(
        authorized_mode("print:n1", Some("n1"), &loose),
        None,
        "should reject a non-exact filter"
    );
}
