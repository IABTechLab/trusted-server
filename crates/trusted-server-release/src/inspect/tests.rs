//! Final-artifact inspection tests over hand-built images.
//!
//! Real-binary observations (dev `ts` builds and the Fastly Wasm) are manual
//! evidence, not tests, because they depend on host build outputs.

pub(crate) mod builders;
mod elf;
mod macho;
mod wasm;

use error_stack::Report;

use super::*;
use crate::error::ReleaseError;

/// Returns the [`Violation`] attached to an inspection rejection.
fn violation<T: fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> Violation {
    let report = result.expect_err("should reject the artifact");
    assert!(
        matches!(report.current_context(), ReleaseError::Inspect),
        "should report an inspection rejection: {report:?}"
    );
    *report
        .downcast_ref::<Violation>()
        .expect("should attach the violated rule")
}

/// Asserts every strict prefix of `bytes` is rejected without panicking.
fn assert_truncations_rejected<T: fmt::Debug>(
    bytes: &[u8],
    inspect: impl Fn(&[u8]) -> Result<T, Report<ReleaseError>>,
) {
    for length in 0..bytes.len() {
        assert!(
            inspect(&bytes[..length]).is_err(),
            "should reject the input truncated to {length} of {} bytes",
            bytes.len()
        );
    }
}

#[test]
fn glibc_versions_parse_numerically() {
    assert_eq!(GlibcVersion::parse("2.34"), Some(GlibcVersion::new(2, 34)));
    assert!(
        GlibcVersion::parse("2.2.5").expect("should parse three components")
            < GlibcVersion::new(2, 3),
        "should compare 2.2.5 below 2.3"
    );
    assert!(
        GlibcVersion::new(2, 9) < GlibcVersion::new(2, 34),
        "should compare minors numerically, not lexically"
    );
    for text in [
        "",
        "2",
        "PRIVATE",
        "ABI_DT_RELR",
        "2.34.x",
        "2.+4",
        "2.034",
        "2..34",
        "1.2.3.4",
    ] {
        assert_eq!(GlibcVersion::parse(text), None, "should reject {text:?}");
    }
}

#[test]
fn macos_versions_decode_nibbles() {
    let version = MacosVersion::from_encoded(0x000d_0100);
    assert_eq!(version.to_string(), "13.1.0", "should decode xxxx.yy.zz");
    assert_eq!(
        MacosVersion::from_encoded(0x000d_0000),
        MacosVersion::new(13, 0)
    );
}

#[test]
fn denied_path_scan_reports_offsets() {
    let bytes = b"ok /usr/lib x /home/runner/work y /tmp/abc /tmpfoo";
    let hits: Vec<_> = find_denied_paths(bytes, RUNNER_PATH_PREFIXES).collect();
    assert_eq!(
        hits,
        vec![
            PathHit {
                prefix: "/home/",
                offset: 14
            },
            PathHit {
                prefix: "/tmp/",
                offset: 34
            },
        ],
        "should report each runner-only prefix with its offset"
    );
    assert!(
        find_denied_paths(
            b"serde/src/private/de.rs /rust/deps/x/src/lib.rs",
            RUNNER_PATH_PREFIXES
        )
        .next()
        .is_none(),
        "should not match crate-relative or remapped paths"
    );
    let report = reject_denied_paths(bytes, RUNNER_PATH_PREFIXES)
        .expect_err("should reject embedded runner paths");
    assert!(
        format!("{report:?}").contains("/home/@0xe"),
        "should report the hit offset: {report:?}"
    );
}

#[test]
fn denied_path_contexts_are_bounded_and_capped_without_weakening_rejection() {
    let mut bytes = vec![b'x'; 4096];
    for offset in (128..4096).step_by(128) {
        bytes[offset..offset + 6].copy_from_slice(b"/tmp/\n");
    }
    let report = reject_denied_paths(&bytes, RUNNER_PATH_PREFIXES)
        .expect_err("should still reject every denied path");
    let diagnostic = format!("{report:?}");
    assert!(
        diagnostic.contains("31 runner-only path occurrence(s)"),
        "should count all occurrences: {diagnostic}"
    );
    assert_eq!(
        diagnostic.matches("context[").count(),
        REPORTED_PATH_HITS,
        "should report at most sixteen contexts"
    );
    assert!(
        diagnostic.contains("context[0x70..0xd0]"),
        "should bound each context to sixteen bytes before and eighty after the hit"
    );
    assert!(
        diagnostic.contains("/tmp/\\n"),
        "should escape raw control bytes"
    );
    assert!(
        diagnostic.len() < 10_000,
        "should bound output even for many hits"
    );
}
