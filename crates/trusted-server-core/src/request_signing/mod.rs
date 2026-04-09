//! Request signing utilities for secure communication.
//!
//! This module provides cryptographic signing capabilities using Ed25519 keys,
//! including JWKS management, key rotation, and signature verification.
//!
//! # Store names vs store IDs
//!
//! Platform stores have two identifiers:
//!
//! - **Store name** ([`JWKS_CONFIG_STORE_NAME`], [`SIGNING_SECRET_STORE_NAME`]):
//!   used for runtime reads via [`crate::platform::PlatformConfigStore::get`]
//!   and [`crate::platform::PlatformSecretStore::get_bytes`] through
//!   [`crate::platform::RuntimeServices`]. These names are configured in
//!   `fastly.toml` for the Fastly adapter.
//!
//! - **Store ID**: used for write operations via
//!   [`crate::platform::PlatformConfigStore::put`] /
//!   [`crate::platform::PlatformConfigStore::delete`] and
//!   [`crate::platform::PlatformSecretStore::create`] /
//!   [`crate::platform::PlatformSecretStore::delete`]. These identifiers come
//!   from the request-signing settings in `trusted-server.toml`.

use std::sync::LazyLock;

use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::platform::{RuntimeServices, StoreName};

pub mod discovery;
pub mod endpoints;
pub mod jwks;
pub mod rotation;
pub mod signing;

/// Config store name for JWKS public keys used by runtime read operations.
///
/// This must match the store name declared in `fastly.toml` under
/// `[local_server.config_stores]`.
pub const JWKS_CONFIG_STORE_NAME: &str = "jwks_store";

/// Secret store name for Ed25519 signing keys used by runtime read operations.
///
/// This must match the store name declared in `fastly.toml` under
/// `[local_server.secret_stores]`.
pub const SIGNING_SECRET_STORE_NAME: &str = "signing_keys";

/// Lazily constructed [`StoreName`] for JWKS config-store reads.
pub static JWKS_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(JWKS_CONFIG_STORE_NAME));

/// Lazily constructed [`StoreName`] for signing-key secret-store reads.
pub static SIGNING_STORE_NAME: LazyLock<StoreName> =
    LazyLock::new(|| StoreName::from(SIGNING_SECRET_STORE_NAME));

fn parse_active_kids(active_kids: &str) -> Vec<String> {
    active_kids
        .split(',')
        .map(|kid| kid.trim().to_string())
        .filter(|kid| !kid.is_empty())
        .collect()
}

fn read_active_kids(services: &RuntimeServices) -> Result<Vec<String>, Report<TrustedServerError>> {
    services
        .config_store()
        .get(&JWKS_STORE_NAME, "active-kids")
        .change_context(TrustedServerError::Configuration {
            message: "failed to read active-kids from config store".into(),
        })
        .attach("while fetching active kids list")
        .map(|active_kids| parse_active_kids(&active_kids))
}

pub use discovery::*;
pub use endpoints::*;
pub use jwks::*;
pub use rotation::*;
pub use signing::*;
