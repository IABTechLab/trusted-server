//! Public release verification dispatch, without provider configuration.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use assert_cmd::Command;
use predicates::str::contains;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn command() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("ts"));
    command.env_clear().env("PATH", "/usr/bin:/bin");
    command
}

#[test]
fn required_verify_path_reaches_explicit_token_requirement() {
    command()
        .args([
            "release",
            "verify",
            "v1.2.0",
            "--asset",
            "ts-aarch64-apple-darwin",
            "--expected-source-commit",
            COMMIT,
        ])
        .assert()
        .failure()
        .stderr(contains("explicit"));
}

#[test]
fn invalid_inputs_fail_before_environment_or_provider_configuration() {
    for (tag, asset, commit, diagnostic) in [
        ("latest", "ts-aarch64-apple-darwin", COMMIT, "tag"),
        ("v1.2.0", "../../ts", COMMIT, "asset"),
        ("v1.2.0", "ts-aarch64-apple-darwin", "main", "commit"),
    ] {
        let output = command()
            .args([
                "release",
                "verify",
                tag,
                "--asset",
                asset,
                "--expected-source-commit",
                commit,
            ])
            .output()
            .expect("should run verifier");
        assert!(!output.status.success(), "should reject invalid input");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(diagnostic),
            "should identify invalid {diagnostic}: {stderr}"
        );
        assert!(
            !stderr.contains("token"),
            "should validate before token lookup: {stderr}"
        );
        assert!(
            !stderr.contains("configuration"),
            "should not load provider configuration"
        );
    }
}

#[test]
fn unsupported_public_surfaces_are_rejected() {
    for flag in [
        "--offline",
        "--offline-deploy",
        "--deploy",
        "--cache",
        "--output",
        "--export",
    ] {
        command()
            .args([
                "release",
                "verify",
                "v1.2.0",
                "--asset",
                "ts-aarch64-apple-darwin",
                "--expected-source-commit",
                COMMIT,
                flag,
            ])
            .assert()
            .failure()
            .stderr(contains("unexpected argument"));
    }
}

#[test]
fn invalid_inputs_cannot_invoke_build_provider_or_gh_hooks() {
    let directory = tempfile::tempdir().expect("should create isolated caller directory");
    let marker = directory.path().join("hook-invoked");
    let script = format!("#!/bin/sh\necho invoked > '{}'\nexit 9\n", marker.display());
    for name in ["gh", "cargo", "fastly", "edgezero"] {
        let path = directory.path().join(name);
        std::fs::write(&path, &script).expect("should write forbidden hook");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("should mark hook executable");
    }
    for name in ["edgezero.toml", "trusted-server.toml"] {
        std::fs::write(
            directory.path().join(name),
            "invalid configuration sentinel {{{",
        )
        .expect("should write unreadable configuration sentinel");
    }
    for args in [
        vec![
            "release",
            "verify",
            "v1.2.0",
            "--asset",
            "../../ts",
            "--expected-source-commit",
            COMMIT,
        ],
        vec![
            "release",
            "verify",
            "v1.2.0",
            "--asset",
            "ts-aarch64-apple-darwin",
            "--expected-source-commit",
            "main",
        ],
        vec![
            "release",
            "verify",
            "v1.2.0",
            "--asset",
            "ts-aarch64-apple-darwin",
            "--expected-source-commit",
            COMMIT,
            "--offline-deploy",
        ],
    ] {
        command()
            .current_dir(directory.path())
            .env("PATH", directory.path())
            .env("GH_TOKEN", "fictional-token")
            .args(args)
            .assert()
            .failure();
        assert!(
            !marker.exists(),
            "should invoke zero provider, build or verification hooks for invalid input"
        );
    }
}

struct OwnedVerifier {
    child: std::process::Child,
    gh_group: Option<i32>,
}

impl Drop for OwnedVerifier {
    fn drop(&mut self) {
        if let Some(group) = self.gh_group {
            // The PID belongs to the fake gh process spawned by this test.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn interrupt_during_first_gh_child_cancels_and_reaps_it() {
    let directory = tempfile::tempdir().expect("should create isolated fake gh installation");
    let pid_path = directory.path().join("owned-gh.pid");
    let gh = directory.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 30\n",
            pid_path.display()
        ),
    )
    .expect("should write stalled fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))
        .expect("should mark fake gh executable");
    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin!("ts"))
        .args([
            "release",
            "verify",
            "v1.2.0",
            "--asset",
            "ts-aarch64-apple-darwin",
            "--expected-source-commit",
            COMMIT,
        ])
        .env_clear()
        .env("PATH", directory.path())
        .env("GH_TOKEN", "fictional-read-only-token")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("should spawn verifier process");
    let mut owned = OwnedVerifier {
        child,
        gh_group: None,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pid_path.exists() && std::time::Instant::now() < deadline {
        assert!(
            owned
                .child
                .try_wait()
                .expect("should inspect verifier process")
                .is_none(),
            "should reach initial gh child before exiting"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !pid_path.exists() {
        let _ = owned.child.kill();
        let _ = owned.child.wait();
        let diagnostic =
            std::io::read_to_string(owned.child.stderr.take().expect("should own stderr pipe"))
                .expect("should collect early diagnostic");
        panic!("should reach owned gh child before deadline: {diagnostic}");
    }
    let gh_pid: i32 = std::fs::read_to_string(pid_path)
        .expect("should observe test-owned gh PID")
        .trim()
        .parse()
        .expect("should parse owned PID");
    owned.gh_group = Some(gh_pid);
    // Signal only this test-owned verifier, after its listener and child exist.
    let signalled = unsafe { libc::kill(owned.child.id() as i32, libc::SIGINT) };
    assert_eq!(signalled, 0, "should send interrupt to owned verifier");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = owned
            .child
            .try_wait()
            .expect("should inspect interrupted verifier")
        {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "should cancel verifier promptly"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(!status.success(), "should return nonzero on interrupt");
    let stderr = std::io::read_to_string(
        owned
            .child
            .stderr
            .take()
            .expect("should retain diagnostic pipe"),
    )
    .expect("should read bounded diagnostic");
    assert!(
        stderr.contains("cancelled"),
        "should report controlled cancellation: {stderr}"
    );
    // The supervised leader must have been killed and reaped, rather than orphaned.
    assert_eq!(
        unsafe { libc::kill(gh_pid, 0) },
        -1,
        "should reap initial gh process before returning"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH),
        "should leave no gh process alive"
    );
    owned.gh_group = None;
}
