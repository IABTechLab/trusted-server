use std::collections::BTreeMap;
use std::io::Write as _;

use flate2::Crc;

use super::{Profile, contents};

pub(in crate::ordinary) fn zip(members: &[(&str, &[u8])], descriptor: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, body) in members {
        let offset = bytes.len() as u32;
        let mut crc = Crc::new();
        crc.update(body);
        let crc = crc.sum();
        let size = body.len() as u32;
        let flags = if descriptor { 8_u16 } else { 0 };
        bytes.extend(0x04034b50_u32.to_le_bytes());
        bytes.extend(20_u16.to_le_bytes());
        bytes.extend(flags.to_le_bytes());
        bytes.extend([0; 6]);
        for field in if descriptor {
            [0, 0, 0]
        } else {
            [crc, size, size]
        } {
            bytes.extend(field.to_le_bytes());
        }
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(*body);
        if descriptor {
            for field in [0x08074b50, crc, size, size] {
                bytes.extend(field.to_le_bytes());
            }
        }
        central.extend(0x02014b50_u32.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend(20_u16.to_le_bytes());
        central.extend(flags.to_le_bytes());
        central.extend([0; 6]);
        for field in [crc, size, size] {
            central.extend(field.to_le_bytes());
        }
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let offset = bytes.len() as u32;
    let size = central.len() as u32;
    bytes.extend(central);
    bytes.extend(0x06054b50_u32.to_le_bytes());
    bytes.extend([0; 4]);
    for _ in 0..2 {
        bytes.extend((members.len() as u16).to_le_bytes());
    }
    bytes.extend(size.to_le_bytes());
    bytes.extend(offset.to_le_bytes());
    bytes.extend([0; 2]);
    bytes
}

#[test]
fn reads_exact_ordinary_members_with_or_without_descriptors() {
    for descriptor in [false, true] {
        let bytes = zip(
            &[
                ("trusted-server-fastly.wasm", b"final bytes"),
                ("trusted-server-fastly.wasm.spdx.json", b"{}"),
            ],
            descriptor,
        );
        let members =
            contents(&bytes, Profile::Build).expect("should validate the ordinary build archive");
        assert_eq!(
            members["trusted-server-fastly.wasm"], b"final bytes",
            "should retain exact final bytes"
        );
        assert_eq!(
            members.len(),
            2,
            "should retain only the fixed exact members"
        );
    }
}

#[test]
fn ordinary_profiles_reject_missing_extra_duplicate_and_cross_role_members() {
    for members in [
        vec![("trusted-server-fastly.wasm", b"x".as_slice())],
        vec![
            ("trusted-server-fastly.wasm", b"x".as_slice()),
            ("trusted-server-fastly.wasm.spdx.json", b"{}"),
            ("extra", b"x"),
        ],
        vec![
            ("trusted-server-fastly.wasm", b"x".as_slice()),
            ("trusted-server-fastly.wasm", b"x"),
        ],
        vec![
            ("trusted-server-fastly.wasm", b"x".as_slice()),
            ("fastly-observation.json", b"{}"),
        ],
    ] {
        assert!(
            contents(&zip(&members, false), Profile::Build).is_err(),
            "should reject an archive outside the exact ordinary build profile"
        );
    }
    let observation = zip(&[("fastly-observation.json", b"{}")], true);
    assert_eq!(
        contents(&observation, Profile::Observation)
            .expect("should accept the exact observation member")["fastly-observation.json"],
        b"{}",
        "should retain only the opaque observation bytes"
    );
    assert!(
        contents(&observation, Profile::Build).is_err(),
        "should not treat observation archives as build archives"
    );
}

#[test]
fn ordinary_metadata_bounds_are_checked_before_decompression() {
    let mut bytes = zip(&[("fastly-observation.json", b"{}")], false);
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(
        bytes[end + 16..end + 20]
            .try_into()
            .expect("should read fixture central offset"),
    ) as usize;
    bytes[central + 24..central + 28].copy_from_slice(&(1024 * 1024 + 1_u32).to_le_bytes());
    let error = contents(&bytes, Profile::Observation)
        .expect_err("should reject role overflow before allocation or inflation");
    assert!(
        format!("{error:?}").contains("member identity or size rejected"),
        "should reach the source-owned role bound before CRC/decompression"
    );
}

#[test]
fn ordinary_profiles_reject_empty_members_and_truncation() {
    let bytes = zip(&[("fastly-observation.json", b"{}")], false);
    for length in 0..bytes.len() {
        assert!(
            contents(&bytes[..length], Profile::Observation).is_err(),
            "should reject a truncated ordinary archive at {length}"
        );
    }
    assert!(
        contents(
            &zip(&[("fastly-observation.json", b"")], false),
            Profile::Observation
        )
        .is_err(),
        "should reject an empty observation instead of producing a placeholder"
    );
}

#[test]
fn zip_rejects_paths_duplicates_hidden_data_and_corrupt_descriptors() {
    for name in [
        "../artifact",
        "/artifact",
        "a/artifact",
        "a\\artifact",
        "Artifact",
        ".",
        "",
    ] {
        let bytes = zip(&[(name, b"x")], false);
        assert!(
            super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject unsafe or undeclared {name:?}"
        );
    }
    for descriptor in [false, true] {
        let bytes = zip(&[("artifact", b"x"), ("artifact", b"x")], descriptor);
        assert!(
            super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject duplicate members"
        );
    }
    let mut bytes = zip(&[("artifact", b"x")], true);
    bytes[38] ^= 1;
    assert!(
        super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject corrupt member bytes"
    );
    let mut bytes = zip(&[("artifact", b"x")], true);
    bytes[43] ^= 1;
    assert!(
        super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject descriptor mismatch"
    );
    let mut bytes = zip(&[("artifact", b"x")], false);
    bytes.insert(0, 0);
    assert!(
        super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject leading hidden bytes"
    );
    let mut bytes = zip(&[("artifact", b"x")], false);
    bytes.push(0);
    assert!(
        super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
        "should reject trailing hidden bytes"
    );
}

#[test]
fn zip_rejects_oversized_declarations_and_unsupported_features() {
    let declared = BTreeMap::from([("artifact".to_owned(), 1)]);
    for index in [6, 8] {
        let mut bytes = zip(&[("artifact", b"x")], false);
        bytes[index] = 1;
        assert!(
            super::read(&bytes, &declared).is_err(),
            "should reject encryption or unsupported method"
        );
    }
    let bytes = zip(&[("artifact", b"x")], false);
    assert!(
        super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), u64::MAX)])).is_err(),
        "should reject declaration before allocation"
    );
    for length in 0..bytes.len() {
        assert!(
            super::read(&bytes[..length], &declared).is_err(),
            "should reject truncation at {length}"
        );
    }
}

#[test]
fn zip_deflate_preserves_full_bytes_and_rejects_extra_compressed_data() {
    let body = vec![b'x'; 128 * 1024 + 17];
    for descriptor in [false, true] {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&body).expect("should compress fixture");
        let compressed = encoder.finish().expect("should finish compression");
        let mut bytes = zip(&[("artifact", &body)], descriptor);
        bytes.splice(38..38 + body.len(), compressed.iter().copied());
        let central = 38 + compressed.len() + if descriptor { 16 } else { 0 };
        bytes[8..10].copy_from_slice(&8_u16.to_le_bytes());
        bytes[18..22].copy_from_slice(
            &(if descriptor {
                0
            } else {
                compressed.len() as u32
            })
            .to_le_bytes(),
        );
        bytes[central + 10..central + 12].copy_from_slice(&8_u16.to_le_bytes());
        bytes[central + 20..central + 24].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        if descriptor {
            bytes[38 + compressed.len() + 8..38 + compressed.len() + 12]
                .copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        }
        let end = bytes.len() - 22;
        bytes[end + 16..end + 20].copy_from_slice(&(central as u32).to_le_bytes());
        let declared = BTreeMap::from([("artifact".to_owned(), body.len() as u64)]);
        assert_eq!(
            super::read(&bytes, &declared).expect("should validate deflated stream")["artifact"],
            body,
            "should decompress exact full bytes"
        );
        bytes[central + 24..central + 28].copy_from_slice(&1_u32.to_le_bytes());
        assert!(
            super::read(&bytes, &BTreeMap::from([("artifact".to_owned(), 1)])).is_err(),
            "should reject expanded bytes beyond declared bound"
        );
    }
}

#[test]
fn zip_rejects_symlinks_zip64_and_local_central_disagreement() {
    let original = zip(&[("artifact", b"x")], false);
    let central = 39;
    let declared = BTreeMap::from([("artifact".to_owned(), 1)]);
    for (offset, bytes) in [
        (central + 38, (0o120777_u32 << 16).to_le_bytes().to_vec()),
        (central + 20, u32::MAX.to_le_bytes().to_vec()),
        (central + 42, 1_u32.to_le_bytes().to_vec()),
        (central + 6, 45_u16.to_le_bytes().to_vec()),
        (14, 1_u32.to_le_bytes().to_vec()),
        (30, b"different".to_vec()),
    ] {
        let mut changed = original.clone();
        changed[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(
            super::read(&changed, &declared).is_err(),
            "should reject ZIP ambiguity at offset {offset}"
        );
    }
    let bytes = zip(&[("artifact", b"x"), ("Artifact", b"x")], false);
    let declared = BTreeMap::from([("artifact".to_owned(), 1), ("Artifact".to_owned(), 1)]);
    assert!(
        super::read(&bytes, &declared).is_err(),
        "should reject case-colliding declared identities"
    );
}

#[test]
fn zip_requires_deflate_stream_end_even_when_crc_size_and_consumption_match() {
    for descriptor in [false, true] {
        for (body, nonfinal) in [
            (b"x".as_slice(), b"\x00\x01\x00\xfe\xffx".as_slice()),
            (b"".as_slice(), b"\x00\x00\x00\xff\xff".as_slice()),
        ] {
            let declared = BTreeMap::from([("artifact".to_owned(), body.len() as u64)]);
            let truncated = raw_deflate_zip(body, nonfinal, descriptor);
            assert!(
                super::read(&truncated, &declared).is_err(),
                "should reject nonfinal raw DEFLATE block despite exact output, CRC and consumed bytes"
            );
            let mut final_block = nonfinal.to_vec();
            final_block[0] |= 1;
            let valid = raw_deflate_zip(body, &final_block, descriptor);
            let mut trailing = final_block.clone();
            trailing.push(0);
            assert!(
                super::read(&raw_deflate_zip(body, &trailing, descriptor), &declared).is_err(),
                "should reject compressed bytes after the final DEFLATE marker"
            );
            for length in 0..final_block.len() {
                assert!(
                    super::read(
                        &raw_deflate_zip(body, &final_block[..length], descriptor),
                        &declared
                    )
                    .is_err(),
                    "should reject every truncated final DEFLATE block"
                );
            }
            assert_eq!(
                super::read(&valid, &declared).expect("should accept explicit final DEFLATE block")
                    ["artifact"],
                body,
                "should retain exact terminated stream output"
            );
        }
    }
}

fn raw_deflate_zip(body: &[u8], compressed: &[u8], descriptor: bool) -> Vec<u8> {
    let mut bytes = zip(&[("artifact", body)], descriptor);
    bytes.splice(38..38 + body.len(), compressed.iter().copied());
    let central = 38 + compressed.len() + if descriptor { 16 } else { 0 };
    bytes[8..10].copy_from_slice(&8_u16.to_le_bytes());
    bytes[18..22].copy_from_slice(
        &(if descriptor {
            0
        } else {
            compressed.len() as u32
        })
        .to_le_bytes(),
    );
    bytes[central + 10..central + 12].copy_from_slice(&8_u16.to_le_bytes());
    bytes[central + 20..central + 24].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    if descriptor {
        bytes[38 + compressed.len() + 8..38 + compressed.len() + 12]
            .copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    }
    let end = bytes.len() - 22;
    bytes[end + 16..end + 20].copy_from_slice(&(central as u32).to_le_bytes());
    bytes
}
