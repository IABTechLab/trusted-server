//! Core Wasm inspection tests.

use super::builders::{Entry, command_module, section, wasm_module, wasm_module_with_customs};
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
fn reviewed_fastly_async_io_imports_are_recorded() {
    let bytes = wasm_module(&[
        ("fastly_async_io", "is_ready"),
        ("fastly_async_io", "select"),
    ]);
    let facts = inspect_wasm(&bytes).expect("should accept the reviewed asynchronous host module");
    assert_eq!(
        facts.import_modules(),
        ["fastly_async_io"],
        "should retain the exact observed import module"
    );
    assert_eq!(
        facts.function_imports(),
        2,
        "should record both asynchronous host calls"
    );
    assert_eq!(
        facts.sha256(),
        sha256_hex(&bytes),
        "should bind the exact inspected bytes"
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
        "fastly_async_io_extra",
        "FASTLY_ASYNC_IO",
        "fastly_async_io/v1",
        "fastly_backend",
        "fastly_abi",
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
    for module in ["fastly_log", "fastly_async_io"] {
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();
        let mut body = vec![1];
        for name in [module, "memory"] {
            body.push(u8::try_from(name.len()).expect("should fit"));
            body.extend_from_slice(name.as_bytes());
        }
        body.extend_from_slice(&[2, 0, 1]);
        section(&mut bytes, 2, &body);

        assert_eq!(
            violation(inspect_wasm(&bytes)),
            Violation::WasmImport,
            "should reject non-function imports from {module}"
        );
    }
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

/// Imports of a minimal Fastly command: WASI plus both downstream hostcalls.
const FASTLY_COMMAND_IMPORTS: [(&str, &str); 3] = [
    ("wasi_snapshot_preview1", "fd_write"),
    ("fastly_http_req", "body_downstream_get"),
    ("fastly_http_resp", "send_downstream"),
];

#[test]
fn fastly_command_entry_abi_is_accepted_and_exports_are_recorded() {
    let facts = inspect_wasm(&command_module(
        &FASTLY_COMMAND_IMPORTS,
        Entry::Nullary,
        true,
    ))
    .expect("should accept a Fastly command module");

    facts
        .require_fastly_entry_abi()
        .expect("should accept the Fastly entry ABI");
    let observed: Vec<(&str, WasmExportKind)> = facts
        .exports()
        .iter()
        .map(|export| (export.name(), export.kind()))
        .collect();
    assert_eq!(
        observed,
        [
            (
                "_start",
                WasmExportKind::Function {
                    params: 0,
                    results: 0
                }
            ),
            ("memory", WasmExportKind::Memory),
        ],
        "should record every export with its kind in module order"
    );
}

#[test]
fn wrong_fastly_entry_abi_is_rejected() {
    let only_wasi = [("wasi_snapshot_preview1", "fd_write")];
    let without_response = [
        ("wasi_snapshot_preview1", "fd_write"),
        ("fastly_http_req", "body_downstream_get"),
    ];
    let cases: [(&str, Vec<u8>); 6] = [
        (
            "missing _start",
            command_module(&FASTLY_COMMAND_IMPORTS, Entry::Renamed, true),
        ),
        (
            "_start taking an argument",
            command_module(&FASTLY_COMMAND_IMPORTS, Entry::TakesI32, true),
        ),
        (
            "unexported memory",
            command_module(&FASTLY_COMMAND_IMPORTS, Entry::Nullary, false),
        ),
        (
            "plain WASI command",
            command_module(&only_wasi, Entry::Nullary, true),
        ),
        (
            "missing response hostcalls",
            command_module(&without_response, Entry::Nullary, true),
        ),
        (
            "library without exports",
            wasm_module(&FASTLY_COMMAND_IMPORTS),
        ),
    ];
    for (label, bytes) in cases {
        let facts = inspect_wasm(&bytes).expect("should accept the module itself");
        assert_eq!(
            violation(facts.require_fastly_entry_abi()),
            Violation::WasmAbi,
            "should reject a {label} as the Fastly entry ABI"
        );
    }
}

#[test]
fn foreign_host_abi_command_is_rejected_before_the_entry_check() {
    let bytes = command_module(
        &[
            ("fastly_http_req", "body_downstream_get"),
            ("fastly_http_resp", "send_downstream"),
            ("wasi:http/types@0.3.0", "handle"),
        ],
        Entry::Nullary,
        true,
    );

    assert_eq!(
        violation(inspect_wasm(&bytes)),
        Violation::WasmImport,
        "should reject a command importing a foreign host ABI"
    );
}
