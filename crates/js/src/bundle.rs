use super::macros::{count_variants, define_tsjs_bundles};
use hex::encode;
use sha2::{Digest, Sha256};

#[derive(Copy, Clone)]
struct TsjsMeta {
    filename: &'static str,
    bundle: &'static str,
}

impl TsjsMeta {
    const fn new(filename: &'static str, bundle: &'static str) -> Self {
        Self { filename, bundle }
    }
}

define_tsjs_bundles!(
    Core => "tsjs-core.js",
    Ext => "tsjs-ext.js",
    Creative => "tsjs-creative.js",
);

pub fn bundle_hash(bundle: TsjsBundle) -> String {
    hash_bundle(bundle.bundle())
}

pub fn bundle_for_filename(name: &str) -> Option<&'static str> {
    TsjsBundle::from_filename(name).map(|bundle| bundle.bundle())
}

fn hash_bundle(bundle: &'static str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bundle.as_bytes());
    encode(hasher.finalize())
}
