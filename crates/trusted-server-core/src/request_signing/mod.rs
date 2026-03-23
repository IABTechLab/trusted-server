//! Request signing utilities for secure communication.
//!
//! This module provides cryptographic signing capabilities using Ed25519 keys,
//! including JWKS management, key rotation, and signature verification.
//!
//! # Store names vs store IDs
//!
//! Fastly stores have two identifiers:
//!
//! - **Store name** ([`JWKS_CONFIG_STORE_NAME`], [`SIGNING_SECRET_STORE_NAME`]):
//!   used at the edge for reads via `ConfigStore::open` / `SecretStore::open`.
//!   These are configured in `fastly.toml`.
//!
//! - **Store ID** (`RequestSigning::config_store_id`, `RequestSigning::secret_store_id`):
//!   used by the Fastly management API for writes (creating, updating, and
//!   deleting items). These are set in `trusted-server.toml`.

pub mod discovery;
pub mod endpoints;
pub mod jwks;
pub mod rotation;
pub mod signing;

/// Config store name for JWKS public keys (edge reads via `ConfigStore::open`).
///
/// This must match the store name declared in `fastly.toml` under
/// `[local_server.config_stores]`.
pub const JWKS_CONFIG_STORE_NAME: &str = "jwks_store";

/// Secret store name for Ed25519 signing keys (edge reads via `SecretStore::open`).
///
/// This must match the store name declared in `fastly.toml` under
/// `[local_server.secret_stores]`.
pub const SIGNING_SECRET_STORE_NAME: &str = "signing_keys";

pub use discovery::*;
pub use endpoints::*;
pub use jwks::*;
pub use rotation::*;
pub use signing::*;
