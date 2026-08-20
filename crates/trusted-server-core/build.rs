use std::env;
use std::path::PathBuf;

use edgezero_core::manifest::ManifestLoader;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Keep every adapter's compiled default synchronized with the repository manifest.
    let manifest_path = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("should receive CARGO_MANIFEST_DIR from Cargo"),
    )
    .join("../..")
    .join("edgezero.toml");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest = match ManifestLoader::from_path(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            println!(
                "cargo::error=should load EdgeZero manifest at {}: {error}",
                manifest_path.display()
            );
            std::process::exit(1);
        }
    };
    let Some(config_store) = manifest.manifest().stores.config.as_ref() else {
        println!(
            "cargo::error=should declare [stores.config] in EdgeZero manifest at {}",
            manifest_path.display()
        );
        std::process::exit(1);
    };
    let default_store_id = config_store.default_id();
    println!("cargo:rustc-env=TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID={default_store_id}");
}
