//! What the 51Degrees modules share.
//!
//! Each 51Degrees module lives in its own crate under its type. What more
//! than one of them needs lives here, being their shared table, the one call
//! to the cloud service and its held answer, the evidence the call sends and
//! the cache the answer is held in, so none depends on another and none of
//! it is in core.

/// The id the 51Degrees builder and its registration carry. No configuration
/// names it, because `[shared.51degrees]` runs the builder and each module is
/// selected by its crate folder.
pub const BUILDER_ID: &str = "51degrees";

/// Folded into the dynamic backend name, so these crates' calls are not
/// confused with another caller's to the same host.
pub const BACKEND_DISCRIMINATOR: &str = BUILDER_ID;

/// The value the service uses for a property it could not determine.
pub const UNKNOWN: &str = "Unknown";

pub mod client;
pub mod config;
pub mod engine_cache;
pub mod evidence;
pub mod hints_engine;
pub mod license;
/// Runtime services for the 51Degrees crates' tests, behind a feature so
/// nothing outside a test can reach them.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_services;
