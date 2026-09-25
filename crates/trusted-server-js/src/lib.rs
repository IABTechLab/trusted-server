#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;

// Build-script helpers, compiled here only so their unit tests run with the crate's.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "only the pure helpers are exercised by unit tests"
)]
#[path = "../build/bundle_set.rs"]
mod bundle_set;

pub use bundle::{
    all_module_ids, concatenate_modules, concatenated_hash, module_bundle, single_module_hash,
};
