//! Deliberately narrow ZIP32 profile: flat regular files, stored or deflated.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use flate2::{Crc, Decompress, FlushDecompress, Status};

use crate::documents::Role;

const ARCHIVE_LIMIT: usize = 768 * 1024 * 1024;

fn collection_error(detail: impl core::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Document).attach(detail.to_string())
}

use crate::error::ReleaseError;

const MEMBER_LIMIT: u64 = 512 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 768 * 1024 * 1024;
const MEMBER_COUNT: usize = 32;

type Result<T> = std::result::Result<T, Report<ReleaseError>>;

fn slice(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| collection_error("ZIP offset overflow"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| collection_error("truncated ZIP"))
}
fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        slice(bytes, offset, 2)?
            .try_into()
            .map_err(collection_error)?,
    ))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        slice(bytes, offset, 4)?
            .try_into()
            .map_err(collection_error)?,
    ))
}
fn require(condition: bool, detail: &str) -> Result<()> {
    if !condition {
        return Err(collection_error(detail));
    }
    Ok(())
}

struct Entry {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: usize,
    expanded: usize,
    offset: usize,
}

fn central(
    bytes: &[u8],
    cursor: usize,
    declared: &BTreeMap<String, u64>,
) -> Result<(Entry, usize)> {
    require(
        u32_at(bytes, cursor)? == 0x02014b50,
        "missing central ZIP member",
    )?;
    let flags = u16_at(bytes, cursor + 8)?;
    let method = u16_at(bytes, cursor + 10)?;
    let name_length = usize::from(u16_at(bytes, cursor + 28)?);
    let extra = usize::from(u16_at(bytes, cursor + 30)?);
    let comment = usize::from(u16_at(bytes, cursor + 32)?);
    require(
        flags & !0x0808 == 0 && matches!(method, 0 | 8),
        "unsupported ZIP flags or compression",
    )?;
    require(
        u16_at(bytes, cursor + 6)? <= 20
            && extra == 0
            && comment == 0
            && u16_at(bytes, cursor + 34)? == 0,
        "ZIP64, extras, comments or multidisk ZIP rejected",
    )?;
    let mode = u32_at(bytes, cursor + 38)? >> 16;
    require(
        mode & 0o170000 == 0 || mode & 0o170000 == 0o100000,
        "ZIP member is not a regular file",
    )?;
    require(
        u32_at(bytes, cursor + 38)? & 0x10 == 0,
        "ZIP directory rejected",
    )?;
    let name = std::str::from_utf8(slice(bytes, cursor + 46, name_length)?)
        .map_err(collection_error)?
        .to_owned();
    safe_name(&name).map_err(|error| error.change_context(ReleaseError::Document))?;
    let expanded = u32_at(bytes, cursor + 24)?;
    let compressed = u32_at(bytes, cursor + 20)?;
    require(
        expanded != u32::MAX
            && compressed != u32::MAX
            && u64::from(expanded) <= MEMBER_LIMIT
            && u64::from(compressed) <= MEMBER_LIMIT,
        "ZIP member limit exceeded",
    )?;
    require(
        declared.get(&name) == Some(&u64::from(expanded)),
        "undeclared ZIP member or changed declared length",
    )?;
    Ok((
        Entry {
            name,
            flags,
            method,
            crc: u32_at(bytes, cursor + 16)?,
            compressed: compressed as usize,
            expanded: expanded as usize,
            offset: u32_at(bytes, cursor + 42)? as usize,
        },
        cursor + 46 + name_length,
    ))
}

fn local(bytes: &[u8], entry: &Entry, central_start: usize) -> Result<(usize, Vec<u8>)> {
    let start = entry.offset;
    require(
        u32_at(bytes, start)? == 0x04034b50,
        "missing local ZIP member",
    )?;
    require(
        u16_at(bytes, start + 4)? <= 20
            && u16_at(bytes, start + 6)? == entry.flags
            && u16_at(bytes, start + 8)? == entry.method,
        "local ZIP profile disagrees with central entry",
    )?;
    let name_length = usize::from(u16_at(bytes, start + 26)?);
    require(u16_at(bytes, start + 28)? == 0, "local ZIP extras rejected")?;
    require(
        slice(bytes, start + 30, name_length)? == entry.name.as_bytes(),
        "local ZIP name disagrees",
    )?;
    let descriptor = entry.flags & 8 != 0;
    for (offset, value) in [
        (14, entry.crc),
        (18, entry.compressed as u32),
        (22, entry.expanded as u32),
    ] {
        let local = u32_at(bytes, start + offset)?;
        require(
            local == value || (descriptor && local == 0),
            "local ZIP sizes or CRC disagree",
        )?;
    }
    let body_start = start + 30 + name_length;
    let compressed = slice(bytes, body_start, entry.compressed)?;
    let mut end = body_start + entry.compressed;
    if descriptor {
        if u32_at(bytes, end)? == 0x08074b50 {
            end += 4;
        }
        require(
            u32_at(bytes, end)? == entry.crc
                && u32_at(bytes, end + 4)? == entry.compressed as u32
                && u32_at(bytes, end + 8)? == entry.expanded as u32,
            "ZIP data descriptor disagrees",
        )?;
        end += 12;
    }
    require(
        end <= central_start,
        "ZIP member overlaps central directory",
    )?;
    let body = if entry.method == 0 {
        require(
            entry.compressed == entry.expanded,
            "stored ZIP sizes disagree",
        )?;
        compressed.to_vec()
    } else {
        inflate(compressed, entry.expanded)?
    };
    let mut crc = Crc::new();
    crc.update(&body);
    require(
        body.len() == entry.expanded && crc.sum() == entry.crc,
        "ZIP member CRC mismatch",
    )?;
    Ok((end, body))
}

fn read(bytes: &[u8], declared: &BTreeMap<String, u64>) -> Result<BTreeMap<String, Vec<u8>>> {
    require(
        bytes.len() <= ARCHIVE_LIMIT
            && bytes.len() >= 22
            && !declared.is_empty()
            && declared.len() <= MEMBER_COUNT,
        "ZIP archive/count limit exceeded",
    )?;
    let mut total = 0_u64;
    let mut names = BTreeSet::new();
    for (name, size) in declared {
        safe_name(name).map_err(|error| error.change_context(ReleaseError::Document))?;
        total = total
            .checked_add(*size)
            .ok_or_else(|| collection_error("ZIP expanded limit overflow"))?;
        require(
            *size <= MEMBER_LIMIT
                && total <= TOTAL_LIMIT
                && names.insert(name.to_ascii_lowercase()),
            "ZIP declared size or duplicate name rejected",
        )?;
    }
    let end = bytes.len() - 22;
    require(
        u32_at(bytes, end)? == 0x06054b50 && u16_at(bytes, end + 20)? == 0,
        "ZIP end record or trailing data rejected",
    )?;
    let count = usize::from(u16_at(bytes, end + 10)?);
    require(
        u16_at(bytes, end + 4)? == 0
            && u16_at(bytes, end + 6)? == 0
            && usize::from(u16_at(bytes, end + 8)?) == count
            && count == declared.len(),
        "ZIP disk/count mismatch",
    )?;
    let central_start = u32_at(bytes, end + 16)? as usize;
    let central_size = u32_at(bytes, end + 12)? as usize;
    require(
        central_start.checked_add(central_size) == Some(end),
        "ZIP central bounds mismatch",
    )?;
    let mut cursor = central_start;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let (entry, next) = central(bytes, cursor, declared)?;
        require(
            seen.insert(entry.name.to_ascii_lowercase()),
            "duplicate ZIP member",
        )?;
        entries.push(entry);
        cursor = next;
    }
    require(cursor == end, "hidden central ZIP members rejected")?;
    entries.sort_by_key(|entry| entry.offset);
    let mut expected_offset = 0;
    let mut members = BTreeMap::new();
    for entry in entries {
        require(
            entry.offset == expected_offset,
            "overlapping or hidden local ZIP members rejected",
        )?;
        let (next, body) = local(bytes, &entry, central_start)?;
        members.insert(entry.name, body);
        expected_offset = next;
    }
    require(
        expected_offset == central_start,
        "hidden data before central ZIP rejected",
    )?;
    Ok(members)
}

fn safe_name(name: &str) -> Result<()> {
    require(
        !name.is_empty()
            && name.len() <= 160
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            }),
        "unsafe flat ZIP member name",
    )
}

// Exhausted input is not a terminal DEFLATE marker, even with matching output and CRC.
fn inflate(compressed: &[u8], expanded: usize) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(false);
    let mut body = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let offset = usize::try_from(before_in).map_err(collection_error)?;
        let input = compressed
            .get(offset..)
            .ok_or_else(|| collection_error("deflated ZIP consumed beyond its member"))?;
        let remaining = expanded - body.len();
        let limit = buffer.len().min(remaining + 1);
        let status = decoder
            .decompress(input, &mut buffer[..limit], FlushDecompress::None)
            .map_err(collection_error)?;
        let written =
            usize::try_from(decoder.total_out() - before_out).map_err(collection_error)?;
        require(
            written <= remaining,
            "deflated ZIP exceeds declared expanded size",
        )?;
        body.extend_from_slice(&buffer[..written]);
        if status == Status::StreamEnd {
            require(
                decoder.total_in() == compressed.len() as u64
                    && decoder.total_out() == expanded as u64,
                "deflated ZIP length or compressed consumption mismatch",
            )?;
            return Ok(body);
        }
        require(
            decoder.total_in() != before_in || decoder.total_out() != before_out,
            "deflated ZIP did not reach its final stream marker",
        )?;
    }
}

/// Source-owned fixed member sets for the ordinary transfer.
#[derive(Clone, Copy)]
pub(super) enum Profile {
    Build,
    Observation,
}

/// Reads only the fixed flat regular members, under source-owned limits.
///
/// Directory lengths are untrusted allocation inputs, not observed build facts.
/// The carried reader still verifies every local/central/descriptor/CRC join;
/// the caller separately checks the authenticated archive and artifact digests.
pub(super) fn contents(bytes: &[u8], profile: Profile) -> Result<BTreeMap<String, Vec<u8>>> {
    let limits = match profile {
        Profile::Build => BTreeMap::from([
            (super::ASSET.to_owned(), Role::Executable.limit()),
            (format!("{}.spdx.json", super::ASSET), Role::Spdx.limit()),
        ]),
        Profile::Observation => {
            BTreeMap::from([("fastly-observation.json".to_owned(), Role::Metadata.limit())])
        }
    };
    require(
        bytes.len() >= 22 && bytes.len() <= ARCHIVE_LIMIT,
        "ordinary ZIP archive size rejected",
    )?;
    let end = bytes.len() - 22;
    require(
        u32_at(bytes, end)? == 0x06054b50 && usize::from(u16_at(bytes, end + 10)?) == limits.len(),
        "ordinary ZIP exact member count rejected",
    )?;
    let mut cursor = u32_at(bytes, end + 16)? as usize;
    let mut declared = BTreeMap::new();
    for _ in 0..limits.len() {
        require(
            u32_at(bytes, cursor)? == 0x02014b50,
            "ordinary ZIP central record rejected",
        )?;
        let length = usize::from(u16_at(bytes, cursor + 28)?);
        let extra = usize::from(u16_at(bytes, cursor + 30)?);
        let comment = usize::from(u16_at(bytes, cursor + 32)?);
        let name =
            std::str::from_utf8(slice(bytes, cursor + 46, length)?).map_err(collection_error)?;
        let expanded = u64::from(u32_at(bytes, cursor + 24)?);
        require(
            limits
                .get(name)
                .is_some_and(|limit| expanded > 0 && expanded <= *limit)
                && declared.insert(name.to_owned(), expanded).is_none(),
            "ordinary ZIP member identity or size rejected",
        )?;
        cursor += 46 + length + extra + comment;
    }
    require(cursor == end, "ordinary ZIP central length rejected")?;
    read(bytes, &declared)
}

#[cfg(test)]
pub(super) mod tests;
