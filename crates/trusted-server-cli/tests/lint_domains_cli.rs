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

/// Spec case 28: when HEAD is behind the base ref, the merge-base
/// is HEAD itself and the diff is empty — so no violations are
/// reported even if the base ref has introduced one. This exercises
/// the merge-base path with an "anti-symmetric" topology.
#[test]
fn changed_vs_branch_behind_base_reports_nothing() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());

    // Base: a single clean commit on `main`.
    std::fs::write(temp.path().join("a.rs"), "let ok = 1;\n").expect("should write base");
    common::stage_all(&repo);
    common::commit_all(&repo, "base");

    // Branch from `main` at the base commit (no further commits on
    // the feature branch — HEAD is at the merge-base).
    common::create_and_checkout_branch(&repo, "feature");

    // Advance `main` past the merge-base with a commit that, if
    // wrongly attributed to the feature branch, would be a
    // violation. Then move HEAD back to `feature`.
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};
    let main_ref: FullName = "refs/heads/main".try_into().expect("valid ref name");
    let head_edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: gix::bstr::BString::from("switch to main"),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(main_ref),
        },
        name: "HEAD".try_into().expect("HEAD"),
        deref: false,
    };
    repo.edit_reference(head_edit)
        .expect("should switch HEAD to main");
    std::fs::write(
        temp.path().join("a.rs"),
        "let ok = 1;\nlet ahead = \"https://test.com\";\n",
    )
    .expect("should write main-ahead change");
    common::stage_all(&repo);
    common::commit_all(&repo, "main: ahead of feature");

    // Move HEAD back to feature.
    let feature_ref: FullName = "refs/heads/feature".try_into().expect("valid ref name");
    let head_edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: gix::bstr::BString::from("switch to feature"),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(feature_ref),
        },
        name: "HEAD".try_into().expect("HEAD"),
        deref: false,
    };
    repo.edit_reference(head_edit)
        .expect("should switch HEAD back to feature");

    // `--changed-vs main`: merge-base(main, feature) == feature, so
    // diff is empty. The `main`-introduced violation must NOT appear.
    ts_in(&temp)
        .args(["dev", "lint", "domains", "--changed-vs", "main"])
        .assert()
        .code(0);
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

/// Binary-level coverage for spec cases 30, 31, 32, 34: paths under
/// `node_modules/`, `.worktrees/`, integrations fixtures, and known
/// lockfiles must be skipped even when they contain a disallowed
/// URL; one violation in a non-excluded file is still reported.
#[test]
fn full_repo_path_exclusions_are_skipped() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let repo = common::init_repo(temp.path());

    let bad = "let bad = \"https://test.com\";\n";

    // Excluded.
    let nm = temp.path().join("node_modules");
    std::fs::create_dir_all(&nm).expect("node_modules");
    std::fs::write(nm.join("pkg.js"), bad).expect("write node_modules pkg.js");

    let wt = temp.path().join(".worktrees/branch");
    std::fs::create_dir_all(&wt).expect(".worktrees/branch");
    std::fs::write(wt.join("a.rs"), bad).expect("write .worktrees a.rs");

    let fixtures = temp
        .path()
        .join("crates/trusted-server-core/src/integrations/x/fixtures");
    std::fs::create_dir_all(&fixtures).expect("fixtures dir");
    std::fs::write(fixtures.join("captured.html"), bad).expect("write fixtures captured.html");

    std::fs::write(temp.path().join("package-lock.json"), bad).expect("write lockfile");

    // Reported (sole non-excluded file).
    std::fs::write(temp.path().join("ok.rs"), bad).expect("write ok.rs");

    common::stage_all(&repo);
    common::commit_all(&repo, "seed mixed paths");

    let assert = ts_in(&temp)
        .args(["dev", "lint", "domains"])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    assert!(
        stdout.contains("ok.rs:1: disallowed host test.com"),
        "ok.rs should be reported: {stdout}"
    );
    assert!(
        !stdout.contains("pkg.js")
            && !stdout.contains(".worktrees")
            && !stdout.contains("fixtures")
            && !stdout.contains("package-lock.json"),
        "excluded paths must not appear in the report: {stdout}"
    );
    assert!(
        stdout.contains("1 disallowed host(s) found"),
        "summary should reflect exactly one violation: {stdout}"
    );
}

/// Explicit absolute path pointing at the linter's own source file
/// must still self-exclude — regression for the absolute-path
/// bypass of `SELF_PATH`.
#[test]
fn explicit_absolute_path_to_self_skips() {
    let temp = tempfile::tempdir().expect("should create tempdir");
    let nested = temp.path().join("crates/trusted-server-cli/src/dev/lint");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let self_clone = nested.join("domains.rs");
    std::fs::write(&self_clone, "let bad = \"https://test.com\";\n")
        .expect("write fake linter source");

    let abs = self_clone
        .canonicalize()
        .expect("should canonicalize self-clone");
    ts_in(&temp)
        .args(["dev", "lint", "domains", abs.to_str().expect("utf-8 path")])
        .assert()
        .code(0);
}

// === Markdown coverage (spec cases 36, 37, 39, 40, 42, 43) ===

/// Spec case 37 (autolink), 42 (reference-link target), 43 (image
/// link), 39 (multiple links on one line), 40 (fenced code block).
/// One Markdown file exercises all five forms in one binary
/// invocation.
#[test]
fn markdown_link_variants_all_reported() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("reopen repo");
    let body = "\
# Doc

Autolink: <https://test.com>
Inline: [bad](https://partner.com)
Image: ![alt](https://test.com/img.png)
Multi: see [a](https://github.com/x) and [b](https://test.com)

```
curl https://test.com/foo
```

[1]: https://test.com
";
    std::fs::write(temp.path().join("doc.md"), body).expect("write doc.md");
    common::stage_all(&repo);

    let assert = ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");

    // Every line that carries a disallowed host is reported. We
    // assert the *line numbers* match the file body exactly.
    for needle in [
        "doc.md:3: disallowed host test.com",    // autolink
        "doc.md:4: disallowed host partner.com", // inline link
        "doc.md:5: disallowed host test.com",    // image
        "doc.md:6: disallowed host test.com",    // multi (github.com allowed, test.com flagged)
        "doc.md:9: disallowed host test.com",    // fenced code block
        "doc.md:12: disallowed host test.com",   // reference list
    ] {
        assert!(
            stdout.contains(needle),
            "expected line `{needle}` in:\n{stdout}"
        );
    }
}

/// Spec case 38: an HTML-comment suppression marker on a Markdown
/// line suppresses the violation; a wrong-host marker still flags
/// the real host and emits a stderr "unused marker" warning.
#[test]
fn markdown_html_comment_suppression() {
    let temp = repo_with_initial_commit();
    let repo = gix::open(temp.path()).expect("reopen repo");
    let body = "\
ok: see [docs](https://test.com) <!-- allow-domain: test.com -->
bad: see [docs](https://test.com) <!-- allow-domain: other.com -->
";
    std::fs::write(temp.path().join("doc.md"), body).expect("write doc.md");
    common::stage_all(&repo);

    ts_in(&temp)
        .args(["dev", "lint", "domains", "--staged"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("doc.md:1: disallowed host test.com").not())
        .stdout(predicate::str::contains(
            "doc.md:2: disallowed host test.com",
        ))
        .stderr(predicate::str::contains(
            "marker listed `other.com` but it does not appear",
        ));
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
