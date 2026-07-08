use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use hex::encode;
use sha2::{Digest as _, Sha256};

include!(concat!(env!("OUT_DIR"), "/tsjs_modules.rs"));

/// Return the JS bundle content for a given module ID (e.g., "core", "prebid").
#[must_use]
#[inline]
pub fn module_bundle(id: &str) -> Option<&'static str> {
    module_meta_map().get(id).map(|module| module.bundle)
}

/// Return all available module IDs, in discovery order (core first).
#[must_use]
#[inline]
pub fn all_module_ids() -> Vec<&'static str> {
    TSJS_MODULES.iter().map(|module| module.id).collect()
}

/// Concatenate core + the requested integration modules into a single JS string.
///
/// Core is always included first regardless of whether it appears in `ids`.
/// Each IIFE is separated by `;\n` for safety.
#[must_use]
#[inline]
pub fn concatenate_modules(ids: &[&str]) -> String {
    let ordered_ids = concatenated_module_ids(ids);
    let mut body = String::new();
    visit_concatenated_module_parts(&ordered_ids, |part| body.push_str(part));
    body
}

/// SHA-256 hash of the concatenated modules, for cache-busting URLs.
///
/// The hash is computed over the same byte sequence as [`concatenate_modules`]
/// without allocating that concatenated body. Results are cached by ordered
/// module ID list so HTML injection does not re-hash the full JS payload on
/// every page view.
#[must_use]
#[inline]
pub fn concatenated_hash(ids: &[&str]) -> String {
    let key = concatenated_module_ids(ids);
    if let Some(hash) = lock_concatenated_hash_cache().get(&key).cloned() {
        return hash;
    }

    let hash = hash_concatenated_modules(&key);
    lock_concatenated_hash_cache().insert(key, hash.clone());
    hash
}

/// SHA-256 hash of a single module's content (without prepending core).
///
/// Used for cache-busting URLs of deferred modules served individually.
#[must_use]
#[inline]
pub fn single_module_hash(id: &str) -> Option<&'static str> {
    module_meta_map().get(id).map(|module| module.sha256)
}

fn concatenated_module_ids(ids: &[&str]) -> Vec<&'static str> {
    let map = module_meta_map();
    let mut ordered = Vec::new();

    if let Some(core) = map.get("core") {
        ordered.push(core.id);
    }

    for id in ids {
        if *id == "core" {
            continue;
        }
        if let Some(module) = map.get(*id) {
            ordered.push(module.id);
        }
    }

    ordered
}

fn hash_concatenated_modules(ids: &[&'static str]) -> String {
    let mut hasher = Sha256::new();
    visit_concatenated_module_parts(ids, |part| hasher.update(part.as_bytes()));
    encode(hasher.finalize())
}

fn visit_concatenated_module_parts<F>(ids: &[&'static str], mut visit: F)
where
    F: FnMut(&'static str),
{
    let map = module_meta_map();
    let mut first = true;

    for id in ids {
        let Some(module) = map.get(*id) else {
            continue;
        };
        if first {
            first = false;
        } else {
            visit(";\n");
        }
        visit(module.bundle);
    }
}

fn module_meta_map() -> &'static HashMap<&'static str, &'static TsjsModuleMeta> {
    static MAP: OnceLock<HashMap<&'static str, &'static TsjsModuleMeta>> = OnceLock::new();
    MAP.get_or_init(|| {
        TSJS_MODULES
            .iter()
            .map(|module| (module.id, module))
            .collect()
    })
}

fn lock_concatenated_hash_cache() -> MutexGuard<'static, HashMap<Vec<&'static str>, String>> {
    match concatenated_hash_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn concatenated_hash_cache() -> &'static Mutex<HashMap<Vec<&'static str>, String>> {
    static CACHE: OnceLock<Mutex<HashMap<Vec<&'static str>, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex(bytes: &[u8]) -> String {
        encode(Sha256::digest(bytes))
    }

    #[test]
    fn generated_single_module_hashes_match_bundle_contents() {
        for id in all_module_ids() {
            let bundle = module_bundle(id).expect("should have module bundle");
            let generated_hash = single_module_hash(id).expect("should have generated hash");

            assert_eq!(
                generated_hash,
                sha256_hex(bundle.as_bytes()),
                "generated hash for module {id} should match included bundle bytes"
            );
        }
    }

    #[test]
    fn concatenated_hash_matches_concatenated_bundle_contents() {
        let available_ids = all_module_ids();
        let non_core_ids = available_ids
            .iter()
            .copied()
            .filter(|id| *id != "core")
            .take(3)
            .collect::<Vec<_>>();

        let mut cases: Vec<Vec<&str>> = vec![Vec::new()];
        if let Some(first) = non_core_ids.first().copied() {
            cases.push(vec![first]);
        }
        if non_core_ids.len() >= 2 {
            cases.push(non_core_ids[..2].to_vec());
            cases.push(non_core_ids[..2].iter().rev().copied().collect());
        }
        if non_core_ids.len() >= 3 {
            cases.push(non_core_ids[..3].to_vec());
        }

        for ids in cases {
            let concatenated = concatenate_modules(&ids);
            assert_eq!(
                concatenated_hash(&ids),
                sha256_hex(concatenated.as_bytes()),
                "concatenated hash should match concatenated bundle bytes for {ids:?}"
            );
        }
    }
}
