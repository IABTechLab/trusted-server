use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::config::check_git_config;
use super::git::Git;
use super::tree::{TreeLimits, parse_tree_object};
use super::*;
use crate::policy::ReleaseTarget;
use crate::preflight::audit_environment;
use crate::preflight::child::{ChildRequest, child_environment};

const EPOCH: u64 = 1_700_000_000;

struct Fixture {
    parent: TempDir,
    root: PathBuf,
    commit: String,
}

/// Fixtures fail preflight if any temporary-directory ancestor holds a Cargo
/// configuration layer, so report that host precondition explicitly.
fn assert_clean_ancestry(path: &Path) {
    let canonical = fs::canonicalize(path).expect("should resolve fixture directory");
    for ancestor in canonical.ancestors() {
        for name in ["config", "config.toml"] {
            let layer = ancestor.join(".cargo").join(name);
            assert!(
                !layer.exists(),
                "fixture temporary directory ancestry must not contain {}; set TMPDIR to a clean directory",
                layer.display()
            );
        }
    }
}

fn host_path() -> OsString {
    std::env::var_os("PATH").expect("should have a PATH to locate git")
}

fn fixture_git(root: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", host_path())
        .env("HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fictional Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", format!("{EPOCH} +0000"))
        .env("GIT_COMMITTER_NAME", "Fictional Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", format!("{EPOCH} +0000"))
        .output()
        .expect("should run fixture git");
    assert!(
        output.status.success(),
        "should run fixture git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("should emit UTF-8")
        .trim()
        .to_owned()
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("should have parent")).expect("should create dirs");
    fs::write(path, bytes).expect("should write fixture file");
}

impl Fixture {
    fn new() -> Self {
        let parent = tempdir().expect("should create fixture parent");
        assert_clean_ancestry(parent.path());
        let root = parent.path().join("source");
        fs::create_dir(&root).expect("should create fixture root");
        write(
            &root,
            "Cargo.toml",
            b"[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/trusted-server-openrtb\",\n    \"crates/trusted-server-release\",\n]\n",
        );
        write(&root, "Cargo.lock", b"# fictional lock\nversion = 4\n");
        write(
            &root,
            "rust-toolchain.toml",
            b"[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write(&root, ".cargo/config.toml", b"[alias]\n");
        write(&root, ".tool-versions", b"rust 1.95.0\n");
        write(
            &root,
            "scripts/release/bootstrap.sh",
            b"#!/bin/sh\nexit 1\n",
        );
        fs::set_permissions(
            root.join("scripts/release/bootstrap.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("should mark bootstrap executable");
        write(
            &root,
            "crates/trusted-server-openrtb/Cargo.toml",
            b"[package]\nname = \"fictional-openrtb\"\n",
        );
        write(
            &root,
            "crates/trusted-server-release/Cargo.toml",
            b"[package]\nname = \"fictional-release\"\n",
        );
        write(
            &root,
            "crates/trusted-server-release/src/main.rs",
            b"fn main() {}\n",
        );
        write(&root, "crates/other/src/lib.rs", b"// outside closure\n");
        write(&root, "AGENTS.md", b"# Fictional agents\n");
        symlink("AGENTS.md", root.join("CLAUDE.md")).expect("should create reviewed symlink");
        let mut fixture = Self {
            parent,
            root,
            commit: String::new(),
        };
        fixture.git(&["init", "-q"]);
        fixture.commit_all();
        fixture
    }

    fn home(&self) -> PathBuf {
        let home = self.parent.path().join("fixture-home");
        fs::create_dir_all(&home).expect("should create fixture home");
        home
    }

    fn git(&self, args: &[&str]) -> String {
        fixture_git(&self.root, &self.home(), args)
    }

    fn commit_all(&mut self) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "fixture"]);
        self.commit = self.git(&["rev-parse", "HEAD"]);
    }

    fn audited(&self) -> AuditedEnvironment {
        audit_environment(&[(OsString::from("PATH"), host_path())])
            .expect("should audit fixture environment")
    }

    fn authenticate(&self) -> Result<ApprovedSource, Report<ReleaseError>> {
        approve(&self.root, &self.commit, &self.audited())
    }

    fn rejection(&self) -> String {
        format!(
            "{:?}",
            self.authenticate()
                .err()
                .expect("should reject fixture source")
        )
    }
}

/// Parses `commit` like the CLI does, then authenticates `root`.
fn approve(
    root: &Path,
    commit: &str,
    audited: &AuditedEnvironment,
) -> Result<ApprovedSource, Report<ReleaseError>> {
    authenticate(root, &CommitId::parse(commit)?, audited)
}

fn assert_rejects(fixture: &Fixture, needle: &str) {
    let text = fixture.rejection();
    assert!(
        text.contains(needle),
        "should explain rejection with `{needle}`, got: {text}"
    );
}

mod closure;
mod layout;
mod objects;
mod worktree;
