//! Synthetic Mach-O debug-map symbols and code signature blobs.

use object::macho as macho_format;

use super::Symbol;

/// Fictional relative debug-map entries of a linked `ts`.
pub(crate) fn debug_map_symbols() -> Vec<Symbol> {
    vec![
        Symbol {
            n_type: macho_format::N_OSO,
            name: "cargo-target/aarch64-apple-darwin/release/deps/ts-0123456789abcdef.o",
            value: 0,
        },
        Symbol {
            n_type: macho_format::N_OSO,
            name: "rust-tools/lib/rustlib/aarch64-apple-darwin/lib/libstd-0123.rlib(std.o)",
            value: 0,
        },
        Symbol {
            n_type: macho_format::N_SECT | macho_format::N_EXT,
            name: "_main",
            value: 0x1_0000_0000,
        },
    ]
}

/// A big-endian `SuperBlob` with one `CodeDirectory` carrying `flags`, an
/// optional team identifier offset and `extra_slots` index entries.
pub(crate) fn code_signature(flags: u32, team_offset: u32, extra_slots: &[u32]) -> Vec<u8> {
    let count = 1 + extra_slots.len();
    let header = 12 + 8 * count;
    let mut directory = Vec::new();
    for value in [0xfade_0c02_u32, 88, 0x2_0400, flags, 0, 0, 0, 0, 0] {
        directory.extend_from_slice(&value.to_be_bytes());
    }
    directory.extend_from_slice(&[32, 2, 0, 12]);
    for value in [0_u32, 0, team_offset] {
        directory.extend_from_slice(&value.to_be_bytes());
    }
    directory.resize(88, 0);
    let mut extra = Vec::new();
    let mut entries = vec![(0_u32, header)];
    for slot in extra_slots {
        entries.push((*slot, header + directory.len() + extra.len()));
        for value in [0xfade_0c01_u32, 12, 0] {
            extra.extend_from_slice(&value.to_be_bytes());
        }
    }
    let length = header + directory.len() + extra.len();
    let mut out = Vec::new();
    for value in [
        0xfade_0cc0_u32,
        u32::try_from(length).expect("should fit"),
        u32::try_from(count).expect("should fit"),
    ] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    for (slot, offset) in entries {
        out.extend_from_slice(&slot.to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).expect("should fit").to_be_bytes());
    }
    out.extend_from_slice(&directory);
    out.extend_from_slice(&extra);
    out
}

/// The linker's ad-hoc signature: flags `0x20002`, no team identifier and an
/// empty requirements slot.
pub(crate) fn linker_signature() -> Vec<u8> {
    code_signature(0x2_0002, 0, &[2])
}
