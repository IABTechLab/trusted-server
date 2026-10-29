//! Native repeatability measurement, operator value scan and Linux startup
//! acceptance, over fictional builds and outputs.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use object::elf as elf_format;

use super::super::native::{
    check_operator_values_absent, elf_section_at, measure_native_repeatability,
};
use super::super::startup::{
    LINUX_CLI_ASSET, StartupCopy, StartupDistribution, StartupImage, check_startup_output,
    expected_version_output, startup_arguments,
};
use super::complete::{Built, built};
use super::complete_darwin::darwin_executable;
use super::*;
use crate::inspect::tests::builders::{ElfSpec, elf};
use crate::process::{ProcessOutcome, Stream};

const CLI_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../trusted-server-cli/Cargo.toml"
));
const RELEASE_MANIFEST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));

/// A completed fictional build with the fixture roots its output lives in.
struct NativeBuild {
    _built: Built,
    completed: RecipeCompleted,
}

/// Completes a fictional build of `asset` for `role` with exactly `bytes`.
fn native_build(asset: &'static str, role: Role, bytes: &[u8]) -> NativeBuild {
    let mut built = built(asset, role, bytes);
    let stdout = built.stdout(|_| {});
    let completed = built.finish(stdout).expect("should complete the build");
    NativeBuild {
        _built: built,
        completed,
    }
}

/// A conforming Linux executable whose `.debug_info` holds `info`.
fn linux_executable(info: &'static [u8]) -> Vec<u8> {
    elf(&ElfSpec {
        payload: b"/trusted-server/src/main.rs",
        debug_sections: vec![
            (".debug_info", elf_format::SHT_PROGBITS, info),
            (
                ".debug_line",
                elf_format::SHT_PROGBITS,
                b"fictional dwarf lines",
            ),
        ],
        ..ElfSpec::conforming()
    })
}

#[test]
fn identical_native_pair_is_measured_as_identical() {
    let bytes = linux_executable(b"fictional dwarf info");
    let canonical = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
    let reproduction = native_build(LINUX_CLI_ASSET, Role::Reproduction, &bytes);

    let measured = measure_native_repeatability(&canonical.completed, &reproduction.completed)
        .expect("should measure the pair");

    assert!(measured.identical(), "should record identical final bytes");
    assert_eq!(
        measured.canonical(),
        measured.reproduction(),
        "should report one identity for identical bytes"
    );
    assert_eq!(
        measured.asset().artifact().name,
        LINUX_CLI_ASSET,
        "should name the measured asset"
    );
}

#[test]
fn differing_native_pair_is_recorded_not_rejected() {
    let canonical = native_build(
        LINUX_CLI_ASSET,
        Role::Canonical,
        &linux_executable(b"fictional dwarf info"),
    );
    let reproduction = native_build(
        LINUX_CLI_ASSET,
        Role::Reproduction,
        &linux_executable(b"fictional dwarf inf0"),
    );

    let measured = measure_native_repeatability(&canonical.completed, &reproduction.completed)
        .expect("should record a nonmatching native pair as a measurement");

    assert!(!measured.identical(), "should record the difference");
    let difference = measured.difference().expect("should locate the difference");
    assert_eq!(difference.differing_bytes, 1, "should count one byte");
    assert_eq!(
        difference.canonical_section.as_deref(),
        Some(".debug_info"),
        "should name the section holding the first difference"
    );
    assert_ne!(
        measured.canonical().sha256,
        measured.reproduction().sha256,
        "should report both digests"
    );
}

#[test]
fn native_measurement_rejects_fastly_and_mismatched_pairs() {
    let bytes = linux_executable(b"fictional dwarf info");
    let canonical = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
    let second_canonical = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
    assert!(
        measure_native_repeatability(&canonical.completed, &second_canonical.completed).is_err(),
        "should require a canonical then a reproduction build"
    );
    let other = native_build(
        "ts-aarch64-apple-darwin",
        Role::Reproduction,
        &darwin_executable(),
    );
    assert!(
        measure_native_repeatability(&canonical.completed, &other.completed).is_err(),
        "should compare one asset only"
    );
    let module = super::complete::wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let fastly = native_build("trusted-server-fastly.wasm", Role::Canonical, &module);
    let fastly_reproduction =
        native_build("trusted-server-fastly.wasm", Role::Reproduction, &module);
    assert!(
        measure_native_repeatability(&fastly.completed, &fastly_reproduction.completed).is_err(),
        "should leave Fastly to the equality gate"
    );
}

#[test]
fn elf_sections_are_named_only_inside_file_bytes() {
    let bytes = linux_executable(b"fictional dwarf info");
    assert_eq!(
        elf_section_at(b"fictional text, not an executable", 3),
        None,
        "should name nothing in non-ELF bytes"
    );
    assert_eq!(
        elf_section_at(&bytes, bytes.len() + 10),
        None,
        "should name nothing past the end"
    );
}

#[test]
fn planted_operator_values_must_be_absent() {
    let bytes = b"fictional rodata publisher.example.com";
    check_operator_values_absent(bytes, &["ts-operator-sentinel-0001.example"])
        .expect("should accept bytes without the planted value");
    for (label, values) in [
        ("present", vec!["publisher.example.com"]),
        ("too short", vec!["example"]),
        ("empty list", Vec::new()),
    ] {
        assert!(
            check_operator_values_absent(bytes, &values).is_err(),
            "should reject a {label} scan"
        );
    }
}

#[test]
fn startup_distributions_are_the_reviewed_tag_pinned_set() {
    let labels: Vec<&str> = StartupDistribution::ALL
        .iter()
        .map(|distribution| distribution.label())
        .collect();
    assert_eq!(labels, ["ubi9", "ubuntu-22.04", "debian-12"]);
    for distribution in StartupDistribution::ALL {
        assert_eq!(
            StartupDistribution::parse(distribution.label()).expect("should parse its label"),
            distribution,
            "should round-trip {}",
            distribution.label()
        );
        let tag = distribution
            .reference()
            .rsplit_once('/')
            .and_then(|(_, name)| name.split_once(':'))
            .map(|(_, tag)| tag);
        assert!(
            tag.is_some_and(|tag| tag != "latest" && !tag.is_empty()),
            "should pin {} by an explicit tag",
            distribution.label()
        );
    }
    assert!(StartupDistribution::parse("alpine-3").is_err());
}

#[test]
fn startup_images_are_selected_by_local_image_id_only() {
    let id = format!("sha256:{}", "a".repeat(64));
    let image = StartupImage::new(StartupDistribution::Debian12, &id).expect("should accept");
    assert_eq!(image.id(), id, "should keep the image ID");
    for rejected in [
        "docker.io/library/debian:12".to_owned(),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "a".repeat(63)),
    ] {
        assert!(
            StartupImage::new(StartupDistribution::Debian12, &rejected).is_err(),
            "should reject {rejected:?}"
        );
    }
}

#[test]
fn startup_container_is_offline_read_only_unprivileged_and_bounded() {
    let config = Path::new("/example/docker-config");
    let id = format!("sha256:{}", "b".repeat(64));
    let image = StartupImage::new(StartupDistribution::Ubi9, &id).expect("should accept");

    let arguments: Vec<String> = startup_arguments(
        config,
        &image,
        Path::new("/example/startup-copy"),
        "ts-release-startup-example",
    )
    .into_iter()
    .map(|argument| argument.into_string().expect("should be UTF-8"))
    .collect();

    assert!(
        arguments.iter().any(|argument| argument == "--init"),
        "should reap container descendants"
    );
    assert!(
        arguments
            .iter()
            .any(|argument| argument.starts_with("--name=")),
        "should own a named container for cleanup"
    );
    assert!(
        arguments.iter().any(|argument| argument == "create"),
        "should acknowledge creation before starting guest code"
    );
    assert!(
        !arguments
            .iter()
            .any(|argument| argument == "run" || argument == "--rm"),
        "should retain the created identity through explicit cleanup"
    );
    let expected: Vec<String> = [
        "--config",
        "/example/docker-config",
        "--host",
        "unix:///var/run/docker.sock",
        "create",
        "--init",
        "--name=ts-release-startup-example",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--user=65534:65534",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=64",
        "--memory=268435456",
        "--cpus=1",
        "--ulimit=cpu=30:30",
        "--workdir=/",
        "--mount=type=bind,source=/example/startup-copy,target=/opt/ts,readonly",
        "--entrypoint=/opt/ts/ts",
        id.as_str(),
        "--version",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        arguments, expected,
        "should run exactly the bounded container"
    );
}

#[test]
fn startup_output_must_be_exactly_the_build_facts_with_exit_zero() {
    let commit = CommitId::parse(COMMIT).expect("should parse the commit");
    let expected = expected_version_output(&commit, ReleaseTarget::X86_64UnknownLinuxGnu);
    assert_eq!(
        expected,
        format!(
            "ts package={} source={COMMIT} target=x86_64-unknown-linux-gnu variant=default\n",
            env!("CARGO_PKG_VERSION")
        ),
        "should print the four labeled owning build facts"
    );
    let distribution = StartupDistribution::Ubuntu2204;
    let success = ProcessOutcome::Exited { code: 0 };
    check_startup_output(distribution, success, expected.as_bytes(), &expected)
        .expect("should accept the exact version line");
    let without_newline = expected.trim_end();
    let with_fallback = format!("{expected}loaded fallback configuration\n");
    let cases: [(&str, ProcessOutcome, &[u8]); 5] = [
        ("loader failure", ProcessOutcome::Exited { code: 127 }, b""),
        ("timeout", ProcessOutcome::TimedOut, expected.as_bytes()),
        (
            "output cap",
            ProcessOutcome::OutputLimit {
                stream: Stream::Stdout,
            },
            expected.as_bytes(),
        ),
        ("truncated line", success, without_newline.as_bytes()),
        ("configuration fallback", success, with_fallback.as_bytes()),
    ];
    for (label, outcome, stdout) in cases {
        assert!(
            check_startup_output(distribution, outcome, stdout, &expected).is_err(),
            "should reject a {label}"
        );
    }
}

#[test]
fn cli_and_producer_share_the_workspace_package_version() {
    assert!(
        CLI_MANIFEST.contains("version = { workspace = true }"),
        "should inherit the CLI version from the workspace"
    );
    assert!(
        RELEASE_MANIFEST.contains("version.workspace = true"),
        "should inherit the producer version from the workspace"
    );
}

#[test]
fn startup_copy_is_read_only_rehashed_and_linux_only() {
    let bytes = linux_executable(b"fictional dwarf info");
    let build = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
    let scratch = tempfile::tempdir().expect("should create scratch space");
    let base = fs::canonicalize(scratch.path()).expect("should resolve scratch space");
    let directory = base.join("startup-copy");

    let copy = StartupCopy::create(&build.completed, &directory).expect("should copy");

    let mode = |path: &Path| {
        fs::metadata(path)
            .expect("should inspect the copy")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&directory), 0o555, "should seal the copy directory");
    assert_eq!(mode(&directory.join("ts")), 0o555, "should seal the copy");
    fs::set_permissions(directory.join("ts"), fs::Permissions::from_mode(0o644))
        .expect("should reopen the fictional copy for the tamper case");
    fs::write(directory.join("ts"), b"tampered").expect("should tamper with the copy");
    assert!(copy.verify().is_err(), "should detect a changed copy");
    copy.remove().expect("should remove the copy");
    assert!(!directory.exists(), "should leave nothing behind");

    assert!(
        StartupCopy::create(&build.completed, &base.join("startup,copy")).is_err(),
        "should refuse a directory that would split the bind mount"
    );
    let other = native_build(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    assert!(
        StartupCopy::create(&other.completed, &base.join("other-copy")).is_err(),
        "should start only the Linux executable"
    );
}

#[test]
fn native_startup_copy_is_sealed_and_darwin_only() {
    let darwin = native_build(
        "ts-aarch64-apple-darwin",
        Role::Canonical,
        &darwin_executable(),
    );
    let scratch = tempfile::tempdir().expect("should create scratch space");
    let base = fs::canonicalize(scratch.path()).expect("should resolve scratch space");
    let directory = base.join("native-copy");

    let copy = StartupCopy::create_native(&darwin.completed, &directory).expect("should copy");

    let mode = |path: &Path| {
        fs::metadata(path)
            .expect("should inspect the copy")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&directory), 0o555, "should seal the copy directory");
    assert_eq!(mode(&directory.join("ts")), 0o555, "should seal the copy");
    copy.verify().expect("should rehash the untouched copy");
    copy.remove().expect("should remove the copy");

    let linux = native_build(
        LINUX_CLI_ASSET,
        Role::Canonical,
        &linux_executable(b"fictional dwarf info"),
    );
    assert!(
        StartupCopy::create_native(&linux.completed, &base.join("linux-copy")).is_err(),
        "should start only Darwin executables natively"
    );
    assert!(
        StartupCopy::create_native(&darwin.completed, Path::new("relative-copy")).is_err(),
        "should refuse a relative copy directory"
    );
}

#[test]
fn startup_copy_rejects_aliases_links_and_extra_entries() {
    for change in ["symlink", "hardlink", "extra", "oversize"] {
        let bytes = linux_executable(b"fictional dwarf info");
        let build = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
        let scratch = tempfile::tempdir().expect("should create scratch space");
        let base = fs::canonicalize(scratch.path()).expect("should resolve scratch space");
        let directory = base.join("startup-copy");
        let copy = StartupCopy::create(&build.completed, &directory).expect("should create copy");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("should reopen directory");
        let executable = directory.join("ts");
        match change {
            "symlink" => {
                let outside = base.join("outside");
                fs::write(&outside, &bytes).expect("should write same bytes");
                fs::remove_file(&executable).expect("should unlink original");
                symlink(&outside, &executable).expect("should replace with alias");
            }
            "hardlink" => {
                fs::hard_link(&executable, base.join("second-link")).expect("should add link")
            }
            "extra" => {
                fs::write(directory.join("extra"), b"extra").expect("should add extra entry")
            }
            "oversize" => {
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o600))
                    .expect("should reopen copy");
                fs::OpenOptions::new()
                    .write(true)
                    .open(&executable)
                    .expect("should open copy")
                    .set_len(super::super::artifact::EXECUTABLE_LIMIT + 1)
                    .expect("should grow sparse file");
            }
            _ => unreachable!(),
        }
        assert!(copy.verify().is_err(), "should reject {change}");
        copy.remove().expect("should remove rejected copy");
    }
}

#[test]
fn startup_copy_drop_removes_copy_after_an_abandoned_run() {
    let bytes = linux_executable(b"fictional dwarf info");
    let build = native_build(LINUX_CLI_ASSET, Role::Canonical, &bytes);
    let scratch = tempfile::tempdir().expect("should create scratch space");
    let directory = fs::canonicalize(scratch.path())
        .expect("should resolve scratch space")
        .join("startup-copy");
    {
        let _copy = StartupCopy::create(&build.completed, &directory).expect("should copy");
    }
    assert!(
        !directory.exists(),
        "should attempt cleanup when explicit removal is skipped"
    );
}
