use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::config::check_git_config;
use super::git::Git;
use super::tree::{TreeLimits, parse_tree_object};
use super::*;
use crate::git_objects::checked_sha1;
use crate::policy::ReleaseTarget;
use crate::preflight::audit_environment;
use crate::preflight::child::{ChildRequest, child_environment};
use crate::process::Cancellation;

const EPOCH: u64 = 1_700_000_000;

#[test]
fn bound_workspace_origins_recheck_nonmanifest_source_and_unexpected_files() {
    for mutation in ["rust", "other", "extra", "git", "cancel"] {
        let fixture = Fixture::new();
        let approved = fixture.authenticate().expect("should authenticate source");
        let cancellation = Cancellation::default();
        let fresh = approved
            .materialize(Instant::now() + Duration::from_secs(30), &cancellation)
            .expect("should materialize source");
        let origins = crate::composition::origin::WorkspaceOrigins::bind(&approved, &fresh)
            .expect("should bind source origins");
        origins
            .verify_unchanged()
            .expect("should accept unchanged source");
        match mutation {
            "rust" => write(
                fresh.root(),
                "crates/trusted-server-release/src/main.rs",
                b"fn main() { panic!(); }\n",
            ),
            "other" => write(fresh.root(), "AGENTS.md", b"changed source\n"),
            "extra" => write(fresh.root(), "unexpected.txt", b"unexpected\n"),
            "git" => write(fresh.root(), ".git/config", b"unexpected\n"),
            "cancel" => cancellation.cancel(),
            _ => unreachable!(),
        }
        assert!(
            origins.verify_unchanged().is_err(),
            "should reject {mutation} after binding"
        );
    }
}

#[test]
fn bound_source_verification_preserves_original_deadline() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let fresh = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should materialize source");
    let mut verification = fresh.verification();
    verification.expire_for_tests();
    assert!(
        verification.verify_unchanged(&[]).is_err(),
        "should reject an expired source attempt"
    );
}

#[test]
fn workspace_origins_require_an_unchanged_approved_materialization() {
    let fixture = Fixture::new();
    let approved = fixture
        .authenticate()
        .expect("should authenticate fixture source");
    let fresh = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should materialize independent source");
    crate::composition::origin::WorkspaceOrigins::bind(&approved, &fresh)
        .expect("should bind exact approved checkout and manifest identities");
    let other = ApprovedSource::in_memory_for_tests(
        &CommitId::parse(&"b".repeat(40)).expect("should parse different source"),
        &["Cargo.toml"],
    );
    assert!(
        crate::composition::origin::WorkspaceOrigins::bind(&other, &fresh).is_err(),
        "should reject another approved source identity"
    );
    fs::write(fresh.root().join("AGENTS.md"), b"changed").expect("should change private source");
    assert!(
        crate::composition::origin::WorkspaceOrigins::bind(&approved, &fresh).is_err(),
        "should reject changed checkout before issuing origin authority"
    );
}

#[test]
fn cargo_manifest_catalog_contains_only_approved_regular_manifests() {
    let commit = CommitId::parse(&"a".repeat(40)).expect("should parse approved commit");
    let mut approved = ApprovedSource::in_memory_for_tests(
        &commit,
        &[
            "Cargo.toml",
            "crates/example/Cargo.toml",
            "README.md",
            "notCargo.toml",
        ],
    );
    approved.snapshot.records.insert(
        "crates/alias/Cargo.toml".to_owned(),
        (Mode::Symlink, "b".repeat(64)),
    );
    let manifests = approved.cargo_manifest_digests();
    assert_eq!(
        manifests.keys().map(PathBuf::as_path).collect::<Vec<_>>(),
        [
            Path::new("Cargo.toml"),
            Path::new("crates/example/Cargo.toml")
        ],
        "should expose only approved regular Cargo manifests for origin joining"
    );
    assert!(
        manifests.values().all(|digest| digest == &"0".repeat(64)),
        "should preserve approved content identities"
    );
}

#[test]
fn fresh_directory_canonicalizes_an_aliased_parent_and_preserves_cleanup() {
    let directory = tempdir().expect("should create parent fixture");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
    let parent = base.join("real-parent");
    let alias = base.join("alias-parent");
    fs::create_dir(&parent).expect("should create real parent");
    symlink(&parent, &alias).expect("should create parent alias");
    let owned =
        super::materialize::fresh_directory(&alias).expect("should create private checkout");
    let path = owned.path().to_owned();
    assert_eq!(
        path,
        fs::canonicalize(&path).expect("should resolve checkout"),
        "should expose the canonical owning path"
    );
    assert_eq!(
        path.parent(),
        Some(parent.as_path()),
        "should allocate under the resolved parent"
    );
    owned.close().expect("should remove owned checkout");
    assert!(
        !path.exists(),
        "should clean up the exact canonical checkout"
    );
}

#[test]
fn fresh_source_attempt_controls_reject_cancelled_and_expired_work() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let cancellation = Cancellation::default();
    cancellation.cancel();
    assert!(
        approved
            .materialize(Instant::now() + Duration::from_secs(30), &cancellation)
            .is_err(),
        "should reject cancellation before copying"
    );
    assert!(
        approved
            .materialize(Instant::now(), &Cancellation::default())
            .is_err(),
        "should reject expired attempts before copying"
    );
    let cancellation = Cancellation::default();
    let fresh = approved
        .materialize(Instant::now() + Duration::from_secs(30), &cancellation)
        .expect("should copy active source");
    cancellation.cancel();
    assert!(
        fresh.verify_unchanged(&[]).is_err(),
        "should retain attempt cancellation for handoff checks"
    );
}

#[test]
fn fresh_source_copy_and_source_scan_stop_at_the_first_cancelled_check() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let baseline = Cell::new(0);
    super::worktree::compare_with_outputs_controlled(
        &approved.snapshot.root,
        &approved.snapshot.records,
        &[],
        &|| {
            baseline.set(baseline.get() + 1);
            Ok(())
        },
    )
    .expect("should measure deterministic initial scan");
    let calls = Cell::new(0);
    let stop = baseline.get() + 12;
    let result = approved.materialize_with_check(
        Instant::now() + Duration::from_secs(30),
        &Cancellation::default(),
        &|| {
            calls.set(calls.get() + 1);
            if calls.get() >= stop {
                return Err(source_error("cancelled during private copy"));
            }
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "should stop copying instead of returning fresh source"
    );
    assert_eq!(calls.get(), stop, "should stop exactly at cancellation");
    let scans = Cell::new(0);
    let scanned = super::worktree::compare_with_outputs_controlled(
        &approved.snapshot.root,
        &approved.snapshot.records,
        &[],
        &|| {
            scans.set(scans.get() + 1);
            if scans.get() >= 3 {
                return Err(source_error("cancelled during directory scan"));
            }
            Ok(())
        },
    );
    assert!(
        scanned.is_err(),
        "should abort source enumeration when cancelled"
    );
    assert_eq!(scans.get(), 3, "should not enumerate after cancellation");
}

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
    // Keep the duplicated fixture setup free of detached maintenance writes.
    let output = Command::new("git")
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
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
        for script in [
            "build-js.sh",
            "build.sh",
            "collect-js.mjs",
            "observe-npm.mjs",
        ] {
            write(
                &root,
                &format!("scripts/release/{script}"),
                b"// fictional driver\n",
            );
        }
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
mod process;
mod worktree;

#[test]
fn fresh_source_copies_share_no_files_and_reject_changed_inputs() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let first = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should materialize first source");
    let second = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should materialize second source");
    for fresh in [&first, &second] {
        assert_eq!(
            fresh.root(),
            fs::canonicalize(fresh.root()).expect("should resolve materialized checkout"),
            "should expose the same path that boundary roots bind"
        );
    }
    assert_ne!(
        first.root(),
        second.root(),
        "should own distinct source roots"
    );
    assert!(
        !first.root().join(".git").exists(),
        "should not inherit repository configuration"
    );
    first
        .verify_unchanged(&[])
        .expect("should verify pristine copy");
    fs::write(first.root().join("AGENTS.md"), b"changed").expect("should modify private copy");
    assert!(
        first.verify_unchanged(&[]).is_err(),
        "should reject changed copied source"
    );
    second
        .verify_unchanged(&[])
        .expect("should preserve independent copy");
    approved
        .snapshot()
        .verify_unchanged()
        .expect("should preserve original source");
    fs::write(fixture.root.join("AGENTS.md"), b"changed").expect("should change original");
    assert!(
        approved
            .materialize(
                Instant::now() + Duration::from_secs(30),
                &Cancellation::default()
            )
            .is_err(),
        "should reject changed original before copying"
    );
}

#[test]
fn fresh_source_output_exclusions_cannot_hide_approved_files() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let fresh = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should copy source");
    fs::create_dir(fresh.root().join("generated")).expect("should create generated root");
    fs::write(fresh.root().join("generated/output"), b"output").expect("should write output");
    assert!(
        fresh.verify_unchanged(&[]).is_err(),
        "should reject unapproved output root"
    );
    fresh
        .verify_unchanged(&["generated"])
        .expect("should allow disjoint generated root");
    assert!(
        fresh.verify_unchanged(&["crates"]).is_err(),
        "should reject exclusion overlapping tracked source"
    );
    assert!(
        fresh.verify_unchanged(&["../source"]).is_err(),
        "should reject nonnormal exclusions"
    );
    fs::write(fresh.root().join("AGENTS.md"), b"changed").expect("should change tracked file");
    assert!(
        fresh.verify_unchanged(&["generated"]).is_err(),
        "should still reject source changes outside generated root"
    );
}

#[test]
fn fresh_source_recheck_rejects_replaced_root_alias() {
    let fixture = Fixture::new();
    let approved = fixture.authenticate().expect("should authenticate source");
    let fresh = approved
        .materialize(
            Instant::now() + Duration::from_secs(30),
            &Cancellation::default(),
        )
        .expect("should create source copy");
    let saved = fixture.parent.path().join("saved-copy");
    fs::rename(fresh.root(), &saved).expect("should move owned source");
    symlink(&saved, fresh.root()).expect("should alias identical source");
    assert!(
        fresh.verify_unchanged(&[]).is_err(),
        "should reject source root substitution"
    );
}
