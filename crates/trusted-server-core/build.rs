#![allow(
    clippy::print_stdout,
    reason = "Cargo build scripts communicate rebuild inputs on stdout"
)]

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use edgezero_core::manifest::ManifestLoader;
use sha2::{Digest as _, Sha256};

/// Crate-relative build inputs that are both watched and hashed besides `src/`.
const CRATE_INPUTS: [&str; 2] = ["build.rs", "Cargo.toml"];
/// Workspace-relative inputs hashed under a `workspace/` logical prefix.
const WORKSPACE_INPUTS: [&str; 3] = ["Cargo.toml", "Cargo.lock", "edgezero.toml"];

fn main() {
    // Watching the directory also catches newly added and removed source files.
    println!("cargo:rerun-if-changed=src");
    for input in CRATE_INPUTS {
        println!("cargo:rerun-if-changed={input}");
    }
    for input in WORKSPACE_INPUTS {
        println!("cargo:rerun-if-changed=../../{input}");
    }

    let crate_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("should set the core crate directory"),
    );
    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("should set the build output directory"));
    let digest = template_build_digest(&crate_dir);
    fs::write(
        out_dir.join("template_build_digest.rs"),
        format!("const TEMPLATE_BUILD_DIGEST: &str = \"{digest}\";\n"),
    )
    .expect("should write the template build digest");

    // Keep every adapter's compiled default synchronized with the repository manifest.
    let manifest_path = crate_dir
        .ancestors()
        .nth(2)
        .expect("should resolve the workspace root from CARGO_MANIFEST_DIR")
        .join("edgezero.toml");
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

/// Hash the core implementation and its dependency resolution without checkout paths.
///
/// This private workspace crate lives two levels below the workspace manifest.
/// All non-hidden source files are included, including embedded JS and future asset types.
/// Unrelated edits intentionally invalidate templates rather than risk stale code.
///
/// # Panics
///
/// Panics if a required input cannot be read or source paths are not UTF-8 regular
/// files/directories. Only an absent workspace lockfile is optional.
pub(crate) fn template_build_digest(crate_dir: &Path) -> String {
    let mut sources = Vec::new();
    collect_sources(crate_dir, Path::new("src"), &mut sources);
    sources.extend(CRATE_INPUTS.map(PathBuf::from));
    let mut inputs: Vec<_> = sources
        .into_iter()
        .map(|path| {
            let logical_path = path
                .components()
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .expect("should have UTF-8 source paths")
                })
                .collect::<Vec<_>>()
                .join("/");
            let bytes = fs::read(crate_dir.join(path)).expect("should read template build input");
            (logical_path, bytes)
        })
        .collect();
    for input in WORKSPACE_INPUTS {
        let bytes = match fs::read(crate_dir.join("../..").join(input)) {
            Err(error) if input == "Cargo.lock" && error.kind() == ErrorKind::NotFound => {
                continue;
            }
            result => result.expect("should read template build input"),
        };
        inputs.push((format!("workspace/{input}"), bytes));
    }
    inputs.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    for (path, bytes) in inputs {
        // Fixed-width lengths frame both fields without delimiter ambiguity.
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hex::encode(hasher.finalize())
}

fn collect_sources(crate_dir: &Path, relative: &Path, sources: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(crate_dir.join(relative)).expect("should read core source directory");
    for entry in entries {
        let entry = entry.expect("should read core source entry");
        let name = entry.file_name();
        let bytes = name.as_encoded_bytes();
        // Editor lock symlinks, swap/auto-save/backup files, and OS metadata are
        // not compiled sources.
        if bytes.starts_with(b".") || bytes.starts_with(b"#") || bytes.ends_with(b"~") {
            continue;
        }
        let path = relative.join(name);
        let kind = entry
            .file_type()
            .expect("should read core source file type");
        if kind.is_dir() {
            collect_sources(crate_dir, &path, sources);
        } else {
            assert!(kind.is_file(), "should use regular files for core sources");
            sources.push(path);
        }
    }
}
