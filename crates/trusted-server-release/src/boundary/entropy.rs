//! Exact, bounded reads from the kernel entropy source.

use std::fs::File;
use std::io::Read;

use error_stack::Report;

use super::boundary_error;
use crate::error::ReleaseError;

/// Returns `N` bytes from the kernel random source.
///
/// # Errors
///
/// Rejects inaccessible entropy, read failures and short reads without a fallback.
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], Report<ReleaseError>> {
    let source = File::open("/dev/urandom")
        .map_err(|error| boundary_error(format!("cannot open kernel randomness: {error}")))?;
    read_entropy(source)
}

fn read_entropy<const N: usize>(source: impl Read) -> Result<[u8; N], Report<ReleaseError>> {
    let limit =
        u64::try_from(N).map_err(|_| boundary_error("entropy request exceeds read limit"))?;
    let mut bytes = Vec::new();
    source
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| boundary_error(format!("cannot read kernel randomness: {error}")))?;
    // Array conversion errors contain the bytes, so never attach the error itself.
    bytes
        .try_into()
        .map_err(|_| boundary_error("kernel randomness returned fewer bytes than requested"))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};

    use super::*;

    struct FailedRead;

    impl Read for FailedRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("example read failure"))
        }
    }

    #[test]
    fn entropy_read_preserves_source_bytes_and_does_not_consume_past_limit() {
        let mut source = Cursor::new(b"example entropy bytes");
        let bytes = read_entropy::<7>(&mut source).expect("should read the requested bytes");
        assert_eq!(bytes, *b"example", "should preserve source bytes exactly");
        assert_eq!(source.position(), 7, "should stop at the requested limit");
    }

    #[test]
    fn entropy_read_rejects_short_input_without_disclosing_bytes() {
        let report = read_entropy::<32>(&b"example partial entropy"[..])
            .expect_err("should reject a short entropy source");
        let text = format!("{report:?}");
        assert!(text.contains("fewer bytes"), "should identify a short read");
        assert!(
            !text.contains("example partial entropy"),
            "should not disclose partial entropy"
        );
    }

    #[test]
    fn entropy_read_rejects_io_failure() {
        let source = io::repeat(1).take(1).chain(FailedRead);
        let report = read_entropy::<32>(source).expect_err("should reject a read failure");
        assert!(
            format!("{report:?}").contains("cannot read kernel randomness"),
            "should identify the entropy read failure"
        );
    }
}
