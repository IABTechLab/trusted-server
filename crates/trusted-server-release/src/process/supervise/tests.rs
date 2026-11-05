//! Pure syscall-sequence checks; no process or signal is created.

use std::io;

use super::*;

#[test]
fn confirmation_only_observes_after_reaping() {
    let mut probes = 0;
    let mut pauses = 0;
    confirm_group_with(
        700,
        |group, signal| {
            assert_eq!(group, 700, "should observe the selected group");
            assert_eq!(signal, 0, "should never deliver a signal after reaping");
            probes += 1;
            match probes {
                1 => Ok(()),
                2 => Err(io::Error::from_raw_os_error(libc::EPERM)),
                _ => Err(io::Error::from_raw_os_error(libc::ESRCH)),
            }
        },
        || false,
        || pauses += 1,
    )
    .expect("should confirm disappearance after bounded observations");
    assert_eq!(probes, 3, "should observe each sequence state once");
    assert_eq!(pauses, 2, "should wait only while absence is unconfirmed");
}

#[test]
fn confirmation_accepts_immediate_absence_at_the_bound() {
    confirm_group_with(
        700,
        |_, signal| {
            assert_eq!(signal, 0, "should use a read-only probe");
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        },
        || true,
        || panic!("should not wait after observing absence"),
    )
    .expect("should accept confirmed absence even at the deadline");
}

#[test]
fn confirmation_bounds_persistent_presence_and_permission_refusal() {
    for (errno, expected) in [
        (None, ProcessFailure::GroupSurvived),
        (
            Some(libc::EPERM),
            ProcessFailure::Signal {
                errno: Some(libc::EPERM),
            },
        ),
    ] {
        let mut probes = 0;
        let mut ticks = 0;
        let mut pauses = 0;
        let report = confirm_group_with(
            700,
            |_, signal| {
                assert_eq!(
                    signal, 0,
                    "should never signal a potentially recycled group"
                );
                probes += 1;
                errno.map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
            },
            || {
                ticks += 1;
                ticks == 3
            },
            || pauses += 1,
        )
        .expect_err("should fail once the observation grace expires");
        assert_eq!(
            report.downcast_ref::<ProcessFailure>(),
            Some(&expected),
            "should preserve the real failure classification"
        );
        assert_eq!(probes, 3, "should stop at the supplied bound");
        assert_eq!(pauses, 2, "should not wait after the deadline");
    }
}

#[test]
fn confirmation_rejects_other_probe_errors_without_retry() {
    let report = confirm_group_with(
        700,
        |_, signal| {
            assert_eq!(signal, 0, "should use a read-only probe");
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        },
        || false,
        || panic!("should not retry a definitive observation error"),
    )
    .expect_err("should preserve a definitive probe failure");
    assert_eq!(
        report.downcast_ref::<ProcessFailure>(),
        Some(&ProcessFailure::Signal {
            errno: Some(libc::EINVAL)
        }),
        "should preserve the observed errno"
    );
}

#[test]
fn group_probe_refuses_unowned_ids_before_the_syscall() {
    for group in [-1, 0, 1] {
        let error =
            group_signal(group, 0).expect_err("should refuse an unsafe group before observing it");
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidInput,
            "should reject the group without a system call"
        );
    }
}
