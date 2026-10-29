//! Host checks of actual owning build scripts and the compiled CLI binary.

#![cfg(not(target_arch = "wasm32"))]

use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use trusted_server_cli::BUILD_INFO;
use trusted_server_core::build_info::BuildVariant;

const SOURCE: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("should resolve crates")
        .parent()
        .expect("should resolve workspace")
}

fn compile_build_script(owner: &str, directory: &TempDir) -> PathBuf {
    let output = directory.path().join(format!("{owner}-build"));
    let compiled = Command::new("rustc")
        .args(["--edition=2024"])
        .arg(workspace().join("crates").join(owner).join("build.rs"))
        .arg("-o")
        .arg(&output)
        .output()
        .expect("should run rustc");
    assert!(
        compiled.status.success(),
        "should compile actual owner script: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    output
}

fn owner_command(script: &Path) -> Command {
    let mut command = Command::new(script);
    command
        .env("TARGET", "example-target")
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env_remove("TRUSTED_SERVER__GIT_VERSION")
        .env_remove("CARGO_FEATURE_REUSABLE_SANDBOX");
    command
}

#[cfg(unix)]
#[test]
fn actual_owner_scripts_reject_invalid_release_controls_and_sources() {
    let directory = tempfile::tempdir().expect("should create fixture");
    for owner in ["trusted-server-cli", "trusted-server-adapter-fastly"] {
        let script = compile_build_script(owner, &directory);
        for control in [
            OsStr::new(""),
            OsStr::new("0"),
            OsStr::new(" 1"),
            OsStr::new("1\n"),
            OsStr::from_bytes(b"\xff"),
        ] {
            let output = owner_command(&script)
                .env("TRUSTED_SERVER__RELEASE_BUILD", control)
                .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
                .output()
                .expect("should run actual owner script");
            assert!(
                !output.status.success(),
                "should reject control for {owner}: {control:?}"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("TRUSTED_SERVER__RELEASE_BUILD must be exactly 1"),
                "should identify malformed release control"
            );
        }
        for source in [
            None,
            Some(OsStr::new("main")),
            Some(OsStr::new("v1.2.3")),
            Some(OsStr::new("abcdef")),
            Some(OsStr::new("ABCDEF0123456789abcdef0123456789abcdef01")),
            Some(OsStr::new(" abcdef0123456789abcdef0123456789abcdef01")),
            Some(OsStr::new("abcdef0123456789abcdef0123456789abcdef01\n")),
            Some(OsStr::from_bytes(b"\xff")),
        ] {
            let mut command = owner_command(&script);
            command.env("TRUSTED_SERVER__RELEASE_BUILD", "1");
            if let Some(source) = source {
                command.env("TRUSTED_SERVER__GIT_VERSION", source);
            }
            let output = command.output().expect("should run actual owner script");
            assert!(
                !output.status.success(),
                "should reject source for {owner}: {source:?}"
            );
            let diagnostic = if source.and_then(OsStr::to_str).is_none() {
                "release builds require TRUSTED_SERVER__GIT_VERSION"
            } else {
                "release source must be exactly 40 lowercase ASCII hexadecimal bytes"
            };
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(diagnostic),
                "should identify malformed release source: {diagnostic}"
            );
        }
    }
}

#[test]
fn actual_owner_scripts_emit_only_own_target_and_cli_release_cfg() {
    let directory = tempfile::tempdir().expect("should create fixture");
    for owner in ["trusted-server-cli", "trusted-server-adapter-fastly"] {
        let script = compile_build_script(owner, &directory);
        let mut outputs = Vec::new();
        for tag in ["v1.2.3", "v1.2.4"] {
            let output = owner_command(&script)
                .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
                .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
                .env("GITHUB_REF_NAME", tag)
                .env("GITHUB_RUN_ID", tag)
                .output()
                .expect("should run release owner script");
            assert!(
                output.status.success(),
                "should accept valid release source"
            );
            let directives =
                String::from_utf8(output.stdout).expect("should emit UTF-8 directives");
            assert!(
                directives.contains("cargo:rustc-env=TRUSTED_SERVER__BUILD_TARGET=example-target"),
                "should emit the target owned by Cargo TARGET"
            );
            assert!(
                !directives.contains("cargo:rustc-env=TRUSTED_SERVER__GIT_VERSION="),
                "should consume the source fact owned by core"
            );
            assert!(
                !directives.contains("BUILD_VARIANT"),
                "should derive variant from owning feature cfg"
            );
            if owner == "trusted-server-cli" {
                assert!(
                    directives.contains("cargo:rustc-cfg=trusted_server_release_build"),
                    "should identify release mode for immutable-fact tests"
                );
            }
            outputs.push(directives);
        }
        assert_eq!(
            outputs[0], outputs[1],
            "should exclude external tags and run IDs from compiled facts"
        );
        let output = owner_command(&script)
            .output()
            .expect("should run development script");
        assert!(
            output.status.success(),
            "should preserve development builds"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains("cargo:rustc-cfg=trusted_server_release_build"),
            "should retain development build mode"
        );
    }
}

#[test]
fn actual_cli_version_reads_all_owning_facts_and_ignores_external_tags() {
    assert_eq!(
        BUILD_INFO.package_version(),
        env!("CARGO_PKG_VERSION"),
        "should own package version"
    );
    assert_eq!(
        BUILD_INFO.source(),
        trusted_server_core::constants::TS_GIT_VERSION,
        "should consume the source owned by core"
    );
    assert_eq!(
        BUILD_INFO.target(),
        env!("TRUSTED_SERVER__BUILD_TARGET"),
        "should own Cargo target"
    );
    assert_eq!(
        BUILD_INFO.variant(),
        BuildVariant::Default,
        "should use CLI default variant"
    );
    // CI compiles this test once in release mode; the supplied commit must
    // reach the actual CLI unchanged.
    assert_eq!(
        cfg!(trusted_server_release_build),
        option_env!("TRUSTED_SERVER__RELEASE_BUILD").is_some(),
        "should compile in release mode exactly when the release control is supplied"
    );
    if cfg!(trusted_server_release_build) {
        assert_eq!(
            BUILD_INFO.source(),
            option_env!("TRUSTED_SERVER__GIT_VERSION"),
            "should compile the exact supplied release source into the actual CLI"
        );
    }
    let directory = tempfile::tempdir().expect("should create outside-checkout fixture");
    for tag in ["v1.2.3", "v1.2.4"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ts"))
            .current_dir(directory.path())
            .arg("--version")
            .env("GITHUB_REF_NAME", tag)
            .env("GITHUB_RUN_ID", tag)
            .output()
            .expect("should run actual CLI binary");
        assert!(
            output.status.success(),
            "should print version outside checkout"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!(
                "ts package={} source={} target={} variant=default\n",
                BUILD_INFO.package_version(),
                BUILD_INFO.source().unwrap_or("unknown"),
                BUILD_INFO.target()
            ),
            "should label all four actual immutable facts"
        );
    }
}

#[test]
fn help_and_config_init_work_outside_the_checkout() {
    let directory = tempfile::tempdir().expect("should create outside-checkout fixture");
    let binary = env!("CARGO_BIN_EXE_ts");
    let output = Command::new(binary)
        .current_dir(directory.path())
        .arg("--help")
        .output()
        .expect("should run portable CLI");
    assert!(
        output.status.success(),
        "should print help outside checkout"
    );
    let output = Command::new(binary)
        .current_dir(directory.path())
        .args(["config", "init"])
        .output()
        .expect("should initialize scaffold outside checkout");
    assert!(
        output.status.success(),
        "should initialize embedded example config"
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("trusted-server.toml"))
            .expect("should read config scaffold"),
        include_str!("../../../trusted-server.example.toml"),
        "should use tracked fictional scaffold"
    );
}
