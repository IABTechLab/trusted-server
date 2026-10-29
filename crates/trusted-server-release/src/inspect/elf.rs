//! ELF inspection for `x86_64-unknown-linux-gnu` executables.
//!
//! Section observations are accepted only after matching the loader's
//! dynamic addresses to their actual file-backed `PT_LOAD` ranges.

mod loader;

use std::collections::BTreeSet;

use error_stack::Report;
use object::Endianness;
use object::elf::{self as format, FileHeader64};
use object::read::elf::{Dyn as _, FileHeader as _, ProgramHeader as _, SectionHeader as _};
use object::read::{SectionIndex, StringTable};

use super::{
    ElfDebugSection, ElfFacts, GlibcVersion, REQUIRED_DEBUG_SECTIONS, VersionRequirement,
    Violation, malformed, reject_denied_paths_with_sections, rejected, sha256_hex, utf8,
};
use crate::error::ReleaseError;

type Header = FileHeader64<Endianness>;
type Sections<'data> = object::read::elf::SectionTable<'data, Header, &'data [u8]>;

/// Version-name prefix whose suffix is compared to the glibc ceiling.
const GLIBC_PREFIX: &str = "GLIBC_";

/// Reviewed `x86_64-unknown-linux-gnu` loader.
const LINUX_X86_64_INTERPRETER: &str = "/lib64/ld-linux-x86-64.so.2";

/// Reviewed `DT_NEEDED` set for the `ts` CLI.
///
/// Rust std on `linux-gnu` links libc, libm, `libgcc_s` (unwinding) and the
/// loader (`__tls_get_addr`). From glibc 2.34 `libdl`, `libpthread` and
/// `librt` are merged into libc, so they are deliberately absent: a builder
/// that still emits them is not the qualified floor.
const LINUX_X86_64_NEEDED: &[&str] = &[
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libgcc_s.so.1",
    "libm.so.6",
];

/// GNU-specific runner path prefixes, excluding the generic runtime `/tmp/`.
///
/// The Linux CLI includes a dependency's browser lookup under `/tmp/`; that
/// runtime string is not a build path. Complete GNU recipe acceptance also
/// requires read-only unbound scratch, IPC without writable shared memory,
/// no image-declared volumes, and the bound host/execution-root scan in
/// [`crate::recipe::PreparedRecipe`]. This list alone cannot establish build
/// path provenance. The default macOS list retains `/tmp/` until its own
/// filesystem boundary is qualified.
const LINUX_X86_64_DENIED_PATH_PREFIXES: &[&str] = &[
    "/home/",
    "/Users/",
    "/root/",
    "/private/var/",
    "/private/tmp/",
    "/var/folders/",
    "/github/workspace/",
    "/__w/",
];

/// Version-name prefixes accepted per library, besides `GLIBC_`.
///
/// `libgcc_s` versions its unwinder as `GCC_x.y[.z]`; those carry no glibc
/// floor and are recorded only.
const OTHER_VERSION_PREFIXES: &[(&str, &str)] = &[("libgcc_s.so.1", "GCC_")];

/// Compiled-in ELF acceptance policy.
#[derive(Debug, Clone)]
pub(crate) struct ElfPolicy {
    pub(crate) interpreter: &'static str,
    pub(crate) allowed_needed: &'static [&'static str],
    pub(crate) glibc_maximum: GlibcVersion,
    pub(crate) denied_path_prefixes: &'static [&'static str],
}

impl ElfPolicy {
    /// The reviewed `x86_64-unknown-linux-gnu` policy: glibc 2.34 ceiling.
    pub(crate) const fn linux_x86_64_gnu() -> Self {
        Self {
            interpreter: LINUX_X86_64_INTERPRETER,
            allowed_needed: LINUX_X86_64_NEEDED,
            glibc_maximum: GlibcVersion::new(2, 34),
            denied_path_prefixes: LINUX_X86_64_DENIED_PATH_PREFIXES,
        }
    }
}

/// Inspects a final ELF executable against `policy`.
///
/// # Errors
///
/// Returns [`ReleaseError::Inspect`] with a [`Violation`] when the bytes are
/// not a little-endian 64-bit `x86_64` executable, the interpreter differs,
/// a `DT_NEEDED` entry is outside the allowlist, `DT_RPATH`/`DT_RUNPATH` (or
/// a filter) is present, a version requirement is unparsable or above the
/// glibc ceiling, a section name or debug section is malformed, or a
/// runner-only path is embedded anywhere in the file. Missing debug info is
/// recorded, not rejected here; see [`ElfFacts::require_debug_sections`].
///
/// Structural inspection records final-file facts, not complete build-path
/// provenance. The GNU policy permits runtime `/tmp/` constants; its owning
/// recipe must additionally enforce the filesystem restrictions and scan its
/// actual bound host and execution roots before accepting the artifact.
pub(crate) fn inspect_elf(
    bytes: &[u8],
    policy: &ElfPolicy,
) -> Result<ElfFacts, Report<ReleaseError>> {
    let header = Header::parse(bytes).map_err(|error| malformed("ELF header", error))?;
    let endian = header
        .endian()
        .map_err(|error| malformed("ELF endianness", error))?;
    check_identity(header, endian)?;
    let interpreter = interpreter(header, endian, bytes)?;
    if interpreter != policy.interpreter {
        return Err(rejected(
            Violation::Interpreter,
            format!(
                "PT_INTERP is {interpreter:?}, expected {:?}",
                policy.interpreter
            ),
        ));
    }
    let sections = header
        .sections(endian, bytes)
        .map_err(|error| malformed("ELF section table", error))?;
    loader::validate(header, endian, bytes, &sections)?;
    let needed = dynamic_needed(header, endian, bytes, &sections)?;
    if let Some(name) = needed
        .iter()
        .find(|name| !policy.allowed_needed.contains(&name.as_str()))
    {
        return Err(rejected(Violation::Library, format!("DT_NEEDED {name:?}")));
    }
    let version_requirements = version_requirements(endian, bytes, &sections)?;
    let max_glibc = check_versions(&version_requirements, &needed, policy.glibc_maximum)?;
    let debug_sections = debug_sections(endian, bytes, &sections)?;
    reject_denied_paths_with_sections(bytes, policy.denied_path_prefixes, |offset| {
        section_at(endian, &sections, offset)
    })?;
    Ok(ElfFacts {
        sha256: sha256_hex(bytes),
        interpreter: interpreter.to_owned(),
        needed,
        version_requirements,
        max_glibc,
        debug_sections,
    })
}

/// Names the file-backed section containing one denied-path offset.
fn section_at<'a>(endian: Endianness, sections: &Sections<'a>, offset: usize) -> Option<&'a [u8]> {
    let offset = u64::try_from(offset).ok()?;
    sections.iter().find_map(|section| {
        if section.sh_type(endian) == format::SHT_NOBITS {
            return None;
        }
        let start = section.sh_offset(endian);
        let end = start.checked_add(section.sh_size(endian))?;
        (start..end)
            .contains(&offset)
            .then(|| sections.section_name(endian, section).ok())
            .flatten()
    })
}

/// Returns every section named like a required DWARF section, with the
/// number of file bytes it holds (zero for `SHT_NOBITS`).
///
/// Recording is not acceptance: [`ElfFacts::require_debug_sections`] decides.
fn debug_sections(
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
) -> Result<Vec<ElfDebugSection>, Report<ReleaseError>> {
    let mut found = Vec::new();
    for section in sections.iter() {
        let name = sections
            .section_name(endian, section)
            .map_err(|error| malformed("ELF section name", error))?;
        let Some(required) = REQUIRED_DEBUG_SECTIONS
            .iter()
            .find(|required| required.as_bytes() == name)
        else {
            continue;
        };
        let size = if section.sh_type(endian) == format::SHT_NOBITS {
            0
        } else {
            section
                .data(endian, bytes)
                .map_err(|error| malformed("ELF debug section", error))?
                .len()
        };
        found.push(ElfDebugSection {
            name: (*required).to_owned(),
            size,
        });
    }
    Ok(found)
}

/// Requires a little-endian `x86_64` `ET_EXEC`/`ET_DYN` file.
fn check_identity(header: &Header, endian: Endianness) -> Result<(), Report<ReleaseError>> {
    let ident = header.e_ident();
    let machine = header.e_machine(endian);
    let kind = header.e_type(endian);
    if ident.data != format::ELFDATA2LSB
        || machine != format::EM_X86_64
        || !matches!(kind, format::ET_EXEC | format::ET_DYN)
    {
        return Err(rejected(
            Violation::Architecture,
            format!(
                "ELF data {} machine {machine} type {kind}; expected little-endian x86_64 \
                 executable",
                ident.data
            ),
        ));
    }
    Ok(())
}

/// Returns the single `PT_INTERP` path.
fn interpreter<'data>(
    header: &Header,
    endian: Endianness,
    bytes: &'data [u8],
) -> Result<&'data str, Report<ReleaseError>> {
    let headers = header
        .program_headers(endian, bytes)
        .map_err(|error| malformed("ELF program headers", error))?;
    let mut found = Vec::new();
    for segment in headers {
        if let Some(path) = segment
            .interpreter(endian, bytes)
            .map_err(|error| malformed("ELF PT_INTERP", error))?
        {
            found.push(path);
        }
    }
    match found.as_slice() {
        [path] => utf8(path, "ELF PT_INTERP"),
        _ => Err(rejected(
            Violation::Interpreter,
            format!("expected exactly one PT_INTERP, found {}", found.len()),
        )),
    }
}

/// Returns `DT_NEEDED` names after cross-checking the dynamic section.
fn dynamic_needed(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
) -> Result<Vec<String>, Report<ReleaseError>> {
    let segment = single_dynamic_segment(header, endian, bytes)?;
    let mut dynamic_sections = sections
        .iter()
        .filter(|section| section.sh_type(endian) == format::SHT_DYNAMIC);
    let (Some(section), None) = (dynamic_sections.next(), dynamic_sections.next()) else {
        return Err(malformed(
            "ELF dynamic section",
            "expected exactly one SHT_DYNAMIC",
        ));
    };
    if (section.sh_offset(endian), section.sh_size(endian)) != segment {
        return Err(malformed(
            "ELF dynamic section",
            "does not match PT_DYNAMIC",
        ));
    }
    let (entries, link) = section
        .dynamic(endian, bytes)
        .map_err(|error| malformed("ELF dynamic entries", error))?
        .ok_or_else(|| malformed("ELF dynamic entries", "missing"))?;
    let strings = linked_strings(endian, bytes, sections, link)?;
    let strtab_address = sections
        .section(link)
        .map_err(|error| malformed("ELF dynamic string table", error))?
        .sh_addr(endian);
    read_dynamic_entries(entries, endian, strings, strtab_address)
}

/// Returns `(p_offset, p_filesz)` of the single `PT_DYNAMIC` segment.
fn single_dynamic_segment(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
) -> Result<(u64, u64), Report<ReleaseError>> {
    let headers = header
        .program_headers(endian, bytes)
        .map_err(|error| malformed("ELF program headers", error))?;
    let mut dynamic = headers
        .iter()
        .filter(|segment| segment.p_type(endian) == format::PT_DYNAMIC);
    match (dynamic.next(), dynamic.next()) {
        (Some(segment), None) => Ok((segment.p_offset(endian), segment.p_filesz(endian))),
        _ => Err(rejected(
            Violation::Interpreter,
            "expected exactly one PT_DYNAMIC: not a dynamically linked executable",
        )),
    }
}

/// Reads the dynamic entries up to `DT_NULL`.
fn read_dynamic_entries(
    entries: &[format::Dyn64<Endianness>],
    endian: Endianness,
    strings: StringTable<'_>,
    strtab_address: u64,
) -> Result<Vec<String>, Report<ReleaseError>> {
    let mut needed = Vec::new();
    let mut strtab = None;
    let mut terminated = false;
    for entry in entries {
        let tag = entry.tag32(endian);
        match tag {
            Some(format::DT_NULL) => {
                terminated = true;
                break;
            }
            Some(format::DT_STRTAB) => strtab = Some(entry.d_val(endian)),
            Some(format::DT_NEEDED) => needed.push(dynamic_string(entry, endian, strings)?),
            Some(format::DT_RPATH | format::DT_RUNPATH) => {
                let path = dynamic_string(entry, endian, strings)?;
                return Err(rejected(
                    Violation::SearchPath,
                    format!("DT_RPATH/DT_RUNPATH {path:?}"),
                ));
            }
            Some(
                format::DT_AUXILIARY | format::DT_FILTER | format::DT_AUDIT | format::DT_DEPAUDIT,
            ) => {
                return Err(rejected(
                    Violation::Library,
                    "additional loader library mechanism present",
                ));
            }
            _ => {}
        }
    }
    if !terminated || strtab != Some(strtab_address) {
        return Err(malformed(
            "ELF dynamic string table",
            "DT_STRTAB does not match the linked section",
        ));
    }
    Ok(needed)
}

/// Decodes a string-valued dynamic entry.
fn dynamic_string(
    entry: &format::Dyn64<Endianness>,
    endian: Endianness,
    strings: StringTable<'_>,
) -> Result<String, Report<ReleaseError>> {
    let raw = entry
        .string(endian, strings)
        .map_err(|error| malformed("ELF dynamic string", error))?;
    utf8(raw, "ELF dynamic string").map(str::to_owned)
}

/// Returns the string table linked from a section.
fn linked_strings<'data>(
    endian: Endianness,
    bytes: &'data [u8],
    sections: &Sections<'data>,
    link: SectionIndex,
) -> Result<StringTable<'data>, Report<ReleaseError>> {
    if link == SectionIndex(0) {
        return Err(malformed("ELF string table", "missing link"));
    }
    sections
        .strings(endian, bytes, link)
        .map_err(|error| malformed("ELF string table", error))
}

/// Returns every `.gnu.version_r` requirement, sorted and de-duplicated.
fn version_requirements(
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
) -> Result<Vec<VersionRequirement>, Report<ReleaseError>> {
    let (mut needs, link) = sections
        .gnu_verneed(endian, bytes)
        .map_err(|error| malformed("ELF version requirements", error))?
        .ok_or_else(|| {
            rejected(
                Violation::SymbolVersion,
                "no .gnu.version_r: the glibc ceiling cannot be checked",
            )
        })?;
    let strings = linked_strings(endian, bytes, sections, link)?;
    let mut found = BTreeSet::new();
    let parse_error = |error| malformed("ELF version requirement", error);
    while let Some((need, mut auxiliaries)) = needs.next().map_err(parse_error)? {
        let file = utf8(
            need.file(endian, strings).map_err(parse_error)?,
            "ELF vn_file",
        )?;
        while let Some(auxiliary) = auxiliaries.next().map_err(parse_error)? {
            let name = utf8(
                auxiliary.name(endian, strings).map_err(parse_error)?,
                "ELF vna_name",
            )?;
            found.insert(VersionRequirement {
                file: file.to_owned(),
                name: name.to_owned(),
            });
        }
    }
    Ok(found.into_iter().collect())
}

/// Checks every requirement and returns the highest `GLIBC_` version.
fn check_versions(
    requirements: &[VersionRequirement],
    needed: &[String],
    maximum: GlibcVersion,
) -> Result<GlibcVersion, Report<ReleaseError>> {
    let mut highest = None;
    for requirement in requirements {
        if !needed.contains(&requirement.file) {
            return Err(rejected(
                Violation::SymbolVersion,
                format!(
                    "version requirement on {:?}, which is not DT_NEEDED",
                    requirement.file
                ),
            ));
        }
        if let Some(version) = glibc_requirement(requirement)? {
            if version > maximum {
                return Err(rejected(
                    Violation::SymbolVersion,
                    format!(
                        "{} requires GLIBC_{version} > ceiling {maximum}",
                        requirement.file
                    ),
                ));
            }
            highest = highest.max(Some(version));
        }
    }
    highest.ok_or_else(|| {
        rejected(
            Violation::SymbolVersion,
            "no GLIBC_ requirement was observed",
        )
    })
}

/// Classifies one requirement: a parsed `GLIBC_` version, an accepted
/// non-glibc name (`None`), or a rejection.
fn glibc_requirement(
    requirement: &VersionRequirement,
) -> Result<Option<GlibcVersion>, Report<ReleaseError>> {
    if let Some(suffix) = requirement.name.strip_prefix(GLIBC_PREFIX) {
        return GlibcVersion::parse(suffix).map(Some).ok_or_else(|| {
            rejected(
                Violation::SymbolVersion,
                format!(
                    "unparsable version {:?} from {:?}",
                    requirement.name, requirement.file
                ),
            )
        });
    }
    let accepted = OTHER_VERSION_PREFIXES
        .iter()
        .any(|(file, prefix)| requirement.file == *file && requirement.name.starts_with(prefix));
    if accepted {
        Ok(None)
    } else {
        Err(rejected(
            Violation::SymbolVersion,
            format!(
                "unexpected version {:?} from {:?}",
                requirement.name, requirement.file
            ),
        ))
    }
}
