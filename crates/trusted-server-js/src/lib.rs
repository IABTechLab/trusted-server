#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;

pub use bundle::{
    MAX_CRITICAL_MODULES, MAX_MANIFEST_MODULES, TsjsArtifactMetadata, TsjsArtifactRole,
    TsjsModulePhase, all_artifact_metadata, all_integration_ids, all_integration_metadata,
    all_module_ids, concatenate_modules, concatenated_hash, gpt_bootstrap_fallback_bundle,
    integration_metadata, module_bundle, release_id, single_module_hash,
};
