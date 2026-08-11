use std::collections::HashMap;
use std::sync::OnceLock;

use hex::encode;
use sha2::{Digest as _, Sha256};

include!(concat!(env!("OUT_DIR"), "/tsjs_modules.rs"));

/// Release artifact role recorded in the generated inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TsjsArtifactRole {
    /// Inline minimal bootstrap controller and fallback artifact.
    Bootstrap,
    /// Sole TSJS kernel artifact.
    Core,
    /// Catalogued critical or deferred integration module.
    Integration,
}

/// Fixed catalog phase for one integration module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TsjsModulePhase {
    /// Parser-blocking server-composed first-display module.
    Critical,
    /// Authenticated module loaded only after the protected phase gate.
    Deferred,
}

/// Immutable generated artifact metadata shared with the server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TsjsArtifactMetadata {
    /// Canonical artifact identifier.
    pub id: &'static str,
    /// Artifact release role.
    pub role: TsjsArtifactRole,
    /// Catalog phase for integration artifacts.
    pub phase: Option<TsjsModulePhase>,
    /// Fixed deferred trigger, when applicable.
    pub trigger: Option<&'static str>,
    /// Declared consumed capability edges.
    pub inputs: &'static [&'static str],
    /// Server-owned inclusion predicate from the canonical catalog.
    pub include: Option<&'static str>,
    /// Declared provided capability keys.
    pub outputs: &'static [&'static str],
    /// Generated artifact filename.
    pub file: &'static str,
    /// SHA-256 over exact uncompressed response bytes.
    pub hash: &'static str,
}

/// Maximum catalogued critical modules.
pub const MAX_CRITICAL_MODULES: usize = GENERATED_MAX_CRITICAL_MODULES;
/// Maximum integrations in one boot manifest.
pub const MAX_MANIFEST_MODULES: usize = GENERATED_MAX_MANIFEST_MODULES;
/// Return the sentinel-normalized release identifier shared by every bundle.
#[must_use]
#[inline]
pub const fn release_id() -> &'static str {
    TSJS_RELEASE_ID
}

/// Return the generated, executable GPT bootstrap fallback proposal.
#[must_use]
#[inline]
pub const fn gpt_bootstrap_fallback_bundle() -> &'static str {
    GPT_BOOTSTRAP_FALLBACK
}

/// Return the JS bundle content for a given module ID (e.g., "core", "prebid").
#[must_use]
#[inline]
pub fn module_bundle(id: &str) -> Option<&'static str> {
    module_map().get(id).copied()
}

/// Return all available module IDs, in discovery order (core first).
#[must_use]
#[inline]
pub fn all_module_ids() -> Vec<&'static str> {
    TSJS_ARTIFACTS
        .iter()
        .filter(|artifact| artifact.role != "bootstrap")
        .map(|artifact| artifact.id)
        .collect()
}

/// Return all catalogued integration IDs in canonical phase/injection order.
#[must_use]
pub fn all_integration_ids() -> Vec<&'static str> {
    TSJS_ARTIFACTS
        .iter()
        .filter(|artifact| artifact.role == "integration")
        .map(|artifact| artifact.id)
        .collect()
}

/// Return generated metadata for bootstrap, core, and every catalog module.
#[must_use]
pub fn all_artifact_metadata() -> Vec<TsjsArtifactMetadata> {
    TSJS_ARTIFACTS.iter().map(public_metadata).collect()
}

/// Return generated metadata for the twenty integration modules.
#[must_use]
pub fn all_integration_metadata() -> Vec<TsjsArtifactMetadata> {
    TSJS_ARTIFACTS
        .iter()
        .filter(|artifact| artifact.role == "integration")
        .map(public_metadata)
        .collect()
}

/// Return generated metadata for a catalogued integration module.
#[must_use]
pub fn integration_metadata(id: &str) -> Option<TsjsArtifactMetadata> {
    TSJS_ARTIFACTS
        .iter()
        .find(|artifact| artifact.role == "integration" && artifact.id == id)
        .map(public_metadata)
}

/// Concatenate core + the requested integration modules into a single JS string.
///
/// Core is always included first regardless of whether it appears in `ids`.
/// Each IIFE is separated by `;\n` for safety.
#[must_use]
#[inline]
pub fn concatenate_modules(ids: &[&str]) -> String {
    let map = module_map();
    let mut parts: Vec<&str> = Vec::new();

    // Core always first
    if let Some(core) = map.get("core") {
        parts.push(core);
    }

    // Then requested modules (excluding core, already included)
    for id in ids {
        if *id == "core" {
            continue;
        }
        if let Some(bundle) = map.get(id) {
            parts.push(bundle);
        }
    }

    parts.join(";\n")
}

/// SHA-256 hash of the concatenated modules, for cache-busting URLs.
#[must_use]
#[inline]
pub fn concatenated_hash(ids: &[&str]) -> String {
    let body = concatenate_modules(ids);
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    encode(hasher.finalize())
}

/// SHA-256 hash of a single module's content (without prepending core).
///
/// Used for cache-busting URLs of deferred modules served individually.
#[must_use]
#[inline]
pub fn single_module_hash(id: &str) -> Option<String> {
    TSJS_ARTIFACTS
        .iter()
        .find(|artifact| artifact.role == "integration" && artifact.id == id)
        .map(|artifact| artifact.hash.to_owned())
}

fn module_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        TSJS_ARTIFACTS
            .iter()
            .filter(|artifact| artifact.role != "bootstrap")
            .map(|artifact| (artifact.id, artifact.bundle))
            .collect()
    })
}

fn public_metadata(artifact: &TsjsGeneratedArtifactMeta) -> TsjsArtifactMetadata {
    TsjsArtifactMetadata {
        id: artifact.id,
        role: match artifact.role {
            "bootstrap" => TsjsArtifactRole::Bootstrap,
            "core" => TsjsArtifactRole::Core,
            "integration" => TsjsArtifactRole::Integration,
            _ => unreachable!("generated artifact role should be validated"),
        },
        phase: artifact.phase.map(|phase| match phase {
            "critical" => TsjsModulePhase::Critical,
            "deferred" => TsjsModulePhase::Deferred,
            _ => unreachable!("generated artifact phase should be validated"),
        }),
        trigger: artifact.trigger,
        include: artifact.include,
        inputs: artifact.inputs,
        outputs: artifact.outputs,
        file: artifact.file,
        hash: artifact.hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_catalog_metadata_has_exact_phase_order_and_derived_capacities() {
        let metadata = all_integration_metadata();
        let generated = include_str!(concat!(env!("OUT_DIR"), "/tsjs_modules.rs"));

        assert_eq!(metadata.len(), 20, "should embed all catalog modules");
        assert_eq!(MAX_CRITICAL_MODULES, 14);
        assert_eq!(MAX_MANIFEST_MODULES, 20);
        assert!(
            !generated.contains("INTERNAL_DIAGNOSTICS_SUBSCRIPTIONS"),
            "the synchronous diagnostics ingress must not generate subscription capacity"
        );
        assert_eq!(metadata[0].id, "render_runtime");
        assert_eq!(metadata[0].phase, Some(TsjsModulePhase::Critical));
        assert_eq!(metadata[13].id, "testlight");
        assert_eq!(metadata[13].phase, Some(TsjsModulePhase::Critical));
        assert_eq!(metadata[14].id, "diagnostics_presentation");
        assert_eq!(metadata[14].phase, Some(TsjsModulePhase::Deferred));
        assert_eq!(metadata[19].id, "sourcepoint_lifecycle");
        assert_eq!(metadata[19].trigger, Some("first_display_or_idle"));
        assert_eq!(
            metadata[0].outputs,
            &[
                "slots.v1",
                "auction.v1",
                "render.v1",
                "messages.v1",
                "trace.v1",
                "trace.presentation.v1",
                "direct.v1"
            ]
        );
    }

    #[test]
    fn generated_artifact_inventory_includes_bootstrap_core_and_catalog_once() {
        let artifacts = all_artifact_metadata();

        assert_eq!(artifacts.len(), 22);
        assert_eq!(artifacts[0].id, "bootstrap");
        assert_eq!(artifacts[0].role, TsjsArtifactRole::Bootstrap);
        assert_eq!(artifacts[1].id, "core");
        assert_eq!(artifacts[1].role, TsjsArtifactRole::Core);
        assert!(
            artifacts[2..]
                .iter()
                .all(|artifact| artifact.role == TsjsArtifactRole::Integration)
        );
    }
}
