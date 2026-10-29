//! Shared git-repo fixture helpers for the integration tests.
//!
//! These helpers have a single definition in the crate's
//! `commands/dev/lint/test_support` module; this file includes that
//! source verbatim so the integration suite and the inline unit tests
//! cannot drift. `pub(crate)` items become `pub` within the test
//! binary's own crate root, so the re-export below exposes them under
//! `common::`.
#[path = "../../src/commands/dev/lint/test_support.rs"]
mod test_support;

pub(crate) use test_support::*;
