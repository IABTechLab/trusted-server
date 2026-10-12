//! Immutable, independently embedded assets for the full-document trace page.

/// Verified asset bytes and their byte-derived SHA-256 digest.
///
/// Publisher integration bundle discovery does not include these assets.
#[derive(Clone, Copy, Debug)]
pub struct TraceAsset {
    /// Exact pathname of the versioned asset.
    pub path: &'static str,
    /// Immutable bytes verified against the committed manifest during the build.
    pub bytes: &'static [u8],
    /// Lowercase hexadecimal SHA-256 digest of [`Self::bytes`].
    pub sha256: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/trace_assets.rs"));

/// Look up an asset only by its exact versioned pathname.
///
/// # Examples
///
/// ```
/// use trusted_server_js::trace_assets::trace_asset;
/// assert!(trace_asset("/_ts/trace/assets/v1.js").is_some());
/// assert!(trace_asset("/_ts/trace/assets/v1.js?data=example").is_none());
/// ```
#[must_use]
pub fn trace_asset(path: &str) -> Option<TraceAsset> {
    TRACE_ASSETS
        .iter()
        .find(|asset| asset.path == path)
        .copied()
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    use super::*;

    #[test]
    fn exact_trace_asset_lookup_matches_verified_bytes() {
        for path in ["/_ts/trace/assets/v1.js", "/_ts/trace/assets/v1.css"] {
            let asset = trace_asset(path).expect("should embed the fixed versioned trace asset");
            assert_eq!(
                asset.path, path,
                "should retain the exact public asset path"
            );
            assert_eq!(
                hex::encode(Sha256::digest(asset.bytes)),
                asset.sha256,
                "should embed only bytes matching the manifest digest"
            );
        }
        for path in [
            "/_ts/trace/assets/v2.js",
            "/_ts/trace/assets/v1.js?data=example",
            "/_ts/trace/assets/../v1.js",
        ] {
            assert!(
                trace_asset(path).is_none(),
                "should reject every unregistered asset spelling"
            );
        }
        assert!(
            !crate::all_module_ids().contains(&"trace"),
            "should keep trace assets outside the publisher integration pipeline"
        );
    }
}
