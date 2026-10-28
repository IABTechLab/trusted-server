//! Whether the service runs in its hosting platform's staging environment.
//!
//! A platform that serves a version under test apart from the live one tells
//! the running service so. Its adapter declares it once, before it serves,
//! and a module that behaves differently under test, such as a test bypass,
//! reads it here rather than reading the platform's own variable.

use std::sync::atomic::{AtomicBool, Ordering};

static STAGING: AtomicBool = AtomicBool::new(false);

/// Declares whether the service runs in its hosting platform's staging
/// environment.
pub fn declare_platform_staging(staging: bool) {
    STAGING.store(staging, Ordering::Relaxed);
}

/// Whether the adapter declared that the service runs in its hosting
/// platform's staging environment. False until it does.
#[must_use]
pub fn platform_is_staging() -> bool {
    STAGING.load(Ordering::Relaxed)
}
