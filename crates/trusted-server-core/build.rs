//! Compiles the deployed git version into `trusted-server-core` as `TS_GIT_VERSION`.

#[path = "build_support/git_version.rs"]
mod git_version;

use std::path::Path;
use std::process::Command;

use git_version::{Candidates, is_usable, resolve_git_version};

/// Set by the deploy pipeline. Its CI checkout is shallow and detached, so local git
/// cannot see the tag or branch there.
const OVERRIDE_ENV: &str = "TRUSTED_SERVER_GIT_VERSION";

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
fn resolve_from_local_git() -> Option<String> {
    // HEAD is per-worktree; refs are shared in the common dir. They differ in a
    // linked worktree and coincide in a plain clone. Only the current branch's
    // ref (its commit decides the exact-tag match) and tags can change the
    // result, so other branches and `refs/remotes` are not watched: a commit
    // elsewhere or a fetch must not rebuild core.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        rerun_if_exists(&Path::new(&git_dir).join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common_dir = Path::new(&common_dir);
        if let Some(branch_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            rerun_if_exists(&common_dir.join(branch_ref));
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
    println!("cargo:rerun-if-changed=build.rs");
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

    if let Some(version) = resolved {
        println!("cargo:rustc-env=TS_GIT_VERSION={version}");
    }
}
