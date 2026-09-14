//! Embedded, content-addressed Trusted Server browser bundles.
//!
//! The build script compiles each source module and the unified bundle. This
//! crate exposes their bytes and hashes without requiring filesystem access at
//! runtime.

#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;

pub use bundle::{
    all_module_ids, concatenate_modules, concatenated_hash, module_bundle, single_module_hash,
};
