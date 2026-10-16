//! Encodings `cargo package` writes into registry crates: GNU long names and
//! file-type bits in the mode field.

use super::*;

fn crate_selection(cargo_package: bool) -> Selection<'static> {
    Selection {
        root: "example-1.0.0",
        index_alias: false,
        node_tar_timestamps: false,
        local_pax: false,
        cargo_package,
    }
}

/// A member path longer than the 100-byte header name field.
fn long_member() -> String {
    format!("example-1.0.0/{}/file.rs", "nested".repeat(18))
}

/// A crate as `cargo package` writes it: a GNU long-name record and a
/// regular file whose mode carries the regular-file type bits.
fn cargo_package_archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut long = tar::Header::new_gnu();
    long.set_mode(0o644);
    long.set_size(5);
    builder
        .append_data(&mut long, long_member(), &b"long\n"[..])
        .expect("should append a GNU long-name member");
    let mut typed = tar::Header::new_ustar();
    typed.set_mode(0o100_755);
    typed.set_size(4);
    typed.set_cksum();
    builder
        .append_data(&mut typed, "example-1.0.0/build.sh", &b"#!\n\n"[..])
        .expect("should append a typed-mode member");
    builder
        .into_inner()
        .expect("should finish crate TAR")
        .finish()
        .expect("should finish crate gzip")
}

#[test]
fn cargo_package_long_names_and_typed_modes_extract_only_for_crates() {
    let bytes = cargo_package_archive();
    let extracted = extract(original(&bytes), crate_selection(true))
        .expect("should extract the encodings cargo package writes");
    assert_eq!(
        fs::read(extracted.directory.path().join(long_member())).expect("should read long member"),
        b"long\n",
        "should place the member at its full long name"
    );
    let (_, _, mode) = extracted
        .member_facts("build.sh")
        .expect("should record the typed-mode member");
    assert_eq!(mode, 0o755, "should keep only the permission bits");
    let error = extract(original(&bytes), crate_selection(false))
        .err()
        .expect("should keep npm extraction closed to both encodings");
    assert!(
        format!("{error:?}").contains("GNU long-name"),
        "should name the unreviewed record: {error:?}"
    );
}

/// One raw TAR member: name, entry type, mode and contents.
type Member<'a> = (&'a str, u8, u32, &'a [u8]);

#[test]
fn cargo_package_encodings_never_rename_or_privilege_a_member() {
    let long = long_member();
    let record = format!("{long}\0");
    let header_name = &long[..100];
    let other_name = format!("example-1.0.0/{}", "x".repeat(86));
    let control = compressed(&tar_bytes(&[
        ("././@LongLink", b'L', 0o644, record.as_bytes()),
        (header_name, b'0', 0o100_644, b"data"),
    ]));
    let extracted = extract(original(&control), crate_selection(true))
        .expect("should accept the hand-built record the negatives below alter");
    assert!(
        extracted.directory.path().join(&long).is_file(),
        "should extract the control member at its long name"
    );
    let cases: [(&str, Vec<Member<'_>>); 9] = [
        (
            "renaming header",
            vec![
                ("././@LongLink", b'L', 0o644, record.as_bytes()),
                (other_name.as_str(), b'0', 0o644, b"data"),
            ],
        ),
        (
            "consecutive records",
            vec![
                ("././@LongLink", b'L', 0o644, record.as_bytes()),
                ("././@LongLink", b'L', 0o644, record.as_bytes()),
                (header_name, b'0', 0o644, b"data"),
            ],
        ),
        (
            "dangling record",
            vec![
                ("example-1.0.0/lib.rs", b'0', 0o644, b"data"),
                ("././@LongLink", b'L', 0o644, record.as_bytes()),
            ],
        ),
        (
            "unterminated record",
            vec![
                ("././@LongLink", b'L', 0o644, long.as_bytes()),
                (header_name, b'0', 0o644, b"data"),
            ],
        ),
        (
            "foreign record name",
            vec![
                ("example-1.0.0/LongLink", b'L', 0o644, record.as_bytes()),
                (header_name, b'0', 0o644, b"data"),
            ],
        ),
        (
            "directory-mode record",
            vec![
                ("././@LongLink", b'L', 0o040_755, record.as_bytes()),
                (header_name, b'0', 0o644, b"data"),
            ],
        ),
        (
            "set-user-ID record",
            vec![
                ("././@LongLink", b'L', 0o104_644, record.as_bytes()),
                (header_name, b'0', 0o644, b"data"),
            ],
        ),
        (
            "symlink type bits",
            vec![("example-1.0.0/lib.rs", b'0', 0o120_777, b"data")],
        ),
        (
            "set-user-ID mode",
            vec![("example-1.0.0/lib.rs", b'0', 0o104_755, b"data")],
        ),
    ];
    for (label, entries) in cases {
        let bytes = compressed(&tar_bytes(&entries));
        assert!(
            extract(original(&bytes), crate_selection(true)).is_err(),
            "should reject a crate with a {label}"
        );
    }
}
