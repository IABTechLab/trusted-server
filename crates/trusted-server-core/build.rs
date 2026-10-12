//! Build script for `trusted-server-core`: compiles in the `EdgeZero` default
//! config store ID, the template build digest, and the deployed git version
//! (`TRUSTED_SERVER__GIT_VERSION`).

#![allow(
    clippy::print_stdout,
    reason = "Cargo build scripts communicate rebuild inputs on stdout"
)]

#[path = "build_support/git_version.rs"]
mod git_version;

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use edgezero_core::manifest::ManifestLoader;
use git_version::{Candidates, is_usable, resolve_git_version};
use sha2::{Digest as _, Sha256};

/// Crate-relative build inputs that are both watched and hashed besides `src/`.
const CRATE_INPUTS: [&str; 2] = ["build.rs", "Cargo.toml"];
/// Workspace-relative inputs hashed under a `workspace/` logical prefix.
const WORKSPACE_INPUTS: [&str; 3] = ["Cargo.toml", "Cargo.lock", "edgezero.toml"];

/// Set by the deploy pipeline, which knows the deployed ref even when its checkout
/// cannot name it: a detached SHA or pull-request checkout has no tag or branch.
///
/// The resolved value is re-emitted under the same name, so `option_env!` reads
/// the validated value rather than the raw pipeline input.
const OVERRIDE_ENV: &str = "TRUSTED_SERVER__GIT_VERSION";

/// Runs `git` in the crate directory; `None` if git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Re-runs this script when `path` changes. Skips missing paths: Cargo treats
/// them as always changed, which would rebuild core on every build.
fn rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Resolves the version from local git, watching the paths that move with it.
///
/// `None` unless git's top level is this workspace, so an exported tree nested
/// in an unrelated repository does not report that repository's version.
fn resolve_from_local_git() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    // This crate lives at `<workspace>/crates/trusted-server-core`.
    let workspace_root = Path::new(&manifest_dir).parent()?.parent()?;
    let repository_root = git(&["rev-parse", "--show-toplevel"])?;
    if Path::new(&repository_root).canonicalize().ok()? != workspace_root.canonicalize().ok()? {
        return None;
    }

    // HEAD is per-worktree; refs are shared in the common dir. They differ in a
    // linked worktree and coincide in a plain clone. Only the current branch's
    // ref (its commit decides the exact-tag match) and tags can change the
    // result, so other branches and `refs/remotes` are not watched: a commit
    // elsewhere or a fetch must not rebuild core. When the branch ref is only
    // in `packed-refs`, its nearest existing ancestor (usually `refs/heads`) is
    // watched instead, so the loose ref the next commit writes is noticed.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        rerun_if_exists(&Path::new(&git_dir).join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common_dir = Path::new(&common_dir);
        if let Some(branch_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            let branch_path = common_dir.join(branch_ref);
            // Stop below the common dir: watching all of it would rebuild core
            // on every object write.
            if let Some(watched_path) = branch_path
                .ancestors()
                .take_while(|path| *path != common_dir)
                .find(|path| path.exists())
            {
                rerun_if_exists(watched_path);
            }
        }
        rerun_if_exists(&common_dir.join("refs/tags"));
        rerun_if_exists(&common_dir.join("packed-refs"));
    }

    let exact_tag = git(&["describe", "--tags", "--exact-match"]);
    let branch = git(&["symbolic-ref", "--short", "-q", "HEAD"]);
    let commit = git(&["rev-parse", "HEAD"]);
    resolve_git_version(&Candidates {
        override_value: None,
        exact_tag: exact_tag.as_deref(),
        branch: branch.as_deref(),
        commit: commit.as_deref(),
    })
}

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

    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("should set the build output directory"));
    let digest = template_build_digest(&crate_dir);
    fs::write(
        out_dir.join("template_build_digest.rs"),
        format!("const TEMPLATE_BUILD_DIGEST: &str = \"{digest}\";\n"),
    )
    .expect("should write the template build digest");

    emit_git_version();
}

/// Compiles the deployed git version in as `TRUSTED_SERVER__GIT_VERSION`.
fn emit_git_version() {
    println!("cargo:rerun-if-changed=build_support/git_version.rs");
    println!("cargo:rerun-if-env-changed={OVERRIDE_ENV}");

    let override_value = std::env::var(OVERRIDE_ENV).ok();
    let resolved = match override_value.as_deref() {
        Some(value) if is_usable(value) => resolve_git_version(&Candidates {
            override_value: Some(value),
            exact_tag: None,
            branch: None,
            commit: None,
        }),
        Some(value) => {
            if !value.trim().is_empty() {
                println!(
                    "cargo:warning={OVERRIDE_ENV}={value:?} is not visible ASCII; \
                     falling back to local git for x-ts-version"
                );
            }
            resolve_from_local_git()
        }
        None => resolve_from_local_git(),
    };

    // Always emitted, even when empty, so the raw pipeline value never reaches
    // `option_env!` unvalidated: `rustc-env` shadows the inherited environment.
    println!(
        "cargo:rustc-env={OVERRIDE_ENV}={}",
        resolved.unwrap_or_default()
    );
}

/// Hash the core implementation and its dependency resolution without checkout paths.
///
/// This private workspace crate lives two levels below the workspace manifest.
/// All source files are included except hidden, `#`-prefixed, and `~`-suffixed editor
/// artifacts, so embedded JS and future asset types are covered.
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
