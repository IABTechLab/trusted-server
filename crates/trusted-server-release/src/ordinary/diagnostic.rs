//! Bounded CI-only failure facts; never a publishable equality companion.

use core::fmt;
use std::path::{Path, PathBuf};

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{output, require, transfer};
use crate::documents::{self, Role};
use crate::error::ReleaseError;

const LIMIT: usize = 4096;

/// Fixed classifications established at the rejecting check, never from stderr.
#[derive(Clone, Copy, Debug)]
pub(super) enum Code {
    TransferUnavailable,
    TransferRejected,
    InputsRejected,
    IdentityMismatch,
    SizeMismatch,
    DigestMismatch,
    BytesMismatch,
    OutputRejected,
}

impl Code {
    fn fields(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::TransferUnavailable => ("unavailable", "transfer", "transfer_unavailable"),
            Self::TransferRejected => ("failed", "transfer", "transfer_rejected"),
            Self::InputsRejected => ("failed", "inputs", "source_inputs_rejected"),
            Self::IdentityMismatch => ("failed", "identity", "identity_mismatch"),
            Self::SizeMismatch => ("failed", "size", "observed_size_mismatch"),
            Self::DigestMismatch => ("failed", "digest", "observed_digest_mismatch"),
            Self::BytesMismatch => ("mismatched", "bytes", "final_bytes_mismatch"),
            Self::OutputRejected => ("failed", "output", "companion_write_rejected"),
        }
    }
}

/// Only fixed fields and actually measured, bounded facts can be serialized.
#[derive(Clone, Debug)]
pub(super) struct Failure {
    code: Code,
    artifact_ids: Option<[u64; 4]>,
    binaries: Option<[(u64, String); 2]>,
    difference: Option<(u64, [String; 2])>,
}

impl Failure {
    /// Starts a classified failure with every unobserved fact unavailable.
    pub(super) fn new(code: Code) -> Self {
        Self {
            code,
            artifact_ids: None,
            binaries: None,
            difference: None,
        }
    }

    /// Measures only the two retained binary byte slices.
    pub(super) fn binaries(mut self, left: &[u8], right: &[u8]) -> Self {
        self.binaries = Some(
            [left, right].map(|bytes| (bytes.len() as u64, hex::encode(Sha256::digest(bytes)))),
        );
        self
    }

    /// Retains the actual first difference and at most sixteen bytes per side.
    pub(super) fn mismatch(left: &[u8], right: &[u8], offset: usize) -> Self {
        let mut failure = Self::new(Code::BytesMismatch).binaries(left, right);
        let windows = [left, right].map(|bytes| {
            let start = offset.saturating_sub(8).min(bytes.len());
            hex::encode(&bytes[start..start.saturating_add(16).min(bytes.len())])
        });
        failure.difference = Some((offset as u64, windows));
        failure
    }

    /// Projects only source-owned fields and measured facts for CI retention.
    pub(super) fn value(&self) -> Value {
        let unavailable = || json!("unavailable");
        let (outcome, stage, code) = self.code.fields();
        let id = |index| {
            self.artifact_ids
                .map(|ids| json!(ids[index]))
                .unwrap_or_else(unavailable)
        };
        let binary = |index: usize, digest| {
            self.binaries
                .as_ref()
                .map(|facts| {
                    if digest {
                        json!(facts[index].1)
                    } else {
                        json!(facts[index].0)
                    }
                })
                .unwrap_or_else(unavailable)
        };
        json!({
            "kind":"ordinary-comparison-diagnostic", "version":1,
            "outcome":outcome, "stage":stage, "code":code,
            "facts":{
                "canonical_artifact_id":id(0), "canonical_observation_artifact_id":id(1),
                "reproduction_artifact_id":id(2), "reproduction_observation_artifact_id":id(3),
                "canonical_size":binary(0,false), "reproduction_size":binary(1,false),
                "canonical_sha256":binary(0,true), "reproduction_sha256":binary(1,true),
                "first_difference_offset":self.difference.as_ref().map(|facts| json!(facts.0)).unwrap_or_else(unavailable),
                "canonical_window":self.difference.as_ref().map(|facts| json!(facts.1[0])).unwrap_or_else(unavailable),
                "reproduction_window":self.difference.as_ref().map(|facts| json!(facts.1[1])).unwrap_or_else(unavailable),
            }
        })
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (outcome, stage, code) = self.code.fields();
        write!(
            formatter,
            "ordinary diagnostic outcome={outcome} stage={stage} code={code}"
        )
    }
}

/// Classifies an unclassified failure without replacing deeper checked facts.
pub(super) fn tag(report: Report<ReleaseError>, code: Code) -> Report<ReleaseError> {
    if report.contains::<Failure>() {
        report
    } else {
        report.attach(Failure::new(code))
    }
}

/// Preserves the actual equal binary facts already retained by checked transfer.
pub(super) fn tag_equal(
    report: Report<ReleaseError>,
    code: Code,
    checked: &transfer::Checked,
) -> Report<ReleaseError> {
    if report.contains::<Failure>() {
        report
    } else {
        let (binary, _) = checked.inputs();
        report.attach(Failure::new(code).binaries(binary, binary))
    }
}

fn encoded(value: &Value) -> Result<Value, Report<ReleaseError>> {
    let bytes = documents::canonical_json(value, Role::Metadata)?;
    require(
        bytes.len() <= LIMIT,
        "ordinary diagnostic exceeds 4096-byte cap",
    )?;
    Ok(value.clone())
}

/// Writes separate private failure evidence without touching the companion root.
///
/// # Errors
///
/// Rejects oversized diagnostics, unsafe or reused destinations and failed writes.
pub(super) fn retain(
    report: &Report<ReleaseError>,
    output: &Path,
    artifact_ids: Option<[u64; 4]>,
) -> Result<PathBuf, Report<ReleaseError>> {
    let mut failure = report
        .downcast_ref::<Failure>()
        .cloned()
        .unwrap_or_else(|| Failure::new(Code::InputsRejected));
    failure.artifact_ids = artifact_ids;
    let value = encoded(&failure.value())?;
    let parent = output
        .parent()
        .ok_or_else(|| Report::new(ReleaseError::Document))?;
    output::diagnostic(&parent.join("ordinary-diagnostics"), &value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_diagnostics_are_canonical_bounded_and_never_companions() {
        for code in [
            Code::TransferUnavailable,
            Code::TransferRejected,
            Code::InputsRejected,
            Code::IdentityMismatch,
            Code::SizeMismatch,
            Code::DigestMismatch,
            Code::BytesMismatch,
            Code::OutputRejected,
        ] {
            let value = Failure::new(code).value();
            let checked = encoded(&value).expect("should encode bounded fixed facts");
            let bytes = documents::canonical_json(&checked, Role::Metadata)
                .expect("should serialize canonically");
            assert!(bytes.len() <= LIMIT, "should stay below the hard cap");
            assert!(
                value.get("profile").is_none(),
                "should never resemble a matched companion"
            );
            assert!(
                value["facts"]
                    .as_object()
                    .expect("should have facts")
                    .values()
                    .all(|fact| fact == "unavailable"),
                "should mark every unobserved fact unavailable"
            );
        }
        assert!(
            encoded(&json!({"unbounded":"x".repeat(LIMIT)})).is_err(),
            "should refuse oversized diagnostics rather than truncate JSON"
        );
    }

    #[test]
    fn mismatch_retains_only_actual_binary_facts_and_bounded_windows() {
        let value = Failure::mismatch(b"abc", b"abcd", 3).value();
        assert_eq!(
            value["outcome"], "mismatched",
            "should label actual final-byte inequality"
        );
        assert_eq!(
            value["stage"], "bytes",
            "should not mistake accurately measured unequal lengths for observed-size corruption"
        );
        assert_eq!(
            value["facts"]["first_difference_offset"], 3,
            "should retain actual prefix difference"
        );
        assert_eq!(
            value["facts"]["canonical_size"], 3,
            "should measure canonical bytes"
        );
        assert_eq!(
            value["facts"]["reproduction_size"], 4,
            "should measure reproduction bytes"
        );
        assert_eq!(
            value["facts"]["canonical_artifact_id"], "unavailable",
            "should not invent service facts"
        );
        assert!(
            value["facts"]["canonical_window"]
                .as_str()
                .expect("should retain hex")
                .len()
                <= 32,
            "should bound byte windows"
        );
    }
}
