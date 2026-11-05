//! Scan of the captured Viceroy output for guest failures.
//!
//! A guest trap or panic can still leave a valid-looking response on the
//! wire, so after the run the gate searches both captured streams (each
//! bounded by the command's output cap) for failure markers that Viceroy or
//! the Rust runtime print. Matching ignores ASCII case. The streams are
//! retained only as digests plus a short sanitized stderr tail.

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::{RuntimeRejection, runtime_error};
use crate::error::ReleaseError;

/// Lowercase markers of a guest trap, panic or failed request execution.
const GUEST_FAILURE_MARKERS: [&str; 7] = [
    "wasm trap",
    "wasm backtrace",
    "panicked at",
    "error while executing",
    "there was an error handling the request",
    "error handling request",
    "guest exited with",
];
/// Bytes of stderr kept in the summary.
const TAIL_BYTES: usize = 1024;

/// Digests and a short tail of the captured Viceroy output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViceroyOutput {
    pub(crate) stdout_sha256: String,
    pub(crate) stderr_sha256: String,
    /// The last stderr bytes, lossily decoded with control characters other
    /// than newline replaced by `?`.
    pub(crate) stderr_tail: String,
}

/// Returns the last [`TAIL_BYTES`] of `stderr` as printable text.
pub(in crate::recipe) fn sanitized_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(TAIL_BYTES);
    String::from_utf8_lossy(&stderr[start..])
        .chars()
        .map(|character| {
            if character.is_control() && character != '\n' {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Rejects output carrying a guest failure marker and summarizes the rest.
///
/// # Errors
///
/// Returns [`RuntimeRejection::GuestFailure`] for a stderr line beginning
/// with `Error:` or, naming the stream and marker, for the first failure
/// marker found in either stream.
pub(in crate::recipe) fn scan_viceroy_output(
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ViceroyOutput, Report<ReleaseError>> {
    // A clean run leaves stderr empty; Viceroy reports a failed guest request
    // there as an `Error: ...` line.
    if String::from_utf8_lossy(stderr)
        .lines()
        .any(|line| line.trim_start().to_ascii_lowercase().starts_with("error:"))
    {
        return Err(runtime_error(
            RuntimeRejection::GuestFailure,
            format!(
                "Viceroy stderr reports an error line; stderr tail: {}",
                sanitized_tail(stderr)
            ),
        ));
    }
    for (stream, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        if let Some(marker) = GUEST_FAILURE_MARKERS
            .iter()
            .find(|marker| text.contains(*marker))
        {
            return Err(runtime_error(
                RuntimeRejection::GuestFailure,
                format!(
                    "Viceroy {stream} reports {marker:?}; stderr tail: {}",
                    sanitized_tail(stderr)
                ),
            ));
        }
    }
    Ok(ViceroyOutput {
        stdout_sha256: hex::encode(Sha256::digest(stdout)),
        stderr_sha256: hex::encode(Sha256::digest(stderr)),
        stderr_tail: sanitized_tail(stderr),
    })
}
