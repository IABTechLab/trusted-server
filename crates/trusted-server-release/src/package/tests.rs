use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::symlink;

use flate2::{Compression, write::GzEncoder};
use sha2::Sha256;

use super::*;

const MANIFEST: &[u8] = b"manifest_version = 3\nname = \"example-package\"\n";
const WASM: &[u8] = b"\0asm\x01\0\0\0";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn archive(manifest: &[u8], wasm: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, directory, body) in [
        ("example-package/", true, &[][..]),
        ("example-package/bin/", true, &[][..]),
        ("example-package/bin/main.wasm", false, wasm),
        ("example-package/fastly.toml", false, manifest),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(if directory { 0o755 } else { 0o644 });
        header.set_entry_type(if directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_cksum();
        archive
            .append_data(&mut header, name, body)
            .expect("should append package member");
    }
    let tar = archive.into_inner().expect("should finish archive");
    let mut compressed = GzEncoder::new(Vec::new(), Compression::fast());
    compressed.write_all(&tar).expect("should compress archive");
    compressed.finish().expect("should finish gzip")
}

#[test]
fn accepts_exact_named_package_with_independent_digests() {
    let directory = tempfile::tempdir().expect("should create private fixture");
    let manifest = directory.path().join("fastly.toml");
    fs::write(&manifest, MANIFEST).expect("should write manifest");
    fs::write(
        directory.path().join("example-package.tar.gz"),
        archive(MANIFEST, WASM),
    )
    .expect("should write package");
    fs::write(
        directory.path().join("package.tar.gz"),
        b"ignored generic archive",
    )
    .expect("should write unrelated generic archive");
    let accepted = inspect_package(
        directory.path(),
        &manifest,
        &digest(MANIFEST),
        &digest(WASM),
    )
    .expect("should accept exact package");
    assert_eq!(
        accepted["module_sha256"],
        digest(WASM),
        "should identify actual module bytes"
    );
}

#[test]
fn rejects_manifest_inode_substitution_even_when_bytes_match() {
    let directory = tempfile::tempdir().expect("should create private fixture");
    let path = directory.path().join("fastly.toml");
    fs::write(&path, MANIFEST).expect("should write original manifest");
    let before =
        Stamp::from(&fs::symlink_metadata(&path).expect("should inspect original manifest"));
    let replacement = directory.path().join("replacement");
    fs::write(&replacement, MANIFEST).expect("should write replacement manifest");
    fs::rename(&replacement, &path).expect("should replace manifest path");
    assert!(
        check_path(&path, &before).is_err(),
        "should reject replacement with identical bytes"
    );
}

fn check_archive(bytes: &[u8], expected_wasm: &str) -> Result<Value, Report<ReleaseError>> {
    let directory = tempfile::tempdir().expect("should create private fixture");
    let manifest = directory.path().join("fastly.toml");
    fs::write(&manifest, MANIFEST).expect("should write manifest");
    fs::write(directory.path().join("example-package.tar.gz"), bytes)
        .expect("should write archive");
    inspect_package(
        directory.path(),
        &manifest,
        &digest(MANIFEST),
        expected_wasm,
    )
}

fn tar_bytes() -> Vec<u8> {
    let compressed = archive(MANIFEST, WASM);
    let mut decoded = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut decoded)
        .expect("should decode fixture");
    decoded
}

fn compress(tar: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(tar).expect("should compress mutated TAR");
    encoder.finish().expect("should finish mutated gzip")
}

fn change_header(tar: &mut [u8], offset: usize, change: impl FnOnce(&mut tar::Header)) {
    let mut header = tar::Header::new_ustar();
    header
        .as_mut_bytes()
        .copy_from_slice(&tar[offset..offset + 512]);
    change(&mut header);
    header.set_cksum();
    tar[offset..offset + 512].copy_from_slice(header.as_bytes());
}

#[test]
fn accepts_only_the_known_directory_names_with_optional_single_trailing_slashes() {
    for root in ["example-package", "example-package/"] {
        for bin in ["example-package/bin", "example-package/bin/"] {
            let mut tar = tar_bytes();
            for (offset, path) in [(0, root), (512, bin)] {
                change_header(&mut tar, offset, |header| {
                    header.as_old_mut().name.fill(0);
                    header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
                });
            }
            check_archive(&compress(&tar), &digest(WASM))
                .expect("should accept each exact zero-sized type-5 directory representation");
        }
    }
}

#[test]
fn rejects_directory_alias_duplicates_across_both_trailing_slash_forms() {
    for directory in ["example-package", "example-package/bin"] {
        let slash = format!("{directory}/");
        for (first, second) in [(directory, slash.as_str()), (slash.as_str(), directory)] {
            let mut tar = tar_bytes();
            for (offset, path) in [(0, first), (512, second)] {
                change_header(&mut tar, offset, |header| {
                    header.as_old_mut().name.fill(0);
                    header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
                });
            }
            let error = check_archive(&compress(&tar), &digest(WASM))
                .expect_err("should reject duplicate logical directories across raw spellings");
            assert!(
                format!("{error:?}").contains("duplicate"),
                "should detect the logical duplicate before missing-member checks"
            );
        }
    }
}

#[test]
fn rejects_wrong_types_nonzero_sizes_and_other_directory_path_aliases() {
    for (offset, path) in [(0, "example-package"), (512, "example-package/bin")] {
        for kind in [
            0, b'0', b'1', b'2', b'3', b'4', b'6', b'x', b'g', b'L', b'K', b'S',
        ] {
            let mut tar = tar_bytes();
            change_header(&mut tar, offset, |header| {
                header.as_old_mut().name.fill(0);
                header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
                header.set_entry_type(tar::EntryType::new(kind));
            });
            assert!(
                check_archive(&compress(&tar), &digest(WASM)).is_err(),
                "should require directory type-5 for both new raw spellings"
            );
        }
        let mut tar = tar_bytes();
        change_header(&mut tar, offset, |header| {
            header.as_old_mut().name.fill(0);
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_size(1);
        });
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should require zero-sized directory entries"
        );
    }
    for path in [
        "example-package//",
        "example-package/./",
        "./example-package",
        "/example-package",
        "example-package/bin//",
        "example-package/./bin",
        "example-package\\bin",
    ] {
        let mut tar = tar_bytes();
        change_header(&mut tar, 0, |header| {
            header.as_old_mut().name.fill(0);
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        });
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should reject every unapproved raw path spelling"
        );
    }
}

#[test]
fn rejects_manifest_only_generic_layout() {
    let mut tar = tar_bytes();
    tar.drain(1024..2048);
    assert!(
        check_archive(&compress(&tar), &digest(WASM)).is_err(),
        "should require module and all four entries"
    );
}

#[test]
fn rejects_extra_members_and_duplicates() {
    let mut tar = tar_bytes();
    let root = tar[..512].to_vec();
    tar.splice(3072..3072, root);
    assert!(
        check_archive(&compress(&tar), &digest(WASM)).is_err(),
        "should reject swept-in extra directory"
    );
    let mut duplicate = tar_bytes();
    duplicate.copy_within(0..512, 512);
    assert!(
        check_archive(&compress(&duplicate), &digest(WASM)).is_err(),
        "should reject duplicate raw names"
    );
}

#[test]
fn rejects_raw_path_aliases_and_traversal() {
    for path in [
        "../main.wasm",
        "/example-package/bin/main.wasm",
        "example-package/./bin/main.wasm",
        "example-package\\bin\\main.wasm",
    ] {
        let mut tar = tar_bytes();
        change_header(&mut tar, 1024, |header| {
            header.as_old_mut().name.fill(0);
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        });
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should reject unsafe raw member {path}"
        );
    }
}

#[test]
fn rejects_links_metadata_and_special_entries() {
    for kind in *b"12346xgLKS" {
        let mut tar = tar_bytes();
        change_header(&mut tar, 1024, |header| {
            header.set_entry_type(tar::EntryType::new(kind))
        });
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should reject unsupported TAR type {kind}"
        );
    }
}

#[test]
fn rejects_changed_module_and_manifest_bytes() {
    assert!(
        check_archive(&archive(MANIFEST, WASM), &"0".repeat(64)).is_err(),
        "should require independently verified module digest"
    );
    let other = b"manifest_version = 3\nname = \"other\"\n";
    assert!(
        check_archive(&archive(other, WASM), &digest(WASM)).is_err(),
        "should compare exact approved manifest bytes"
    );
}

#[test]
fn rejects_malformed_and_component_modules_even_with_matching_hashes() {
    for wasm in [
        &b"not-wasm"[..],
        &b"\0asm\x0d\0\x01\0"[..],
        &b"\0asm\x01\0\0\0\x01\x01\xff"[..],
    ] {
        assert!(
            check_archive(&archive(MANIFEST, wasm), &digest(wasm)).is_err(),
            "should require validated actual core Wasm"
        );
    }
}

#[test]
fn rejects_concatenated_gzip_trailing_bytes_and_bad_trailers() {
    let original = archive(MANIFEST, WASM);
    let mut concatenated = original.clone();
    concatenated.extend_from_slice(&original);
    let mut trailing = original.clone();
    trailing.push(0);
    let mut truncated = original.clone();
    truncated.truncate(truncated.len() - 4);
    let mut bad_crc = original;
    let last = bad_crc.len() - 8;
    bad_crc[last] ^= 1;
    for invalid in [concatenated, trailing, truncated, bad_crc] {
        assert!(
            check_archive(&invalid, &digest(WASM)).is_err(),
            "should consume and validate exact single gzip member"
        );
    }
}

#[test]
fn rejects_hidden_tar_trailing_payload_and_missing_terminator() {
    let mut hidden = tar_bytes();
    hidden.push(1);
    assert!(
        check_archive(&compress(&hidden), &digest(WASM)).is_err(),
        "should reject hidden nonzero trailing data"
    );
    let mut missing = tar_bytes();
    missing.truncate(3072 + 512);
    assert!(
        check_archive(&compress(&missing), &digest(WASM)).is_err(),
        "should require two complete zero terminator blocks"
    );
}

#[test]
fn rejects_oversized_members_and_nonzero_directories_before_allocation() {
    for (offset, size) in [(0, 1), (1024, MODULE_LIMIT + 1), (2048, MANIFEST_LIMIT + 1)] {
        let mut tar = tar_bytes();
        change_header(&mut tar, offset, |header| header.set_size(size));
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should reject declared size {size} at header {offset}"
        );
    }
    let mut expanded = Expanded {
        reader: &[0_u8][..],
        count: EXPANDED_LIMIT,
    };
    assert!(
        expanded.read(&mut [0_u8]).is_err(),
        "should stop expansion at its bound"
    );
}

#[test]
fn rejects_noncanonical_private_and_reserved_manifests() {
    for manifest in [
        "manifest_version = 3\nname = \"\"\n",
        "manifest_version = 3\nname = \"package\"\n",
        "manifest_version = 3\nname = \"example_package\"\n",
        "manifest_version = 3\nname = \"example--package\"\n",
        "manifest_version = 3\nname = \"Example\"\n",
        "manifest_version = 3\nname = \"example\"\n# private material\n",
        "manifest_version = 3\nname = \"example\"\nservice_id = \"fictional-service\"\n",
        "manifest_version = 3\nname = \"example\"\n[local_server]\n",
        "manifest_version = 3\nname = \"example\"\n[scripts]\nbuild = \"false\"\n",
    ] {
        assert!(
            manifest_name(manifest.as_bytes()).is_err(),
            "should reject manifest outside narrow deployment profile"
        );
    }
    let long = format!("manifest_version = 3\nname = \"{}\"\n", "a".repeat(65));
    assert!(
        manifest_name(long.as_bytes()).is_err(),
        "should bound package name bytes"
    );
}

#[test]
fn rejects_unsafe_archive_files_and_substitution() {
    let directory = tempfile::tempdir().expect("should create private fixture");
    let original = directory.path().join("archive");
    fs::write(&original, archive(MANIFEST, WASM)).expect("should write archive");
    let alias = directory.path().join("alias");
    symlink(&original, &alias).expect("should create symlink fixture");
    assert!(
        RetainedArchive::open(alias).is_err(),
        "should reject leaf symlink"
    );
    let retained = RetainedArchive::open(original.clone()).expect("should open regular archive");
    let replacement = directory.path().join("replacement");
    fs::write(&replacement, archive(MANIFEST, WASM)).expect("should write replacement");
    fs::rename(replacement, &original).expect("should replace path");
    assert!(
        retained.check().is_err(),
        "should reject replaced archive path"
    );
    let hard = directory.path().join("hard");
    fs::hard_link(&original, &hard).expect("should create hard link fixture");
    assert!(
        RetainedArchive::open(original.clone()).is_err(),
        "should reject multiple hard links"
    );
    fs::remove_file(hard).expect("should remove hard link");
    File::options()
        .write(true)
        .open(&original)
        .expect("should open sparse fixture")
        .set_len(ARCHIVE_LIMIT + 1)
        .expect("should make oversized sparse fixture");
    assert!(
        RetainedArchive::open(original).is_err(),
        "should reject archive length before reading"
    );
    assert!(
        RetainedArchive::open(directory.path().to_owned()).is_err(),
        "should reject directories"
    );
}

#[test]
fn rejects_prefixed_hidden_and_corrupt_tar_headers() {
    let mut prefixed = tar_bytes();
    change_header(&mut prefixed, 1024, |header| {
        header
            .as_ustar_mut()
            .expect("should have USTAR header")
            .prefix[0] = b'x';
    });
    let mut hidden = tar_bytes();
    change_header(&mut hidden, 1024, |header| {
        header.as_old_mut().name[90] = b'x';
    });
    let mut corrupt = tar_bytes();
    corrupt[1024 + 148] ^= 1;
    for tar in [prefixed, hidden, corrupt] {
        assert!(
            check_archive(&compress(&tar), &digest(WASM)).is_err(),
            "should reject raw path prefixes, hidden bytes and bad checksums"
        );
    }
}

#[test]
fn rejects_invalid_expected_digest_syntax() {
    for invalid in [
        String::new(),
        "A".repeat(64),
        "g".repeat(64),
        "0".repeat(63),
    ] {
        assert!(
            check_archive(&archive(MANIFEST, WASM), &invalid).is_err(),
            "should require bounded lowercase SHA-256 identity"
        );
    }
}

#[test]
fn detects_changed_held_archive_contents() {
    let directory = tempfile::tempdir().expect("should create private fixture");
    let path = directory.path().join("archive");
    fs::write(&path, archive(MANIFEST, WASM)).expect("should write archive");
    let retained = RetainedArchive::open(path.clone()).expect("should open original archive");
    fs::write(path, b"changed").expect("should mutate held inode");
    assert!(
        retained.check().is_err(),
        "should reject changed metadata on held inode"
    );
}

#[test]
fn rejects_non_utf8_package_directory_without_panicking() {
    let package = PathBuf::from(std::ffi::OsString::from_vec(vec![b'p', 0xff]));
    assert!(
        diagnostic_path(&package).is_err(),
        "should reject non-UTF-8 diagnostic path without panic"
    );
}
