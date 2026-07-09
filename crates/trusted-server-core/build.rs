use std::env;
use std::path::PathBuf;

use edgezero_core::manifest::ManifestLoader;

fn main() {
    let manifest_path = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("should receive CARGO_MANIFEST_DIR from Cargo"),
    )
    .join("../..")
    .join("edgezero.toml");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest = ManifestLoader::from_path(&manifest_path)
        .expect("should load the repository EdgeZero manifest");
    let config_store = manifest
        .manifest()
        .stores
        .config
        .as_ref()
        .expect("should declare [stores.config] in edgezero.toml");
    let default_store_id = config_store.default_id();
    println!("cargo:rustc-env=TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID={default_store_id}");
}
