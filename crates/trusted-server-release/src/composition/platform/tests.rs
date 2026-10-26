use std::fs;
use std::path::PathBuf;

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::*;
use crate::inspect::tests::builders::{Command, ElfSpec, MachoSpec, elf, macho, wasm_module};
use crate::inspect::{
    ElfPolicy, MachoCpu, MachoPolicy, MacosVersion, inspect_elf, inspect_macho, inspect_wasm,
};
use crate::recipe::{AppleDeveloperTools, AppleToolFacts};

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    target: ReleaseTarget,
}

impl Fixture {
    fn new(target: ReleaseTarget) -> Self {
        let directory = tempfile::tempdir().expect("should allocate tool fixture");
        let root = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
        let standard = root
            .join("tools/lib/rustlib")
            .join(target.triple())
            .join("lib");
        fs::create_dir_all(&standard).expect("should create selected target sysroot");
        fs::create_dir_all(root.join("tools/bin")).expect("should create tool directory");
        for path in [
            root.join("tools/bin/cargo"),
            root.join("tools/bin/rustc"),
            standard.join("libstd-example.rlib"),
        ] {
            fs::write(path, b"fictional measured Rust input").expect("should write Rust fixture");
        }
        Self {
            _directory: directory,
            root,
            target,
        }
    }

    fn tools(&self) -> SelectedRustTools {
        SelectedRustTools::for_tests(&self.root.join("tools"), self.target)
            .expect("should select measured candidate tools")
    }

    fn apple(&self) -> AppleDeveloperTools {
        self.apple_with_detached(false)
    }

    fn apple_with_detached(&self, detached: bool) -> AppleDeveloperTools {
        let developer_dir = self.root.join("developer");
        let bin = developer_dir.join("Toolchains/XcodeDefault.xctoolchain/usr/bin");
        let sdk_root =
            developer_dir.join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX15.5.sdk");
        fs::create_dir_all(&bin).expect("should create Apple tools");
        fs::create_dir_all(&sdk_root).expect("should create SDK settings directory");
        for path in [
            bin.join("clang"),
            bin.join("ar"),
            bin.join("ld"),
            sdk_root.join("SDKSettings.json"),
        ] {
            fs::write(path, b"fictional measured Apple input").expect("should write Apple fixture");
        }
        let digest = hex::encode(Sha256::digest(b"fictional measured Apple input"));
        let facts = AppleToolFacts {
            developer_dir,
            sdk_root,
            clang: bin.join("clang"),
            ar: bin.join("ar"),
            ld: bin.join("ld"),
            xcode_version: "16.4".to_owned(),
            sdk_version: MacosVersion::new(15, 5),
            clang_sha256: digest.clone(),
            ar_sha256: digest.clone(),
            ld_sha256: digest.clone(),
            sdk_settings_sha256: digest,
        };
        if detached {
            AppleDeveloperTools::with_detached_sdk_for_tests(
                facts,
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                &crate::process::Cancellation::default(),
            )
            .expect("should stage fictional detached SDK")
        } else {
            AppleDeveloperTools::from_facts_for_tests(facts)
                .expect("should select measured Apple candidate")
        }
    }
}

fn requirements(target: ReleaseTarget) -> ExternalRequirements {
    match target {
        ReleaseTarget::Wasm32Wasip1 => ExternalRequirements::from_wasm(
            &inspect_wasm(&wasm_module(&[("fastly_http_req", "downstream_get")]))
                .expect("should accept fixture Wasm"),
        ),
        ReleaseTarget::X86_64UnknownLinuxGnu => ExternalRequirements::from_elf(
            &inspect_elf(&elf(&ElfSpec::conforming()), &ElfPolicy::linux_x86_64_gnu())
                .expect("should accept fixture ELF"),
        ),
        ReleaseTarget::Aarch64AppleDarwin | ReleaseTarget::X86_64AppleDarwin => {
            let cpu = if target == ReleaseTarget::Aarch64AppleDarwin {
                MachoCpu::Arm64
            } else {
                MachoCpu::X86_64
            };
            let mut spec = MachoSpec::conforming();
            if target == ReleaseTarget::X86_64AppleDarwin {
                spec.cputype = object::macho::CPU_TYPE_X86_64;
                spec.cpusubtype = object::macho::CPU_SUBTYPE_X86_64_ALL;
            }
            ExternalRequirements::from_macho(
                &inspect_macho(&macho(&spec), &MachoPolicy::macos(cpu))
                    .expect("should accept fixture Mach-O"),
            )
        }
    }
}

fn collect(
    fixture: &Fixture,
    tools: &SelectedRustTools,
) -> Result<PlatformEvidence, Report<ReleaseError>> {
    let external = requirements(fixture.target);
    let artifact = external.artifact_sha256().to_owned();
    PlatformEvidence::collect(tools, &tools.digest()?, external, fixture.target, &artifact)
}

/// Constructs typed platform fixtures only by measuring fictional tool files.
pub(crate) fn projection_fixture(target: ReleaseTarget) -> PlatformEvidence {
    let fixture = Fixture::new(target);
    let tools = if target.is_darwin() {
        fixture.tools().with_apple_developer_tools(fixture.apple())
    } else {
        fixture.tools()
    };
    collect(&fixture, &tools).expect("should collect measured platform fixture")
}

/// Measures an actual detached fictional SDK for the platform adapter tests.
pub(crate) fn projection_detached_sdk_fixture() -> PlatformEvidence {
    let fixture = Fixture::new(ReleaseTarget::Aarch64AppleDarwin);
    let tools = fixture
        .tools()
        .with_apple_developer_tools(fixture.apple_with_detached(true));
    collect(&fixture, &tools).expect("should collect detached SDK fixture")
}

/// Measures a detached SDK and an in-place fictional toolchain.
pub(crate) fn projection_measured_toolchain_fixture() -> PlatformEvidence {
    projection_toolchain_with_libtapi_fixture(false)
}

/// Measures a fictional toolchain with an explicitly selected optional TAPI file.
pub(crate) fn projection_toolchain_with_libtapi_fixture(libtapi: bool) -> PlatformEvidence {
    let fixture = Fixture::new(ReleaseTarget::Aarch64AppleDarwin);
    let apple = fixture.apple_with_detached(true);
    let usr = fixture
        .root
        .join("developer/Toolchains/XcodeDefault.xctoolchain/usr");
    fs::create_dir_all(usr.join("lib/clang/17/include"))
        .expect("should create fictional resource directory");
    fs::write(usr.join("lib/libLTO.dylib"), b"fictional libLTO")
        .expect("should write fictional libLTO");
    if libtapi {
        fs::write(usr.join("lib/libtapi.dylib"), b"fictional libtapi")
            .expect("should write fictional libtapi");
    }
    fs::write(
        usr.join("lib/clang/17/include/stddef.h"),
        b"fictional header",
    )
    .expect("should write fictional resource header");
    let apple = apple
        .with_measured_toolchain_for_tests(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &crate::process::Cancellation::default(),
        )
        .expect("should measure fictional toolchain");
    let tools = fixture.tools().with_apple_developer_tools(apple);
    collect(&fixture, &tools).expect("should collect measured toolchain fixture")
}

#[test]
fn candidate_platform_receipt_binds_actual_tools_and_external_artifact_without_completing_coverage()
{
    let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxGnu);
    let tools = fixture.tools();
    let receipt = collect(&fixture, &tools).expect("should bind candidate platform evidence");
    let value = serde_json::to_value(receipt).expect("should serialize platform receipt");
    assert_eq!(
        value["rust"]["tree_sha256"],
        tools.digest().expect("should measure tree"),
        "should bind actual selected tool tree"
    );
    assert_eq!(
        value["target"], "x86_64-unknown-linux-gnu",
        "should preserve selected target"
    );
    assert_eq!(
        value["artifact_sha256"], value["external_requirements"]["artifact_sha256"],
        "should bind requirements to inspected executable"
    );
    assert_eq!(
        value["material_native_coverage_complete"], false,
        "should retain incomplete coverage gate"
    );
    assert_eq!(
        value["native_tools"]["scope"], "host_path_unresolved",
        "should not invent identities for Linux host PATH tools"
    );
    assert_eq!(
        value["native_tools"]["roles"],
        serde_json::json!(["compiler", "assembler", "archiver", "linker", "build_tools"]),
        "should retain every unresolved native tool role"
    );
    assert_eq!(
        value["rust"]["qualification"], "unqualified",
        "should preserve candidate measurement authority"
    );
}

#[test]
fn platform_binding_rejects_tool_target_requirement_target_artifact_and_digest_mismatches() {
    for mismatch in [
        "tool_target",
        "requirements_target",
        "artifact",
        "bound_digest",
    ] {
        let fixture = Fixture::new(ReleaseTarget::Wasm32Wasip1);
        let tools = fixture.tools();
        let external = requirements(
            if matches!(mismatch, "tool_target" | "requirements_target") {
                ReleaseTarget::X86_64UnknownLinuxGnu
            } else {
                fixture.target
            },
        );
        let artifact = if mismatch == "artifact" {
            "a".repeat(64)
        } else {
            external.artifact_sha256().to_owned()
        };
        let bound = if mismatch == "bound_digest" {
            "b".repeat(64)
        } else {
            tools.digest().expect("should measure bound tools")
        };
        let target = if mismatch == "tool_target" {
            ReleaseTarget::X86_64UnknownLinuxGnu
        } else {
            fixture.target
        };
        assert!(
            PlatformEvidence::collect(&tools, &bound, external, target, &artifact).is_err(),
            "should reject {mismatch}"
        );
    }
}

#[test]
fn platform_binding_rejects_rust_compiler_and_sysroot_changes_after_binding() {
    for relative in [
        "bin/rustc",
        "lib/rustlib/wasm32-wasip1/lib/libstd-example.rlib",
    ] {
        let fixture = Fixture::new(ReleaseTarget::Wasm32Wasip1);
        let tools = fixture.tools();
        let bound = tools
            .digest()
            .expect("should measure tools before mutation");
        fs::write(
            fixture.root.join("tools").join(relative),
            b"changed selected tool input",
        )
        .expect("should change tool input");
        let external = requirements(fixture.target);
        let artifact = external.artifact_sha256().to_owned();
        assert!(
            PlatformEvidence::collect(&tools, &bound, external, fixture.target, &artifact).is_err(),
            "should reject changed {relative}"
        );
    }
}

#[test]
fn darwin_platform_receipt_requires_apple_files_and_retains_sdk_coverage_exclusions() {
    let fixture = Fixture::new(ReleaseTarget::Aarch64AppleDarwin);
    assert!(
        collect(&fixture, &fixture.tools()).is_err(),
        "should require selected Apple tools for Darwin"
    );
    let tools = fixture.tools().with_apple_developer_tools(fixture.apple());
    let receipt = collect(&fixture, &tools).expect("should retain measured Apple files");
    let value = serde_json::to_value(receipt).expect("should serialize Apple receipt");
    assert_eq!(
        value["apple"]["coverage_exclusions"],
        serde_json::json!([
            "sdk_headers",
            "sdk_stubs",
            "sdk_frameworks",
            "tool_helpers",
            "liblto",
            "clang_resource_directory",
            "toolchain_libraries"
        ]),
        "should never imply complete SDK coverage"
    );
    assert_eq!(
        value["native_tools"]["full_closure_measured"], false,
        "should retain incomplete native tool closure"
    );
    fs::write(
        tools.apple().expect("should have Apple selection").clang(),
        b"changed Apple compiler",
    )
    .expect("should change selected Apple tool");
    assert!(
        collect(&fixture, &tools).is_err(),
        "should reject changed Apple files"
    );
}

#[test]
fn non_darwin_platform_receipt_rejects_apple_selection() {
    let fixture = Fixture::new(ReleaseTarget::Wasm32Wasip1);
    let tools = fixture.tools().with_apple_developer_tools(fixture.apple());
    assert!(
        collect(&fixture, &tools).is_err(),
        "should reject Apple selection for a non-Darwin target"
    );
}

#[test]
fn platform_binding_rejects_an_artifact_built_for_another_apple_sdk() {
    let fixture = Fixture::new(ReleaseTarget::Aarch64AppleDarwin);
    let tools = fixture.tools().with_apple_developer_tools(fixture.apple());
    let mut spec = MachoSpec::conforming();
    for command in &mut spec.commands {
        if let Command::BuildVersion { platform, minos } = *command {
            *command = Command::BuildVersionSdk {
                platform,
                minos,
                sdk: (15 << 16) | (4 << 8),
            };
        }
    }
    let facts = inspect_macho(&macho(&spec), &MachoPolicy::macos(MachoCpu::Arm64))
        .expect("should inspect an otherwise accepted executable with another SDK");
    let external = ExternalRequirements::from_macho(&facts);
    assert!(
        PlatformEvidence::collect(
            &tools,
            &tools.digest().expect("should measure bound tools"),
            external,
            fixture.target,
            facts.sha256(),
        )
        .is_err(),
        "should reject a recorded SDK differing from selected Apple tools"
    );
}

#[test]
fn fastly_tool_attachment_preserves_target_and_receipt_incompleteness() {
    let platform = projection_fixture(ReleaseTarget::Wasm32Wasip1)
        .with_fastly_tools(crate::recipe::fastly_tools::FastlyToolEvidence::projection_fixture())
        .expect("should attach selected Fastly host files");
    assert_eq!(
        platform.unresolved_native_tool_roles(),
        ["assembler", "build_tools"],
        "should retain unmeasured closure roles"
    );
    assert!(
        !platform.material_native_coverage_complete(),
        "should retain incomplete native material coverage"
    );
    let receipt = serde_json::to_value(&platform).expect("should serialize receipt");
    assert_eq!(
        receipt["native_tools"]["evidence"]["full_closure_measured"], false,
        "should preserve bounded measurement scope"
    );
    assert!(
        projection_fixture(ReleaseTarget::X86_64UnknownLinuxGnu)
            .with_fastly_tools(
                crate::recipe::fastly_tools::FastlyToolEvidence::projection_fixture()
            )
            .is_err(),
        "should reject another target"
    );
}
