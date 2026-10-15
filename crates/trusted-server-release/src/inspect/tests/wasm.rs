//! Core Wasm inspection tests.

use super::builders::{section, wasm_module, wasm_module_with_customs};
use super::*;

#[test]
fn valid_core_module_is_accepted_with_observed_imports() {
    let bytes = wasm_module(&[
        ("wasi_snapshot_preview1", "fd_write"),
        ("fastly_http_req", "send"),
        ("fastly_http_req", "body_downstream_get"),
    ]);

    let facts = inspect_wasm(&bytes).expect("should accept a Fastly core module");

    assert_eq!(
        facts.import_modules(),
        ["fastly_http_req", "wasi_snapshot_preview1"],
        "should record distinct sorted import modules"
    );
    assert_eq!(
        facts.function_imports(),
        3,
        "should count every function import"
    );
    assert_eq!(
        facts.sha256(),
        sha256_hex(&bytes),
        "should hash the inspected bytes"
    );
}

#[test]
fn import_free_module_is_accepted() {
    let facts = inspect_wasm(&wasm_module(&[])).expect("should accept an empty module");

    assert!(
        facts.import_modules().is_empty(),
        "should record no imports"
    );
}

#[test]
fn unknown_import_module_is_rejected() {
    for module in [
        "wasi:http/types@0.3.0",
        "spin:example/key-value",
        "env",
        "fastly_example",
    ] {
        let bytes = wasm_module(&[("wasi_snapshot_preview1", "fd_write"), (module, "call")]);

        assert_eq!(
            violation(inspect_wasm(&bytes)),
            Violation::WasmImport,
            "should reject {module}"
        );
    }
}

#[test]
fn non_function_import_is_rejected() {
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    let mut body = vec![1];
    for name in ["fastly_log", "memory"] {
        body.push(u8::try_from(name.len()).expect("should fit"));
        body.extend_from_slice(name.as_bytes());
    }
    body.extend_from_slice(&[2, 0, 1]);
    section(&mut bytes, 2, &body);

    assert_eq!(violation(inspect_wasm(&bytes)), Violation::WasmImport);
}

#[test]
fn component_is_rejected() {
    assert_eq!(
        violation(inspect_wasm(b"\0asm\x0d\0\x01\0")),
        Violation::WasmEncoding,
        "should reject a component before validation"
    );
}

#[test]
fn non_wasm_is_rejected() {
    for bytes in [
        &b""[..],
        b"\0asm",
        b"\0asm\x02\0\0\0",
        b"\x7fELF\x02\x01\x01\0",
    ] {
        assert_eq!(violation(inspect_wasm(bytes)), Violation::WasmEncoding);
    }
}

#[test]
fn invalid_core_module_is_rejected() {
    let mut import_without_type = b"\0asm\x01\0\0\0".to_vec();
    section(&mut import_without_type, 2, &[1, 1, b'a', 1, b'b', 0, 0]);
    let mut unknown_section = b"\0asm\x01\0\0\0".to_vec();
    section(&mut unknown_section, 99, &[0]);

    for bytes in [import_without_type, unknown_section] {
        assert_eq!(violation(inspect_wasm(&bytes)), Violation::WasmValidation);
    }
}

/// Prefix lengths that end on a section boundary and so form a valid module:
/// the bare header and the header plus the type section.
const SECTION_BOUNDARIES: [usize; 2] = [8, 14];

#[test]
fn truncated_modules_are_handled_without_panic() {
    let bytes = wasm_module(&[
        ("fastly_log", "write"),
        ("wasi_snapshot_preview1", "fd_write"),
    ]);

    for length in 0..bytes.len() {
        let truncated = &bytes[..length];
        let result = inspect_wasm(truncated);
        assert!(
            result.is_err() || SECTION_BOUNDARIES.contains(&length),
            "should reject the module truncated to {length} bytes"
        );
    }
}

#[test]
fn custom_sections_are_recorded_in_file_order_with_sizes() {
    let bytes = wasm_module_with_customs(
        &[("fastly_log", "write")],
        &[
            (".debug_info", b"info"),
            ("name", b"xy"),
            (".debug_info", b""),
        ],
    );

    let facts = inspect_wasm(&bytes).expect("should accept a module with custom sections");

    let observed: Vec<(&str, usize)> = facts
        .custom_sections()
        .iter()
        .map(|section| (section.name(), section.size()))
        .collect();
    assert_eq!(
        observed,
        [(".debug_info", 4), ("name", 2), (".debug_info", 0)],
        "should record every custom section name and payload size in order"
    );
    assert!(
        inspect_wasm(&wasm_module(&[]))
            .expect("should accept an empty module")
            .custom_sections()
            .is_empty(),
        "should record no custom sections for a bare module"
    );
}

#[test]
fn debug_sections_are_required_and_nonempty() {
    let complete = wasm_module_with_customs(
        &[],
        &[(".debug_info", b"i"), (".debug_line", b"l"), ("name", b"n")],
    );
    inspect_wasm(&complete)
        .expect("should accept a module with debug sections")
        .require_debug_sections()
        .expect("should accept both debug sections");

    let cases: [(&str, Vec<u8>); 4] = [
        ("stripped", wasm_module_with_customs(&[], &[("name", b"n")])),
        (
            "missing line",
            wasm_module_with_customs(&[], &[(".debug_info", b"i")]),
        ),
        (
            "missing info",
            wasm_module_with_customs(&[], &[(".debug_line", b"l")]),
        ),
        (
            "empty info",
            wasm_module_with_customs(&[], &[(".debug_info", b""), (".debug_line", b"l")]),
        ),
    ];
    for (label, bytes) in cases {
        let facts = inspect_wasm(&bytes).expect("should accept the module itself");
        assert_eq!(
            violation(facts.require_debug_sections()),
            Violation::WasmDebugInfo,
            "should reject a {label} module for the no-strip check"
        );
    }
}
