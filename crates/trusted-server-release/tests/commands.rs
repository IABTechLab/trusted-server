use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use flate2::Crc;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

const RELEASE_BIN: &str = env!("CARGO_BIN_EXE_trusted-server-release");

#[test]
fn release_package_is_deterministic_and_inspectable() {
    let temporary = tempdir().expect("should create package fixture");
    let root = temporary.path();
    let wasm = b"\0asm\x01\0\0\0";
    let mut packages = Vec::new();
    for (name, mode) in [("first", 0o600), ("second", 0o755)] {
        let directory = root.join(name);
        fs::create_dir(&directory).expect("should create role directory");
        let input = directory.join("input.wasm");
        fs::write(&input, wasm).expect("should write wasm");
        fs::set_permissions(&input, fs::Permissions::from_mode(mode))
            .expect("should vary source mode");
        let output = directory.join("trusted-server-fastly.tar.gz");
        let result = Command::new(RELEASE_BIN)
            .args(["pack-fastly-package", "--wasm"])
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .output()
            .expect("should build package");
        assert!(
            result.status.success(),
            "should build deterministic package: {}",
            stderr(&result)
        );
        packages.push(fs::read(&output).expect("should read package"));
        let manifest = directory.join("fastly.toml");
        fs::write(
            &manifest,
            b"manifest_version = 3\nname = \"trusted-server-fastly\"\n",
        )
        .expect("should write fixed manifest");
        let result = Command::new(RELEASE_BIN)
            .arg("inspect-fastly-package")
            .arg("--package-dir")
            .arg(&directory)
            .arg("--manifest")
            .arg(&manifest)
            .args([
                "--expected-manifest-sha256",
                &hex::encode(Sha256::digest(
                    fs::read(&manifest).expect("should read manifest"),
                )),
                "--expected-wasm-sha256",
                &hex::encode(Sha256::digest(wasm)),
            ])
            .output()
            .expect("should inspect package");
        assert!(
            result.status.success(),
            "should accept generated package: {}",
            stderr(&result)
        );
        let again = Command::new(RELEASE_BIN)
            .args(["pack-fastly-package", "--wasm"])
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .output()
            .expect("should attempt duplicate output");
        assert!(
            !again.status.success(),
            "should refuse overwriting a package"
        );
    }
    assert_eq!(
        packages[0], packages[1],
        "should normalize all packaging metadata"
    );
    assert_eq!(&packages[0][4..8], &[0; 4], "should omit gzip timestamp");
}

#[test]
fn deferred_producer_commands_are_not_registered() {
    for command in [
        "preflight",
        "audit-inputs",
        "boundary-probe",
        "gnu-image-measurement",
    ] {
        let output = Command::new(RELEASE_BIN)
            .arg(command)
            .output()
            .expect("should invoke release tool");
        assert!(
            !output.status.success(),
            "should refuse deferred command {command}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("Invalid release tool invocation"),
            "should leave {command} unregistered: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

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

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("should have parent")).expect("should create parent");
    fs::write(path, bytes).expect("should write fixture");
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ordinary_measurement(root: &Path, node_body: &str) -> Output {
    ordinary_measurement_with(root, node_body, |_, _| {})
}

/// Runs the measurement after `adjust` may change the tools or the request.
fn ordinary_measurement_with(
    root: &Path,
    node_body: &str,
    adjust: impl FnOnce(&Path, &mut Value),
) -> Output {
    ordinary_measurement_command(root, node_body, adjust)
        .output()
        .expect("should execute fixed measurement command only in CI")
}

/// Builds the fixture and returns the unlaunched measurement command.
fn ordinary_measurement_command(
    root: &Path,
    node_body: &str,
    adjust: impl FnOnce(&Path, &mut Value),
) -> Command {
    let checkout = root.join("workspace/ordinary-canonical-source");
    let target = root.join("temp/ordinary-fastly-canonical-target");
    let cargo_home = root.join("temp/ordinary-canonical-cargo-home");
    fs::create_dir_all(&checkout).expect("should create ordinary source root");
    fs::create_dir_all(&target).expect("should create ordinary target root");
    fs::create_dir_all(&cargo_home).expect("should create the private Cargo home");
    for name in [
        "scripts/release/phase1-build.sh",
        "scripts/release/fastly.toml",
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
        .env("CARGO_HOME", &cargo_home)
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
    write(
        &checkout,
        "release-assets/trusted-server-fastly.tar.gz",
        b"package",
    );
    let mut request = json!({
        "role":"canonical","source_commit":"a".repeat(40),
        "environment":{
            "checkout_root":checkout,"target_root":target,"cargo_home":cargo_home,"started_at_seconds":1700000000_u64,
            "umask":18,"jobs":4,"locale":"C","timezone":"UTC",
            "uid":fs::metadata(&checkout).expect("should inspect actual fixture owner").uid()
        },
        "rustc":checkout.join("tools/rustc"),"node":checkout.join("tools/node"),
        "cargo_messages":target.join("messages.jsonl"),"cargo_metadata":target.join("metadata.json"),
        "rustflags":root.join("arguments/rustflags.json")
    });
    adjust(&checkout, &mut request);
    let request_path = root.join("request.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&request).expect("should encode capture"),
    )
    .expect("should retain capture");
    let mut command = Command::new(RELEASE_BIN);
    command
        .current_dir(root)
        .env_clear()
        .env("GH_TOKEN", "fictional-credential-canary")
        .arg("ordinary-measure")
        .arg("--request")
        .arg(request_path)
        .arg("--output")
        .arg(root.join("observed"));
    command
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
fn ordinary_measurement_command_waits_for_children_despite_inherited_ignored_sigchld() {
    let temporary = tempdir().expect("should create process fixture");
    let root = fs::canonicalize(temporary.path()).expect("should resolve process fixture");
    let mut command = ordinary_measurement_command(&root, "printf 'v24.20.0\\n'", |_, _| {});
    // SAFETY: the closure runs in the forked child before exec and only calls
    // `signal`, which is async-signal-safe. An ignored SIGCHLD survives exec,
    // so the producer starts the way an ignoring parent would launch it.
    unsafe {
        command.pre_exec(|| {
            if libc::signal(libc::SIGCHLD, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let result = command
        .output()
        .expect("should execute fixed measurement command only in CI");
    assert!(
        result.status.success(),
        "should keep tool-query children waitable under an inherited ignored SIGCHLD: {}",
        stderr(&result)
    );
    let observed: Value = serde_json::from_slice(
        &fs::read(root.join("observed/fastly-observation.json"))
            .expect("should retain unpublished observation"),
    )
    .expect("should parse real observation");
    assert_eq!(
        observed["tools"]["node"]["version"], "v24.20.0",
        "should measure child version output under an inherited ignored SIGCHLD"
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

#[test]
fn ordinary_measurement_command_rejects_unsafe_tool_selection_and_claimed_owner() {
    type Adjust = fn(&Path, &mut Value);
    let cases: [(&str, &str, Adjust, &str); 6] = [
        (
            "wrapper name",
            "printf 'v24.20.0\\n'",
            |checkout, request| {
                let renamed = checkout.join("tools/node-wrapper");
                fs::rename(checkout.join("tools/node"), &renamed).expect("should rename node");
                request["node"] = json!(renamed);
            },
            "not a shim",
        ),
        (
            "symlinked shim",
            "printf 'v24.20.0\\n'",
            |checkout, request| {
                let shim = checkout.join("tools/shim");
                fs::create_dir(&shim).expect("should create shim directory");
                std::os::unix::fs::symlink(checkout.join("tools/node"), shim.join("node"))
                    .expect("should link shim");
                request["node"] = json!(shim.join("node"));
            },
            "not a shim",
        ),
        (
            "non-executable mode",
            "printf 'v24.20.0\\n'",
            |checkout, _| {
                fs::set_permissions(
                    checkout.join("tools/node"),
                    fs::Permissions::from_mode(0o644),
                )
                .expect("should clear executable bits");
            },
            "privileged or not executable",
        ),
        (
            "setuid mode",
            "printf 'v24.20.0\\n'",
            |checkout, _| {
                fs::set_permissions(
                    checkout.join("tools/node"),
                    fs::Permissions::from_mode(0o4755),
                )
                .expect("should set the setuid bit");
            },
            "privileged or not executable",
        ),
        (
            "self-modifying tool",
            "printf 'v24.20.0\\n'; printf '#\\n' >> \"$0\"",
            |_, _| {},
            "changed while querying",
        ),
        (
            "claimed owner",
            "printf 'v24.20.0\\n'",
            |_, request| {
                let uid = request["environment"]["uid"]
                    .as_u64()
                    .expect("should record the fixture owner");
                request["environment"]["uid"] = json!(uid + 1);
            },
            "source, role or environment is invalid",
        ),
    ];
    for (label, body, adjust, reason) in cases {
        let temporary = tempdir().expect("should create unsafe tool fixture");
        let root = fs::canonicalize(temporary.path()).expect("should resolve process fixture");
        let result = ordinary_measurement_with(&root, body, adjust);
        assert!(
            !result.status.success(),
            "should reject the {label} before recording an observation"
        );
        assert!(
            stderr(&result).contains(reason),
            "should report `{reason}` for the {label}, got: {}",
            stderr(&result)
        );
        assert!(
            !root.join("observed").exists(),
            "should retain no output for the {label}"
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
fn ordinary_compare_mismatch_writes_no_companion() {
    comparison_mismatch(false);
}

#[test]
fn ordinary_package_mismatch_writes_no_companion() {
    comparison_mismatch(true);
}

fn comparison_mismatch(package_only: bool) {
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
    let different = if package_only {
        binary.clone()
    } else {
        [binary.as_slice(), b"\0\x02\x01x"].concat()
    };
    let right_package = if package_only {
        b"changed package".as_slice()
    } else {
        b"package".as_slice()
    };
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
    reproduction["environment"]["cargo_home"] =
        json!(root.join("temp/ordinary-reproduction-cargo-home"));
    reproduction["environment"]["started_at_seconds"] = json!(1_700_000_900_u64);
    reproduction["environment"]["jobs"] = json!(2);
    reproduction["environment"]["umask"] = json!(23);
    reproduction["artifact"] =
        json!({"size":different.len(),"sha256":hex::encode(Sha256::digest(&different))});
    let canonical =
        serde_json::to_vec(&canonical).expect("should encode the retained canonical facts");
    reproduction["package"] =
        json!({"size":right_package.len(),"sha256":hex::encode(Sha256::digest(right_package))});
    let reproduction = serde_json::to_vec(&reproduction)
        .expect("should encode correctly measured different final bytes");
    let archives = [
        ordinary_fixture_zip(&[
            ("trusted-server-fastly.wasm", &binary),
            ("trusted-server-fastly.wasm.spdx.json", b"{}"),
            ("trusted-server-fastly.tar.gz", b"package"),
        ]),
        ordinary_fixture_zip(&[("fastly-observation.json", &canonical)]),
        ordinary_fixture_zip(&[
            ("trusted-server-fastly.wasm", &different),
            ("trusted-server-fastly.wasm.spdx.json", b"{}"),
            ("trusted-server-fastly.tar.gz", right_package),
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
        if package_only {
            stderr(&result).contains("deployment package final-byte mismatch")
        } else {
            stderr(&result).contains("ordinary final-byte mismatch")
        },
        "should reach actual equality, not fail on fixture setup: {}",
        stderr(&result)
    );
    assert!(
        stderr(&result).starts_with("Ordinary Fastly rebuild bytes differ"),
        "should name the reproducibility failure first: {}",
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
    assert_eq!(
        diagnostic["asset"],
        if package_only {
            "trusted-server-fastly.tar.gz"
        } else {
            "trusted-server-fastly.wasm"
        },
        "should identify the artifact whose bytes differ"
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
