//! Reviewed npm compatibility: node-tar timestamp prefixes and local PAX headers.

use super::*;

#[test]
fn accepts_only_reviewed_node_tar_timestamp_layout() {
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes()[476..488].copy_from_slice(b"15075543223\0");
    header.as_mut_bytes()[488..500].copy_from_slice(b"15075543223\0");
    header.as_mut_bytes()[..12].copy_from_slice(b"chai/file.js");
    let selection = Selection {
        root: "chai",
        index_alias: false,
        node_tar_timestamps: true,
        local_pax: false,
        cargo_package: false,
    };
    assert_eq!(
        raw_name(&header, &selection).expect("should parse reviewed layout"),
        "chai/file.js"
    );
    assert!(
        raw_name(
            &header,
            &Selection {
                node_tar_timestamps: false,
                ..selection
            }
        )
        .is_err(),
        "should reject layout outside approved tuple"
    );
    header.as_mut_bytes()[475] = b'x';
    assert!(
        raw_name(&header, &selection).is_err(),
        "should reject hidden prefix bytes"
    );
    header.as_mut_bytes()[475] = 0;
    header.as_mut_bytes()[476] = b'9';
    assert!(
        raw_name(&header, &selection).is_err(),
        "should reject nonoctal timestamps"
    );
}

fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let content = format!(" {key}={value}\n");
    let mut length = content.len() + 1;
    loop {
        let next = content.len() + length.to_string().len();
        if next == length {
            return format!("{length}{content}").into_bytes();
        }
        length = next;
    }
}
#[test]
fn reviewed_local_pax_never_overrides_member_selection() {
    let mut body = pax_record("path", "package/file.js");
    body.extend(pax_record("size", "3"));
    body.extend(pax_record("NODETAR.type", "File"));
    let parsed = compatibility::parse_pax(&body).expect("should parse reviewed metadata");
    assert!(
        parsed.join("package/file.js", 3).is_ok(),
        "should accept redundant selectors"
    );
    assert!(
        parsed.join("package/other.js", 3).is_err(),
        "should reject path override"
    );
    assert!(
        parsed.join("package/file.js", 4).is_err(),
        "should reject size override"
    );
    for (key, value) in [
        ("linkpath", "package/other"),
        ("GNU.sparse.size", "3"),
        ("size", "03"),
    ] {
        let mut invalid = body.clone();
        invalid.extend(pax_record(key, value));
        assert!(
            compatibility::parse_pax(&invalid).is_err(),
            "should reject semantic or duplicate fields"
        );
    }
    assert!(
        compatibility::parse_pax(b"99 path=package/file.js\n").is_err(),
        "should reject malformed framing"
    );
}

#[test]
fn reviewed_pax_is_local_bounded_and_never_redirects_extraction() {
    let metadata = |path: &str, size: &str| {
        let mut bytes = pax_record("path", path);
        bytes.extend(pax_record("size", size));
        bytes.extend(pax_record("NODETAR.type", "File"));
        bytes
    };
    let valid = metadata("package/file.js", "3");
    let selection = Selection {
        root: "package",
        index_alias: false,
        node_tar_timestamps: false,
        local_pax: true,
        cargo_package: false,
    };
    let archive = compressed(&tar_bytes(&[
        ("PaxHeader/package/file.js", b'x', 0o644, &valid),
        ("package/file.js", b'0', 0o644, b"abc"),
    ]));
    let extracted =
        extract(original(&archive), selection).expect("should extract redundant local metadata");
    extracted
        .verify_unchanged()
        .expect("should preserve regular member facts");
    let bad_path = metadata("package/other.js", "3");
    let bad_size = metadata("package/file.js", "4");
    let cases = [
        vec![("PaxHeader/package/file.js", b'x', 0o644, valid.as_slice())],
        vec![
            ("PaxHeader/package/file.js", b'x', 0o644, valid.as_slice()),
            ("PaxHeader/other.js", b'x', 0o644, valid.as_slice()),
        ],
        vec![
            ("PaxHeader/package/file.js", b'g', 0o644, valid.as_slice()),
            ("package/file.js", b'0', 0o644, b"abc"),
        ],
        vec![
            (
                "PaxHeader/package/file.js",
                b'x',
                0o644,
                bad_path.as_slice(),
            ),
            ("package/file.js", b'0', 0o644, b"abc"),
        ],
        vec![
            (
                "PaxHeader/package/file.js",
                b'x',
                0o644,
                bad_size.as_slice(),
            ),
            ("package/file.js", b'0', 0o644, b"abc"),
        ],
    ];
    for entries in cases {
        let bytes = compressed(&tar_bytes(&entries));
        assert!(
            extract(original(&bytes), selection).is_err(),
            "should reject nonlocal or overriding metadata"
        );
    }
    for (name, mode) in [
        ("PaxHeader/../file.js", 0o644),
        ("PaxHeader/package/file.js", 0o4644),
    ] {
        let bytes = compressed(&tar_bytes(&[
            (name, b'x', mode, &valid),
            ("package/file.js", b'0', 0o644, b"abc"),
        ]));
        assert!(
            extract(original(&bytes), selection).is_err(),
            "should reject unsafe PAX header itself"
        );
    }
    let unreviewed = Selection {
        local_pax: false,
        ..selection
    };
    assert!(
        extract(original(&archive), unreviewed).is_err(),
        "should reject PAX outside exact approved tuple"
    );
}
