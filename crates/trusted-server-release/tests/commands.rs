use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use flate2::Crc;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::{TempDir, tempdir};

const RELEASE_BIN: &str = env!("CARGO_BIN_EXE_trusted-server-release");
const REPOSITORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const COHORT: &str = "scripts/release/release-cohort.json";
const TOOLS: &str = "scripts/release/release-tools.json";
const CLI_POLICY: &str = "crates/trusted-server-cli/release-policy.json";
const BOOTSTRAP: &str = "scripts/release/bootstrap.sh";

fn manual_package_fixture(root: &Path) -> (String, String) {
    let manifest = b"manifest_version = 3\nname = \"example-app\"\n";
    let wasm = b"\0asm\x01\0\0\0";
    let directory = root.join("pkg");
    fs::create_dir(&directory).expect("should create package directory");
    fs::write(root.join("fastly.toml"), manifest).expect("should write reviewed manifest");
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (path, kind, bytes) in [
        ("example-app/", tar::EntryType::Directory, &b""[..]),
        ("example-app/bin/", tar::EntryType::Directory, &b""[..]),
        (
            "example-app/bin/main.wasm",
            tar::EntryType::Regular,
            &wasm[..],
        ),
        (
            "example-app/fastly.toml",
            tar::EntryType::Regular,
            &manifest[..],
        ),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(kind);
        header.set_mode(0o755);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive
            .append_data(&mut header, path, bytes)
            .expect("should write bounded fictional package member");
    }
    let bytes = archive
        .into_inner()
        .expect("should finish archive")
        .finish()
        .expect("should finish gzip");
    fs::write(directory.join("example-app.tar.gz"), bytes).expect("should write package");
    (
        hex::encode(Sha256::digest(manifest)),
        hex::encode(Sha256::digest(wasm)),
    )
}

fn inspect_manual_package(root: &Path, manifest: &str, wasm: &str) -> Output {
    Command::new(RELEASE_BIN)
        .current_dir(root)
        .env_clear()
        .args([
            "inspect-fastly-package",
            "--package-dir",
            "pkg",
            "--manifest",
            "fastly.toml",
            "--expected-manifest-sha256",
            manifest,
            "--expected-wasm-sha256",
            wasm,
        ])
        .output()
        .expect("should run only the local release inspector")
}

#[test]
fn manual_fastly_package_command_accepts_exact_bytes_without_release_authority() {
    let root = tempdir().expect("should create private package fixture");
    let (manifest, wasm) = manual_package_fixture(root.path());
    let output = inspect_manual_package(root.path(), &manifest, &wasm);
    assert!(
        output.status.success(),
        "should accept exact package: {}",
        stderr(&output)
    );
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("should parse inspection report");
    assert_eq!(
        report["accepted"], true,
        "should report the bounded byte checks"
    );
    assert_eq!(
        report["module_sha256"], wasm,
        "should bind the actual module member"
    );
    assert_eq!(
        report["manifest_sha256"], manifest,
        "should bind reviewed manifest bytes"
    );
    assert_eq!(
        report["limits"]["runtime_validated"], false,
        "should retain runtime limitation"
    );
    assert_eq!(
        report["limits"]["official_release_verified"], false,
        "should not mint release trust"
    );
    assert_eq!(
        report["limits"]["deployment_authorized"], false,
        "should not authorize mutation"
    );
}

#[test]
fn manual_fastly_package_command_rejects_an_unmatched_expected_digest() {
    let root = tempdir().expect("should create private package fixture");
    let (manifest, _) = manual_package_fixture(root.path());
    let output = inspect_manual_package(root.path(), &manifest, &"b".repeat(64));
    assert!(
        !output.status.success(),
        "should reject another expected executable"
    );
    assert!(
        output.stdout.is_empty(),
        "should emit no acceptance on failure"
    );
}

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
        // Finish fixture writes synchronously before taking no-write snapshots.
        let output = Command::new("git")
            .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
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
fn inherited_js_build_controls_fail_naming_the_variable() {
    let fixture = SourceFixture::new(|_| {});
    for name in ["TSJS_PREBUILT_DIR", "TSJS_SKIP_BUILD", "TSJS_TEST"] {
        for value in ["", "0", "1"] {
            let output = fixture.preflight(true, &[(name, value)]);
            assert_failed_without_output(&output, name);
        }
    }
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

/// Process fixtures stay in this CI-only integration harness, never the pure selection.
fn ordinary_measurement(root: &Path, node_body: &str) -> Output {
    let checkout = root.join("workspace/ordinary-canonical-source");
    let target = root.join("temp/ordinary-fastly-canonical-target");
    fs::create_dir_all(&checkout).expect("should create ordinary source root");
    fs::create_dir_all(&target).expect("should create ordinary target root");
    for name in [
        "scripts/release/phase1-build.sh",
        ".cargo/config.toml",
        "Cargo.toml",
        "Cargo.lock",
        "crates/trusted-server-js/lib/package-lock.json",
    ] {
        write(&checkout, name, b"fictional source input\n");
    }
    write(&checkout, ".cargo/config.toml", b"[alias]\n");
    let arguments = Command::new(RELEASE_BIN)
        .env_clear()
        .current_dir(root)
        .arg("ordinary-rustflags")
        .arg("--checkout")
        .arg(&checkout)
        .arg("--target")
        .arg(&target)
        .arg("--role")
        .arg("canonical")
        .arg("--output")
        .arg(root.join("arguments"))
        .output()
        .expect("should prepare fixed arguments only in CI");
    assert!(
        arguments.status.success(),
        "should seal actual arguments before building the fixture: {}",
        String::from_utf8_lossy(&arguments.stderr)
    );
    write(
        &checkout,
        "rust-toolchain.toml",
        b"[toolchain]\nchannel = \"1.98.1\"\n",
    );
    write(&checkout, ".tool-versions", b"nodejs 24.20.0\n");
    write(
        &checkout,
        "crates/trusted-server-js/Cargo.toml",
        b"fictional JS package\n",
    );
    write(
        &checkout,
        "crates/trusted-server-js/lib/src/core/index.ts",
        b"export {};\n",
    );
    write(
        &checkout,
        "release-assets/trusted-server-fastly.wasm",
        b"\0asm\x01\0\0\0\0\x0d\x0b.debug_info\0\0\x1b\x0a.debug_str/trusted-server\0\0\x0d\x0b.debug_line\0\0\x06\x04name\0",
    );
    let js = checkout.join("crates/trusted-server-js");
    fs::create_dir_all(js.join("lib/src/integrations"))
        .expect("should retain an empty integration module set");
    let out = target.join("wasm32-wasip1/release/build/trusted-server-js-fixture/out");
    fs::create_dir_all(out.join("tsjs-dist")).expect("should create consumed private output");
    fs::write(
        out.join("tsjs-dist/tsjs-core.js"),
        b"fictional private bundle",
    )
    .expect("should retain consumed bundle");
    let package = format!("path+file://{}#0.1.0", js.display());
    fs::write(
        target.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspace_root":checkout,"workspace_members":[package],"packages":[{
                "id":package,"name":"trusted-server-js","manifest_path":js.join("Cargo.toml")
            }]
        }))
        .expect("should encode metadata"),
    )
    .expect("should retain locked metadata fixture");
    let messages = [
        json!({"reason":"build-script-executed","package_id":package,"out_dir":out,"env":[]}),
        json!({"reason":"build-finished","success":true}),
    ];
    let lines = messages
        .iter()
        .map(|value| serde_json::to_string(value).expect("should encode Cargo line"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(target.join("messages.jsonl"), lines).expect("should retain completed Cargo output");
    for (name, body) in [
        ("rustc", "printf 'rustc 1.98.1 (abcdef123 2026-01-01)\\n'"),
        ("node", node_body),
    ] {
        let script = format!(
            "#!/bin/sh\nset -eu\ntest \"$#\" -eq 1\ntest \"$1\" = --version\ntest -z \"${{GH_TOKEN+x}}\"\ntest \"$LC_ALL/$TZ\" = C/UTC\n{body}\n"
        );
        write(&checkout, &format!("tools/{name}"), script.as_bytes());
        fs::set_permissions(
            checkout.join("tools").join(name),
            fs::Permissions::from_mode(0o755),
        )
        .expect("should set fixture executable permissions");
    }
    let request = json!({
        "role":"canonical","source_commit":"a".repeat(40),
        "environment":{
            "checkout_root":checkout,"target_root":target,"started_at_seconds":1700000000_u64,
            "umask":18,"jobs":4,"locale":"C","timezone":"UTC",
            "uid":fs::metadata(&checkout).expect("should inspect actual fixture owner").uid()
        },
        "rustc":checkout.join("tools/rustc"),"node":checkout.join("tools/node"),
        "cargo_messages":target.join("messages.jsonl"),"cargo_metadata":target.join("metadata.json"),
        "rustflags":root.join("arguments/rustflags.json")
    });
    let request_path = root.join("request.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&request).expect("should encode capture"),
    )
    .expect("should retain capture");
    Command::new(RELEASE_BIN)
        .current_dir(root)
        .env_clear()
        .env("GH_TOKEN", "fictional-credential-canary")
        .arg("ordinary-measure")
        .arg("--request")
        .arg(request_path)
        .arg("--output")
        .arg(root.join("observed"))
        .output()
        .expect("should execute fixed measurement command only in CI")
}

#[test]
fn ordinary_measurement_command_queries_only_versions_without_inherited_secrets() {
    let temporary = tempdir().expect("should create process fixture");
    let root = fs::canonicalize(temporary.path()).expect("should resolve process fixture");
    let result = ordinary_measurement(&root, "printf 'v24.20.0\\n'");
    assert!(
        result.status.success(),
        "should retain actual fixed-query measurements: {}",
        stderr(&result)
    );
    let observed: Value = serde_json::from_slice(
        &fs::read(root.join("observed/fastly-observation.json"))
            .expect("should retain unpublished observation"),
    )
    .expect("should parse real observation");
    assert_eq!(
        observed["tools"]["node"]["version"], "v24.20.0",
        "should measure child version output"
    );
    assert_eq!(
        observed["release_qualification"], false,
        "should not qualify ordinary builds"
    );
    assert!(
        !root
            .join("observed/trusted-server-fastly.wasm.rebuild-check.json")
            .exists(),
        "should not manufacture equality from one build"
    );
}

#[test]
fn ordinary_measurement_command_rejects_wrong_versions_and_sanitizes_failed_queries() {
    for body in [
        "printf 'v24.12.0\\n'",
        "printf 'fictional-query-secret\\n' >&2; exit 1",
    ] {
        let temporary = tempdir().expect("should create failing process fixture");
        let root = fs::canonicalize(temporary.path()).expect("should resolve process fixture");
        let result = ordinary_measurement(&root, body);
        assert!(
            !result.status.success(),
            "should fail on wrong version or failed query"
        );
        assert!(
            result.stdout.is_empty(),
            "should emit no acceptance on failure"
        );
        assert!(
            !stderr(&result).contains("fictional-query-secret"),
            "should not leak captured child diagnostics"
        );
        assert!(
            !root.join("observed").exists(),
            "should retain no output after failed measurement"
        );
    }
}

// The fixture uses the same narrow stored-ZIP construction as the ordinary
// archive unit tests; it invokes no archive tools or producer qualification.
fn ordinary_fixture_zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, body) in members {
        let offset = bytes.len() as u32;
        let mut crc = Crc::new();
        crc.update(body);
        let crc = crc.sum();
        let size = body.len() as u32;
        bytes.extend(0x04034b50_u32.to_le_bytes());
        bytes.extend(20_u16.to_le_bytes());
        bytes.extend([0; 8]);
        for field in [crc, size, size] {
            bytes.extend(field.to_le_bytes());
        }
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend([0; 2]);
        bytes.extend(name.as_bytes());
        bytes.extend(*body);
        central.extend(0x02014b50_u32.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend([0; 8]);
        for field in [crc, size, size] {
            central.extend(field.to_le_bytes());
        }
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let offset = bytes.len() as u32;
    let size = central.len() as u32;
    bytes.extend(central);
    bytes.extend(0x06054b50_u32.to_le_bytes());
    bytes.extend([0; 4]);
    for _ in 0..2 {
        bytes.extend((members.len() as u16).to_le_bytes());
    }
    bytes.extend(size.to_le_bytes());
    bytes.extend(offset.to_le_bytes());
    bytes.extend([0; 2]);
    bytes
}

fn bounded_negative_compare(root: &Path, source: &Path) -> Output {
    let stdout = root.join("negative.stdout");
    let stderr = root.join("negative.stderr");
    let mut child = Command::new(RELEASE_BIN)
        .env_clear()
        .current_dir(root)
        .arg("ordinary-compare")
        .arg("--input")
        .arg(root.join("input"))
        .arg("--archives")
        .arg(root.join("archives"))
        .arg("--source")
        .arg(source)
        .arg("--output")
        .arg(root.join("matched"))
        .stdout(Stdio::from(
            fs::File::create(&stdout).expect("should create bounded stdout capture"),
        ))
        .stderr(Stdio::from(
            fs::File::create(&stderr).expect("should create bounded stderr capture"),
        ))
        .spawn()
        .expect("should execute the real comparator only in CI");
    let started = Instant::now();
    let status = loop {
        let bounded = [&stdout, &stderr]
            .into_iter()
            .all(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() <= 32768));
        if !bounded || started.elapsed() > Duration::from_secs(10) {
            // Reap even if the child exited between the cap check and kill.
            let _ = child.kill();
            let reaped = child.wait();
            assert!(reaped.is_ok(), "should reap an over-bound comparator");
            assert!(
                bounded && started.elapsed() <= Duration::from_secs(10),
                "should finish the negative comparison within ten seconds and 32 KiB per stream"
            );
        }
        if let Some(status) = child.try_wait().expect("should poll the comparator") {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let read = |path: &Path| {
        assert!(
            fs::metadata(path).is_ok_and(|metadata| metadata.len() <= 32768),
            "should retain bounded post-exit output"
        );
        let mut bytes = Vec::new();
        fs::File::open(path)
            .expect("should open the retained output")
            .take(32769)
            .read_to_end(&mut bytes)
            .expect("should read bounded output");
        assert!(
            bytes.len() <= 32768,
            "should refuse growth during the bounded read"
        );
        bytes
    };
    let stdout = read(&stdout);
    let stderr = read(&stderr);
    assert!(
        started.elapsed() <= Duration::from_secs(10),
        "should finish bounded reads within the comparison deadline"
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

#[test]
fn ordinary_comparison_failure_prevents_signing_and_publication_sentinels() {
    let temporary = tempdir().expect("should create a CI-only negative fixture");
    let root = fs::canonicalize(temporary.path()).expect("should resolve the negative fixture");
    let measured = ordinary_measurement(&root, "printf 'v24.20.0\\n'");
    assert!(
        measured.status.success(),
        "should establish valid ordinary inputs before changing only final bytes: {}",
        stderr(&measured)
    );
    let source = root.join("workspace/ordinary-canonical-source");
    let binary = fs::read(source.join("release-assets/trusted-server-fastly.wasm"))
        .expect("should read the measured final fixture");
    let different = [binary.as_slice(), b"\0\x02\x01x"].concat();
    let canonical: Value = serde_json::from_slice(
        &fs::read(root.join("observed/fastly-observation.json"))
            .expect("should retain actual observation"),
    )
    .expect("should parse the actual observation");
    let mut reproduction = canonical.clone();
    reproduction["role"] = json!("reproduction");
    reproduction["environment"]["checkout_root"] =
        json!(root.join("workspace/ordinary-reproduction-source"));
    reproduction["environment"]["target_root"] =
        json!(root.join("temp/ordinary-fastly-reproduction-target"));
    reproduction["environment"]["started_at_seconds"] = json!(1_700_000_900_u64);
    reproduction["environment"]["jobs"] = json!(2);
    reproduction["environment"]["umask"] = json!(23);
    reproduction["artifact"] =
        json!({"size":different.len(),"sha256":hex::encode(Sha256::digest(&different))});
    let canonical =
        serde_json::to_vec(&canonical).expect("should encode the retained canonical facts");
    let reproduction = serde_json::to_vec(&reproduction)
        .expect("should encode correctly measured different final bytes");
    let archives = [
        ordinary_fixture_zip(&[
            ("trusted-server-fastly.wasm", &binary),
            ("trusted-server-fastly.wasm.spdx.json", b"{}"),
        ]),
        ordinary_fixture_zip(&[("fastly-observation.json", &canonical)]),
        ordinary_fixture_zip(&[
            ("trusted-server-fastly.wasm", &different),
            ("trusted-server-fastly.wasm.spdx.json", b"{}"),
        ]),
        ordinary_fixture_zip(&[("fastly-observation.json", &reproduction)]),
    ];
    let mut context = json!({"repository":"example-owner/example-server","repository_id":17,"head_repository_id":17,"source_commit":"a".repeat(40),"run_id":42,"run_attempt":1,"canonical":{},"reproduction":{}});
    let mut artifact_records = Vec::new();
    for (index, ((name, created), bytes)) in [
        ("release-trusted-server-fastly.wasm", "2023-11-14T22:13:25Z"),
        ("fastly-observation-canonical", "2023-11-14T22:13:26Z"),
        (
            "reproduction-trusted-server-fastly.wasm",
            "2023-11-14T22:28:25Z",
        ),
        ("fastly-observation-reproduction", "2023-11-14T22:28:26Z"),
    ]
    .into_iter()
    .zip(&archives)
    .enumerate()
    {
        let digest = hex::encode(Sha256::digest(bytes));
        let id = 200 + index;
        let role = if index < 2 {
            "canonical"
        } else {
            "reproduction"
        };
        let prefix = if index % 2 == 0 {
            "artifact"
        } else {
            "observation"
        };
        context[role][format!("{prefix}_id")] = json!(id);
        context[role][format!("{prefix}_digest")] = json!(digest);
        artifact_records.push(json!({"id":id,"name":name,"digest":format!("sha256:{digest}"),"size_in_bytes":bytes.len(),"expired":false,"created_at":created,"workflow_run":{"id":42,"repository_id":17,"head_repository_id":17,"head_sha":"a".repeat(40)}}));
    }
    let jobs: Vec<_> = [
        (100,"Build trusted-server-fastly.wasm","canonical","2023-11-14T22:13:10Z","2023-11-14T22:13:30Z"),
        (101,"Reproduce trusted-server-fastly.wasm","reproduction","2023-11-14T22:28:10Z","2023-11-14T22:28:30Z"),
    ].into_iter().map(|(id,name,role,start,end)| json!({"id":id,"name":format!("build / {name}"),"run_id":42,"run_attempt":1,"head_sha":"a".repeat(40),"status":"completed","conclusion":"success","started_at":start,"completed_at":end,"steps":[{"name":format!("Upload {role} release artifact"),"status":"completed","conclusion":"success"},{"name":format!("Upload {role} observation"),"status":"completed","conclusion":"success"}]})).collect();
    for (file, value) in [
        ("context.json", context),
        ("jobs.json", json!({"total_count":jobs.len(),"jobs":jobs})),
        (
            "artifacts.json",
            json!({"total_count":artifact_records.len(),"artifacts":artifact_records}),
        ),
    ] {
        write(
            &root,
            &format!("input/{file}"),
            &serde_json::to_vec(&value).expect("should encode valid synthetic service records"),
        );
    }
    for (file, bytes) in [
        "canonical.zip",
        "canonical-observation.zip",
        "reproduction.zip",
        "reproduction-observation.zip",
    ]
    .into_iter()
    .zip(archives)
    {
        write(&root, &format!("archives/{file}"), &bytes);
    }
    let result = bounded_negative_compare(&root, &source);
    if result.status.success() {
        write(&root, "signing-sentinel", b"unexpected signing stage");
        write(
            &root,
            "publication-sentinel",
            b"unexpected publication stage",
        );
    }
    assert!(
        !result.status.success(),
        "should reject complete differing final bytes"
    );
    assert!(
        stderr(&result).contains("ordinary final-byte mismatch"),
        "should reach actual equality, not fail on fixture setup: {}",
        stderr(&result)
    );
    let diagnostic: Value = serde_json::from_slice(
        &fs::read(root.join("ordinary-diagnostics/ordinary-comparison-diagnostic.json"))
            .expect("should retain the real comparator's structured failure evidence"),
    )
    .expect("should parse bounded CI-only diagnostic JSON");
    assert_eq!(
        diagnostic["outcome"], "mismatched",
        "should report actual mismatch"
    );
    assert_eq!(
        diagnostic["stage"], "bytes",
        "should reach complete-byte comparison"
    );
    assert_eq!(
        diagnostic["code"], "final_bytes_mismatch",
        "should retain fixed mismatch code"
    );
    assert!(
        result.stdout.is_empty(),
        "should emit no successful comparison output"
    );
    for path in ["matched", "signing-sentinel", "publication-sentinel"] {
        assert!(
            !root.join(path).exists(),
            "should leave no matched output or later stage after a failed exit"
        );
    }
}
