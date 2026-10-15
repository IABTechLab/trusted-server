//! Secret redaction in Debug output and failure excerpts.

use super::*;

#[test]
fn secret_values_never_appear_in_debug_or_error_text() {
    let dir = workspace();
    let command = helper(&dir, "secret", limits(SLACK))
        .secret_environment("TOKEN", SECRET)
        .expect("should accept a secret variable");

    let command_text = format!("{command:?}");
    let result = command.run().expect("should run the child");
    let result_text = format!("{result:?}");
    let report = result
        .require_success()
        .expect_err("should reject a nonzero exit");
    let report_text = format!("{report:?}");

    assert!(
        command_text.contains("TOKEN"),
        "should still name the secret variable"
    );
    for text in [&command_text, &result_text, &report_text] {
        assert!(!text.contains(SECRET), "should redact the secret value");
    }
    assert!(
        report_text.contains("leak:[REDACTED]:"),
        "should mark the redacted stderr span: {report_text}"
    );
}

#[test]
fn secret_cut_at_stderr_cap_is_redacted() {
    let dir = workspace();
    let process_limits = ProcessLimits {
        wall: SLACK,
        stdout_max: 1024,
        stderr_max: u64::try_from(CUT_PADDING + CUT_KEPT).expect("should fit in u64"),
    };
    let command = helper(&dir, "secret-cut", process_limits)
        .secret_environment("TOKEN", CUT_SECRET)
        .expect("should accept a secret variable");

    let result = command.run().expect("should run the child");
    assert_eq!(
        result.outcome(),
        ProcessOutcome::OutputLimit {
            stream: Stream::Stderr
        },
        "should cut stderr at its cap"
    );
    let report = result
        .require_success()
        .expect_err("should reject a capped run");
    let text = format!("{report:?}");

    let kept = CUT_SECRET
        .get(..CUT_KEPT)
        .expect("should slice the fixture");
    assert!(
        !text.contains(kept),
        "should redact a secret prefix cut by the cap: {text}"
    );
    assert!(text.contains("[REDACTED]"), "should mark the cut: {text}");
}

#[test]
fn cut_tail_redaction_follows_actual_stderr_truncation() {
    let secrets = vec![CUT_SECRET.as_bytes().to_vec()];
    let result = |outcome, stderr_truncated| ProcessResult {
        program: PathBuf::from("/fixture/tool"),
        outcome,
        stdout: Vec::new(),
        stderr: b"exit 1: Q".to_vec(),
        stderr_truncated,
        elapsed: Duration::ZERO,
        secrets: secrets.clone(),
    };
    let text = |result: ProcessResult| {
        format!(
            "{:?}",
            result
                .require_success()
                .expect_err("should reject an unsuccessful run")
        )
    };

    let complete = text(result(ProcessOutcome::Exited { code: 1 }, false));
    let stderr_capped = text(result(
        ProcessOutcome::OutputLimit {
            stream: Stream::Stderr,
        },
        true,
    ));
    // Stdout overflowed first, but stderr was also cut mid-secret.
    let both_capped = text(result(
        ProcessOutcome::OutputLimit {
            stream: Stream::Stdout,
        },
        true,
    ));
    let stdout_only = text(result(
        ProcessOutcome::OutputLimit {
            stream: Stream::Stdout,
        },
        false,
    ));

    for untouched in [&complete, &stdout_only] {
        assert!(
            untouched.contains("exit 1: Q") && !untouched.contains("[REDACTED]"),
            "should leave untruncated stderr untouched: {untouched}"
        );
    }
    for redacted in [&stderr_capped, &both_capped] {
        assert!(
            redacted.contains("exit 1: [REDACTED]"),
            "should redact a possible secret prefix at a stderr cut: {redacted}"
        );
    }
}

#[test]
fn overlapping_secrets_are_fully_redacted() {
    let dir = workspace();
    let command = helper(&dir, "overlap", limits(SLACK))
        .secret_environment("FIRST", OVERLAP_FIRST)
        .expect("should accept a secret variable")
        .secret_environment("SECOND", OVERLAP_SECOND)
        .expect("should accept a secret variable");

    let report = command
        .run()
        .expect("should run the child")
        .require_success()
        .expect_err("should reject a nonzero exit");
    let text = format!("{report:?}");

    for fragment in ["Kq1", "Vv8", "Jx3"] {
        assert!(
            !text.contains(fragment),
            "should redact every byte of overlapping secrets: {text}"
        );
    }
}

#[test]
fn redaction_merges_overlapping_ranges() {
    let secrets = vec![b"abcXYZ".to_vec(), b"XYZdef".to_vec()];

    assert_eq!(
        redact(b"<abcXYZdef>", &secrets),
        b"<[REDACTED]>".to_vec(),
        "should replace the merged span once"
    );
}

#[test]
fn debug_redacts_program_and_working_directory() {
    let program = PathBuf::from(format!("/fixture/{SECRET}/tool"));
    let working_directory = PathBuf::from(format!("/fixture/{SECRET}"));
    let command = BoundedCommand::new(&program, &working_directory, limits(SLACK))
        .expect("should accept absolute paths")
        .secret_environment("TOKEN", SECRET)
        .expect("should accept a secret variable");

    let text = format!("{command:?}");

    assert!(!text.contains(SECRET), "should redact paths");
}
