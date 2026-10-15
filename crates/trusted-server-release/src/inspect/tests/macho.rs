//! Mach-O inspection tests.

use object::macho as macho_format;

use super::builders::{Command, ElfSpec, MachoSpec, elf, fat, macho, macos};
use super::*;

fn inspect(spec: &MachoSpec) -> Result<MachoFacts, Report<ReleaseError>> {
    inspect_macho(&macho(spec), &MachoPolicy::macos(MachoCpu::Arm64))
}

fn with_command(command: Command) -> MachoSpec {
    let mut spec = MachoSpec::conforming();
    spec.commands.push(command);
    spec
}

#[test]
fn conforming_executable_is_accepted_with_observed_facts() {
    let bytes = macho(&MachoSpec::conforming());

    let facts = inspect_macho(&bytes, &MachoPolicy::macos(MachoCpu::Arm64))
        .expect("should accept a conforming executable");

    assert_eq!(facts.cpu(), MachoCpu::Arm64);
    assert_eq!(
        facts.minos(),
        MacosVersion::new(13, 0),
        "should record minos 13.0"
    );
    assert_eq!(
        facts.sdk(),
        MacosVersion::new(15, 5),
        "should record the SDK version"
    );
    assert_eq!(
        facts.dylibs(),
        [
            "/System/Library/Frameworks/Security.framework/Versions/A/Security",
            "/usr/lib/libSystem.B.dylib",
        ]
    );
    assert_eq!(
        facts.sha256(),
        sha256_hex(&bytes),
        "should hash the inspected bytes"
    );
}

#[test]
fn x86_64_policy_accepts_x86_64_executable() {
    let mut spec = MachoSpec::conforming();
    spec.cputype = macho_format::CPU_TYPE_X86_64;
    spec.cpusubtype = macho_format::CPU_SUBTYPE_X86_64_ALL | macho_format::CPU_SUBTYPE_LIB64;

    let facts = inspect_macho(&macho(&spec), &MachoPolicy::macos(MachoCpu::X86_64))
        .expect("should accept x86_64 with capability bits masked");

    assert_eq!(facts.cpu(), MachoCpu::X86_64);
}

#[test]
fn deployment_target_other_than_floor_is_rejected() {
    for (major, minor) in [(11, 0), (14, 0), (12, 7)] {
        let mut spec = MachoSpec::conforming();
        spec.commands[0] = Command::BuildVersion {
            platform: macho_format::PLATFORM_MACOS,
            minos: macos(major, minor),
        };

        assert_eq!(
            violation(inspect(&spec)),
            Violation::DeploymentTarget,
            "should reject minos {major}.{minor}"
        );
    }
}

#[test]
fn missing_or_legacy_deployment_target_is_rejected() {
    let mut missing = MachoSpec::conforming();
    missing.commands.remove(0);
    let mut legacy = MachoSpec::conforming();
    legacy.commands[0] = Command::VersionMin {
        version: macos(13, 0),
    };
    let duplicate = with_command(Command::BuildVersion {
        platform: macho_format::PLATFORM_MACOS,
        minos: macos(13, 0),
    });

    for spec in [missing, legacy, duplicate] {
        assert_eq!(violation(inspect(&spec)), Violation::DeploymentTarget);
    }
}

#[test]
fn non_macos_platform_is_rejected() {
    let mut spec = MachoSpec::conforming();
    spec.commands[0] = Command::BuildVersion {
        platform: macho_format::PLATFORM_IOS,
        minos: macos(13, 0),
    };

    assert_eq!(violation(inspect(&spec)), Violation::DeploymentTarget);
}

#[test]
fn wrong_cpu_is_rejected() {
    let mut x86 = MachoSpec::conforming();
    x86.cputype = macho_format::CPU_TYPE_X86_64;
    x86.cpusubtype = macho_format::CPU_SUBTYPE_X86_64_ALL;
    let mut arm64e = MachoSpec::conforming();
    arm64e.cpusubtype = macho_format::CPU_SUBTYPE_ARM64E;

    for spec in [x86, arm64e] {
        assert_eq!(violation(inspect(&spec)), Violation::Architecture);
    }
}

#[test]
fn fat_binary_is_rejected() {
    let thin = macho(&MachoSpec::conforming());

    let result = inspect_macho(&fat(&thin), &MachoPolicy::macos(MachoCpu::Arm64));

    assert_eq!(violation(result), Violation::Architecture);
}

#[test]
fn unexpected_dylib_is_rejected() {
    let spec = with_command(Command::Dylib {
        cmd: macho_format::LC_LOAD_DYLIB,
        name: "/usr/local/lib/libexample.dylib",
    });

    assert_eq!(violation(inspect(&spec)), Violation::Library);
}

#[test]
fn non_plain_dylib_loads_are_rejected() {
    for cmd in [
        macho_format::LC_LOAD_WEAK_DYLIB,
        macho_format::LC_REEXPORT_DYLIB,
        macho_format::LC_LAZY_LOAD_DYLIB,
        macho_format::LC_LOAD_UPWARD_DYLIB,
    ] {
        let spec = with_command(Command::Dylib {
            cmd,
            name: "/usr/lib/libSystem.B.dylib",
        });

        assert_eq!(
            violation(inspect(&spec)),
            Violation::LinkKind,
            "should reject {cmd:#x}"
        );
    }
}

#[test]
fn rpath_is_rejected() {
    let spec = with_command(Command::Rpath("@executable_path/../lib"));

    assert_eq!(violation(inspect(&spec)), Violation::SearchPath);
}

#[test]
fn unexpected_dylinker_is_rejected() {
    let mut spec = MachoSpec::conforming();
    spec.commands[1] = Command::Dylinker("/opt/example/dyld");

    assert_eq!(violation(inspect(&spec)), Violation::Interpreter);
}

#[test]
fn embedded_runner_path_is_rejected() {
    let mut spec = MachoSpec::conforming();
    spec.payload = b"panic at /Users/runner/work/example/src/lib.rs";

    assert_eq!(violation(inspect(&spec)), Violation::EmbeddedPath);
}

#[test]
fn non_macho_inputs_are_rejected() {
    let policy = MachoPolicy::macos(MachoCpu::Arm64);
    let linux = elf(&ElfSpec::conforming());

    assert_eq!(
        violation(inspect_macho(&linux, &policy)),
        Violation::Malformed
    );
    assert_eq!(
        violation(inspect_macho(b"not an executable", &policy)),
        Violation::Malformed
    );
}

#[test]
fn truncated_executables_are_rejected_without_panic() {
    let policy = MachoPolicy::macos(MachoCpu::Arm64);
    let mut spec = MachoSpec::conforming();
    spec.payload = b"";

    assert_truncations_rejected(&macho(&spec), |bytes| inspect_macho(bytes, &policy));
}
