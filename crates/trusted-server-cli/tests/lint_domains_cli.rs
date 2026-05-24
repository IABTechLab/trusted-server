//! End-to-end tests for `ts dev lint domains`, exercising the `ts`
//! binary as a whole: exit codes, stdout, and stderr.
//!
//! The pure-function and collector logic is covered by inline unit
//! tests in `src/dev/lint/domains.rs`; this file locks the
//! binary-observable contract (exit 0 / 1 / 2, report shape).

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Build the `ts` command rooted at `dir`.
fn ts_in(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("ts").expect("should locate the ts binary");
    cmd.current_dir(dir.path());
    cmd
}

/// A repo with one committed clean file and HEAD established.
fn repo_with_initial_commit() -> TempDir {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(temp.path().join("ok.rs"), "fn ok() {}\n").expect("should write ok.rs");
    common::stage_all(&repo);
    common::commit_all(&repo, "initial");
    temp
}

// === --staged mode ===

#[test]
fn staged_clean_exits_zero() {
    let temp = repo_with_initial_commit();
    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

#[test]
fn staged_violation_exits_one_human() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "bad.rs:1: disallowed host test.com",
        ))
        .stdout(predicate::str::contains("1 disallowed host(s) found"));
}

#[test]
fn staged_violation_json_format() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);

    let assert = ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged", "--format", "json"])
        .assert()
        .code(1);
    let stdout =
        String::from_utf8(assert.get_output().stdout.clone()).expect("stdout should be UTF-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert_eq!(parsed["count"], 1);
    assert_eq!(parsed["violations"][0]["host"], "test.com");
}

#[test]
fn staged_suppression_marker_passes() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("sec.rs"),
        "let attacker = \"https://evil.com\"; // allow-domain: evil.com\n",
    )
    .expect("should write sec.rs");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

/// Spec test case 25: non-UTF-8 staged paths are reported (not
/// skipped) with a lossy-path stderr warning. Linux-only — macOS
/// rejects non-UTF-8 filenames with `EILSEQ`.
#[cfg(target_os = "linux")]
#[test]
fn staged_non_utf8_path_warns_and_reports() {
    use std::os::unix::ffi::OsStrExt;

    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    let name = std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f, 0x2e, 0x72, 0x73]);
    std::fs::write(temp.path().join(name), "let bad = \"https://test.com\";\n")
        .expect("should write non-utf8-named file");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host test.com"))
        .stderr(predicate::str::contains("not valid UTF-8"));
}

/// Regression for the rename bug: a pure rename of a file containing
/// a disallowed URL must exit clean. The previous implementation
/// reported every line of the renamed file as added.
#[test]
fn staged_pure_rename_exits_zero() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(
        temp.path().join("old.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write old");
    common::stage_all(&repo);
    common::commit_all(&repo, "initial");

    std::fs::remove_file(temp.path().join("old.rs")).expect("should remove old");
    std::fs::write(
        temp.path().join("new.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write new");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

#[test]
fn staged_deletion_exits_zero() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(
        temp.path().join("doomed.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write doomed");
    common::stage_all(&repo);
    common::commit_all(&repo, "initial");

    std::fs::remove_file(temp.path().join("doomed.rs")).expect("should remove doomed");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

/// Existing committed violations must not be re-reported when an
/// unrelated, clean change is staged.
#[test]
fn staged_existing_violation_with_unrelated_change_exits_zero() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(
        temp.path().join("legacy.rs"),
        "let pre_existing = \"https://test.com\";\n",
    )
    .expect("should write legacy");
    common::stage_all(&repo);
    common::commit_all(&repo, "commit pre-existing violation");

    std::fs::write(temp.path().join("clean.rs"), "let ok = 1;\n").expect("should write clean");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

/// Multi-hunk same-file edit: both added regions are scanned and
/// both violations reported with their correct new-side line numbers.
#[test]
fn staged_multi_hunk_reports_both_added_violations() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(
        temp.path().join("a.rs"),
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
    )
    .expect("should write initial");
    common::stage_all(&repo);
    common::commit_all(&repo, "initial");

    std::fs::write(
        temp.path().join("a.rs"),
        "alpha\nlet bad1 = \"https://test.com\";\nbeta\ngamma\ndelta\nlet bad2 = \"https://partner.com\";\nepsilon\n",
    )
    .expect("should write multi-hunk");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("a.rs:2: disallowed host test.com"))
        .stdout(predicate::str::contains(
            "a.rs:6: disallowed host partner.com",
        ));
}

/// JSON output shape: `count`, `files_affected`, and each
/// violation's `path`, `line_no`, `host`, `line` fields.
#[test]
fn staged_violation_json_full_shape() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);

    let assert = ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged", "--format", "json"])
        .assert()
        .code(1);
    let stdout =
        String::from_utf8(assert.get_output().stdout.clone()).expect("stdout should be UTF-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert_eq!(parsed["count"], 1);
    assert_eq!(parsed["files_affected"], 1);
    let v = &parsed["violations"][0];
    assert_eq!(v["path"], "bad.rs");
    assert_eq!(v["line_no"], 1);
    assert_eq!(v["host"], "test.com");
    assert_eq!(v["line"], "let bad = \"https://test.com\";");
}

/// `--verbose` writes a per-file scan-progress note to stderr; exit
/// code and violation count are unchanged.
#[test]
fn staged_verbose_writes_per_file_progress_to_stderr() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged", "--verbose"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host test.com"))
        .stderr(predicate::str::contains("scanned"))
        .stderr(predicate::str::contains("bad.rs"));
}

// === --changed-vs mode ===

#[test]
fn changed_vs_reports_feature_branch_lines() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(temp.path().join("a.rs"), "let ok = 1;\n").expect("should write base");
    common::stage_all(&repo);
    common::commit_all(&repo, "base");

    common::create_and_checkout_branch(&repo, "feature");
    std::fs::write(
        temp.path().join("a.rs"),
        "let ok = 1;\nlet bad = \"https://test.com\";\n",
    )
    .expect("should write feature change");
    common::stage_all(&repo);
    common::commit_all(&repo, "feature change");

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--changed-vs", "main"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host test.com"));
}

/// A `--changed-vs` ref that doesn't resolve in any of the four
/// fallback locations is an environment error (exit 2), not a
/// violation (exit 1).
#[test]
fn changed_vs_unknown_ref_exits_two() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(temp.path().join("a.rs"), "let ok = 1;\n").expect("should write base");
    common::stage_all(&repo);
    common::commit_all(&repo, "base");

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--changed-vs", "no-such-ref"])
        .assert()
        .code(2);
}

// === full-repo mode ===

#[test]
fn full_repo_reports_committed_violation() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://partner.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);
    common::commit_all(&repo, "commit with a violation");

    ts_in(&temp)
        .args(["dev", "lint", "domains"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host partner.com"));
}

// === explicit-path mode ===

#[test]
fn explicit_path_scans_named_file() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    std::fs::write(
        temp.path().join("named.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write named.rs");

    ts_in(&temp)
        .args(["dev", "lint", "domains", "named.rs"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host test.com"));
}

#[test]
fn explicit_missing_path_exits_two() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    ts_in(&temp)
        .args(["dev", "lint", "domains", "does-not-exist.rs"])
        .assert()
        .code(2);
}

// === Markdown ===

#[test]
fn markdown_disallowed_link_reported() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("doc.md"),
        "See [the tracker](https://test.com) for details.\n",
    )
    .expect("should write doc.md");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "doc.md:1: disallowed host test.com",
        ));
}

#[test]
fn markdown_allowed_inline_link_passes() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("doc.md"),
        "See [the Fastly docs](https://developer.fastly.com/learning).\n",
    )
    .expect("should write doc.md");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(0);
}

// === Environment cases ===

#[test]
fn outside_git_repo_exits_two() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    // No repo initialised — gix::open fails → EnvironmentError → exit 2.
    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(2);
}

/// The linter must not require a `git` binary on `PATH` — all git
/// work goes through gitoxide. Run with an emptied `PATH` and confirm
/// it still functions. Unix-only (Windows PATH semantics differ).
#[cfg(unix)]
#[test]
fn works_without_git_on_path() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("should reopen repo");
    std::fs::write(
        temp.path().join("bad.rs"),
        "let bad = \"https://test.com\";\n",
    )
    .expect("should write bad.rs");
    common::stage_all(&repo);

    ts_in(&temp)
        .env_clear()
        .env("PATH", "")
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("disallowed host test.com"));
}
