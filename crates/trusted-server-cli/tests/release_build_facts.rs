//! Host-only checks of the actual owning build scripts and compiled facts.

#![cfg(not(target_arch = "wasm32"))]

use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

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
    let mut compiler = Command::new("rustc");
    compiler
        .args(["--edition=2024"])
        .arg(workspace().join("crates").join(owner).join("build.rs"))
        .arg("-o")
        .arg(&output);
    if owner == "trusted-server-core" {
        // Exercise the actual core emission control flow without linking its
        // unrelated manifest-loader and template-digest dependencies.
        let source = fs::read_to_string(workspace().join("crates/trusted-server-core/build.rs"))
            .expect("should read core build script");
        let git_start = source.find("fn git(").expect("should locate Git lookup");
        let git_end = source.find("fn main()").expect("should locate core main");
        let emit_start = source
            .find("fn emit_git_version()")
            .expect("should locate core source emission");
        let emit_end = source[emit_start..]
            .find("/// Hash the core implementation")
            .expect("should locate following digest function")
            + emit_start;
        let fixture = directory.path().join("core-build.rs");
        fs::write(
            &fixture,
            format!(
                r#"
use std::env;
use std::path::Path;
use std::process::Command;
#[path = "{}"] mod git_version;
use git_version::{{Candidates, is_usable, release_source, resolve_git_version}};
const OVERRIDE_ENV: &str = "TRUSTED_SERVER__GIT_VERSION";
{}
{}
fn main() {{ emit_git_version(); }}
"#,
                workspace()
                    .join("crates/trusted-server-core/build_support/git_version.rs")
                    .display(),
                &source[git_start..git_end],
                &source[emit_start..emit_end]
            ),
        )
        .expect("should write actual core emission fixture");
        compiler = Command::new("rustc");
        compiler
            .args(["--edition=2024"])
            .arg(fixture)
            .arg("-o")
            .arg(&output);
    }
    let compiled = compiler.output().expect("should run rustc");
    assert!(
        compiled.status.success(),
        "should compile the actual owner script: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    output
}

fn owner_command(script: &Path, owner: &str, directory: &TempDir) -> Command {
    let manifest = workspace().join("crates").join(owner);
    let mut command = Command::new(script);
    command
        .current_dir(&manifest)
        .env("CARGO_MANIFEST_DIR", manifest)
        .env("OUT_DIR", directory.path())
        .env("TARGET", "example-target")
        .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
        .env_remove("TRUSTED_SERVER__GIT_VERSION")
        .env_remove("CARGO_FEATURE_REUSABLE_SANDBOX");
    command
}

#[cfg(unix)]
fn record_git(directory: &TempDir) -> PathBuf {
    let script = directory.path().join("git");
    fs::write(
        &script,
        "#!/bin/sh\nprintf invoked >> \"$GIT_RECORD\"\nexit 1\n",
    )
    .expect("should write fake git");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
        .expect("should make fake git executable");
    directory.path().join("git-record")
}

#[cfg(unix)]
#[test]
fn release_scripts_reject_invalid_inputs_without_git_fallback() {
    let directory = tempfile::tempdir().expect("should create fixture");
    let record = record_git(&directory);
    for owner in [
        "trusted-server-cli",
        "trusted-server-adapter-fastly",
        "trusted-server-core",
    ] {
        let script = compile_build_script(owner, &directory);
        for control in [
            OsStr::new(""),
            OsStr::new("0"),
            OsStr::new(" 1"),
            OsStr::new("1\n"),
            OsStr::from_bytes(b"\xff"),
        ] {
            let output = owner_command(&script, owner, &directory)
                .env("PATH", directory.path())
                .env("GIT_RECORD", &record)
                .env("TRUSTED_SERVER__RELEASE_BUILD", control)
                .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
                .output()
                .expect("should run actual script");
            assert!(
                !output.status.success(),
                "should reject invalid release control for {owner}: {control:?}"
            );
            assert!(!record.exists(), "should reject control before Git lookup");
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
            let mut command = owner_command(&script, owner, &directory);
            command
                .env("PATH", directory.path())
                .env("GIT_RECORD", &record)
                .env("TRUSTED_SERVER__RELEASE_BUILD", "1");
            if let Some(source) = source {
                command.env("TRUSTED_SERVER__GIT_VERSION", source);
            }
            let output = command.output().expect("should run actual script");
            assert!(
                !output.status.success(),
                "should reject malformed release source for {owner}: {source:?}"
            );
            assert!(!record.exists(), "should reject source before Git lookup");
        }
        let output = owner_command(&script, owner, &directory)
            .env("PATH", directory.path())
            .env("GIT_RECORD", &record)
            .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
            .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
            .output()
            .expect("should run valid release script");
        assert!(
            output.status.success(),
            "should accept valid release source for {owner}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!(
                "cargo:rustc-env=TRUSTED_SERVER__GIT_VERSION={SOURCE}"
            )),
            "should emit the exact complete source"
        );
        assert!(
            !record.exists(),
            "should emit release source without Git lookup"
        );
        if owner == "trusted-server-core" {
            let output = owner_command(&script, owner, &directory)
                .env("PATH", directory.path())
                .env("GIT_RECORD", &record)
                .output()
                .expect("should run development source discovery");
            assert!(
                output.status.success(),
                "should retain development Git fallback"
            );
            assert!(
                record.exists(),
                "should prove the Git recorder detects development lookup"
            );
        }
    }
}

#[test]
fn fastly_owns_the_reusable_sandbox_variant() {
    let directory = tempfile::tempdir().expect("should create fixture");
    let owner = "trusted-server-adapter-fastly";
    let script = compile_build_script(owner, &directory);
    for (feature, variant) in [(false, "default"), (true, "reusable-sandbox")] {
        let mut command = owner_command(&script, owner, &directory);
        command
            .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
            .env("TRUSTED_SERVER__GIT_VERSION", SOURCE);
        if feature {
            command.env("CARGO_FEATURE_REUSABLE_SANDBOX", "1");
        }
        let output = command.output().expect("should run actual Fastly script");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!(
                "cargo:rustc-env=TRUSTED_SERVER_BUILD_VARIANT={variant}"
            )),
            "should derive the variant in the owning Fastly script"
        );
        let expected = if feature {
            "ReusableSandbox"
        } else {
            "Default"
        };
        assert_eq!(
            compile_fact_payload(owner, &directory, &String::from_utf8_lossy(&output.stdout)),
            format!("9.8.7|{SOURCE}|example-target|{expected}\n"),
            "should compile the owning feature variant into the facts"
        );
    }
}

#[test]
fn owners_emit_the_cargo_target() {
    let directory = tempfile::tempdir().expect("should create fixture");
    for owner in ["trusted-server-cli", "trusted-server-adapter-fastly"] {
        let script = compile_build_script(owner, &directory);
        let output = Command::new(script)
            .env_remove("TRUSTED_SERVER__RELEASE_BUILD")
            .env_remove("TRUSTED_SERVER__GIT_VERSION")
            .env("TARGET", "example-target")
            .output()
            .expect("should run actual owner script");
        assert!(output.status.success(), "should build development facts");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("cargo:rustc-env=TRUSTED_SERVER_BUILD_TARGET=example-target"),
            "should derive the owning target from Cargo TARGET"
        );
    }
}

fn compile_fact_payload(owner: &str, directory: &TempDir, directives: &str) -> String {
    let owner_file = if owner == "trusted-server-cli" {
        "run.rs"
    } else {
        "main.rs"
    };
    let source = fs::read_to_string(
        workspace()
            .join("crates")
            .join(owner)
            .join("src")
            .join(owner_file),
    )
    .expect("should read owning source");
    let start = source
        .find("pub const BUILD_INFO:")
        .expect("should expose owning immutable facts");
    let end = source[start..]
        .find(';')
        .expect("should end owning constant")
        + start
        + 1;
    let record = &source[start..end];
    let fixture = directory.path().join("facts.rs");
    fs::write(&fixture, format!(r#"
extern crate self as trusted_server_core;
#[path = "{}"] pub mod build_info;
pub mod constants {{ pub const TS_GIT_VERSION: Option<&str> = option_env!("TRUSTED_SERVER__GIT_VERSION"); }}
use build_info::{{BuildInfo, BuildVariant}};
{record}
fn main() {{ println!("{{}}|{{}}|{{}}|{{:?}}", BUILD_INFO.package_version(), BUILD_INFO.source().expect("should have source"), BUILD_INFO.target(), BUILD_INFO.variant()); }}
"#, workspace().join("crates/trusted-server-core/src/build_info.rs").display())).expect("should write fact fixture");
    let output = directory.path().join("facts");
    let mut compiler = Command::new("rustc");
    compiler
        .args(["--edition=2024"])
        .arg(&fixture)
        .arg("-o")
        .arg(&output)
        .env("CARGO_PKG_VERSION", "9.8.7");
    for line in directives.lines() {
        if let Some(fact) = line.strip_prefix("cargo:rustc-env=") {
            let (key, value) = fact.split_once('=').expect("should emit environment fact");
            compiler.env(key, value);
            if key == "TRUSTED_SERVER_BUILD_VARIANT" && value == "reusable-sandbox" {
                compiler.args(["--cfg", "feature=\"reusable-sandbox\""]);
            }
        }
    }
    let compiled = compiler.output().expect("should compile actual facts");
    assert!(
        compiled.status.success(),
        "should compile actual owning facts: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = Command::new(output)
        .output()
        .expect("should run compiled facts");
    assert!(output.status.success(), "should read immutable facts");
    String::from_utf8(output.stdout).expect("should emit UTF-8 facts")
}

#[test]
fn external_tags_do_not_change_compiled_facts() {
    let directory = tempfile::tempdir().expect("should create fixture");
    for owner in ["trusted-server-cli", "trusted-server-adapter-fastly"] {
        let script = compile_build_script(owner, &directory);
        let mut facts = Vec::new();
        for tag in ["v1.2.3", "v1.2.4"] {
            let output = owner_command(&script, owner, &directory)
                .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
                .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
                .env("GITHUB_REF_NAME", tag)
                .env("GITHUB_RUN_ID", tag)
                .output()
                .expect("should run release owner script");
            assert!(output.status.success(), "should emit valid release facts");
            facts.push(compile_fact_payload(
                owner,
                &directory,
                &String::from_utf8_lossy(&output.stdout),
            ));
        }
        assert_eq!(
            facts[0], facts[1],
            "should keep external tags and runs out of compiled facts"
        );
        assert_eq!(
            facts[0],
            format!("9.8.7|{SOURCE}|example-target|Default\n"),
            "should use owning package version, exact source, Cargo target and variant"
        );
    }
}

#[test]
fn version_help_and_config_init_work_outside_the_checkout() {
    let directory = tempfile::tempdir().expect("should create outside checkout fixture");
    let binary = env!("CARGO_BIN_EXE_ts");
    for argument in ["--help", "--version"] {
        let output = Command::new(binary)
            .current_dir(directory.path())
            .arg(argument)
            .output()
            .expect("should run portable CLI");
        assert!(
            output.status.success(),
            "should print {argument} outside checkout"
        );
        if argument == "--version" {
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!(
                    "ts {} ({})\n",
                    env!("CARGO_PKG_VERSION"),
                    trusted_server_core::constants::TS_GIT_VERSION.unwrap_or("unknown")
                ),
                "should print package version and source"
            );
        }
    }
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
        "should use the tracked fictional scaffold"
    );
}

#[test]
fn release_resource_discovery_excludes_the_compiled_developer_checkout() {
    let directory = tempfile::tempdir().expect("should create fixture");
    let source =
        fs::read_to_string(workspace().join("crates/trusted-server-cli/src/prebid_bundle.rs"))
            .expect("should read resource discovery");
    let start = source
        .find("fn find_js_lib_dir(")
        .expect("should find discovery function");
    let end = source[start..]
        .find("fn resolve_output_dir(")
        .expect("should find following function")
        + start;
    let fixture = directory.path().join("discovery.rs");
    fs::write(&fixture, format!(r#"
use std::path::{{Path, PathBuf}};
type CliResult<T> = Result<T, String>;
fn cli_error<T>(message: impl Into<String>) -> CliResult<T> {{ Err(message.into()) }}
fn report_error(message: impl Into<String>) -> String {{ message.into() }}
{}
fn main() {{ match find_js_lib_dir(Path::new(&std::env::args().nth(1).expect("should pass directory"))) {{ Ok(path) => println!("{{}}", path.display()), Err(error) => {{ eprintln!("{{error}}"); std::process::exit(1); }} }} }}
"#, &source[start..end])).expect("should write actual discovery fixture");
    let binary = directory.path().join("discovery");
    let owner = "trusted-server-cli";
    let script = compile_build_script(owner, &directory);
    let directives = owner_command(&script, owner, &directory)
        .env("TRUSTED_SERVER__RELEASE_BUILD", "1")
        .env("TRUSTED_SERVER__GIT_VERSION", SOURCE)
        .output()
        .expect("should run release CLI build script");
    assert!(
        directives.status.success(),
        "should emit release CLI compile directives"
    );
    let directives = String::from_utf8(directives.stdout).expect("should emit UTF-8 directives");
    assert!(
        directives.contains("cargo:rustc-check-cfg=cfg(trusted_server_release_build)"),
        "should register the checked release cfg"
    );
    let mut compiler = Command::new("rustc");
    compiler
        .args(["--edition=2024"])
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .env(
            "CARGO_MANIFEST_DIR",
            workspace().join("crates/trusted-server-cli"),
        );
    for directive in directives
        .lines()
        .filter_map(|line| line.strip_prefix("cargo:rustc-cfg="))
    {
        compiler.arg("--cfg").arg(directive);
    }
    let compiled = compiler.output().expect("should compile release discovery");
    assert!(
        compiled.status.success(),
        "should compile discovery fixture: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = Command::new(&binary)
        .arg(directory.path())
        .output()
        .expect("should discover release resources");
    assert!(
        !output.status.success(),
        "should reject the existing compiled developer checkout fallback"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("run `ts prebid client` from the Trusted Server repository"),
        "should explain source prerequisites"
    );
    let bytes = fs::read(&binary).expect("should read compiled release fixture");
    let checkout = workspace().join("crates/trusted-server-cli");
    assert!(
        !bytes
            .windows(checkout.as_os_str().as_encoded_bytes().len())
            .any(|part| part == checkout.as_os_str().as_encoded_bytes()),
        "should compile the developer checkout literal out of release discovery"
    );
    let resources = directory.path().join("crates/trusted-server-js/lib");
    fs::create_dir_all(&resources).expect("should create current-checkout resources");
    fs::write(resources.join("package.json"), "{}").expect("should write package fixture");
    fs::write(resources.join("build-prebid-external.mjs"), "").expect("should write build fixture");
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).expect("should create nested working directory");
    let output = Command::new(binary)
        .arg(&nested)
        .output()
        .expect("should discover ancestor resources");
    assert!(
        output.status.success(),
        "should preserve explicit current-checkout ancestor discovery"
    );
}
