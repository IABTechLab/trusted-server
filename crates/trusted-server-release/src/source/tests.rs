use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::policy::ReleaseTarget;
use crate::preflight::{ChildRequest, audit_environment, child_environment};

const EPOCH: u64 = 1_700_000_000;

struct Fixture {
    parent: TempDir,
    root: PathBuf,
    commit: String,
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
        authenticate(&self.root, &self.commit, &self.audited())
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

fn assert_rejects(fixture: &Fixture, needle: &str) {
    let text = fixture.rejection();
    assert!(
        text.contains(needle),
        "should explain rejection with `{needle}`, got: {text}"
    );
}

#[test]
fn authenticates_clean_fixture_with_commit_epoch_and_closure() {
    let fixture = Fixture::new();
    let source = fixture
        .authenticate()
        .expect("should authenticate clean fixture");
    assert_eq!(
        source.commit(),
        fixture.commit,
        "should keep approved commit"
    );
    assert_eq!(source.epoch(), EPOCH, "should derive committer epoch");
    assert!(
        lower_hex(source.snapshot().fingerprint(), 64),
        "should fingerprint the tracked snapshot"
    );
    assert!(
        lower_hex(source.closure_digest(), 64),
        "should digest closure"
    );
    assert_eq!(
        source.closure_paths(),
        [
            ".cargo/config.toml",
            ".tool-versions",
            "Cargo.lock",
            "Cargo.toml",
            "crates/trusted-server-openrtb/Cargo.toml",
            "crates/trusted-server-release/Cargo.toml",
            "crates/trusted-server-release/src/main.rs",
            "rust-toolchain.toml",
            "scripts/release/bootstrap.sh",
        ],
        "should record the complete helper-source closure"
    );
    assert_eq!(
        source.tracked_sha256("AGENTS.md"),
        Some(hex::encode(Sha256::digest(b"# Fictional agents\n")).as_str()),
        "should expose tracked content digests"
    );
    source
        .snapshot()
        .verify_unchanged()
        .expect("should verify unchanged worktree");
}

#[test]
fn rejects_malformed_commit_arguments() {
    let fixture = Fixture::new();
    for commit in [
        "ABC",
        "--help",
        &fixture.commit.to_uppercase(),
        &fixture.commit[..39],
    ] {
        assert!(
            authenticate(&fixture.root, commit, &fixture.audited()).is_err(),
            "should reject commit argument {commit}"
        );
    }
}

#[test]
fn rejects_unknown_commit_with_bounded_git_detail() {
    let fixture = Fixture::new();
    let text = format!(
        "{:?}",
        authenticate(&fixture.root, &"0".repeat(40), &fixture.audited())
            .err()
            .expect("should reject unknown commit")
    );
    assert!(
        text.contains("git cat-file"),
        "should name the failing git step: {text}"
    );
}

#[test]
fn rejects_non_commit_objects() {
    let fixture = Fixture::new();
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let text = format!(
        "{:?}",
        authenticate(&fixture.root, &tree, &fixture.audited())
            .err()
            .expect("should reject tree object")
    );
    assert!(text.contains("not a commit"), "should reject tree: {text}");
}

#[test]
fn rejects_head_that_differs_from_approved_commit() {
    let mut fixture = Fixture::new();
    let approved = fixture.commit.clone();
    write(&fixture.root, "AGENTS.md", b"# Changed\n");
    fixture.commit_all();
    let text = format!(
        "{:?}",
        authenticate(&fixture.root, &approved, &fixture.audited())
            .err()
            .expect("should reject HEAD mismatch")
    );
    assert!(
        text.contains("HEAD"),
        "should explain HEAD mismatch: {text}"
    );
}

#[test]
fn rejects_extensionless_rust_toolchain_before_other_checks() {
    let fixture = Fixture::new();
    write(&fixture.root, "rust-toolchain", b"nightly\n");
    write(
        &fixture.root,
        ".git/objects/info/alternates",
        b"/elsewhere\n",
    );
    assert_rejects(&fixture, "rust-toolchain");
}

#[test]
fn rejects_linked_worktree_gitfile_indirection() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.root.join(".git"),
        fixture.parent.path().join("moved-git"),
    )
    .expect("should move git directory");
    write(&fixture.root, ".git", b"gitdir: ../moved-git\n");
    assert_rejects(&fixture, "unsupported");
}

#[test]
fn rejects_commondir_indirection() {
    let fixture = Fixture::new();
    write(&fixture.root, ".git/commondir", b"../other\n");
    assert_rejects(&fixture, "commondir");
}

#[test]
fn rejects_object_and_history_rewriting_channels() {
    for (path, needle) in [
        ("objects/info/alternates", ".git/objects/info/alternates"),
        (
            "objects/info/http-alternates",
            ".git/objects/info/http-alternates",
        ),
        ("info/grafts", ".git/info/grafts"),
        ("shallow", ".git/shallow"),
        ("info/attributes", ".git/info/attributes"),
        (
            "refs/replace/0000000000000000000000000000000000000000",
            ".git/refs/replace",
        ),
    ] {
        let fixture = Fixture::new();
        write(&fixture.root.join(".git"), path, b"fictional\n");
        assert_rejects(&fixture, needle);
    }
}

#[test]
fn rejects_packed_replace_refs() {
    let fixture = Fixture::new();
    let line = format!(
        "# pack-refs with: peeled fully-peeled sorted\n{} refs/replace/{}\n",
        fixture.commit, fixture.commit
    );
    write(&fixture.root, ".git/packed-refs", line.as_bytes());
    assert_rejects(&fixture, "refs/replace");
}

#[test]
fn rejects_unreviewed_git_config_keys() {
    for addition in [
        "[include]\n\tpath = /dev/null\n",
        "[includeIf \"gitdir:/\"]\n\tpath = /dev/null\n",
        "[core]\n\tfsmonitor = true\n",
        "[core]\n\thooksPath = /dev/null\n",
        "[core]\n\tsshCommand = ssh\n",
        "[core]\n\tworktree = /\n",
        "[filter \"lfs\"]\n\tclean = cat\n",
        "[diff \"x\"]\n\ttextconv = cat\n",
        "[extensions]\n\tobjectformat = sha256\n",
        "[user]\n\tname = Fictional\n",
    ] {
        let fixture = Fixture::new();
        let config = fixture.root.join(".git/config");
        let mut text = fs::read_to_string(&config).expect("should read fixture config");
        text.push_str(addition);
        fs::write(&config, text).expect("should write fixture config");
        assert_rejects(&fixture, "git config");
    }
}

#[test]
fn parses_only_allowlisted_config_values() {
    assert!(
        check_git_config("[core]\n\trepositoryformatversion = 0\n\tbare = false\n[remote \"origin\"]\n\turl = https://example.com/r.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n").is_ok(),
        "should accept allowlisted keys"
    );
    for text in [
        "[core]\n\trepositoryformatversion = 1\n",
        "[core]\n\tbare = true\n",
        "[core]\n\tfilemode = maybe\n",
        "[core]\n\tsymlinks = \"true\"\n",
        "[core]\n\tfilemode = true \\\n",
        "[remote \"origin\"]\n\tpushurl = https://example.com\n",
        "[core.x]\n\tfilemode = true\n",
        "\tfilemode = true\n",
    ] {
        assert!(
            check_git_config(text).is_err(),
            "should reject config {text:?}"
        );
    }
}

#[test]
fn rejects_fsmonitor_without_executing_it() {
    let fixture = Fixture::new();
    let marker = fixture.parent.path().join("fsmonitor-ran");
    let hook = fixture.parent.path().join("fsmonitor.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display()))
        .expect("should write hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("should chmod hook");
    let config = fixture.root.join(".git/config");
    let mut text = fs::read_to_string(&config).expect("should read config");
    text.push_str(&format!("[core]\n\tfsmonitor = {}\n", hook.display()));
    fs::write(&config, text).expect("should write config");
    assert_rejects(&fixture, "git config");
    assert!(
        !marker.exists(),
        "should never execute the fsmonitor command"
    );
}

#[test]
fn rejects_modified_tracked_file() {
    let fixture = Fixture::new();
    write(&fixture.root, "AGENTS.md", b"# Modified\n");
    assert_rejects(&fixture, "AGENTS.md");
}

#[test]
fn rejects_changed_executable_bit() {
    let fixture = Fixture::new();
    fs::set_permissions(
        fixture.root.join("scripts/release/bootstrap.sh"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("should chmod");
    assert_rejects(&fixture, "scripts/release/bootstrap.sh");
}

#[test]
fn rejects_file_replaced_by_symlink() {
    let fixture = Fixture::new();
    let outside = fixture.parent.path().join("outside.md");
    fs::write(&outside, b"# Fictional agents\n").expect("should write outside file");
    fs::remove_file(fixture.root.join("AGENTS.md")).expect("should remove tracked file");
    symlink(&outside, fixture.root.join("AGENTS.md")).expect("should create symlink");
    assert_rejects(&fixture, "AGENTS.md");
}

#[test]
fn rejects_preexisting_hard_link() {
    let fixture = Fixture::new();
    fs::hard_link(
        fixture.root.join("Cargo.lock"),
        fixture.parent.path().join("lock-link"),
    )
    .expect("should create hard link outside source");
    assert_rejects(&fixture, "Cargo.lock");
}

#[test]
fn rejects_missing_tracked_file() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("crates/other/src/lib.rs")).expect("should remove file");
    assert_rejects(&fixture, "crates/other/src/lib.rs");
}

#[test]
fn rejects_untracked_and_ignored_paths() {
    for (path, needle) in [
        (".DS_Store", ".DS_Store"),
        (
            "crates/trusted-server-js/lib/node_modules/x/index.js",
            "crates/trusted-server-js",
        ),
        (
            "crates/other/src/extra_test.rs",
            "crates/other/src/extra_test.rs",
        ),
        ("target/debug/fictional", "target"),
    ] {
        let mut fixture = Fixture::new();
        write(
            &fixture.root,
            ".gitignore",
            b".DS_Store\nnode_modules\ntarget\n*_test.rs\n",
        );
        fixture.commit_all();
        write(&fixture.root, path, b"untracked\n");
        assert_rejects(&fixture, needle);
    }
}

#[test]
fn rejects_empty_untracked_directory() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("empty")).expect("should create directory");
    assert_rejects(&fixture, "empty");
}

#[test]
fn rejects_unreviewed_tracked_symlink() {
    let mut fixture = Fixture::new();
    symlink("Cargo.toml", fixture.root.join("manifest-link")).expect("should create symlink");
    fixture.commit_all();
    assert_rejects(&fixture, "manifest-link");
}

#[test]
fn rejects_tracked_submodule() {
    let mut fixture = Fixture::new();
    let gitlink = format!("160000,{},vendor/module", fixture.commit);
    fixture.git(&["update-index", "--add", "--cacheinfo", &gitlink]);
    fixture.git(&["commit", "-q", "-m", "submodule"]);
    fixture.commit = fixture.git(&["rev-parse", "HEAD"]);
    assert_rejects(&fixture, "vendor/module");
}

type Mutation = fn(&Fixture);

#[test]
fn verify_unchanged_detects_mutations_after_snapshot() {
    let mutations: [(&str, Mutation); 5] = [
        ("content", |fixture| {
            write(&fixture.root, "AGENTS.md", b"# Changed\n")
        }),
        ("mode", |fixture| {
            fs::set_permissions(
                fixture.root.join("Cargo.lock"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("should chmod");
        }),
        ("symlink", |fixture| {
            fs::remove_file(fixture.root.join("Cargo.lock")).expect("should remove");
            symlink("Cargo.toml", fixture.root.join("Cargo.lock")).expect("should link");
        }),
        ("hard link", |fixture| {
            fs::hard_link(
                fixture.root.join("Cargo.lock"),
                fixture.parent.path().join("late-link"),
            )
            .expect("should hard link");
        }),
        ("new file", |fixture| {
            write(&fixture.root, "late.txt", b"late\n")
        }),
    ];
    for (label, mutate) in mutations {
        let fixture = Fixture::new();
        let source = fixture.authenticate().expect("should authenticate");
        mutate(&fixture);
        assert!(
            source.snapshot().verify_unchanged().is_err(),
            "should detect {label} mutation"
        );
    }
}

#[test]
fn closure_requires_every_listed_member_manifest() {
    let mut fixture = Fixture::new();
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/trusted-server-openrtb\", \"crates/missing\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "crates/missing/Cargo.toml");
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/trusted-server-release\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "crates/trusted-server-openrtb/Cargo.toml");
    write(
        &fixture.root,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/*\"]\n",
    );
    fixture.commit_all();
    assert_rejects(&fixture, "glob");
}

#[test]
fn closure_digest_changes_with_helper_sources() {
    let mut fixture = Fixture::new();
    let before = fixture.authenticate().expect("should authenticate");
    write(
        &fixture.root,
        "crates/trusted-server-release/src/main.rs",
        b"fn main() { }\n",
    );
    fixture.commit_all();
    let after = fixture
        .authenticate()
        .expect("should authenticate changed helper");
    assert_ne!(
        before.closure_digest(),
        after.closure_digest(),
        "should invalidate the closure when helper source changes"
    );
    write(
        &fixture.root,
        "crates/other/src/lib.rs",
        b"// changed outside closure\n",
    );
    fixture.commit_all();
    let outside = fixture
        .authenticate()
        .expect("should authenticate outside change");
    assert_eq!(
        after.closure_digest(),
        outside.closure_digest(),
        "should ignore sources outside the helper closure"
    );
}

#[test]
fn parses_committer_epoch_strictly() {
    let header =
        "tree 0000000000000000000000000000000000000000\nauthor A <a@example.com> 1 +0000\n";
    let valid = format!("{header}committer C <c@example.com> 1700000000 -0130\n\nmessage\n");
    assert_eq!(
        committer_epoch(valid.as_bytes()).expect("should parse committer epoch"),
        EPOCH
    );
    for committer in [
        "",
        "committer C <c@example.com> +1700000000 +0000\n",
        "committer C <c@example.com> 17e8 +0000\n",
        "committer C <c@example.com> 1700000000 0000\n",
        "committer C <c@example.com> 1700000000\n",
        "committer C <c@example.com> 1 +0000\ncommitter D <d@example.com> 2 +0000\n",
    ] {
        let raw = format!("{header}{committer}\nmessage\n");
        assert!(
            committer_epoch(raw.as_bytes()).is_err(),
            "should reject committer line {committer:?}"
        );
    }
}

#[test]
fn joins_inventory_only_to_the_approved_commit() {
    let fixture = Fixture::new();
    let source = fixture.authenticate().expect("should authenticate");
    let inventory = |commit: &str| {
        serde_json::to_vec(&json!({
            "materials": [],
            "modules": [],
            "schema_version": 1,
            "source_commit": commit,
            "tools": [],
        }))
        .expect("should encode inventory")
    };
    join_inventory(&source, &inventory(&fixture.commit)).expect("should join matching inventory");
    assert!(
        join_inventory(&source, &inventory(&"a".repeat(40))).is_err(),
        "should reject a different inventory commit"
    );
    let mut extra: Value =
        serde_json::from_slice(&inventory(&fixture.commit)).expect("should decode inventory");
    extra["approved"] = json!(true);
    assert!(
        join_inventory(&source, &serde_json::to_vec(&extra).expect("should encode")).is_err(),
        "should reject unknown inventory fields"
    );
}

#[test]
fn child_environment_takes_commit_and_epoch_from_approved_source() {
    let fixture = Fixture::new();
    let audited = fixture.audited();
    let source = fixture.authenticate().expect("should authenticate");
    let child = child_environment(
        &audited,
        &source,
        &ChildRequest {
            target: ReleaseTarget::Wasm32Wasip1,
            home: PathBuf::from("/private/release/home"),
            cargo_home: PathBuf::from("/private/release/cargo-home"),
            tool_directories: Vec::new(),
        },
    )
    .expect("should build child environment");
    assert_eq!(
        child.variables().get("TRUSTED_SERVER__GIT_VERSION"),
        Some(&fixture.commit),
        "should inject the approved commit"
    );
    assert_eq!(
        child
            .variables()
            .get("SOURCE_DATE_EPOCH")
            .map(String::as_str),
        Some("1700000000"),
        "should inject the approved committer epoch"
    );
}
