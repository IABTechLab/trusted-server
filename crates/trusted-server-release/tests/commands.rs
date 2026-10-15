use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const RELEASE_BIN: &str = env!("CARGO_BIN_EXE_trusted-server-release");
const REPOSITORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const COHORT: &str = "scripts/release/release-cohort.json";
const TOOLS: &str = "scripts/release/release-tools.json";
const CLI_POLICY: &str = "crates/trusted-server-cli/release-policy.json";
const BOOTSTRAP: &str = "scripts/release/bootstrap.sh";

/// Preflight rejects any ancestor Cargo configuration layer, so fixtures need a
/// temporary directory whose ancestry has none; report that host precondition.
/// Integration tests cannot import the crate-private unit-test fixture, so
/// this helper and the Git fixture setup are intentionally duplicated here.
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

fn tracked_input(path: &str) -> Vec<u8> {
    fs::read(Path::new(REPOSITORY).join(path)).expect("should read tracked release input")
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("should have parent")).expect("should create dirs");
    fs::write(path, bytes).expect("should write fixture file");
}

/// A clean fixture repository holding the real tracked release inputs.
struct SourceFixture {
    parent: TempDir,
    root: PathBuf,
    commit: String,
}

impl SourceFixture {
    fn new(customize: impl FnOnce(&Path)) -> Self {
        let parent = tempdir().expect("should create fixture parent");
        assert_clean_ancestry(parent.path());
        let root = parent.path().join("source");
        for path in [COHORT, TOOLS, CLI_POLICY] {
            write(&root, path, &tracked_input(path));
        }
        write(
            &root,
            "Cargo.toml",
            b"[workspace]\nmembers = [\n    \"crates/trusted-server-openrtb\",\n    \"crates/trusted-server-release\",\n]\n",
        );
        write(&root, "Cargo.lock", b"# fictional lock\nversion = 4\n");
        write(
            &root,
            "rust-toolchain.toml",
            b"[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write(&root, ".cargo/config.toml", b"[alias]\n");
        write(&root, ".tool-versions", b"rust 1.95.0\n");
        write(&root, BOOTSTRAP, b"#!/bin/sh\nexit 1\n");
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
        write(&root, "AGENTS.md", b"# Fictional agents\n");
        symlink("AGENTS.md", root.join("CLAUDE.md")).expect("should create reviewed symlink");
        customize(&root);
        let fixture = Self {
            parent,
            root,
            commit: String::new(),
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-q", "-m", "fixture"]);
        Self {
            commit: fixture.git(&["rev-parse", "HEAD"]),
            ..fixture
        }
    }

    fn git(&self, args: &[&str]) -> String {
        let home = self.parent.path().join("fixture-home");
        fs::create_dir_all(&home).expect("should create fixture home");
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", host_path())
            .env("HOME", &home)
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fictional Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "Fictional Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

    fn release(&self, args: &[&str], environment: &[(&str, &str)]) -> Output {
        Command::new(RELEASE_BIN)
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", host_path())
            .envs(environment.iter().copied())
            .output()
            .expect("should run release producer")
    }

    fn preflight(&self, require_qualified: bool, environment: &[(&str, &str)]) -> Output {
        let mut args = vec![
            "preflight",
            "--cohort",
            COHORT,
            "--source-commit",
            &self.commit,
        ];
        if require_qualified {
            args.push("--require-qualified");
        }
        self.release(&args, environment)
    }

    fn listing(&self) -> BTreeSet<PathBuf> {
        let mut paths = BTreeSet::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).expect("should list fixture") {
                let path = entry.expect("should read fixture entry").path();
                if fs::symlink_metadata(&path)
                    .expect("should inspect fixture entry")
                    .is_dir()
                {
                    pending.push(path.clone());
                }
                paths.insert(path);
            }
        }
        paths
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_failed_without_output(output: &Output, needle: &str) {
    let text = stderr(output);
    assert!(!output.status.success(), "should fail closed: {text}");
    assert!(output.stdout.is_empty(), "should emit no stdout on failure");
    assert!(
        text.contains(needle),
        "should explain failure with `{needle}`, got: {text}"
    );
}

#[test]
fn producer_dispatch_remains_closed_for_every_invocation() {
    let root = tempdir().expect("should create private process fixture");
    fs::write(root.path().join("sentinel"), b"untouched").expect("should create sentinel");
    for arguments in [
        vec![],
        vec!["--help"],
        vec!["--version"],
        vec!["help"],
        vec!["preflight", "--require-qualified"],
        vec!["schema", "--schema-file", "override.json"],
        vec!["publish"],
    ] {
        let output = Command::new(RELEASE_BIN)
            .args(&arguments)
            .current_dir(root.path())
            .env_clear()
            .output()
            .expect("should run closed producer binary");
        assert!(
            !output.status.success(),
            "should reject pending invocation {arguments:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "should emit no result or release bytes"
        );
        assert!(
            stderr(&output).contains("release execution remains disabled"),
            "should explain pending execution gate"
        );
        if !arguments.is_empty() {
            assert!(
                stderr(&output).contains("Invalid release invocation"),
                "should report a usage error for {arguments:?}"
            );
            assert!(
                stderr(&output).contains("invalid invocation:"),
                "should surface the argument error kind for {arguments:?}"
            );
        }
        assert_eq!(
            fs::read_dir(root.path())
                .expect("should inspect process writes")
                .count(),
            1,
            "should create no release output"
        );
    }
    assert_eq!(
        fs::read(root.path().join("sentinel")).expect("should read sentinel"),
        b"untouched",
        "should preserve existing operator files"
    );
}

#[test]
fn inherited_approval_labels_cannot_enable_preflight() {
    let fixture = SourceFixture::new(|_| {});
    let output = fixture.preflight(
        true,
        &[
            ("TS_RELEASE_BUILD", "1"),
            ("TS_RELEASE_SOURCE_COMMIT", &"a".repeat(40)),
            ("RELEASE_AUTHENTICATED", "true"),
            ("RELEASE_QUALIFIED", "true"),
        ],
    );
    assert_failed_without_output(&output, "not qualified");
}

#[test]
fn clean_fixture_preflight_fails_at_closed_selector_without_writes() {
    let fixture = SourceFixture::new(|_| {});
    let before = fixture.listing();
    let output = fixture.preflight(true, &[]);
    assert_failed_without_output(&output, "Production release selection is not qualified");
    assert!(
        stderr(&output).contains(
            "source matched locally; git executable and remote eligibility not established"
        ),
        "should pass every earlier check before the selector without overclaiming"
    );
    assert_eq!(fixture.listing(), before, "should write no files");
}

#[test]
fn qualified_looking_tools_still_fail_at_closed_receipt_check() {
    let fixture = SourceFixture::new(|root| {
        let digest = "ab".repeat(32);
        let mut tools: Value =
            serde_json::from_slice(&tracked_input(TOOLS)).expect("should parse tools");
        for section in ["environments", "recipes", "tools"] {
            for entry in tools[section]
                .as_array_mut()
                .expect("should contain entries")
            {
                for (key, field) in entry.as_object_mut().expect("should be an object") {
                    if key == "qualification" {
                        *field = json!("qualified");
                    } else if field.is_null() && key.ends_with("sha256") {
                        *field = json!(digest);
                    } else if field.is_null() && key != "revision" {
                        *field = json!("fictional.example.com/qualified");
                    }
                }
            }
        }
        write(
            root,
            TOOLS,
            &serde_json::to_vec(&tools).expect("should encode tools"),
        );
    });
    let output = fixture.preflight(true, &[]);
    assert_failed_without_output(&output, "production selection remains closed");
}

#[test]
fn omitting_require_qualified_still_fails() {
    let fixture = SourceFixture::new(|_| {});
    let output = fixture.preflight(false, &[]);
    assert_failed_without_output(&output, "Production release selection is not qualified");
}

#[test]
fn empty_skip_flag_fails_naming_the_variable() {
    let fixture = SourceFixture::new(|_| {});
    let output = fixture.preflight(true, &[("TSJS_SKIP_BUILD", "")]);
    assert_failed_without_output(&output, "TSJS_SKIP_BUILD");
}

#[test]
fn untracked_extensionless_rust_toolchain_fails() {
    let fixture = SourceFixture::new(|_| {});
    write(&fixture.root, "rust-toolchain", b"nightly\n");
    let output = fixture.preflight(true, &[]);
    assert_failed_without_output(&output, "rust-toolchain");
}

#[test]
fn wrong_cohort_path_fails() {
    let fixture = SourceFixture::new(|root| {
        write(root, "other/release-cohort.json", &tracked_input(COHORT));
    });
    let outside = fixture.parent.path().join("release-cohort.json");
    for cohort in [
        "other/release-cohort.json",
        "scripts/release/../release/release-cohort.json",
        outside.to_str().expect("should be UTF-8"),
    ] {
        let output = fixture.release(
            &[
                "preflight",
                "--cohort",
                cohort,
                "--source-commit",
                &fixture.commit,
                "--require-qualified",
            ],
            &[],
        );
        assert_failed_without_output(&output, "--cohort must name");
    }
}

#[test]
fn candidate_cohort_listing_its_own_rc_branch_fails() {
    let fixture = SourceFixture::new(|root| {
        let mut cohort: Value =
            serde_json::from_slice(&tracked_input(COHORT)).expect("should parse cohort");
        cohort["eligible_branches"] = json!(["main", "rc/202610"]);
        write(
            root,
            COHORT,
            &serde_json::to_vec(&cohort).expect("should encode cohort"),
        );
    });
    let output = fixture.preflight(true, &[]);
    assert_failed_without_output(&output, "eligible_branches");
}

#[test]
fn fsmonitor_config_fails_without_executing_it() {
    let fixture = SourceFixture::new(|_| {});
    let marker = fixture.parent.path().join("fsmonitor-ran");
    let hook = fixture.parent.path().join("fsmonitor.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display()))
        .expect("should write fsmonitor command");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("should chmod");
    let config = fixture.root.join(".git/config");
    let mut text = fs::read_to_string(&config).expect("should read config");
    text.push_str(&format!("[core]\n\tfsmonitor = {}\n", hook.display()));
    fs::write(&config, text).expect("should write config");
    let output = fixture.preflight(true, &[]);
    assert_failed_without_output(&output, "git config");
    assert!(
        !marker.exists(),
        "should never execute the fsmonitor command"
    );
}

#[test]
fn audit_inputs_reports_incomplete_with_distinct_status() {
    let fixture = SourceFixture::new(|_| {});
    let output = fixture.release(&["audit-inputs"], &[]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "should use the incomplete status"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("should print JSON report");
    assert_eq!(report["status"], json!("incomplete"));
    let not_established = report["not_established"].to_string();
    for fact in [
        "git executable identity (Task 8)",
        "rustup override/toolchain selection (bootstrap, Task 8)",
    ] {
        assert!(
            not_established.contains(fact),
            "should list {fact} as not established"
        );
    }
    assert!(
        report["pending"]
            .as_array()
            .is_some_and(|pending| !pending.is_empty()),
        "should list pending pins"
    );
    assert_eq!(
        report["inputs"].as_array().map(Vec::len),
        Some(3),
        "should list input digests"
    );
}

fn bootstrap(directory: &Path) -> Output {
    Command::new("/bin/sh")
        .arg(Path::new(REPOSITORY).join(BOOTSTRAP))
        .current_dir(directory)
        .env_clear()
        .output()
        .expect("should run bootstrap")
}

#[test]
fn bootstrap_rejects_extensionless_rust_toolchain_before_closing() {
    let root = tempdir().expect("should create bootstrap fixture");
    let closed = bootstrap(root.path());
    assert!(!closed.status.success(), "should stay closed");
    assert!(
        stderr(&closed).contains("pending"),
        "should explain the closed launcher"
    );
    fs::write(root.path().join("rust-toolchain"), b"nightly\n").expect("should write pin");
    let rejected = bootstrap(root.path());
    assert!(!rejected.status.success(), "should reject the pin");
    assert!(
        stderr(&rejected).contains("rust-toolchain"),
        "should name the extensionless toolchain file"
    );
}

#[test]
fn bootstrap_checks_rust_toolchain_at_its_repository_root() {
    let tree = tempdir().expect("should create bootstrap tree");
    let script = tree.path().join(BOOTSTRAP);
    fs::create_dir_all(script.parent().expect("should have parent")).expect("should create dirs");
    fs::copy(Path::new(REPOSITORY).join(BOOTSTRAP), &script).expect("should copy bootstrap");
    fs::write(tree.path().join("rust-toolchain"), b"nightly\n").expect("should write pin");
    let elsewhere = tempdir().expect("should create unrelated working directory");
    let output = Command::new("/bin/sh")
        .arg(&script)
        .current_dir(elsewhere.path())
        .env_clear()
        .output()
        .expect("should run bootstrap");
    assert!(!output.status.success(), "should stay closed");
    assert!(
        stderr(&output).contains("rust-toolchain"),
        "should reject the repository-root pin even from another working directory"
    );
}
