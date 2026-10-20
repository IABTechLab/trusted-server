//! Native Axum adapter with strict prepared-store startup.
//!
//! Runtime settings and immutable store snapshots are validated before the RC
//! listener becomes ready. Reader geography remains unavailable on native.

/// Application routing and handler registration for the Axum dev server.
pub mod app;
/// Request middleware (auth, response finalisation).
pub mod middleware;
/// Managed outbound HTTP compatibility bridge.
pub mod outbound;
/// Native platform services and request-scoped transport/ingress bridges.
pub mod platform;
/// Prepared-store initialization and immutable native snapshots.
pub mod startup;
