#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;
pub mod trace_assets;

pub use bundle::{
    all_module_ids, concatenate_modules, concatenated_hash, module_bundle, single_module_hash,
};
