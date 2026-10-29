use std::fs;
use std::io;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::{Sha256, Sha512};

use super::*;
use crate::materials::archive::{Expected, verify};
use crate::materials::{LockedMaterials, locked};
use crate::process::Cancellation;

fn compressed(tar: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(tar).expect("should compress TAR bytes");
    encoder.finish().expect("should finish gzip trailer")
}

fn tar_bytes(entries: &[(&str, u8, u32, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind, mode, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::new(*kind));
        header.set_mode(*mode);
        header.set_size(bytes.len() as u64);
        if name.len() <= 100 {
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        } else {
            let (prefix, leaf) = name
                .rsplit_once('/')
                .expect("should split long fixture name");
            let ustar = header.as_ustar_mut().expect("should have USTAR header");
            ustar.prefix[..prefix.len()].copy_from_slice(prefix.as_bytes());
            ustar.name[..leaf.len()].copy_from_slice(leaf.as_bytes());
        }
        header.set_cksum();
        builder
            .append(&header, *bytes)
            .expect("should append fixture entry");
    }
    builder.into_inner().expect("should finish TAR")
}

fn original(bytes: &[u8]) -> VerifiedOriginal {
    original_with_timeout(bytes, Duration::from_secs(10))
}

fn original_with_timeout(bytes: &[u8], timeout: Duration) -> VerifiedOriginal {
    let directory = tempfile::tempdir().expect("should create archive directory");
    let path = directory.path().join("original.tgz");
    fs::write(&path, bytes).expect("should write archive");
    verify(
        &path,
        Expected::Sha256(&hex::encode(Sha256::digest(bytes))),
        &"a".repeat(40),
        "npm:node_modules/fictional-package",
        Instant::now() + timeout,
        &Cancellation::default(),
    )
    .expect("should verify original bytes")
}

#[test]
fn extracts_verified_regular_files_and_preserves_original_bytes() {
    let bytes = compressed(&tar_bytes(&[(
        "package/bin/tool",
        b'0',
        0o755,
        b"fictional executable",
    )]));
    let extracted = extract(
        original(&bytes),
        Selection {
            root: "package",
            index_alias: false,
            node_tar_timestamps: false,
            local_pax: false,
            cargo_package: false,
        },
    )
    .expect("should extract verified regular file");
    assert_eq!(
        fs::read(extracted.directory.path().join("package/bin/tool")).expect("should read file"),
        b"fictional executable",
        "should retain exact member bytes"
    );
    let mut retained = Vec::new();
    let mut original = extracted.original;
    original
        .copy_verified(&mut retained)
        .expect("should retain authenticated compressed original");
    assert_eq!(retained, bytes, "should preserve exact compressed bytes");
}

#[test]
fn accepts_bounded_native_members_above_64_mib() {
    let size = 64 * 1024 * 1024 + 1;
    let encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_ustar();
    header.set_mode(0o644);
    header.set_size(size);
    header.set_cksum();
    builder
        .append_data(&mut header, "package/native.node", io::repeat(0).take(size))
        .expect("should stream a bounded large native member");
    let bytes = builder
        .into_inner()
        .expect("should finish native fixture TAR")
        .finish()
        .expect("should finish native fixture gzip");
    let extracted = extract(
        original_with_timeout(&bytes, Duration::from_secs(60)),
        Selection {
            root: "package",
            index_alias: false,
            node_tar_timestamps: false,
            local_pax: false,
            cargo_package: false,
        },
    )
    .expect("should support authenticated native members larger than 64 MiB");
    assert_eq!(
        fs::metadata(extracted.root().join("native.node"))
            .expect("should inspect native member")
            .len(),
        size,
        "should retain every original native byte"
    );
    extracted
        .verify_unchanged()
        .expect("should stream recheck the entire large native member");
}

#[test]
fn rejects_unsafe_paths_types_modes_duplicates_and_collisions() {
    for entries in [
        vec![("other/file", b'0', 0o644, b"x".as_slice())],
        vec![("package/../file", b'0', 0o644, b"x".as_slice())],
        vec![("package/./file", b'0', 0o644, b"x".as_slice())],
        vec![("package\\file", b'0', 0o644, b"x".as_slice())],
        vec![("/package/file", b'0', 0o644, b"x".as_slice())],
        vec![("package/file", b'0', 0o4644, b"x".as_slice())],
        vec![("package/file", b'2', 0o644, b"".as_slice())],
        vec![("package/file", b'1', 0o644, b"".as_slice())],
        vec![("package/file", b'6', 0o644, b"".as_slice())],
        vec![("package/file", b'x', 0o644, b"x".as_slice())],
        vec![
            ("package/file", b'0', 0o644, b"x".as_slice()),
            ("package/file", b'0', 0o644, b"x".as_slice()),
        ],
        vec![
            ("package/file", b'0', 0o644, b"x".as_slice()),
            ("package/file/child", b'0', 0o644, b"x".as_slice()),
        ],
        vec![
            ("package/File", b'0', 0o644, b"x".as_slice()),
            ("package/file", b'0', 0o644, b"x".as_slice()),
        ],
        vec![
            ("package/", b'5', 0o755, b"".as_slice()),
            ("package", b'5', 0o755, b"".as_slice()),
            ("package/file", b'0', 0o644, b"x".as_slice()),
        ],
    ] {
        assert!(
            extract(
                original(&compressed(&tar_bytes(&entries))),
                Selection {
                    root: "package",
                    index_alias: false,
                    node_tar_timestamps: false,
                    local_pax: false,
                    cargo_package: false,
                }
            )
            .is_err(),
            "should reject unsafe member set"
        );
    }
}

#[test]
fn rejects_excessive_depth_member_size_and_cancelled_extraction() {
    let name = format!("package/{}file", "a/".repeat(64));
    let bytes = compressed(&tar_bytes(&[(&name, b'0', 0o644, b"x")]));
    assert!(
        extract(
            original(&bytes),
            Selection {
                root: "package",
                index_alias: false,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should reject excessive member depth"
    );
    let mut tar = tar_bytes(&[("package/file", b'0', 0o644, b"x")]);
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes().copy_from_slice(&tar[..512]);
    header.set_size(MEMBER_LIMIT + 1);
    header.set_cksum();
    tar[..512].copy_from_slice(header.as_bytes());
    let rejected = extract(
        original(&compressed(&tar)),
        Selection {
            root: "package",
            index_alias: false,
            node_tar_timestamps: false,
            local_pax: false,
            cargo_package: false,
        },
    )
    .err()
    .expect("should reject oversized member before reading body");
    let diagnostic = format!("{rejected:?}");
    assert!(
        diagnostic.contains("package/file")
            && diagnostic.contains("134217729")
            && diagnostic.contains("134217728"),
        "should identify the rejected member, actual size and enforced bound"
    );
    let bytes = compressed(&tar_bytes(&[("package/file", b'0', 0o644, b"x")]));
    let cancellation = Cancellation::default();
    // Reverification binds the extraction to the same cancellation token.
    let directory = tempfile::tempdir().expect("should create archive directory");
    let path = directory.path().join("original.tgz");
    fs::write(&path, &bytes).expect("should write archive");
    let original = verify(
        &path,
        Expected::Sha256(&hex::encode(Sha256::digest(&bytes))),
        &"a".repeat(40),
        "npm:node_modules/fictional-package",
        Instant::now() + Duration::from_secs(10),
        &cancellation,
    )
    .expect("should verify original");
    cancellation.cancel();
    assert!(
        extract(
            original,
            Selection {
                root: "package",
                index_alias: false,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should carry cancellation into extraction"
    );
}

#[test]
fn index_alias_exception_requires_identical_pair_and_no_third_entry() {
    let pair = [
        ("package/./dist/index.js", b'0', 0o644, b"same".as_slice()),
        ("package/dist/index.js", b'0', 0o644, b"same".as_slice()),
    ];
    let bytes = compressed(&tar_bytes(&pair));
    assert!(
        extract(
            original(&compressed(&tar_bytes(&pair[..1]))),
            Selection {
                root: "package",
                index_alias: true,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should require canonical partner for dot alias"
    );
    assert!(
        extract(
            original(&bytes),
            Selection {
                root: "package",
                index_alias: true,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_ok(),
        "should allow exactly reviewed identical pair"
    );
    assert!(
        extract(
            original(&bytes),
            Selection {
                root: "package",
                index_alias: false,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should reject pair for other locked identities"
    );
    let mut changed = pair;
    changed[1].3 = b"evil";
    assert!(
        extract(
            original(&compressed(&tar_bytes(&changed))),
            Selection {
                root: "package",
                index_alias: true,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should reject different alias bytes"
    );
    let mut third = pair.to_vec();
    third.push(pair[0]);
    assert!(
        extract(
            original(&compressed(&tar_bytes(&third))),
            Selection {
                root: "package",
                index_alias: true,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should reject third occurrence"
    );
    let directory_alias = [
        ("package/./dist/index.js/", b'5', 0o755, b"".as_slice()),
        ("package/file", b'0', 0o644, b"x".as_slice()),
    ];
    assert!(
        extract(
            original(&compressed(&tar_bytes(&directory_alias))),
            Selection {
                root: "package",
                index_alias: true,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            }
        )
        .is_err(),
        "should restrict dot alias exception to regular files"
    );
}

#[test]
fn rejects_hidden_data_bad_trailers_concatenation_and_truncation() {
    let tar = tar_bytes(&[("package/file", b'0', 0o644, b"x")]);
    let mut hidden = tar.clone();
    hidden.extend_from_slice(&tar);
    let bytes = compressed(&tar);
    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 8;
    corrupt[last] ^= 1;
    let mut concatenated = bytes.clone();
    concatenated.extend_from_slice(&bytes);
    let mut trailing = bytes.clone();
    trailing.push(0);
    for bad in [
        compressed(&hidden),
        corrupt,
        concatenated,
        trailing,
        bytes[..bytes.len() - 1].to_vec(),
        compressed(&tar[..512]),
    ] {
        assert!(
            extract(
                original(&bad),
                Selection {
                    root: "package",
                    index_alias: false,
                    node_tar_timestamps: false,
                    local_pax: false,
                    cargo_package: false,
                }
            )
            .is_err(),
            "should reject incomplete or ambiguous archive"
        );
    }
}

fn catalog(bytes: &[u8]) -> LockedMaterials {
    let lock = serde_json::to_vec(&json!({"lockfileVersion":3, "packages": {
        "": {}, "node_modules/fictional-package": {
            "version":"1.0.0", "resolved":format!("{}fictional-package/-/fictional-package-1.0.0.tgz", locked::NPM_REGISTRY),
            "integrity":format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
        }
    }})).expect("should encode locked original identity");
    LockedMaterials {
        source_commit: "a".repeat(40),
        cargo_lock_sha256: "0".repeat(64),
        npm_lock_sha256: "0".repeat(64),
        cargo: locked::parse_cargo(b"version = 4\npackage = []\n")
            .expect("should parse empty Cargo lock"),
        npm: locked::parse_npm(&lock).expect("should parse fixture npm lock"),
    }
}

#[test]
fn catalog_extraction_rejoins_source_and_locked_acquisition_identity() {
    let bytes = compressed(&tar_bytes(&[("package/file", b'0', 0o644, b"x")]));
    let mut catalog = catalog(&bytes);
    assert!(
        catalog
            .extract_npm("node_modules/fictional-package", original(&bytes))
            .is_ok(),
        "should extract bound locked original"
    );
    assert!(
        catalog
            .extract_npm("node_modules/other", original(&bytes))
            .is_err(),
        "should reject unknown installation identity"
    );
    assert!(
        catalog
            .extract_registry("fictional-package", "1.0.0", original(&bytes))
            .is_err(),
        "should reject npm original as a crate"
    );
    catalog.source_commit = "b".repeat(40);
    assert!(
        catalog
            .extract_npm("node_modules/fictional-package", original(&bytes))
            .is_err(),
        "should reject original bound to different approved source"
    );
}

#[test]
fn manifest_review_uses_original_members_and_rejects_later_changes() {
    let manifest = serde_json::to_vec(&json!({"name":"fictional-package","version":"1.0.0"}))
        .expect("should encode fixture manifest");
    for binding in [false, true] {
        let mut entries = vec![("package/package.json", b'0', 0o644, manifest.as_slice())];
        if binding {
            entries.push((
                "package/binding.gyp",
                b'0',
                0o644,
                b"fictional binding".as_slice(),
            ));
        }
        let bytes = compressed(&tar_bytes(&entries));
        let catalog = catalog(&bytes);
        let extract_package = || {
            catalog
                .extract_npm("node_modules/fictional-package", original(&bytes))
                .expect("should extract original manifest")
        };
        let reviewed = catalog.review_npm("node_modules/fictional-package", extract_package());
        if binding {
            assert!(reviewed.is_err(), "should reject original implicit install");
            let extracted = extract_package();
            fs::remove_file(extracted.root().join("binding.gyp"))
                .expect("should remove fixture binding");
            assert!(
                catalog
                    .review_npm("node_modules/fictional-package", extracted)
                    .is_err(),
                "should reject removed original binding"
            );
        } else {
            assert!(
                reviewed
                    .expect("should review hook-free original")
                    .hooks()
                    .is_empty(),
                "should declare no execution"
            );
            let extracted = extract_package();
            fs::write(extracted.root().join("binding.gyp"), b"added binding")
                .expect("should add fixture binding");
            assert!(
                catalog
                    .review_npm("node_modules/fictional-package", extracted)
                    .is_err(),
                "should reject unrecorded binding"
            );
        }
        let extracted = extract_package();
        fs::write(extracted.root().join("package.json"), b"{}").expect("should change manifest");
        assert!(
            catalog
                .review_npm("node_modules/fictional-package", extracted)
                .is_err(),
            "should reject changed manifest bytes"
        );
        assert!(
            catalog
                .review_npm("node_modules/other", extract_package())
                .is_err(),
            "should reject another locked location"
        );
    }
}

#[test]
fn enforces_decoded_byte_and_entry_budgets() {
    let bytes = compressed(&tar_bytes(&[
        ("package/file", b'0', 0o644, b"x"),
        ("package/other", b'0', 0o644, b"y"),
    ]));
    assert!(
        extract_bounded(
            original(&bytes),
            Selection {
                root: "package",
                index_alias: false,
                node_tar_timestamps: false,
                local_pax: false,
                cargo_package: false,
            },
            1
        )
        .is_err(),
        "should reject entry exceeding bounded parser budget"
    );
    let original = original(&bytes);
    let mut decoded = Decoded {
        reader: io::repeat(0).take(DECODED_LIMIT + 1),
        original: &original,
        count: 0,
    };
    assert!(
        io::copy(&mut decoded, &mut io::sink()).is_err(),
        "should stop unbounded decoded expansion"
    );
}

#[test]
fn extracted_tree_review_rejects_changed_or_added_members() {
    let bytes = compressed(&tar_bytes(&[(
        "package/index.js",
        b'0',
        0o644,
        b"original",
    )]));
    let extracted = extract(
        original(&bytes),
        Selection {
            root: "package",
            index_alias: false,
            node_tar_timestamps: false,
            local_pax: false,
            cargo_package: false,
        },
    )
    .expect("should extract original");
    extracted
        .verify_unchanged()
        .expect("should verify original tree");
    fs::write(extracted.root().join("added.js"), b"added").expect("should add unrecorded file");
    assert!(
        extracted.verify_unchanged().is_err(),
        "should reject unrecorded member"
    );
    fs::remove_file(extracted.root().join("added.js")).expect("should remove extra file");
    fs::write(extracted.root().join("index.js"), b"changed").expect("should change recorded file");
    assert!(
        extracted.verify_unchanged().is_err(),
        "should reject changed recorded member"
    );
}

#[test]
fn npm_bundle_rechecks_and_retains_originals_for_independent_caches() {
    let manifest = serde_json::to_vec(&json!({"name":"fictional-package","version":"1.0.0"}))
        .expect("should encode manifest");
    let bytes = compressed(&tar_bytes(&[(
        "package/package.json",
        b'0',
        0o644,
        &manifest,
    )]));
    let catalog = catalog(&bytes);
    let inputs = tempfile::tempdir().expect("should create input directory");
    let name = format!("{}.tgz", hex::encode(Sha512::digest(&bytes)));
    fs::write(inputs.path().join(&name), &bytes).expect("should write original");
    let prepared = catalog
        .prepare_npm_originals(
            inputs.path(),
            Instant::now() + Duration::from_secs(10),
            &Cancellation::default(),
        )
        .expect("should authenticate complete original set");
    assert_eq!(
        prepared.archive_paths().len(),
        1,
        "should retain every locked original"
    );
    assert_eq!(
        fs::read(&prepared.archive_paths()[0]).expect("should read retained original"),
        bytes,
        "should retain compressed identity rather than repacked bytes"
    );
    fs::write(inputs.path().join(name), b"changed").expect("should replace caller input");
    prepared
        .verify_unchanged()
        .expect("should own originals independently of caller input");
    assert!(
        catalog
            .prepare_npm_originals(
                inputs.path(),
                Instant::now() + Duration::from_secs(10),
                &Cancellation::default()
            )
            .is_err(),
        "should reject changed original on next preparation"
    );
}

#[test]
fn extraction_recheck_rejects_root_alias_and_missing_empty_directory() {
    let bytes = compressed(&tar_bytes(&[
        ("package/index.js", b'0', 0o644, b"original"),
        ("package/empty", b'5', 0o755, b""),
    ]));
    let extracted = extract(
        original(&bytes),
        Selection {
            root: "package",
            index_alias: false,
            node_tar_timestamps: false,
            local_pax: false,
            cargo_package: false,
        },
    )
    .expect("should extract original");
    extracted
        .verify_unchanged()
        .expect("should recheck explicit empty directory");
    fs::remove_dir(extracted.root().join("empty")).expect("should remove empty directory");
    assert!(
        extracted.verify_unchanged().is_err(),
        "should reject missing recorded empty directory"
    );
    fs::create_dir(extracted.root().join("empty")).expect("should restore directory");
    let saved = extracted.directory.path().join("saved");
    fs::rename(extracted.root(), &saved).expect("should move original root");
    std::os::unix::fs::symlink("saved", extracted.root())
        .expect("should substitute matching root alias");
    assert!(
        extracted.verify_unchanged().is_err(),
        "should reject replaced extraction root even with identical members"
    );
}

mod cargo_package;
mod npm_compatibility;
