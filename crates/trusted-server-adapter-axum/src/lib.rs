//! Native Axum adapter for local Trusted Server development.
//!
//! Runs a full Axum HTTP server on `localhost` as a drop-in dev alternative to
//! the Fastly Compute adapter (via Viceroy). All routes and middleware mirror
//! the Fastly adapter; store and geo primitives fall back to env vars and no-ops.

/// Application routing and handler registration for the Axum dev server.
pub mod app;
/// Request middleware (auth, response finalisation).
pub mod middleware;
/// Platform-trait implementations backed by env vars and `reqwest`.
pub mod platform;
/// Per-request store registry builders for the dev server.
pub mod registries;
