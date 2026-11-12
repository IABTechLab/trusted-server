//! Final-artifact inspection tests over hand-built images.
//!
//! Real-binary observations (dev `ts` builds and the Fastly Wasm) are manual
//! evidence, not tests, because they depend on host build outputs.

pub(crate) mod builders;
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
