//! Composition of the served tsjs script from module parts.
//!
//! `trusted-server-js` knows only the modules compiled into it. A module a
//! vendor crate carries on its registration is not in that map, so the
//! composition of the served script and its cache-busting hash live here,
//! keyed on content rather than on ids. The byte rule is unchanged from
//! `trusted_server_js::concatenate_modules`: core first, then each part in
//! order, joined by `;\n`, so every existing `?v=` hash is preserved.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use sha2::{Digest as _, Sha256};

/// One module of the served script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsModulePart {
    /// Module id, for example `core` or `lockr`.
    pub id: &'static str,
    /// The built IIFE.
    pub source: &'static str,
    /// SHA-256 of `source`, hex encoded. Identifies the content in the memo.
    ///
    /// The memo in [`compose_hash`] trusts this value rather than hashing
    /// `source` again, so a part that declares the wrong hash serves a stale
    /// `?v=` under a valid-looking URL. Debug builds and tests check the
    /// value against `source`; release builds do not.
    pub sha256: &'static str,
}

impl JsModulePart {
    /// Looks up a compile-time module of `trusted-server-js` by id.
    ///
    /// Returns `None` when no module with that id was compiled in.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::tsjs_bundle::JsModulePart;
    ///
    /// assert!(JsModulePart::compile_time("core").is_some());
    /// assert!(JsModulePart::compile_time("not-a-module").is_none());
    /// ```
    #[must_use]
    pub fn compile_time(id: &'static str) -> Option<Self> {
        let source = trusted_server_js::module_bundle(id)?;
        let sha256 = trusted_server_js::single_module_hash(id)?;
        Some(Self { id, source, sha256 })
    }
}

/// Resolves compile-time parts for a list of ids, dropping unknown ids, as
/// `trusted_server_js` does today.
///
/// # Examples
///
/// ```
/// use trusted_server_core::tsjs_bundle::compile_time_parts;
///
/// let parts = compile_time_parts(&["core", "not-a-module"]);
///
/// assert_eq!(parts.len(), 1);
/// assert_eq!(parts[0].id, "core");
/// ```
#[must_use]
pub fn compile_time_parts(ids: &[&'static str]) -> Vec<JsModulePart> {
    ids.iter()
        .filter_map(|id| JsModulePart::compile_time(id))
        .collect()
}

/// Concatenates the parts into the served script.
///
/// Core comes first (the given `core` part, or the compile-time core when
/// none is given), then the remaining parts in input order. Each id appears
/// once, keeping its first occurrence, and parts are joined by `;\n` so one
/// IIFE cannot run into the next.
///
/// # Examples
///
/// ```
/// use sha2::{Digest as _, Sha256};
/// use trusted_server_core::tsjs_bundle::{JsModulePart, compose};
///
/// fn part(id: &'static str, source: &'static str) -> JsModulePart {
///     let sha256 = Box::leak(hex::encode(Sha256::digest(source)).into_boxed_str());
///     JsModulePart { id, source, sha256 }
/// }
///
/// let parts = [
///     part("example", "(() => { window.example = true; })()"),
///     part("core", "(() => { window.tsjs = {}; })()"),
/// ];
///
/// assert_eq!(
///     compose(&parts),
///     "(() => { window.tsjs = {}; })();\n(() => { window.example = true; })()"
/// );
/// ```
#[must_use]
pub fn compose(parts: &[JsModulePart]) -> String {
    let ordered = ordered(parts);
    // Every piece the visit yields has a known length before the walk, so
    // reserve the exact byte count once rather than letting the pushes grow
    // the buffer. A bundle of a dozen parts is several hundred kilobytes, and
    // growing to that size copies roughly twice the bundle on every request
    // that serves it.
    let mut size = 0;
    visit_parts(&ordered, |part| size += part.len());
    let mut body = String::with_capacity(size);
    visit_parts(&ordered, |part| body.push_str(part));
    body
}

/// SHA-256 of [`compose`]'s output, hex encoded, without materializing it.
///
/// The result is memoized per ordered set of `(id, sha256)` pairs. Because the
/// key carries each part's content hash, a carried module that keeps its id
/// but changes its source gets a new hash. The memo never evicts, so feed it
/// only sets derived from configuration, never sets derived from request
/// input.
///
/// The memo can only hit where the process outlives the request, which of the
/// four adapters means the Axum dev server alone, because its `main` builds
/// the router once before serving. Fastly starts a fresh Wasm instance per
/// request, and `edgezero_adapter_cloudflare::run_app` and
/// `edgezero_adapter_spin::run_app` both call `build_app` inside the
/// per-request entry point, so on those three every call is a miss. A miss
/// costs a key vector, two mutex locks and a stored copy of the hash, all
/// beside a SHA-256 over the whole bundle, so the memo is close to free where
/// it cannot hit and removes the hash entirely where it can.
///
/// # Panics
///
/// In debug builds, panics when a part's `sha256` is not the SHA-256 of its
/// `source`. Release builds trust the declared hash.
///
/// # Examples
///
/// ```
/// use sha2::{Digest as _, Sha256};
/// use trusted_server_core::tsjs_bundle::{JsModulePart, compose_hash};
///
/// fn part(id: &'static str, source: &'static str) -> JsModulePart {
///     let sha256 = Box::leak(hex::encode(Sha256::digest(source)).into_boxed_str());
///     JsModulePart { id, source, sha256 }
/// }
///
/// let core = JsModulePart::compile_time("core").expect("should have compiled core in");
/// let before = [core, part("example", "(() => { window.example = 1; })()")];
/// let after = [core, part("example", "(() => { window.example = 2; })()")];
///
/// assert_eq!(compose_hash(&before).len(), 64);
/// assert_eq!(compose_hash(&before), compose_hash(&before));
/// assert_ne!(compose_hash(&before), compose_hash(&after));
/// ```
#[must_use]
pub fn compose_hash(parts: &[JsModulePart]) -> String {
    for part in parts {
        debug_assert_eq!(
            part.sha256,
            hex::encode(Sha256::digest(part.source)),
            "should declare the SHA-256 of its source for part `{}`",
            part.id
        );
    }

    let ordered = ordered(parts);
    let key = ordered
        .iter()
        .map(|part| (part.id, part.sha256))
        .collect::<Vec<_>>();
    if let Some(hash) = lock_cache().get(&key).cloned() {
        return hash;
    }

    let mut hasher = Sha256::new();
    visit_parts(&ordered, |part| hasher.update(part.as_bytes()));
    let hash = hex::encode(hasher.finalize());
    lock_cache().insert(key, hash.clone());
    hash
}

/// Orders the parts for the served script: core first, then every non-core
/// part in input order, keeping the first occurrence of each id.
fn ordered(parts: &[JsModulePart]) -> Vec<JsModulePart> {
    let mut result = Vec::with_capacity(parts.len() + 1);

    let core = parts
        .iter()
        .find(|part| part.id == "core")
        .copied()
        .or_else(|| JsModulePart::compile_time("core"));
    if let Some(core) = core {
        result.push(core);
    }

    for part in parts {
        if part.id == "core" {
            continue;
        }
        if result.iter().any(|taken| taken.id == part.id) {
            continue;
        }
        result.push(*part);
    }

    result
}

/// Visits the byte pieces of the served script in order, with `;\n` between
/// consecutive parts.
fn visit_parts<F: FnMut(&'static str)>(parts: &[JsModulePart], mut visit: F) {
    let mut first = true;
    for part in parts {
        if first {
            first = false;
        } else {
            visit(";\n");
        }
        visit(part.source);
    }
}

type HashCache = HashMap<Vec<(&'static str, &'static str)>, String>;

fn lock_cache() -> MutexGuard<'static, HashCache> {
    static CACHE: OnceLock<Mutex<HashCache>> = OnceLock::new();
    match CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// Builds a part whose `sha256` really is the hash of `source`, so no two
    /// parts with different content ever share a memo key.
    fn part(id: &'static str, source: &'static str) -> JsModulePart {
        let sha256 = Box::leak(sha256_hex(source.as_bytes()).into_boxed_str());
        JsModulePart { id, source, sha256 }
    }

    #[test]
    fn compose_puts_core_first_and_joins_with_a_semicolon_and_newline() {
        let parts = [
            part("lockr", "L"),
            part("core", "C"),
            part("permutive", "P"),
        ];

        assert_eq!(
            compose(&parts),
            "C;\nL;\nP",
            "should order core first and join parts"
        );
    }

    #[test]
    fn compose_hash_matches_the_compile_time_hash_for_built_in_modules() {
        let ids = ["lockr", "permutive"];
        let parts = compile_time_parts(&ids);

        assert_eq!(
            compose_hash(&parts),
            trusted_server_js::concatenated_hash(&ids),
            "should reproduce today's hash for a built-in module set"
        );
        assert_eq!(
            compose(&parts),
            trusted_server_js::concatenate_modules(&ids),
            "should reproduce today's bytes for a built-in module set"
        );
    }

    #[test]
    fn compose_of_no_parts_is_the_compile_time_core_alone() {
        assert_eq!(
            compose(&[]),
            trusted_server_js::concatenate_modules(&[]),
            "should serve core alone when no parts are given"
        );
        assert_eq!(
            compose_hash(&[]),
            trusted_server_js::concatenated_hash(&[]),
            "should hash core alone when no parts are given"
        );
    }

    #[test]
    fn compose_matches_every_compile_time_module_set_the_old_api_produces() {
        let all = trusted_server_js::all_module_ids();
        let non_core = all
            .iter()
            .copied()
            .filter(|id| *id != "core")
            .collect::<Vec<_>>();
        let mut cases = vec![all.clone(), non_core.clone()];
        cases.push(non_core.iter().rev().copied().collect());
        cases.push(vec!["core"]);

        for ids in cases {
            let parts = compile_time_parts(&ids);
            assert_eq!(
                compose(&parts),
                trusted_server_js::concatenate_modules(&ids),
                "should reproduce today's bytes for {ids:?}"
            );
            assert_eq!(
                compose_hash(&parts),
                trusted_server_js::concatenated_hash(&ids),
                "should reproduce today's hash for {ids:?}"
            );
        }
    }

    #[test]
    fn compose_hash_changes_when_a_carried_module_changes() {
        let before = [part("core", "C"), part("probe", "A")];
        let after = [part("core", "C"), part("probe", "B")];

        assert_ne!(
            compose_hash(&before),
            compose_hash(&after),
            "should hash carried content"
        );
    }

    #[test]
    fn compose_keeps_a_duplicate_id_once_using_its_first_occurrence() {
        let parts = [
            part("core", "C"),
            part("lockr", "L1"),
            part("permutive", "P"),
            part("lockr", "L2"),
            part("core", "C2"),
        ];

        assert_eq!(
            compose(&parts),
            "C;\nL1;\nP",
            "should keep the first occurrence of each id and drop later ones"
        );
    }

    #[test]
    fn compose_reserves_the_exact_bundle_size_before_writing_it() {
        let parts = compile_time_parts(&trusted_server_js::all_module_ids());
        let body = compose(&parts);

        assert_eq!(
            body.capacity(),
            body.len(),
            "should allocate the bundle once at its exact size"
        );
    }

    #[test]
    fn compose_hash_is_the_hex_sha256_of_compose() {
        let parts = [
            part("core", "(() => {})()"),
            part("carried", "(() => { window.carried = true; })()"),
            part("lockr", "L"),
        ];

        assert_eq!(
            compose_hash(&parts),
            sha256_hex(compose(&parts).as_bytes()),
            "should hash the exact bytes compose produces"
        );
    }

    #[test]
    #[should_panic(expected = "should declare the SHA-256 of its source for part `lying`")]
    fn compose_hash_rejects_a_part_whose_declared_hash_is_wrong() {
        let parts = [
            part("core", "C"),
            JsModulePart {
                id: "lying",
                source: "A",
                sha256: "not-the-hash-of-a",
            },
        ];

        let _ = compose_hash(&parts);
    }

    #[test]
    fn compile_time_parts_drops_unknown_ids() {
        let parts = compile_time_parts(&["lockr", "not-a-module", "permutive"]);
        let ids = parts.iter().map(|part| part.id).collect::<Vec<_>>();

        assert_eq!(
            ids,
            ["lockr", "permutive"],
            "should keep known ids in order and drop unknown ids"
        );
        for part in parts {
            assert_eq!(
                part.sha256,
                sha256_hex(part.source.as_bytes()),
                "should carry the compile-time hash of {}",
                part.id
            );
        }
    }
}
