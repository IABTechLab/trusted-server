//! Embedded code signature parsing for linker-signed ad-hoc executables.
//!
//! `ld` signs an arm64 executable ad hoc as it links it: a big-endian
//! `SuperBlob` (`0xfade0cc0`) indexing one `CodeDirectory` (`0xfade0c02`)
//! whose flags are exactly `CS_ADHOC | CS_LINKER_SIGNED` (`0x20002`), and at
//! most one requirements blob, which must be the empty set (`0xfade0c01`,
//! length 12, count 0). A `codesign` identity would add a CMS
//! blob wrapper and a team identifier; both are rejected, as is any other
//! slot. The hashes are not recomputed: this records the signature's form,
//! not its validity, which the actual macOS startup exercises.

use error_stack::Report;

use super::super::{Violation, rejected};
use crate::error::ReleaseError;

/// Embedded signature `SuperBlob` magic.
pub(crate) const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
/// `CodeDirectory` blob magic.
pub(crate) const CSMAGIC_CODEDIRECTORY: u32 = 0xfade_0c02;
/// Slot of the primary `CodeDirectory`.
pub(crate) const CSSLOT_CODEDIRECTORY: u32 = 0;
/// Slot of the internal requirements blob.
pub(crate) const CSSLOT_REQUIREMENTS: u32 = 2;
/// Requirements set blob magic.
pub(crate) const CSMAGIC_REQUIREMENTS: u32 = 0xfade_0c01;
/// Length of a requirements set with no entries: magic, length and count.
const EMPTY_REQUIREMENTS_LENGTH: u32 = 12;
/// Slot of the CMS signature blob wrapper a signing identity adds.
pub(crate) const CSSLOT_SIGNATURESLOT: u32 = 0x1_0000;
/// `CS_ADHOC | CS_LINKER_SIGNED`, the only accepted directory flags.
pub(crate) const LINKER_SIGNED_ADHOC: u32 = 0x2_0002;
/// First `CodeDirectory` version carrying a team identifier offset.
const SUPPORTS_TEAM_ID: u32 = 0x2_0200;
/// Byte offset of the team identifier offset in a `CodeDirectory`.
const TEAM_OFFSET_FIELD: usize = 48;
/// Bytes of the `SuperBlob` header: magic, length and count.
const SUPERBLOB_HEADER: usize = 12;
/// Bytes of one `SuperBlob` index entry: slot type and offset.
const INDEX_ENTRY: usize = 8;

/// Reads a big-endian `u32` at `offset`.
fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let word: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(word))
}

fn signature_rejected(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    rejected(Violation::CodeSignature, detail)
}

/// Requires `blob`, the bytes `LC_CODE_SIGNATURE` names, to be exactly one
/// linker-signed ad-hoc `CodeDirectory` with no CMS blob or team identifier.
///
/// # Errors
///
/// Returns [`Violation::CodeSignature`] for a malformed or truncated blob,
/// another magic, a CMS signature slot, any slot other than the directory
/// and requirements, not exactly one directory, other directory flags, a
/// team identifier, or more than one or a non-empty requirements blob.
pub(super) fn check_linker_signed(blob: &[u8]) -> Result<(), Report<ReleaseError>> {
    let truncated = || signature_rejected("code signature is truncated");
    if be_u32(blob, 0) != Some(CSMAGIC_EMBEDDED_SIGNATURE) {
        return Err(signature_rejected(
            "code signature is not an embedded SuperBlob",
        ));
    }
    let length = be_u32(blob, 4)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| (SUPERBLOB_HEADER..=blob.len()).contains(length))
        .ok_or_else(truncated)?;
    let blob = &blob[..length];
    let count = be_u32(blob, 8)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(truncated)?;
    let index_end = count
        .checked_mul(INDEX_ENTRY)
        .and_then(|bytes| bytes.checked_add(SUPERBLOB_HEADER))
        .filter(|end| *end <= blob.len())
        .ok_or_else(truncated)?;
    let mut directories = 0_usize;
    let mut requirements = 0_usize;
    for entry in (SUPERBLOB_HEADER..index_end).step_by(INDEX_ENTRY) {
        let slot = be_u32(blob, entry).ok_or_else(truncated)?;
        let offset = be_u32(blob, entry + 4)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or_else(truncated)?;
        match slot {
            CSSLOT_CODEDIRECTORY => {
                directories += 1;
                check_directory(blob.get(offset..).ok_or_else(truncated)?)?;
            }
            CSSLOT_REQUIREMENTS => {
                requirements += 1;
                check_empty_requirements(blob.get(offset..).ok_or_else(truncated)?)?;
            }
            CSSLOT_SIGNATURESLOT => {
                return Err(signature_rejected(
                    "code signature carries a CMS signature, not a linker ad-hoc signature",
                ));
            }
            other => {
                return Err(signature_rejected(format!(
                    "code signature holds unexpected slot {other:#x}"
                )));
            }
        }
    }
    if directories != 1 {
        return Err(signature_rejected(format!(
            "code signature holds {directories} code directories, expected exactly one"
        )));
    }
    if requirements > 1 {
        return Err(signature_rejected(
            "code signature holds more than one requirements blob",
        ));
    }
    Ok(())
}

/// Requires an empty requirements set: magic `0xfade0c01`, length 12 and
/// a zero count.
fn check_empty_requirements(requirements: &[u8]) -> Result<(), Report<ReleaseError>> {
    let empty = be_u32(requirements, 0) == Some(CSMAGIC_REQUIREMENTS)
        && be_u32(requirements, 4) == Some(EMPTY_REQUIREMENTS_LENGTH)
        && be_u32(requirements, 8) == Some(0);
    if !empty {
        return Err(signature_rejected(
            "code signature requirements are not the empty set",
        ));
    }
    Ok(())
}

/// Requires one `CodeDirectory` with the linker-signed ad-hoc flags and no
/// team identifier.
fn check_directory(directory: &[u8]) -> Result<(), Report<ReleaseError>> {
    let truncated = || signature_rejected("code directory is truncated");
    if be_u32(directory, 0) != Some(CSMAGIC_CODEDIRECTORY) {
        return Err(signature_rejected("code directory has another magic"));
    }
    let length = be_u32(directory, 4)
        .and_then(|length| usize::try_from(length).ok())
        .filter(|length| *length <= directory.len())
        .ok_or_else(truncated)?;
    let directory = &directory[..length];
    let version = be_u32(directory, 8).ok_or_else(truncated)?;
    let flags = be_u32(directory, 12).ok_or_else(truncated)?;
    if flags != LINKER_SIGNED_ADHOC {
        return Err(signature_rejected(format!(
            "code directory flags {flags:#x}, expected linker-signed ad-hoc \
             {LINKER_SIGNED_ADHOC:#x}"
        )));
    }
    if version >= SUPPORTS_TEAM_ID {
        let team = be_u32(directory, TEAM_OFFSET_FIELD).ok_or_else(truncated)?;
        if team != 0 {
            return Err(signature_rejected("code directory names a team identifier"));
        }
    }
    Ok(())
}
