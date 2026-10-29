//! Unconditional bounded cleanup sequencing for an owned startup container.

use error_stack::Report;

use crate::error::ReleaseError;

/// Runs an owned container and attempts stop, removal and absence confirmation.
pub(super) fn run_owned<T>(
    run: impl FnOnce() -> Result<T, Report<ReleaseError>>,
    mut cleanup: impl FnMut(&str) -> Result<bool, Report<ReleaseError>>,
) -> Result<T, Report<ReleaseError>> {
    let mut outcome = run();
    for step in ["stop", "rm", "ps"] {
        let checked = cleanup(step).and_then(|confirmed| {
            if step == "ps" && !confirmed {
                Err(crate::recipe::recipe_error(
                    "startup container was not confirmed removed",
                ))
            } else {
                Ok(())
            }
        });
        outcome = combine(outcome, checked);
    }
    outcome
}

/// Preserves the original run failure and any later integrity or cleanup failure.
pub(super) fn combine<T>(
    run: Result<T, Report<ReleaseError>>,
    later: Result<(), Report<ReleaseError>>,
) -> Result<T, Report<ReleaseError>> {
    match (run, later) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(later)) => Err(error.attach(format!("startup also failed: {later:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::recipe_error;

    #[test]
    fn every_run_outcome_attempts_all_cleanup_steps() {
        for run_failure in [false, true] {
            for cleanup_failure in [None, Some("stop"), Some("rm"), Some("ps")] {
                let mut observed = Vec::new();
                let outcome = run_owned(
                    || {
                        if run_failure {
                            Err(recipe_error("launch or supervision failed"))
                        } else {
                            Ok(())
                        }
                    },
                    |step| {
                        observed.push(step.to_owned());
                        if cleanup_failure == Some(step) {
                            Err(recipe_error(format!("{step} failed")))
                        } else {
                            Ok(true)
                        }
                    },
                );
                assert_eq!(
                    observed,
                    ["stop", "rm", "ps"],
                    "should always attempt all cleanup"
                );
                assert_eq!(
                    outcome.is_err(),
                    run_failure || cleanup_failure.is_some(),
                    "should preserve every failure"
                );
                if run_failure {
                    assert!(
                        format!("{:?}", outcome.expect_err("should reject failed run"))
                            .contains("launch or supervision failed"),
                        "should retain the original failure"
                    );
                }
            }
        }
    }

    #[test]
    fn absence_confirmation_is_mandatory() {
        let outcome = run_owned(|| Ok(()), |step| Ok(step != "ps"));
        assert!(
            outcome.is_err(),
            "should reject a surviving or unconfirmed container"
        );
    }
}

/// Starts guest code only after acknowledged creation, then cleans that identity.
pub(super) fn run_created<C, T>(
    create: impl FnOnce() -> Result<C, Report<ReleaseError>>,
    run: impl FnOnce(&C) -> Result<T, Report<ReleaseError>>,
    mut cleanup: impl FnMut(Option<&C>, &str) -> Result<bool, Report<ReleaseError>>,
) -> Result<T, Report<ReleaseError>> {
    match create() {
        Ok(created) => run_owned(|| run(&created), |step| cleanup(Some(&created), step)),
        Err(error) => run_owned(
            || {
                Err(error.attach("container creation remains unresolved despite fallback cleanup; no guest start was issued"))
            },
            |step| cleanup(None, step),
        ),
    }
}

#[cfg(test)]
mod creation_tests {
    use std::cell::Cell;

    use super::*;
    use crate::recipe::recipe_error;

    #[test]
    fn failed_creation_never_starts_and_cannot_be_cleared_by_absence() {
        let started = Cell::new(false);
        let mut steps = Vec::new();
        let result = run_created(
            || Err::<u8, _>(recipe_error("create timed out")),
            |_| {
                started.set(true);
                Ok(())
            },
            |identity, step| {
                assert!(
                    identity.is_none(),
                    "should use name cleanup without acknowledged identity"
                );
                steps.push(step.to_owned());
                Ok(true)
            },
        );
        assert!(!started.get(), "should never start after failed creation");
        assert_eq!(
            steps,
            ["stop", "rm", "ps"],
            "should attempt every bounded fallback cleanup"
        );
        let error = format!(
            "{:?}",
            result.expect_err("should retain unresolved creation")
        );
        assert!(
            error.contains("create timed out") && error.contains("unresolved"),
            "should preserve original and unresolved creation errors: {error}"
        );
    }

    #[test]
    fn execution_uses_acknowledged_identity_for_all_cleanup() {
        for fail_start in [false, true] {
            let mut steps = Vec::new();
            let result = run_created(
                || Ok(7_u8),
                |identity| {
                    assert_eq!(*identity, 7, "should start only the acknowledged identity");
                    if fail_start {
                        Err(recipe_error("attach timed out"))
                    } else {
                        Ok(())
                    }
                },
                |identity, step| {
                    assert_eq!(
                        identity,
                        Some(&7),
                        "should clean only the acknowledged identity"
                    );
                    steps.push(step.to_owned());
                    Ok(true)
                },
            );
            assert_eq!(
                steps,
                ["stop", "rm", "ps"],
                "should always clean after execution"
            );
            assert_eq!(
                result.is_err(),
                fail_start,
                "should preserve execution outcome"
            );
        }
    }
}
