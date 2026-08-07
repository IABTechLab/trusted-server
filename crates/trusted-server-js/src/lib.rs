#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;

pub use bundle::{
    all_module_ids, concatenate_modules, concatenated_hash, gpt_bootstrap_fallback_bundle,
    module_bundle, release_id, single_module_hash,
};
