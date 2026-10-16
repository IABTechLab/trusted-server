//! ELF inspection tests.

use object::elf as elf_format;

use super::builders::{ElfSpec, ExtraSection, MachoSpec, elf, macho};
use super::*;

fn inspect(spec: &ElfSpec) -> Result<ElfFacts, Report<ReleaseError>> {
    inspect_elf(&elf(spec), &ElfPolicy::linux_x86_64_gnu())
}

#[test]
fn conforming_executable_is_accepted_with_observed_facts() {
    let bytes = elf(&ElfSpec::conforming());

    let facts = inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu())
        .expect("should accept a conforming executable");

    assert_eq!(facts.interpreter(), "/lib64/ld-linux-x86-64.so.2");
    assert_eq!(facts.needed(), ["libgcc_s.so.1", "libm.so.6", "libc.so.6"]);
    assert_eq!(
        facts.max_glibc(),
        GlibcVersion::new(2, 34),
        "should record the highest GLIBC requirement numerically"
    );
    assert_eq!(
        facts.version_requirements().len(),
        5,
        "should record every requirement"
    );
    assert_eq!(
        facts.sha256(),
        sha256_hex(&bytes),
        "should hash the inspected bytes"
    );
}

#[test]
fn glibc_above_ceiling_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.requirements[1].1.push("GLIBC_2.35");

    assert_eq!(violation(inspect(&spec)), Violation::SymbolVersion);
}

#[test]
fn unparsable_glibc_version_is_rejected() {
    for name in ["GLIBC_PRIVATE", "GLIBC_ABI_DT_RELR"] {
        let mut spec = ElfSpec::conforming();
        spec.requirements[1].1.push(name);

        assert_eq!(
            violation(inspect(&spec)),
            Violation::SymbolVersion,
            "should reject {name}"
        );
    }
}

#[test]
fn unexpected_version_namespace_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.requirements[1].1.push("GCC_3.0");

    assert_eq!(
        violation(inspect(&spec)),
        Violation::SymbolVersion,
        "should accept GCC_ names only from libgcc_s"
    );
}

#[test]
fn missing_glibc_requirements_are_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.requirements.truncate(1);

    assert_eq!(violation(inspect(&spec)), Violation::SymbolVersion);
}

#[test]
fn wrong_architecture_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.machine = elf_format::EM_AARCH64;

    assert_eq!(violation(inspect(&spec)), Violation::Architecture);
}

#[test]
fn missing_interpreter_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.interpreter = None;

    assert_eq!(violation(inspect(&spec)), Violation::Interpreter);
}

#[test]
fn unexpected_interpreter_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.interpreter = Some("/opt/example/ld.so");

    assert_eq!(violation(inspect(&spec)), Violation::Interpreter);
}

#[test]
fn unexpected_needed_library_is_rejected() {
    for name in ["libssl.so.3", "libpthread.so.0", "/opt/example/libc.so.6"] {
        let mut spec = ElfSpec::conforming();
        spec.needed.push(name);

        assert_eq!(
            violation(inspect(&spec)),
            Violation::Library,
            "should reject {name}"
        );
    }
}

#[test]
fn runpath_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.runpath = Some("$ORIGIN/../lib");

    assert_eq!(violation(inspect(&spec)), Violation::SearchPath);
}

fn replace_dynamic_terminator(bytes: &mut [u8], tag: u32, value: u64) {
    let section_table = u64::from_le_bytes(bytes[40..48].try_into().expect("should read offset"));
    let section = usize::try_from(section_table).expect("should fit") + 2 * 64;
    let offset = u64::from_le_bytes(
        bytes[section + 24..section + 32]
            .try_into()
            .expect("should read offset"),
    );
    let size = u64::from_le_bytes(
        bytes[section + 32..section + 40]
            .try_into()
            .expect("should read size"),
    );
    let start = usize::try_from(offset).expect("should fit");
    let end = usize::try_from(offset + size).expect("should fit");
    let entry = (start..end)
        .step_by(16)
        .find(|entry| read_u64(bytes, *entry) == 0)
        .expect("should find terminator");
    bytes[entry..entry + 8].copy_from_slice(&u64::from(tag).to_le_bytes());
    bytes[entry + 8..entry + 16].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn loader_auditors_are_rejected() {
    for tag in [elf_format::DT_AUDIT, elf_format::DT_DEPAUDIT] {
        let mut bytes = elf(&ElfSpec::conforming());
        replace_dynamic_terminator(&mut bytes, tag, 0);
        assert_eq!(
            violation(inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu())),
            Violation::Library,
            "should reject loader auditor tags"
        );
    }
}

#[test]
fn unterminated_dynamic_table_is_rejected() {
    let mut bytes = elf(&ElfSpec::conforming());
    replace_dynamic_terminator(&mut bytes, elf_format::DT_DEBUG, 0);
    replace_dynamic_terminator(&mut bytes, elf_format::DT_DEBUG, 0);
    assert!(
        inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu()).is_err(),
        "should require a dynamic table terminator"
    );
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("should read field"),
    )
}

fn section_offset(bytes: &[u8], index: usize) -> usize {
    usize::try_from(read_u64(bytes, 40)).expect("should fit section offset") + index * 64
}

fn change_dynamic_value(bytes: &mut [u8], tag: u32) {
    let section = section_offset(bytes, 2);
    let start = usize::try_from(read_u64(bytes, section + 24)).expect("should fit");
    let end = start + usize::try_from(read_u64(bytes, section + 32)).expect("should fit");
    let entry = (start..end)
        .step_by(16)
        .find(|offset| read_u64(bytes, *offset) == u64::from(tag))
        .expect("should find dynamic tag");
    let changed = read_u64(bytes, entry + 8) + 1;
    bytes[entry + 8..entry + 16].copy_from_slice(&changed.to_le_bytes());
}

#[test]
fn loader_metadata_must_match_all_observed_sections() {
    for tag in [
        elf_format::DT_STRSZ,
        elf_format::DT_VERNEED,
        elf_format::DT_VERNEEDNUM,
        elf_format::DT_SYMTAB,
        elf_format::DT_SYMENT,
        elf_format::DT_VERSYM,
    ] {
        let mut bytes = elf(&ElfSpec::conforming());
        change_dynamic_value(&mut bytes, tag);
        assert!(
            inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu()).is_err(),
            "should reject section/dynamic disagreement for tag {tag}"
        );
    }
}

#[test]
fn shadow_string_table_cannot_hide_loader_libraries() {
    let mut bytes = elf(&ElfSpec::conforming());
    let section = section_offset(&bytes, 1);
    let start = usize::try_from(read_u64(&bytes, section + 24)).expect("should fit");
    let size = usize::try_from(read_u64(&bytes, section + 32)).expect("should fit");
    let shadow = bytes[start..start + size].to_vec();
    let shadow_offset = bytes.len() as u64;
    bytes.extend_from_slice(&shadow);
    bytes[section + 24..section + 32].copy_from_slice(&shadow_offset.to_le_bytes());
    let name = bytes[start..start + size]
        .windows(9)
        .position(|name| name == b"libc.so.6")
        .expect("should find real loader library");
    bytes[start + name..start + name + 9].copy_from_slice(b"evil.so.6");
    assert!(
        inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu()).is_err(),
        "should inspect the loader's actual strings rather than a shadow section"
    );
}

#[test]
fn undefined_symbol_cannot_hide_an_unaccounted_version() {
    let mut bytes = elf(&ElfSpec::conforming());
    let section = section_offset(&bytes, 5);
    let start = usize::try_from(read_u64(&bytes, section + 24)).expect("should fit");
    bytes[start + 2..start + 4].copy_from_slice(&999_u16.to_le_bytes());
    assert!(
        inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu()).is_err(),
        "should bind undefined symbol versions to checked requirements"
    );
}

#[test]
fn partially_overlapping_loads_cannot_replace_observed_bytes() {
    for file_size in [0_u64, 1] {
        let mut bytes = elf(&ElfSpec::conforming());
        let strings = section_offset(&bytes, 1);
        let address = read_u64(&bytes, strings + 16) + 4;
        let note = (64..64 + 4 * 56)
            .step_by(56)
            .find(|offset| bytes[*offset..*offset + 4] == elf_format::PT_NOTE.to_le_bytes())
            .expect("should find spare program header");
        bytes[note..note + 4].copy_from_slice(&elf_format::PT_LOAD.to_le_bytes());
        for (field, value) in [(8, address), (16, address), (32, file_size), (40, 1)] {
            bytes[note + field..note + field + 8].copy_from_slice(&value.to_le_bytes());
        }
        assert!(
            inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu()).is_err(),
            "should reject partial file-backed and zero-fill overlaps"
        );
    }
}

#[test]
fn embedded_runner_path_is_rejected() {
    let mut spec = ElfSpec::conforming();
    spec.payload = b"panic at /home/runner/work/example/src/lib.rs";

    assert_eq!(violation(inspect(&spec)), Violation::EmbeddedPath);
}

#[test]
fn denied_prefixes_are_configurable() {
    let policy = ElfPolicy {
        denied_path_prefixes: &["/opt/example/"],
        ..ElfPolicy::linux_x86_64_gnu()
    };

    let result = inspect_elf(&elf(&ElfSpec::conforming()), &policy);

    assert_eq!(violation(result), Violation::EmbeddedPath);
}

#[test]
fn non_elf_inputs_are_rejected() {
    let policy = ElfPolicy::linux_x86_64_gnu();
    let thin = macho(&MachoSpec::conforming());

    assert_eq!(violation(inspect_elf(&thin, &policy)), Violation::Malformed);
    assert_eq!(
        violation(inspect_elf(b"\0asm\x01\0\0\0", &policy)),
        Violation::Malformed
    );
}

#[test]
fn truncated_executables_are_rejected_without_panic() {
    let policy = ElfPolicy::linux_x86_64_gnu();

    assert_truncations_rejected(&elf(&ElfSpec::conforming()), |bytes| {
        inspect_elf(bytes, &policy)
    });
}

#[test]
fn retained_debug_sections_are_recorded_and_required() {
    let facts = inspect(&ElfSpec::conforming()).expect("should accept a conforming executable");

    let recorded: Vec<(&str, usize)> = facts
        .debug_sections()
        .iter()
        .map(|section| (section.name(), section.size()))
        .collect();
    assert_eq!(
        recorded,
        [(".debug_info", 20), (".debug_line", 21)],
        "should record each required DWARF section with its file bytes"
    );
    facts
        .require_debug_sections()
        .expect("should accept an executable that kept its debug info");
}

#[test]
fn stripped_or_empty_debug_sections_are_rejected() {
    let cases: [(&str, Vec<ExtraSection>); 4] = [
        ("stripped", Vec::new()),
        (
            "missing line table",
            vec![(".debug_info", elf_format::SHT_PROGBITS, b"fictional dwarf")],
        ),
        (
            "empty info",
            vec![
                (".debug_info", elf_format::SHT_PROGBITS, b""),
                (".debug_line", elf_format::SHT_PROGBITS, b"fictional dwarf"),
            ],
        ),
        (
            "no file bytes",
            vec![
                (".debug_info", elf_format::SHT_PROGBITS, b"fictional dwarf"),
                (".debug_line", elf_format::SHT_NOBITS, b"fictional dwarf"),
            ],
        ),
    ];
    for (label, debug_sections) in cases {
        let spec = ElfSpec {
            debug_sections,
            ..ElfSpec::conforming()
        };
        let facts = inspect(&spec).expect("should record, not reject, missing debug info");

        assert_eq!(
            violation(facts.require_debug_sections()),
            Violation::ElfDebugInfo,
            "should reject the {label} executable"
        );
    }
}

#[test]
fn runner_path_rejection_includes_final_elf_diagnostics() {
    let mut spec = ElfSpec::conforming();
    spec.debug_sections.push((
        ".rodata",
        elf_format::SHT_PROGBITS,
        b"before /home/runner/example\n\x1bafter",
    ));
    let bytes = elf(&spec);
    let report = inspect_elf(&bytes, &ElfPolicy::linux_x86_64_gnu())
        .expect_err("should retain rejection of a runner-only path");
    assert_eq!(
        report.downcast_ref::<Violation>(),
        Some(&Violation::EmbeddedPath),
        "should preserve the rejection rule"
    );
    let diagnostic = format!("{report:?}");
    assert!(
        diagnostic.contains(&format!(
            "artifact sha256={} size={}",
            sha256_hex(&bytes),
            bytes.len()
        )),
        "should bind diagnostic to the final bytes: {diagnostic}"
    );
    assert!(
        diagnostic.contains("section=\".rodata\""),
        "should identify the containing ELF section: {diagnostic}"
    );
    assert!(
        diagnostic.contains("/home/runner/example\\n\\x1bafter"),
        "should escape controls in a bounded context: {diagnostic}"
    );
}

#[test]
fn linux_runtime_browser_lookup_path_is_structurally_accepted() {
    let mut spec = ElfSpec::conforming();
    spec.debug_sections.push((
        ".rodata",
        elf_format::SHT_PROGBITS,
        b"/opt/google/chrome/tmp/aws/libCHROME",
    ));
    inspect(&spec).expect("should permit a Linux runtime temporary-directory lookup constant");
}

#[test]
fn linux_keeps_every_other_runner_path_prefix_rejected() {
    for prefix in RUNNER_PATH_PREFIXES
        .iter()
        .copied()
        .filter(|prefix| *prefix != "/tmp/")
    {
        let mut spec = ElfSpec::conforming();
        spec.payload = prefix.as_bytes();
        assert_eq!(
            violation(inspect(&spec)),
            Violation::EmbeddedPath,
            "should keep rejecting runner-only prefix {prefix}"
        );
    }
}
