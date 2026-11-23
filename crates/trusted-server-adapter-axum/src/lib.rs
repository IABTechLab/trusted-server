//! Native Axum adapter with strict prepared-store startup.
//!
//! Runtime settings and immutable config/secret snapshots are validated before
//! the framework listener becomes ready. Optional geography comes from the
//! managed host's protected, immutable request outcome.

// Unit tests may buffer bodies with `Body::into_bytes`; production code must not.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

/// Application routing and handler registration.
pub mod app;
/// Request middleware for authentication and response finalization.
pub mod middleware;
/// Explicit native Geo boot profile and provider composition.
pub mod native_geo;
/// Native request-scoped platform services.
pub mod platform;
/// Prepared-store initialization and immutable native snapshots.
pub mod startup;
