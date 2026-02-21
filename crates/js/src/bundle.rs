use std::collections::HashMap;
use std::sync::OnceLock;

use hex::encode;
use sha2::{Digest, Sha256};

include!(concat!(env!("OUT_DIR"), "/tsjs_modules.rs"));

/// Return the JS bundle content for a given module ID (e.g., "core", "prebid").
#[must_use]
pub fn module_bundle(id: &str) -> Option<&'static str> {
    module_map().get(id).copied()
}

/// Return all available module IDs, in discovery order (core first).
#[must_use]
pub fn all_module_ids() -> Vec<&'static str> {
    TSJS_MODULES.iter().map(|m| m.id).collect()
}

/// Concatenate core + the requested integration modules into a single JS string.
///
/// Core is always included first regardless of whether it appears in `ids`.
/// Each IIFE is separated by `;\n` for safety.
#[must_use]
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
pub fn concatenated_hash(ids: &[&str]) -> String {
    let body = concatenate_modules(ids);
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    encode(hasher.finalize())
}

fn module_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| TSJS_MODULES.iter().map(|m| (m.id, m.bundle)).collect())
}
