//! Binds section observations to the bytes selected by the ELF loader.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use object::Endianness;
use object::elf as format;
use object::read::elf::{Dyn as _, FileHeader as _, ProgramHeader as _, SectionHeader as _};

use super::{Header, Sections};
use crate::error::ReleaseError;
use crate::inspect::{Violation, malformed, rejected};

type Section = format::SectionHeader64<Endianness>;

fn invalid(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    malformed("ELF loader metadata", detail)
}

fn unique_section<'a>(
    sections: &'a Sections<'_>,
    endian: Endianness,
    kind: u32,
) -> Result<(usize, &'a Section), Report<ReleaseError>> {
    let mut found = sections
        .iter()
        .enumerate()
        .filter(|(_, section)| section.sh_type(endian) == kind);
    match (found.next(), found.next()) {
        (Some(section), None) => Ok(section),
        _ => Err(invalid(format!("expected one section of type {kind}"))),
    }
}

/// Resolves an entire virtual range through exactly one file-backed load.
fn mapped_offset(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    address: u64,
    size: u64,
) -> Result<u64, Report<ReleaseError>> {
    let end = address
        .checked_add(size)
        .ok_or_else(|| invalid("address overflow"))?;
    let programs = header.program_headers(endian, bytes).map_err(invalid)?;
    let mut offsets = Vec::new();
    for program in programs {
        if program.p_type(endian) != format::PT_LOAD {
            continue;
        }
        let start = program.p_vaddr(endian);
        let limit = start
            .checked_add(program.p_filesz(endian))
            .ok_or_else(|| invalid("segment overflow"))?;
        let memory_end = start
            .checked_add(program.p_memsz(endian))
            .ok_or_else(|| invalid("memory range overflow"))?;
        // Linux x86_64 mappings include whole 4 KiB pages. A second segment
        // can replace an earlier segment's bytes even before its nominal
        // start, or zero-fill them after its file-backed portion.
        let page_start = start & !4095;
        let page_end = memory_end
            .checked_add(4095)
            .ok_or_else(|| invalid("page range overflow"))?
            & !4095;
        if program.p_filesz(endian) > program.p_memsz(endian) {
            return Err(invalid("load file size exceeds its memory size"));
        }
        if page_start < end && page_end > address {
            if address < start || end > limit {
                return Err(invalid("load mapping partially overlaps observed metadata"));
            }
            let offset = program
                .p_offset(endian)
                .checked_add(address - start)
                .ok_or_else(|| invalid("file offset overflow"))?;
            let file_end = offset
                .checked_add(size)
                .ok_or_else(|| invalid("file range overflow"))?;
            if file_end > bytes.len() as u64 || program.p_filesz(endian) > program.p_memsz(endian) {
                return Err(invalid("load range exceeds file or memory"));
            }
            offsets.push(offset);
        }
    }
    match offsets.as_slice() {
        [offset] => Ok(*offset),
        _ => Err(invalid(
            "virtual range has no unique file-backed load mapping",
        )),
    }
}

fn required(tags: &BTreeMap<u32, u64>, tag: u32) -> Result<u64, Report<ReleaseError>> {
    tags.get(&tag)
        .copied()
        .ok_or_else(|| invalid(format!("missing dynamic tag {tag}")))
}

fn dynamic_tags(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
) -> Result<BTreeMap<u32, u64>, Report<ReleaseError>> {
    let (_, section) = unique_section(sections, endian, format::SHT_DYNAMIC)?;
    let programs = header.program_headers(endian, bytes).map_err(invalid)?;
    let mut segments = programs
        .iter()
        .filter(|program| program.p_type(endian) == format::PT_DYNAMIC);
    let (Some(segment), None) = (segments.next(), segments.next()) else {
        return Err(invalid("expected one dynamic segment"));
    };
    if mapped_offset(
        header,
        endian,
        bytes,
        segment.p_vaddr(endian),
        segment.p_filesz(endian),
    )? != section.sh_offset(endian)
        || segment.p_offset(endian) != section.sh_offset(endian)
        || segment.p_filesz(endian) != section.sh_size(endian)
    {
        return Err(invalid("dynamic section differs from loader range"));
    }
    let (entries, _) = section
        .dynamic(endian, bytes)
        .map_err(invalid)?
        .ok_or_else(|| invalid("missing dynamic entries"))?;
    let mut tags = BTreeMap::new();
    for entry in entries {
        let tag = entry
            .tag32(endian)
            .ok_or_else(|| invalid("unsupported dynamic tag width"))?;
        if tag == format::DT_NULL {
            return Ok(tags);
        }
        if matches!(
            tag,
            format::DT_AUDIT | format::DT_DEPAUDIT | format::DT_AUXILIARY | format::DT_FILTER
        ) {
            return Err(rejected(
                Violation::Library,
                "additional loader library mechanism",
            ));
        }
        if tag != format::DT_NEEDED && tags.insert(tag, entry.d_val(endian)).is_some() {
            return Err(invalid(format!("duplicate dynamic singleton {tag}")));
        }
    }
    Err(invalid("dynamic table has no terminator"))
}

fn check_section(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    section: &Section,
    address: u64,
) -> Result<(), Report<ReleaseError>> {
    if section.sh_addr(endian) != address
        || mapped_offset(header, endian, bytes, address, section.sh_size(endian))?
            != section.sh_offset(endian)
    {
        return Err(invalid("section differs from loader-selected bytes"));
    }
    Ok(())
}

pub(super) fn validate(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
) -> Result<(), Report<ReleaseError>> {
    let tags = dynamic_tags(header, endian, bytes, sections)?;
    let (_, dynamic) = unique_section(sections, endian, format::SHT_DYNAMIC)?;
    let string_index = dynamic.sh_link(endian) as usize;
    let strings = sections
        .iter()
        .nth(string_index)
        .ok_or_else(|| invalid("missing dynamic strings"))?;
    if strings.sh_type(endian) != format::SHT_STRTAB
        || strings.sh_size(endian) != required(&tags, format::DT_STRSZ)?
    {
        return Err(invalid("dynamic string table type or length mismatch"));
    }
    check_section(
        header,
        endian,
        bytes,
        strings,
        required(&tags, format::DT_STRTAB)?,
    )?;
    let (_, needs) = unique_section(sections, endian, format::SHT_GNU_VERNEED)?;
    check_section(
        header,
        endian,
        bytes,
        needs,
        required(&tags, format::DT_VERNEED)?,
    )?;
    if u64::from(needs.sh_info(endian)) != required(&tags, format::DT_VERNEEDNUM)?
        || needs.sh_link(endian) as usize != string_index
    {
        return Err(invalid("version requirement count or strings mismatch"));
    }
    validate_symbols(header, endian, bytes, sections, &tags, string_index)
}

fn validate_symbols(
    header: &Header,
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
    tags: &BTreeMap<u32, u64>,
    string_index: usize,
) -> Result<(), Report<ReleaseError>> {
    let (symbol_index, symbols) = unique_section(sections, endian, format::SHT_DYNSYM)?;
    let (_, versions) = unique_section(sections, endian, format::SHT_GNU_VERSYM)?;
    check_section(
        header,
        endian,
        bytes,
        symbols,
        required(tags, format::DT_SYMTAB)?,
    )?;
    check_section(
        header,
        endian,
        bytes,
        versions,
        required(tags, format::DT_VERSYM)?,
    )?;
    if required(tags, format::DT_SYMENT)? != 24
        || symbols.sh_entsize(endian) != 24
        || !symbols.sh_size(endian).is_multiple_of(24)
        || symbols.sh_size(endian) < 24
        || symbols.sh_link(endian) as usize != string_index
        || versions.sh_link(endian) as usize != symbol_index
        || versions.sh_size(endian) != symbols.sh_size(endian) / 12
    {
        return Err(invalid("dynamic symbol/version table shape mismatch"));
    }
    let indices = requirement_indices(
        endian,
        bytes,
        sections,
        required(tags, format::DT_VERNEEDNUM)?,
    )?;
    let symbol_bytes = symbols.data(endian, bytes).map_err(invalid)?;
    let version_bytes = versions.data(endian, bytes).map_err(invalid)?;
    for (symbol, version) in symbol_bytes
        .as_chunks::<24>()
        .0
        .iter()
        .zip(version_bytes.as_chunks::<2>().0)
    {
        let index = u16::from_le_bytes([version[0], version[1]]) & 0x7fff;
        let undefined = u16::from_le_bytes([symbol[6], symbol[7]]) == format::SHN_UNDEF;
        if undefined && index > 1 && !indices.contains(&index) {
            return Err(invalid(
                "undefined symbol references an unaccounted version",
            ));
        }
    }
    Ok(())
}

fn requirement_indices(
    endian: Endianness,
    bytes: &[u8],
    sections: &Sections<'_>,
    expected: u64,
) -> Result<BTreeSet<u16>, Report<ReleaseError>> {
    let (mut needs, _) = sections
        .gnu_verneed(endian, bytes)
        .map_err(invalid)?
        .ok_or_else(|| invalid("missing version requirements"))?;
    let mut count = 0;
    let mut indices = BTreeSet::new();
    while let Some((need, mut auxiliary)) = needs.next().map_err(invalid)? {
        count += 1;
        let mut auxiliary_count = 0;
        while let Some(version) = auxiliary.next().map_err(invalid)? {
            auxiliary_count += 1;
            let index = version.vna_other.get(endian) & 0x7fff;
            if index <= 1 || !indices.insert(index) {
                return Err(invalid("ambiguous version requirement index"));
            }
        }
        if auxiliary_count != need.vn_cnt.get(endian) || auxiliary_count == 0 {
            return Err(invalid("version auxiliary count mismatch"));
        }
    }
    if count != expected {
        return Err(invalid("version requirement count mismatch"));
    }
    Ok(indices)
}
