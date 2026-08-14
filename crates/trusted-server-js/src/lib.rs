#![allow(
    clippy::pub_use,
    reason = "crate root intentionally re-exports the small public bundle API"
)]

pub mod bundle;

pub use bundle::{
    MAX_CRITICAL_MODULES, MAX_MANIFEST_MODULES, TsjsArtifactMetadata, TsjsArtifactRole,
    TsjsModulePhase, all_artifact_metadata, all_first_display_ids, all_first_display_metadata,
    all_integration_ids, all_integration_metadata, all_module_ids,
    concatenate_first_display_slices, concatenate_modules, concatenated_first_display_hash,
    concatenated_hash, first_display_component_bundle, gpt_bootstrap_fallback_bundle,
    integration_metadata, module_bundle, release_id, single_module_hash,
};
