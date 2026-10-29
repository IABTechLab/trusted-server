//! Construct-only-through-success network denial proof.
//!
//! [`DenialProof`] has private fields, no `Default`, `Clone` or serde
//! implementations, and is built only by [`evaluate`] after, in this process
//! run and for one boundary instance fingerprint (configuration plus a fresh
//! per-instance nonce):
//!
//! 1. the probe ran **unconfined**, exited `0`, reported `connected`, and the
//!    control listener accepted exactly one connection; then
//! 2. the same probe ran **confined**, exited with
//!    [`DENIED_STATUS`], reported a real `EPERM`
//!    or `EACCES`, and the listener accepted nothing.
//!
//! A failed or refused unconfined control (no local socket permission, an
//! enclosing sandbox) yields no proof, so confined denial is never inferred
//! from a probe that could not have connected anyway.

use std::path::Path;

use error_stack::Report;
use serde::Serialize;

use super::probe::{DENIED_STATUS, LoopbackListener, ProbeProgram, ProbeReport};
use super::{Boundary, ConfinedCommand};
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessLimits, ProcessOutcome, ProcessResult};

/// Errnos accepted as an operating-system network denial.
const DENIAL_ERRNOS: [i32; 2] = [libc::EPERM, libc::EACCES];

fn proof_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Evidence that this boundary configuration denies network access.
#[derive(Debug)]
pub(crate) struct DenialProof {
    fingerprint: [u8; 32],
    denied_errno: i32,
    observations: ProofObservations,
}

#[derive(Debug)]
enum ProofObservations {
    Measured {
        control: Observation,
        confined: Observation,
    },
    #[cfg(test)]
    Fixture,
}

/// Read-only measured socket diagnostics; never an execution capability.
#[derive(Debug, Serialize)]
pub(crate) struct DenialReceipt {
    fingerprint: String,
    denied_errno: i32,
    control: ProbeReceipt,
    probe: ProbeReceipt,
}

impl DenialReceipt {
    /// Returns whether these measured diagnostics name the expected boundary instance.
    pub(crate) fn matches_fingerprint(&self, expected: &[u8; 32]) -> bool {
        self.fingerprint == hex::encode(expected)
    }

    /// Lowercase hex fingerprint of the measured boundary instance.
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Real errno the confined probe reported.
    pub(crate) fn denied_errno(&self) -> i32 {
        self.denied_errno
    }

    /// Exit status and listener accepts of the unconfined control run.
    pub(crate) fn control(&self) -> (i32, usize) {
        self.control.measured()
    }

    /// Exit status and listener accepts of the confined probe run.
    pub(crate) fn probe(&self) -> (i32, usize) {
        self.probe.measured()
    }
}

#[derive(Debug, Serialize)]
struct ProbeReceipt {
    outcome: ExitReceipt,
    report: ReportReceipt,
    accepted_connections: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ExitReceipt {
    Exited { status: i32 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReportReceipt {
    Connected,
    Denied { errno: i32 },
}

impl ProbeReceipt {
    fn measured(&self) -> (i32, usize) {
        let ExitReceipt::Exited { status } = self.outcome;
        (status, self.accepted_connections)
    }

    fn from_observation(observed: Observation) -> Result<Self, Report<ReleaseError>> {
        let ProcessOutcome::Exited { code } = observed.outcome else {
            return Err(proof_error(
                "measured socket observation did not exit normally",
            ));
        };
        let report = match observed.report {
            Some(ProbeReport::Connected) => ReportReceipt::Connected,
            Some(ProbeReport::Denied { errno }) => ReportReceipt::Denied { errno },
            None => return Err(proof_error("measured socket observation has no report")),
        };
        Ok(Self {
            outcome: ExitReceipt::Exited { status: code },
            report,
            accepted_connections: observed.accepts,
        })
    }
}

impl DenialProof {
    /// Exports actual socket observations for the expected boundary instance.
    ///
    /// # Errors
    ///
    /// Rejects a mismatched instance, missing measured observations or any
    /// control/probe pair that does not establish successful socket denial.
    pub(crate) fn receipt_for(
        &self,
        expected_boundary_fingerprint: &[u8; 32],
    ) -> Result<DenialReceipt, Report<ReleaseError>> {
        if !self.matches(expected_boundary_fingerprint) {
            return Err(proof_error(
                "measured denial receipt belongs to another boundary instance",
            ));
        }
        let (control, confined) = match &self.observations {
            ProofObservations::Measured { control, confined } => (*control, *confined),
            #[cfg(test)]
            ProofObservations::Fixture => {
                return Err(proof_error(
                    "synthetic test proof has no measured socket observations",
                ));
            }
        };
        if validate_observations(control, confined)? != self.denied_errno {
            return Err(proof_error(
                "stored denial errno differs from measured observations",
            ));
        }
        Ok(DenialReceipt {
            fingerprint: hex::encode(self.fingerprint),
            denied_errno: self.denied_errno,
            control: ProbeReceipt::from_observation(control)?,
            probe: ProbeReceipt::from_observation(confined)?,
        })
    }

    /// Returns whether the proof was established for `fingerprint`.
    pub(super) fn matches(&self, fingerprint: &[u8; 32]) -> bool {
        &self.fingerprint == fingerprint
    }

    /// Builds a proof for unit tests that never run a confined command.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            fingerprint: [0; 32],
            denied_errno: 1,
            observations: ProofObservations::Fixture,
        }
    }

    /// Returns the real errno the confined probe observed.
    #[must_use]
    pub(crate) fn denied_errno(&self) -> i32 {
        self.denied_errno
    }
}

/// One probe run as the controls observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observation {
    /// The probe's exit outcome.
    outcome: ProcessOutcome,
    /// The parsed report, if stdout carried exactly one.
    report: Option<ProbeReport>,
    /// Connections the control listener accepted during this run.
    accepts: usize,
}

/// Constructs a proof only from a successful control and a denied probe.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when the control did not connect with
/// exactly one observed accept, or the confined probe was not denied with
/// `EPERM`/`EACCES` and zero accepts.
fn evaluate(
    fingerprint: [u8; 32],
    control: Observation,
    confined: Observation,
) -> Result<DenialProof, Report<ReleaseError>> {
    let denied_errno = validate_observations(control, confined)?;
    Ok(DenialProof {
        fingerprint,
        denied_errno,
        observations: ProofObservations::Measured { control, confined },
    })
}

fn validate_observations(
    control: Observation,
    confined: Observation,
) -> Result<i32, Report<ReleaseError>> {
    let control_ok = control.outcome == (ProcessOutcome::Exited { code: 0 })
        && control.report == Some(ProbeReport::Connected)
        && control.accepts == 1;
    if !control_ok {
        return Err(proof_error(format!(
            "unconfined socket control failed: {control:?}"
        )));
    }
    let denied_status = ProcessOutcome::Exited {
        code: i32::from(DENIED_STATUS),
    };
    let denied_errno = match confined.report {
        Some(ProbeReport::Denied { errno })
            if confined.outcome == denied_status
                && DENIAL_ERRNOS.contains(&errno)
                && confined.accepts == 0 =>
        {
            errno
        }
        _ => {
            return Err(proof_error(format!(
                "confined probe was not denied: {confined:?}"
            )));
        }
    };
    Ok(denied_errno)
}

fn observe(
    probe: &ProbeProgram,
    listener: &LoopbackListener,
    result: ProcessResult,
) -> Result<Observation, Report<ReleaseError>> {
    let observation = Observation {
        outcome: result.outcome(),
        report: probe.parse(result.stdout()),
        accepts: listener.drain_accepts()?,
    };
    match observation.outcome {
        ProcessOutcome::Exited { code } if code == 0 || code == i32::from(DENIED_STATUS) => {
            Ok(observation)
        }
        _ => result.require_success().map(|_| observation),
    }
}

fn unconfined_command(
    probe: &ProbeProgram,
    listener: &LoopbackListener,
    limits: ProcessLimits,
) -> Result<BoundedCommand, Report<ReleaseError>> {
    let mut command = BoundedCommand::new(probe.program(), Path::new("/"), limits)?
        .arguments(probe.arguments(listener.address()));
    for (name, value) in probe.environment() {
        command = command.environment(name, value)?;
    }
    Ok(command)
}

fn confined_command(
    boundary: &Boundary,
    probe: &ProbeProgram,
    listener: &LoopbackListener,
    limits: ProcessLimits,
) -> Result<ConfinedCommand, Report<ReleaseError>> {
    let working_directory = boundary
        .roots
        .writable()
        .first()
        .ok_or_else(|| proof_error("boundary has no writable root"))?;
    let mut command = ConfinedCommand::new(probe.program(), working_directory, limits)?
        .arguments(probe.arguments(listener.address()));
    for (name, value) in probe.environment() {
        command = command.environment(name, value)?;
    }
    Ok(command)
}

/// Runs the unconfined control, then the confined probe, and evaluates both.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when either control fails, the probe is
/// outside the boundary's roots or the listener fails, or
/// [`ReleaseError::Process`] when a probe child cannot be run.
pub(super) fn establish(
    boundary: &Boundary,
    probe: &ProbeProgram,
    limits: ProcessLimits,
) -> Result<DenialProof, Report<ReleaseError>> {
    let listener = LoopbackListener::bind()?;
    if listener.drain_accepts()? != 0 {
        return Err(proof_error(
            "control listener had a connection before the control",
        ));
    }
    let control_result = unconfined_command(probe, &listener, limits)?.run()?;
    let control = observe(probe, &listener, control_result)?;
    let confined_result =
        boundary.execute(&confined_command(boundary, probe, &listener, limits)?)?;
    let confined = observe(probe, &listener, confined_result)?;
    evaluate(boundary.fingerprint, control, confined)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[test]
    fn unexpected_probe_exit_keeps_bounded_startup_diagnostic() {
        let probe = ProbeProgram::release_binary().expect("should locate probe executable");
        let listener = LoopbackListener::bind().expect("should bind fixture listener");
        let result = BoundedCommand::new(
            Path::new("/bin/sh"),
            Path::new("/"),
            ProcessLimits {
                wall: Duration::from_secs(5),
                stdout_max: 1024,
                stderr_max: 1024,
            },
        )
        .expect("should select fixture command")
        .arguments(
            [
                "-c",
                "printf 'fictional container startup failure' >&2; exit 125",
            ]
            .map(OsString::from),
        )
        .run()
        .expect("should supervise fixture child");
        let error = observe(&probe, &listener, result)
            .expect_err("should reject failed probe child before protocol evaluation");
        assert!(
            format!("{error:?}").contains("fictional container startup failure"),
            "should preserve bounded child failure diagnostic"
        );
    }

    fn observation(code: i32, report: Option<ProbeReport>, accepts: usize) -> Observation {
        Observation {
            outcome: ProcessOutcome::Exited { code },
            report,
            accepts,
        }
    }

    fn denied(errno: i32, accepts: usize) -> Observation {
        observation(3, Some(ProbeReport::Denied { errno }), accepts)
    }

    #[test]
    fn measured_receipt_retains_validated_control_and_probe_observations() {
        let fingerprint = [7; 32];
        let control = observation(0, Some(ProbeReport::Connected), 1);
        for errno in [libc::EPERM, libc::EACCES] {
            let proof = evaluate(fingerprint, control, denied(errno, 0))
                .expect("should evaluate a successful pure observation fixture");
            let receipt = proof
                .receipt_for(&fingerprint)
                .expect("should export validated observations for the same boundary");
            assert!(
                receipt.matches_fingerprint(&fingerprint),
                "should bind diagnostics to the measured boundary instance"
            );
            assert!(
                !receipt.matches_fingerprint(&[8; 32]),
                "should reject an unrelated boundary fingerprint"
            );
            let value =
                serde_json::to_value(&receipt).expect("should serialize measured diagnostics");
            assert_eq!(
                value,
                json!({
                    "fingerprint": hex::encode(fingerprint), "denied_errno": errno,
                    "control": {"outcome":{"kind":"exited","status":0},"report":{"kind":"connected"},"accepted_connections":1},
                    "probe": {"outcome":{"kind":"exited","status":3},"report":{"kind":"denied","errno":errno},"accepted_connections":0}
                }),
                "should retain exactly the successful fixture observations without claiming an operating-system run"
            );
        }
    }

    #[test]
    fn measured_receipt_rejects_other_instances_and_unmeasured_test_proofs() {
        let proof = evaluate(
            [7; 32],
            observation(0, Some(ProbeReport::Connected), 1),
            denied(libc::EPERM, 0),
        )
        .expect("should evaluate a successful pure observation fixture");
        assert!(
            proof.receipt_for(&[8; 32]).is_err(),
            "should reject a different boundary instance"
        );
        assert!(
            DenialProof::for_tests().receipt_for(&[0; 32]).is_err(),
            "should never export measured success from a synthetic test capability"
        );
    }

    #[test]
    fn measured_receipt_revalidates_stored_observations_and_errno() {
        let control = observation(0, Some(ProbeReport::Connected), 1);
        let confined = denied(libc::EPERM, 0);
        for (changed_control, changed_confined) in [
            (observation(0, Some(ProbeReport::Connected), 0), confined),
            (control, denied(libc::EPERM, 1)),
            (control, observation(3, None, 0)),
        ] {
            let mut proof = evaluate([7; 32], control, confined)
                .expect("should evaluate a successful pure observation fixture");
            proof.observations = ProofObservations::Measured {
                control: changed_control,
                confined: changed_confined,
            };
            assert!(
                proof.receipt_for(&[7; 32]).is_err(),
                "should reject corrupted stored observations"
            );
        }
        let mut proof = evaluate([7; 32], control, confined)
            .expect("should evaluate a successful pure observation fixture");
        proof.denied_errno = libc::EACCES;
        assert!(
            proof.receipt_for(&[7; 32]).is_err(),
            "should reject a stored errno that differs from the observed report"
        );
    }

    #[test]
    fn proof_requires_successful_control_and_denied_probe() {
        let fingerprint = [7; 32];
        let control = observation(0, Some(ProbeReport::Connected), 1);
        let proof =
            evaluate(fingerprint, control, denied(libc::EPERM, 0)).expect("should prove denial");
        assert_eq!(
            proof.denied_errno(),
            libc::EPERM,
            "should keep the real errno"
        );
        assert!(proof.matches(&fingerprint), "should bind the instance");
        assert!(
            !proof.matches(&[8; 32]),
            "should not match another instance"
        );
        let eacces =
            evaluate(fingerprint, control, denied(libc::EACCES, 0)).expect("should accept EACCES");
        assert_eq!(eacces.denied_errno(), libc::EACCES, "should keep EACCES");
    }

    #[test]
    fn proof_rejects_failed_controls() {
        let bad_controls = [
            denied(libc::ECONNREFUSED, 0),
            observation(0, Some(ProbeReport::Connected), 0),
            observation(0, Some(ProbeReport::Connected), 2),
            observation(0, None, 1),
            observation(1, Some(ProbeReport::Connected), 1),
        ];
        for control in bad_controls {
            assert!(
                evaluate([7; 32], control, denied(libc::EPERM, 0)).is_err(),
                "should reject failed control {control:?}"
            );
        }
    }

    #[test]
    fn proof_rejects_undenied_probes() {
        let control = observation(0, Some(ProbeReport::Connected), 1);
        let bad_confined = [
            observation(0, Some(ProbeReport::Connected), 1),
            denied(libc::ECONNREFUSED, 0),
            denied(111, 0),
            denied(libc::EPERM, 1),
            observation(0, Some(ProbeReport::Denied { errno: libc::EPERM }), 0),
            observation(3, None, 0),
        ];
        for confined in bad_confined {
            assert!(
                evaluate([7; 32], control, confined).is_err(),
                "should reject undenied probe {confined:?}"
            );
        }
    }
}
