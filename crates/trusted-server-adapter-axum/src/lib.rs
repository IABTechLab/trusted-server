//! Native Axum adapter with strict prepared-store startup.
//!
//! Runtime settings and immutable store snapshots are validated before the RC
//! listener becomes ready. Optional reader geography comes only from the
//! managed native host's protected, immutable request outcome.

/// Application routing and handler registration for the Axum dev server.
pub mod app;
#[cfg(all(feature = "celld-ec", not(target_arch = "wasm32")))]
pub mod celld_ec;
/// Native live DynamoDB conditional identity storage and provider selection.
pub mod ec_kv;
/// Prepared native KV bridge.
#[cfg(feature = "celld-kv")]
pub mod kv;
#[cfg(feature = "lab-store-check")]
mod lab_public_origin;
/// Existing-admin-protected lab storage diagnostic.
#[cfg(feature = "lab-store-check")]
pub mod lab_store_check;
/// Request middleware (auth, response finalisation).
pub mod middleware;
/// Explicit native Geo boot profile and provider composition.
pub mod native_geo;
/// Managed outbound HTTP compatibility bridge.
pub mod outbound;
/// Native platform services and request-scoped transport/ingress bridges.
pub mod platform;
/// Prepared-store initialization and immutable native snapshots.
pub mod startup;
