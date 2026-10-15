//! Deadlines, cancellation, byte caps, termination, reaping and output
//! acceptance with real OS fake children.

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
    let script = format!(
        "set -m\nsleep 30 &\necho $! > '{}'\nexit 0",
        pid_file.display()
    );
    fake.install(&script);
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let started = Instant::now();

    let (error, _) = context(run(
        &runner,
        AttemptDeadline::start(),
        &Cancellation::default(),
    ));

    let escaped = read_pid(&pid_file);
    terminate_process(escaped);
    assert_eq!(
        error,
        ReleaseError::DescendantEscaped,
        "should report escaped descendant"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should not hang on open pipes"
    );
}
