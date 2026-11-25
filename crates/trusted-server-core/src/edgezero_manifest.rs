//! The repository's `edgezero.toml`, baked into the binary for `EdgeZero`'s `Hooks::manifest`.
//!
//! `EdgeZero`'s `app!` macro bakes the manifest for applications it generates. Trusted Server's
//! adapters implement `Hooks` by hand, and the trait's default reports no manifest, which leaves
//! the outbound client-identity policy empty. The core build script loads the same manifest it
//! already reads for the default config store and writes its JSON form, which this module turns
//! back into a [`BakedManifest`] once per process.

use std::sync::OnceLock;

use edgezero_core::manifest::{BakedManifest, Manifest};

/// JSON form of `edgezero.toml`, as `EdgeZero`'s `app!` macro bakes it.
const MANIFEST_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/edgezero-manifest.json"));

/// Returns the repository manifest baked at build time.
///
/// The JSON is parsed and validated on the first call and cached for the life of the process.
/// A manifest the build script could not serialize fails the build, so this returns
/// [`BakedManifest::Present`] for every binary that compiles.
///
/// # Examples
///
/// ```
/// use edgezero_core::manifest::BakedManifest;
/// use trusted_server_core::edgezero_manifest::baked_edgezero_manifest;
///
/// assert!(matches!(baked_edgezero_manifest(), BakedManifest::Present(_)));
/// ```
#[must_use]
pub fn baked_edgezero_manifest() -> BakedManifest {
    static MANIFEST: OnceLock<BakedManifest> = OnceLock::new();
    *MANIFEST.get_or_init(|| Manifest::from_baked_json(MANIFEST_JSON))
}

#[cfg(test)]
mod tests {
    use edgezero_core::manifest::Capability;
    use edgezero_core::outbound::{ClientIdentityName, ClientIdentityPolicy};

    use super::*;
    use crate::auction::plan::MtlsSlot;

    #[test]
    fn baked_manifest_declares_every_auction_client_identity_slot() {
        let BakedManifest::Present(manifest) = baked_edgezero_manifest() else {
            panic!("should bake the repository manifest");
        };
        let policy = ClientIdentityPolicy::from_manifest(manifest);

        for slot in MtlsSlot::ALL {
            let name = ClientIdentityName::new(slot.identity_name())
                .expect("should accept the auction identity name");
            assert!(
                policy.identity(&name).is_some(),
                "should declare the {} identity",
                slot.identity_name()
            );
        }
        assert!(
            manifest
                .capabilities
                .optional
                .contains(&Capability::OutboundClientIdentity),
            "should list outbound-client-identity as optional"
        );
    }
}
