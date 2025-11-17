use hex::encode;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/bundle_manifest.rs"));

static BUNDLE_MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn bundles() -> &'static HashMap<&'static str, &'static str> {
    BUNDLE_MAP.get_or_init(|| BUNDLES.iter().copied().collect())
}

pub fn bundle_for_filename(name: &str) -> Option<&'static str> {
    bundles().get(name).copied()
}

pub fn bundle_hash(filename: &str) -> Option<String> {
    bundle_for_filename(filename).map(hash_bundle)
}

fn hash_bundle(bundle: &'static str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bundle.as_bytes());
    encode(hasher.finalize())
}
