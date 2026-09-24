#![allow(
    clippy::print_stdout,
    reason = "Cargo build scripts communicate rebuild inputs on stdout"
)]

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

fn main() {
    // Watching the directory also catches newly added and removed source files.
    for input in [
        "src",
        "build.rs",
        "Cargo.toml",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }

    let crate_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("should set the core crate directory"),
    );
    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("should set the build output directory"));
    let digest = template_build_digest(&crate_dir);
    fs::write(
        out_dir.join("template_build_digest.rs"),
        format!("pub(crate) const TEMPLATE_BUILD_DIGEST: &str = \"{digest}\";\n"),
    )
    .expect("should write the template build digest");
}

/// Hash the core implementation and its dependency resolution without checkout paths.
///
/// This private workspace crate lives two levels below the workspace manifest.
/// All source files are included, including embedded JS and future asset types.
/// Unrelated edits intentionally invalidate templates rather than risk stale code.
///
/// # Panics
///
/// Panics if a required input cannot be read or source paths are not UTF-8 regular
/// files/directories. Only an absent workspace lockfile is optional.
pub(crate) fn template_build_digest(crate_dir: &Path) -> String {
    let mut sources = Vec::new();
    collect_sources(crate_dir, Path::new("src"), &mut sources);
    sources.extend([PathBuf::from("build.rs"), PathBuf::from("Cargo.toml")]);
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
    inputs.push((
        "workspace/Cargo.toml".to_owned(),
        fs::read(crate_dir.join("../../Cargo.toml")).expect("should read template build input"),
    ));
    match fs::read(crate_dir.join("../../Cargo.lock")) {
        Ok(bytes) => inputs.push(("workspace/Cargo.lock".to_owned(), bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        result => {
            result.expect("should read optional workspace lockfile when present");
        }
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
        let path = relative.join(entry.file_name());
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
