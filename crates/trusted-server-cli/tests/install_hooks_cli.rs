//! End-to-end tests for `ts dev install-hooks` against a real `git`
//! binary. The properties under test (which hooks git executes, and
//! where a linked worktree looks for them) are git's behaviour, not
//! ours, so gix-built fixtures cannot stand in. Each test skips when no
//! `git` is on `PATH`; CI always has one.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use predicates::prelude::*;
use tempfile::TempDir;

const BAD_SOURCE: &str = "let bad = \"https://test.com\";\n"; // allow-domain: test.com

/// A git environment isolated from the developer's own configuration:
/// `GIT_CONFIG_GLOBAL` points at a file this test owns and the system
/// config is ignored. Both `git` and `ts` (via gix) honour these.
struct GitEnv {
    temp: TempDir,
    global_config: PathBuf,
}

impl GitEnv {
    fn new() -> Option<Self> {
        let available = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !available {
            return None;
        }
        let temp = tempfile::tempdir().expect("should create tempdir");
        let global_config = temp.path().join("gitconfig");
        fs::write(&global_config, "").expect("should write empty global config");
        Some(Self {
            temp,
            global_config,
        })
    }

    fn vars(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "GIT_CONFIG_GLOBAL",
                self.global_config.display().to_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
            ("GIT_AUTHOR_NAME", "ts dev lint tests".to_string()),
            ("GIT_AUTHOR_EMAIL", "tests@example.com".to_string()),
            ("GIT_COMMITTER_NAME", "ts dev lint tests".to_string()),
            ("GIT_COMMITTER_EMAIL", "tests@example.com".to_string()),
        ]
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(dir)
            .envs(self.vars())
            .args(args)
            .output()
            .expect("should spawn git")
    }

    fn git_ok(&self, dir: &Path, args: &[&str]) -> Output {
        let out = self.git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn ts(&self, dir: &Path) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("ts").expect("should locate the ts binary");
        cmd.current_dir(dir).envs(self.vars());
        cmd
    }

    /// A repository on branch `main` with one clean committed file.
    fn repo(&self) -> PathBuf {
        let repo = self.temp.path().join("repo");
        fs::create_dir_all(&repo).expect("should create repo dir");
        self.git_ok(&repo, &["init", "-q"]);
        self.git_ok(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        fs::write(repo.join("ok.rs"), "fn ok() {}\n").expect("should write ok.rs");
        self.git_ok(&repo, &["add", "ok.rs"]);
        self.git_ok(&repo, &["commit", "-q", "-m", "initial"]);
        repo
    }

    fn hooks_path_is_unset(&self, dir: &Path) -> bool {
        !self
            .git(dir, &["config", "--get", "core.hooksPath"])
            .status
            .success()
    }
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).expect("should write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("should chmod +x");
}

/// Stage a violating file and attempt a commit; the installed hook must
/// reject it.
fn commit_is_blocked_by_hook(env: &GitEnv, dir: &Path) {
    fs::write(dir.join("bad.rs"), BAD_SOURCE).expect("should write bad.rs");
    env.git_ok(dir, &["add", "bad.rs"]);
    let out = env.git(dir, &["commit", "-q", "-m", "bad"]);
    assert!(
        !out.status.success(),
        "the pre-commit hook should block the commit"
    );
    // Git forwards a hook's stdout to its own stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("disallowed host test.com"),
        "hook output should name the violation: {stderr}"
    );
}

/// Regression for the branch-controlled hook execution path: a branch
/// carrying an executable `.githooks/post-checkout` must stay inert
/// after install, and the installed hook must actually run.
#[test]
fn branch_provided_hooks_directory_is_never_executed() {
    let Some(env) = GitEnv::new() else { return };
    let repo = env.repo();
    let marker = env.temp.path().join("executed");

    env.git_ok(&repo, &["checkout", "-q", "-b", "planted"]);
    fs::create_dir_all(repo.join(".githooks")).expect("should create .githooks");
    write_executable(
        &repo.join(".githooks/post-checkout"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    env.git_ok(&repo, &["add", ".githooks"]);
    env.git_ok(&repo, &["commit", "-q", "-m", "plant hook"]);
    env.git_ok(&repo, &["checkout", "-q", "main"]);

    env.ts(&repo)
        .args(["dev", "install-hooks"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".git/hooks/pre-commit"));

    assert!(
        env.hooks_path_is_unset(&repo),
        "install must not set core.hooksPath"
    );
    env.git_ok(&repo, &["checkout", "-q", "planted"]);
    assert!(
        !marker.exists(),
        "the branch-provided post-checkout hook must not run"
    );
    assert!(
        env.git(&repo, &["status", "--porcelain"]).stdout.is_empty(),
        "install must leave the working tree clean"
    );

    commit_is_blocked_by_hook(&env, &repo);
}

/// From a linked worktree the hook lands in the main repository's
/// hook directory, where git runs it for every worktree.
#[test]
fn linked_worktree_installs_into_shared_hooks_dir() {
    let Some(env) = GitEnv::new() else { return };
    let repo = env.repo();
    let worktree = env.temp.path().join("wt");
    env.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            worktree.to_str().expect("utf8 path"),
        ],
    );

    env.ts(&worktree)
        .args(["dev", "install-hooks"])
        .assert()
        .success();

    let hook = repo.join(".git/hooks/pre-commit");
    let content = fs::read_to_string(&hook).expect("hook should be in the main .git/hooks");
    assert!(content.contains("# ts-install-hooks: managed"));
    assert!(
        !repo.join(".git/worktrees/wt/hooks").exists(),
        "no per-worktree hooks directory may be created"
    );
    assert!(env.hooks_path_is_unset(&worktree));

    commit_is_blocked_by_hook(&env, &worktree);
}

/// A `core.hooksPath` coming from the global config is effective for
/// git, so the installer must see it and refuse.
#[test]
fn global_hooks_path_is_refused() {
    let Some(env) = GitEnv::new() else { return };
    let repo = env.repo();
    fs::write(
        &env.global_config,
        "[core]\n\thooksPath = /tmp/globalhooks\n",
    )
    .expect("should write global config");

    env.ts(&repo)
        .args(["dev", "install-hooks"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "core.hooksPath is set to `/tmp/globalhooks`",
        ));
    assert!(
        !repo.join(".git/hooks/pre-commit").exists(),
        "refused install writes nothing"
    );

    env.ts(&repo)
        .args(["dev", "install-hooks", "--force"])
        .assert()
        .code(2);
}
