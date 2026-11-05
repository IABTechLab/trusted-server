//! Host checks of actual owning build scripts and the compiled CLI binary.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;
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
        .env_remove("TRUSTED_SERVER__GIT_VERSION");
    command
}

// Release control and source validation belongs to core's build script, which
// every product build runs; `git_version_resolve.rs` covers that rule.
#[test]
fn actual_owner_scripts_emit_only_own_target() {
    let directory = tempfile::tempdir().expect("should create fixture");
    for owner in ["trusted-server-cli"] {
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
            assert!(output.status.success(), "should run {owner} build script");
            outputs.push(String::from_utf8(output.stdout).expect("should emit UTF-8 directives"));
        }
        outputs.push(
            String::from_utf8(
                owner_command(&script)
                    .output()
                    .expect("should run development script")
                    .stdout,
            )
            .expect("should emit UTF-8 directives"),
        );
        for (index, directives) in outputs.iter().enumerate() {
            // Only the CLI selects release-only resource discovery, for the two
            // release runs.
            let cli_cfg = match (owner, index) {
                ("trusted-server-cli", 2) => {
                    "cargo:rerun-if-env-changed=TRUSTED_SERVER__RELEASE_BUILD\n\
                     cargo:rustc-check-cfg=cfg(trusted_server_release_build)\n"
                }
                ("trusted-server-cli", _) => {
                    "cargo:rerun-if-env-changed=TRUSTED_SERVER__RELEASE_BUILD\n\
                     cargo:rustc-check-cfg=cfg(trusted_server_release_build)\n\
                     cargo:rustc-cfg=trusted_server_release_build\n"
                }
                _ => "",
            };
            assert_eq!(
                directives,
                &format!(
                    "cargo:rerun-if-changed=build.rs\n{cli_cfg}\
                     cargo:rustc-env=TRUSTED_SERVER__BUILD_TARGET=example-target\n"
                ),
                "should emit only the target owned by Cargo TARGET for {owner}"
            );
        }
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
    if option_env!("TRUSTED_SERVER__RELEASE_BUILD").is_some() {
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
