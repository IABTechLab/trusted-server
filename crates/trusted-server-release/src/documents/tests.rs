use std::cell::Cell;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;

use serde_json::json;
use sha2::Sha256;
use tempfile::tempdir;

use super::*;

#[test]
fn controlled_reads_cancel_before_and_between_bounded_chunks() {
    let root = tempdir().expect("should create controlled input");
    let path = root.path().join("input");
    fs::write(&path, vec![7_u8; 4 * 64 * 1024]).expect("should write multiple chunks");
    let calls = Cell::new(0);
    let check = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 6 {
            return Err(invalid("cancelled fixture"));
        }
        Ok(())
    };
    assert!(
        read_bounded_with_check(&path, Role::Executable, &check).is_err(),
        "should cancel during bounded read"
    );
    assert_eq!(
        calls.get(),
        6,
        "should stop between the first and second chunks"
    );
    let before = || Err(invalid("already cancelled fixture"));
    assert!(
        hash_bounded_with_check(&path, 1024 * 1024, &before).is_err(),
        "should reject before hashing"
    );
}

#[test]
fn controlled_stream_hash_enforces_limit_and_exact_digest() {
    let root = tempdir().expect("should create hash input");
    let path = root.path().join("input");
    let bytes = vec![9_u8; 3 * 64 * 1024];
    fs::write(&path, &bytes).expect("should write hash input");
    let measured = hash_bounded_with_check(&path, bytes.len() as u64, &|| Ok(()))
        .expect("should stream exact bytes");
    assert_eq!(
        measured.sha256,
        hex::encode(Sha256::digest(&bytes)),
        "should hash all original bytes"
    );
    assert_eq!(
        measured.size,
        bytes.len() as u64,
        "should retain original size"
    );
    assert_eq!(
        measured.mode,
        fs::metadata(&path)
            .expect("should inspect measured mode")
            .mode(),
        "should retain mode from the same held input"
    );
    assert!(
        hash_bounded_with_check(&path, bytes.len() as u64 - 1, &|| Ok(())).is_err(),
        "should reject oversized files before reading"
    );
}

#[test]
fn controlled_stream_hash_detects_mutation_and_leaf_replacement() {
    let root = tempdir().expect("should create changing input");
    let path = root.path().join("input");
    let bytes = vec![7_u8; 3 * 64 * 1024];
    fs::write(&path, &bytes).expect("should write original input");
    let calls = Cell::new(0);
    let result = hash_bounded_with_check(&path, bytes.len() as u64, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 4 {
            fs::write(&path, b"changed").expect("should mutate held input");
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "should reject mutations between hash chunks"
    );
    fs::write(&path, &bytes).expect("should restore input fixture");
    let calls = Cell::new(0);
    let result = hash_bounded_with_check(&path, bytes.len() as u64, &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 4 {
            fs::rename(&path, root.path().join("retained")).expect("should replace leaf name");
            fs::write(&path, &bytes).expect("should write identical replacement bytes");
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "should reject changed path identity even with matching bytes"
    );
}

fn parse(bytes: &[u8]) -> Result<Value, Report<ReleaseError>> {
    parse_json(bytes, Role::Metadata)
}

#[test]
fn rejects_duplicate_keys_at_every_depth() {
    for depth in 0..64 {
        let input = format!(
            "{}{{\"same\":1,\"same\":2}}{}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject duplicate at depth {depth}"
        );
    }
}

#[test]
fn rejects_duplicate_decoded_keys_in_opaque_objects() {
    assert!(
        parse(br#"{"opaque":{"x":0,"\u0078":1}}"#).is_err(),
        "should reject escaped duplicate key"
    );
}

#[test]
fn rejects_top_level_integer_overflow() {
    for input in ["18446744073709551616", "-9223372036854775809"] {
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject integer overflow: {input}"
        );
    }
}

#[test]
fn rejects_opaque_integer_overflow() {
    for input in ["18446744073709551616", "-9223372036854775809"] {
        let bytes = format!("{{\"opaque\":[{{\"value\":{input}}}]}}");
        assert!(
            parse(bytes.as_bytes()).is_err(),
            "should reject opaque integer overflow"
        );
    }
}

#[test]
fn rejects_nonfinite_and_decimal_exponent_overflow() {
    for input in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e309",
        "-1e309",
        "1.8e308",
        "9".repeat(400).as_str(),
    ] {
        assert!(
            parse(input.as_bytes()).is_err(),
            "should reject nonfinite or overflowing number"
        );
    }
    for number in [
        format!("{}.0", "9".repeat(400)),
        "1e999".to_owned(),
        "-1.8e308".to_owned(),
    ] {
        let opaque = format!("{{\"opaque\":{{\"number\":{number}}}}}");
        assert!(
            parse(opaque.as_bytes()).is_err(),
            "should reject decimal/exponent overflow in opaque values"
        );
    }
}

#[test]
fn retains_exact_integer_boundaries() {
    assert_eq!(
        parse(b"18446744073709551615")
            .expect("should accept u64 boundary")
            .as_u64(),
        Some(u64::MAX),
        "should retain exact u64"
    );
    assert_eq!(
        parse(b"-9223372036854775808")
            .expect("should accept i64 boundary")
            .as_i64(),
        Some(i64::MIN),
        "should retain exact i64"
    );
}

#[test]
fn number_like_quoted_text_and_escapes_remain_text() {
    let value = json!({"text": "18446744073709551616 -9223372036854775809 1e999 \" quoted \\ backslash \\\" 1.8e308", "nested": ["9".repeat(400)]});
    let bytes = serde_json::to_vec(&value).expect("should serialize text controls");
    assert_eq!(
        parse(&bytes).expect("should preserve number-like text"),
        value,
        "should skip strings including escaped quotes and backslashes"
    );
}

#[test]
fn rejects_invalid_utf8_before_deserialization() {
    let error = parse(b"\"\xff\"").expect_err("should reject invalid UTF-8");
    assert!(
        format!("{error:?}").contains("UTF-8"),
        "should diagnose byte encoding before deserialization"
    );
}

#[test]
fn rejects_bom_before_deserialization() {
    let error = parse(b"\xef\xbb\xbf{}").expect_err("should reject BOM");
    assert!(
        format!("{error:?}").contains("BOM"),
        "should diagnose BOM before deserialization"
    );
}

#[test]
fn rejects_surrogates_and_trailing_controls() {
    for input in [
        br#""\ud800""#.as_slice(),
        br#""\udfff""#,
        b"{}\0",
        b"{}\x0b",
        b"{}\x1f",
        b"{}{}",
    ] {
        assert!(
            parse(input).is_err(),
            "should reject surrogate or trailing data"
        );
    }
}

#[test]
fn rejects_container_depth_65_including_root() {
    for (open, close) in [("[", "]"), ("{\"nested\":", "}")] {
        let good = format!("{}0{}", open.repeat(64), close.repeat(64));
        let bad = format!("{}0{}", open.repeat(65), close.repeat(65));
        assert!(
            parse(good.as_bytes()).is_ok(),
            "should accept 64 containers"
        );
        assert!(
            parse(bad.as_bytes()).is_err(),
            "should reject 65 containers"
        );
    }
}

#[test]
fn rejects_oversized_json_before_encoding_or_parsing() {
    for role in [Role::Metadata, Role::Companion, Role::Spdx] {
        let bytes = vec![0xff; role.limit() as usize + 1];
        let error = parse_json(&bytes, role).expect_err("should reject role byte overflow");
        assert!(
            format!("{error:?}").contains("byte limit"),
            "should enforce size before encoding"
        );
    }
}

#[test]
fn rejects_oversized_files_before_reading() {
    let root = tempdir().expect("should create fixtures");
    for role in [
        Role::Metadata,
        Role::Companion,
        Role::Companion,
        Role::Spdx,
        Role::Executable,
    ] {
        let path = root.path().join("oversized");
        let file = fs::File::create(&path).expect("should create sparse oversized file");
        file.set_len(role.limit() + 1).expect("should size fixture");
        assert!(
            read_bounded(&path, role).is_err(),
            "should reject oversized role before read"
        );
    }
}

#[test]
fn retains_hash_of_exact_original_bytes() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("source.json");
    let bytes = b" { \"a\" : 1 } \n";
    fs::write(&path, bytes).expect("should write original bytes");
    let raw = read_bounded(&path, Role::Metadata).expect("should read exact bytes");
    assert_eq!(raw.bytes, bytes, "should retain original bytes");
    assert_eq!(
        raw.sha256,
        hex::encode(Sha256::digest(bytes)),
        "should hash original bytes before normalization"
    );
}

#[test]
fn rejects_symlinks_hardlinks_and_directories_at_read() {
    let root = tempdir().expect("should create fixtures");
    let real = root.path().join("real");
    fs::write(&real, b"{}").expect("should write fixture");
    let link = root.path().join("symlink");
    symlink(&real, &link).expect("should create symlink");
    assert!(
        read_bounded(&link, Role::Metadata).is_err(),
        "should reject symbolic leaf"
    );
    let hard = root.path().join("hardlink");
    fs::hard_link(&real, &hard).expect("should create hardlink");
    assert_eq!(
        fs::metadata(&hard).expect("should inspect links").nlink(),
        2,
        "should exercise hardlink fixture"
    );
    assert!(
        read_bounded(&hard, Role::Metadata).is_err(),
        "should reject hardlink"
    );
    assert!(
        read_bounded(root.path(), Role::Metadata).is_err(),
        "should reject directory"
    );
}

#[test]
fn canonical_serialization_sorts_recursively_and_adds_one_newline() {
    let value = json!({"z":[{"z": "é", "a":true}], "a":0});
    let bytes = canonical_json(&value, Role::Metadata).expect("should serialize canonical bytes");
    assert_eq!(
        bytes,
        "{\"a\":0,\"z\":[{\"a\":true,\"z\":\"é\"}]}\n".as_bytes(),
        "should sort recursively and retain compact UTF-8"
    );
}

#[test]
fn detects_mutation_on_held_input_descriptor() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("held");
    fs::write(&path, b"old").expect("should create input");
    let file = File::open(&path).expect("should hold original descriptor");
    let before = file.metadata().expect("should capture original metadata");
    fs::write(&path, b"new").expect("should mutate same-length input");
    assert!(
        read_held(&file, &before, Role::Metadata).is_err(),
        "should detect same-length mutation against captured metadata"
    );
}

#[test]
fn held_read_does_not_reopen_replaced_path() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("held");
    fs::write(&path, b"original").expect("should create input");
    let file = File::open(&path).expect("should hold original descriptor");
    let before = file.metadata().expect("should capture input metadata");
    fs::remove_file(&path).expect("should remove input pathname");
    fs::write(&path, b"replacement").expect("should create different file");
    assert!(
        read_held(&file, &before, Role::Metadata).is_err(),
        "should reject removed held file rather than reopen replacement"
    );
}

#[test]
fn rejects_fifo_without_waiting_for_a_writer() {
    let root = tempdir().expect("should create fixture");
    let path = root.path().join("input-fifo");
    let name = CString::new(path.as_os_str().as_bytes()).expect("should encode fixture pathname");
    // The CString supplies a valid NUL-terminated pathname for this Unix syscall.
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
        0,
        "should create FIFO fixture"
    );
    assert!(
        read_bounded(&path, Role::Metadata).is_err(),
        "should reject FIFO without blocking on open or read"
    );
}
