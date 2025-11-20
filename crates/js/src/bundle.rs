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

const TSJS_BUNDLE_COUNT: usize = 1;

#[repr(usize)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TsjsBundle {
    Unified,
}

const METAS: [TsjsMeta; TSJS_BUNDLE_COUNT] = [
    TsjsMeta::new("tsjs-unified.js", include_str!(concat!(env!("OUT_DIR"), "/tsjs-unified.js"))),
];

const ALL_BUNDLES: [TsjsBundle; TSJS_BUNDLE_COUNT] = [
    TsjsBundle::Unified,
];

impl TsjsBundle {
    pub const COUNT: usize = TSJS_BUNDLE_COUNT;

    pub const fn filename(self) -> &'static str {
        METAS[self as usize].filename
    }

    pub fn minified_filename(self) -> String {
        let base = self.filename();
        match base.strip_suffix(".js") {
            Some(stem) => format!("{stem}.min.js"),
            None => format!("{base}.min.js"),
        }
    }

    pub(crate) const fn bundle(self) -> &'static str {
        METAS[self as usize].bundle
    }

    pub(crate) fn filename_map() -> &'static std::collections::HashMap<&'static str, TsjsBundle> {
        static MAP: std::sync::OnceLock<std::collections::HashMap<&'static str, TsjsBundle>> = std::sync::OnceLock::new();

        MAP.get_or_init(|| {
            ALL_BUNDLES
                .iter()
                .copied()
                .map(|bundle| (bundle.filename(), bundle))
                .collect::<std::collections::HashMap<_, _>>()
        })
    }

    pub fn from_filename(name: &str) -> Option<Self> {
        Self::filename_map().get(name).copied()
    }
}

pub fn bundle_hash(bundle: TsjsBundle) -> String {
    hash_bundle(bundle.bundle())
}

pub fn bundle_for_filename(name: &str) -> Option<&'static str> {
    TsjsBundle::from_filename(name).map(|bundle| bundle.bundle())
}

pub fn bundle_hash_for_filename(name: &str) -> Option<String> {
    TsjsBundle::from_filename(name).map(|bundle| hash_bundle(bundle.bundle()))
}

fn hash_bundle(bundle: &'static str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bundle.as_bytes());
    encode(hasher.finalize())
}
