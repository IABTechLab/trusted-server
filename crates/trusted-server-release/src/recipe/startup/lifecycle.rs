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
