//! Mach-O inspection tests.

use object::macho as macho_format;

use super::builders::{
    Command, ElfSpec, MachoSpec, Symbol, code_signature, debug_map_symbols, elf, fat,
    linker_signature, macho, macos,
};
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
        facts.loader(),
        "/usr/lib/dyld",
        "should retain the observed and validated dynamic linker"
    );
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

#[test]
fn macos_temporary_directory_paths_remain_rejected() {
    let mut spec = MachoSpec::conforming();
    spec.payload = b"/tmp/example/generated.rs";
    assert_eq!(
        violation(inspect(&spec)),
        Violation::EmbeddedPath,
        "should retain the macOS policy until its filesystem boundary is qualified"
    );
}

fn signature_index(spec: &MachoSpec) -> usize {
    spec.position(|command| matches!(command, Command::CodeSignature(_)))
}

fn symtab_index(spec: &MachoSpec) -> usize {
    spec.position(|command| matches!(command, Command::Symtab(_)))
}

fn with_signature(blob: Vec<u8>) -> MachoSpec {
    let mut spec = MachoSpec::conforming();
    let index = signature_index(&spec);
    spec.commands[index] = Command::CodeSignature(blob);
    spec
}

fn with_symbols(symbols: Vec<Symbol>) -> MachoSpec {
    let mut spec = MachoSpec::conforming();
    let index = symtab_index(&spec);
    spec.commands[index] = Command::Symtab(symbols);
    spec
}

fn object(name: &'static str, value: u64) -> Symbol {
    Symbol {
        n_type: macho_format::N_OSO,
        name,
        value,
    }
}

#[test]
fn conforming_executable_records_uuid_debug_map_and_signature() {
    let facts = inspect(&MachoSpec::conforming()).expect("should accept a conforming executable");

    assert_eq!(facts.uuid(), [0x5a; 16], "should record LC_UUID");
    assert_eq!(facts.symbol_count(), 3, "should count every symbol");
    assert_eq!(
        facts.debug_map_objects(),
        2,
        "should count the relative debug-map objects"
    );
    assert_eq!(
        facts.signature(),
        CodeSignature::LinkerSignedAdhoc,
        "should record the linker's ad-hoc signature"
    );
    facts
        .require_debug_map()
        .expect("should keep the debug map a release build needs");
}

#[test]
fn arm64_must_be_linker_signed_while_x86_64_may_be_unsigned() {
    let mut unsigned_arm64 = MachoSpec::conforming();
    unsigned_arm64
        .commands
        .remove(signature_index(&unsigned_arm64));
    assert_eq!(
        violation(inspect(&unsigned_arm64)),
        Violation::CodeSignature,
        "should require a signature on arm64"
    );

    let mut unsigned_x86_64 = MachoSpec::conforming();
    unsigned_x86_64
        .commands
        .remove(signature_index(&unsigned_x86_64));
    unsigned_x86_64.cputype = macho_format::CPU_TYPE_X86_64;
    unsigned_x86_64.cpusubtype = macho_format::CPU_SUBTYPE_X86_64_ALL;
    let facts = inspect_macho(
        &macho(&unsigned_x86_64),
        &MachoPolicy::macos(MachoCpu::X86_64),
    )
    .expect("should accept an unsigned x86_64 executable");
    assert_eq!(facts.signature(), CodeSignature::Unsigned);
}

#[test]
fn signatures_other_than_linker_signed_ad_hoc_are_rejected() {
    let mut bad_magic = linker_signature();
    bad_magic[0] = 0;
    let mut short_length = linker_signature();
    short_length[4..8].copy_from_slice(&4_u32.to_be_bytes());
    let mut long_length = linker_signature();
    long_length[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    let cases: [(&str, Vec<u8>); 9] = [
        (
            "ad hoc without linker signing",
            code_signature(0x2, 0, &[2]),
        ),
        (
            "linker signing without ad hoc",
            code_signature(0x2_0000, 0, &[2]),
        ),
        ("no flags", code_signature(0, 0, &[2])),
        ("a team identifier", code_signature(0x2_0002, 52, &[2])),
        (
            "a CMS signature blob",
            code_signature(0x2_0002, 0, &[0x1_0000]),
        ),
        ("an entitlements slot", code_signature(0x2_0002, 0, &[5])),
        ("another magic", bad_magic),
        ("a length below the header", short_length),
        ("a length past the blob", long_length),
    ];
    for (label, blob) in cases {
        assert_eq!(
            violation(inspect(&with_signature(blob))),
            Violation::CodeSignature,
            "should reject a signature with {label}"
        );
    }
}

#[test]
fn duplicate_code_directories_or_signatures_are_rejected() {
    let two_directories = with_signature(code_signature(0x2_0002, 0, &[0]));
    assert_eq!(
        violation(inspect(&two_directories)),
        Violation::CodeSignature
    );

    let doubled = with_command(Command::CodeSignature(linker_signature()));
    assert_eq!(violation(inspect(&doubled)), Violation::CodeSignature);
}

#[test]
fn debug_map_objects_must_be_relative_and_zero_time() {
    let cases: [(&str, Symbol); 4] = [
        (
            "an absolute object",
            object("/opt/example/target/release/deps/ts.o", 0),
        ),
        ("an empty object path", object("", 0)),
        (
            "a modification time",
            object("cargo-target/release/deps/ts.o", 1_700_000_000),
        ),
        (
            "an absolute archive member",
            object("/opt/example/rust-tools/libstd.rlib(std.o)", 0),
        ),
    ];
    for (label, symbol) in cases {
        let mut symbols = debug_map_symbols();
        symbols.push(symbol);
        assert_eq!(
            violation(inspect(&with_symbols(symbols))),
            Violation::MachoDebugMap,
            "should reject {label}"
        );
    }
}

#[test]
fn stripped_executables_fail_the_debug_map_requirement() {
    let mut no_symbols = MachoSpec::conforming();
    no_symbols.commands.remove(symtab_index(&no_symbols));
    let no_objects = with_symbols(vec![Symbol {
        n_type: macho_format::N_SECT | macho_format::N_EXT,
        name: "_main",
        value: 0x1_0000_0000,
    }]);
    let empty = with_symbols(Vec::new());
    for (label, spec) in [
        ("without a symbol table", no_symbols),
        ("without debug-map objects", no_objects),
        ("with an empty symbol table", empty),
    ] {
        let facts = inspect(&spec).expect("should inspect the stripped executable");
        assert_eq!(
            violation(facts.require_debug_map()),
            Violation::MachoDebugMap,
            "should reject an executable {label}"
        );
    }
    let doubled = with_command(Command::Symtab(debug_map_symbols()));
    assert_eq!(violation(inspect(&doubled)), Violation::MachoDebugMap);
}

#[test]
fn uuid_is_required_exactly_once() {
    let mut missing = MachoSpec::conforming();
    missing
        .commands
        .remove(missing.position(|command| matches!(command, Command::Uuid(_))));
    assert_eq!(
        violation(inspect(&missing)),
        Violation::MachoIdentity,
        "should reject a linked executable without LC_UUID"
    );

    let doubled = with_command(Command::Uuid([0x6b; 16]));
    assert_eq!(violation(inspect(&doubled)), Violation::MachoIdentity);
}

#[test]
fn requirements_must_be_one_empty_set() {
    let mut populated = linker_signature();
    let count_at = populated.len() - 4;
    populated[count_at..].copy_from_slice(&1_u32.to_be_bytes());
    let mut wrong_magic = linker_signature();
    let magic_at = wrong_magic.len() - 12;
    wrong_magic[magic_at..magic_at + 4].copy_from_slice(&0xfade_0c05_u32.to_be_bytes());
    let cases: [(&str, Vec<u8>); 3] = [
        ("a populated requirements set", populated),
        ("another requirements magic", wrong_magic),
        (
            "two requirements blobs",
            code_signature(0x2_0002, 0, &[2, 2]),
        ),
    ];
    for (label, blob) in cases {
        assert_eq!(
            violation(inspect(&with_signature(blob))),
            Violation::CodeSignature,
            "should reject {label}"
        );
    }
    inspect(&with_signature(code_signature(0x2_0002, 0, &[])))
        .expect("should accept a signature without a requirements blob");
}

#[test]
fn build_version_records_the_linked_sdk() {
    let mut spec = MachoSpec::conforming();
    spec.commands[0] = Command::BuildVersionSdk {
        platform: macho_format::PLATFORM_MACOS,
        minos: macos(13, 0),
        sdk: macos(26, 1),
    };

    let facts = inspect(&spec).expect("should accept another SDK version");

    assert_eq!(
        facts.sdk(),
        MacosVersion::new(26, 1),
        "should record the SDK"
    );
}

#[test]
fn sdk_versions_parse_only_dotted_decimal_text() {
    assert_eq!(MacosVersion::parse("15.5"), Some(MacosVersion::new(15, 5)));
    assert_eq!(
        MacosVersion::parse("26.0.1").map(|version| version.to_string()),
        Some("26.0.1".to_owned())
    );
    for text in [
        "", "15", "15.", "15.5.1.2", "015.5", "15.05", "+15.5", "15.256", "a.b",
    ] {
        assert_eq!(MacosVersion::parse(text), None, "should reject {text:?}");
    }
}
