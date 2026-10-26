//! Pure tests of the final-byte Fastly runtime gate.
//!
//! None of these start Viceroy. The real gate runs only in the ordinary CI
//! canonical and reproduction job (see `full.rs`).

use std::net::SocketAddr;
use std::time::Duration;

use error_stack::Report;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::runtime::tests_support::{
    HttpResponse, OriginFixture, app_config_data, canonical, check_publisher_response,
    config_store_id, envelope_json, get, is_health_ok, origin_page, parse_response,
    parse_version_output, require_built_source_fixtures, sanitized_tail, scan_viceroy_output,
    viceroy_config,
};
use super::super::runtime::{
    OriginPresence, PINNED_VICEROY_VERSION, RuntimeRejection, SelectedViceroy, run_gate,
};
use crate::error::ReleaseError;
use crate::inspect::Violation;
use crate::inspect::tests::builders::{Entry, append_customs, command_module};

const TOOL_VERSIONS: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.tool-versions"));
const FASTLY_TOML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fastly.toml"));
const ORIGIN: &str = "http://127.0.0.1:9999";
const MARKER: &str = "trusted-server-release-origin-0123456789abcdef01234567";
const SCRIPT: &str =
    "<script src=\"/static/tsjs=tsjs-unified.min.js?v=0123abcd\" id=\"trustedserver-js\"></script>";

/// Returns the attached [`RuntimeRejection`] of a runtime rejection.
fn rejection<T: std::fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> RuntimeRejection {
    let report = result.expect_err("should reject");
    assert!(
        matches!(report.current_context(), ReleaseError::Runtime),
        "should report a runtime rejection: {report:?}"
    );
    *report
        .downcast_ref::<RuntimeRejection>()
        .expect("should attach the runtime rejection")
}

/// Returns the attached inspection [`Violation`].
fn violation<T: std::fmt::Debug>(result: Result<T, Report<ReleaseError>>) -> Violation {
    let report = result.expect_err("should reject");
    assert!(
        matches!(report.current_context(), ReleaseError::Inspect),
        "should report an inspection rejection: {report:?}"
    );
    *report
        .downcast_ref::<Violation>()
        .expect("should attach the violated rule")
}

#[test]
fn pinned_viceroy_version_matches_tool_versions() {
    let pinned = TOOL_VERSIONS
        .lines()
        .find_map(|line| line.strip_prefix("viceroy"))
        .map(str::trim)
        .expect("should pin Viceroy in .tool-versions");
    assert_eq!(
        pinned, PINNED_VICEROY_VERSION,
        "should gate on the tracked pin"
    );
}

#[test]
fn viceroy_version_output_must_be_exactly_the_pin() {
    assert_eq!(
        parse_version_output(b"viceroy 0.21.0\n").expect("should accept the pinned output"),
        "0.21.0"
    );
    for output in [
        &b""[..],
        b"viceroy 0.21.0",
        b"viceroy 0.21.1\n",
        b"viceroy 0.17.0\n",
        b"viceroy 0.16.0\n",
        b"viceroy 0.21.0-rc.1\n",
        b"Viceroy 0.21.0\n",
        b"viceroy  0.21.0\n",
        b"viceroy 0.21.0\nextra\n",
        b"fastly 0.21.0\n",
        b"viceroy \xff\n",
    ] {
        assert_eq!(
            rejection(parse_version_output(output)),
            RuntimeRejection::Viceroy,
            "should reject version output {:?}",
            String::from_utf8_lossy(output)
        );
    }
}

#[test]
fn viceroy_selection_requires_an_absolute_regular_file() {
    let directory = tempfile::tempdir().expect("should create a directory");
    for path in [
        std::path::Path::new("viceroy"),
        directory.path(),
        &directory.path().join("missing"),
    ] {
        assert_eq!(
            rejection(SelectedViceroy::select(path)),
            RuntimeRejection::Viceroy,
            "should reject Viceroy at {path:?} without running it"
        );
    }
}

#[test]
fn viceroy_selection_rejects_a_symlink() {
    let directory = tempfile::tempdir().expect("should create a directory");
    let target = directory.path().join("viceroy-target");
    std::fs::write(&target, b"example executable").expect("should write a stand-in");
    let link = directory.path().join("viceroy");
    std::os::unix::fs::symlink(&target, &link).expect("should create a symlink");

    assert_eq!(
        rejection(SelectedViceroy::select(&link)),
        RuntimeRejection::Viceroy,
        "should reject a symlinked Viceroy without following or running it"
    );
}

/// Returns the SHA-256 of a repository file, as the built source tracks it.
fn repository_sha256(path: &str) -> Option<String> {
    let bytes = std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).ok()?;
    Some(hex::encode(Sha256::digest(bytes)))
}

#[test]
fn embedded_fixtures_must_match_the_built_source() {
    require_built_source_fixtures(&repository_sha256)
        .expect("should accept a built source tracking the embedded fixtures");

    let changed = |path: &str| {
        if path == "edgezero.toml" {
            Some("0".repeat(64))
        } else {
            repository_sha256(path)
        }
    };
    let untracked = |path: &str| {
        if path.ends_with("viceroy-template.toml") {
            None
        } else {
            repository_sha256(path)
        }
    };
    for (label, tracked) in [
        (
            "changed manifest",
            &changed as &dyn Fn(&str) -> Option<String>,
        ),
        ("untracked template", &untracked),
    ] {
        let report = require_built_source_fixtures(tracked).expect_err("should fail closed");
        assert_eq!(
            report.downcast_ref::<RuntimeRejection>().copied(),
            Some(RuntimeRejection::Fixture),
            "should reject a {label}"
        );
        assert!(
            format!("{report:?}").contains("built source"),
            "should name the built source difference for a {label}: {report:?}"
        );
    }
}

#[test]
fn clean_viceroy_output_is_summarized() {
    let output = scan_viceroy_output(b"Listening on http://127.0.0.1:7676\n", b"")
        .expect("should accept clean output");
    assert_eq!(
        output.stderr_sha256,
        hex::encode(Sha256::digest(b"")),
        "should digest the empty stderr"
    );
    assert_eq!(output.stderr_tail, "");
}

#[test]
fn guest_failures_in_viceroy_output_are_rejected() {
    let cases: [(&str, &[u8], &[u8]); 5] = [
        (
            "wasm trap",
            b"",
            b"2026-10-16T00:00:00Z ERROR request failed: wasm trap: wasm `unreachable` instruction executed\n",
        ),
        (
            "guest panic",
            b"thread '<unnamed>' panicked at src/example.rs:1:1:\nexample\n",
            b"",
        ),
        (
            "error line",
            b"",
            b"Error: hyper::Error(Connect, \"example\")\n",
        ),
        (
            "request handling error",
            b"",
            b"WARN There was an error handling the request: example\n",
        ),
        ("backtrace", b"wasm backtrace:\n    0: example\n", b""),
    ];
    for (label, stdout, stderr) in cases {
        assert_eq!(
            rejection(scan_viceroy_output(stdout, stderr)),
            RuntimeRejection::GuestFailure,
            "should reject a {label}"
        );
    }
}

#[test]
fn stderr_tail_is_bounded_and_printable() {
    let mut stderr = vec![b'x'; 4096];
    stderr.extend_from_slice(b"\x1b[31mred\x07\nend");
    let tail = sanitized_tail(&stderr);
    assert!(tail.len() <= 1024, "should keep a bounded tail");
    assert!(
        tail.ends_with("?[31mred?\nend"),
        "should replace control characters but keep newlines: {tail:?}"
    );
}

#[test]
fn config_store_id_matches_the_fastly_local_store() {
    let id = config_store_id().expect("should read the default config store");
    let fastly: toml::Table = toml::from_str(FASTLY_TOML).expect("should parse fastly.toml");
    assert!(
        fastly["local_server"]["config_stores"].get(&id).is_some(),
        "should name a config store the Fastly local server declares: {id}"
    );
}

#[test]
fn canonical_form_sorts_keys_escapes_and_rejects_floats() {
    let value = json!({"b": 1, "a": [true, null, "x\n\"\u{1}"], "B": {"z": -2, "y": {}}});
    assert_eq!(
        canonical(&value).expect("should encode integers"),
        r#"{"B":{"y":{},"z":-2},"a":[true,null,"x\n\"\u0001"],"b":1}"#,
        "should sort keys by bytes and use the fixed escape table"
    );
    assert_eq!(
        rejection(canonical(&json!({"ratio": 1.5}))),
        RuntimeRejection::Fixture,
        "should reject floats"
    );
}

#[test]
fn app_config_keeps_secret_names_and_replaces_only_the_origin() {
    let data = app_config_data(ORIGIN).expect("should convert the integration fixture");
    assert_eq!(
        data["publisher"]["origin_url"], ORIGIN,
        "should override the origin"
    );
    assert_eq!(
        data["publisher"]["proxy_secret"], "integration_proxy_secret",
        "should keep secret store key names, not values"
    );
    assert_eq!(data["publisher"]["domain"], "localhost");
}

#[test]
fn envelope_binds_the_canonical_data_digest() {
    let data = app_config_data(ORIGIN).expect("should convert the fixture");
    let envelope: Value = serde_json::from_str(&envelope_json(&data).expect("should wrap"))
        .expect("should parse the envelope");
    let expected = hex::encode(Sha256::digest(
        canonical(&data).expect("should encode").as_bytes(),
    ));
    assert_eq!(envelope["sha256"], Value::String(expected));
    assert_eq!(envelope["version"], 1);
    assert_eq!(envelope["generated_at"], "2026-06-23T00:00:00Z");
    assert_eq!(envelope["data"], data, "should carry the data unchanged");
}

#[test]
fn viceroy_config_is_exact_and_deterministic() {
    let generated = viceroy_config(ORIGIN).expect("should generate the config");
    let again = viceroy_config(ORIGIN).expect("should generate the config again");
    assert_eq!(generated, again, "should be deterministic");
    assert_eq!(
        generated.sha256,
        hex::encode(Sha256::digest(generated.text.as_bytes())),
        "should digest the exact text"
    );
    let parsed: toml::Table = toml::from_str(&generated.text).expect("should parse as TOML");
    let id = config_store_id().expect("should read the store ID");
    let store = &parsed["local_server"]["config_stores"][id.as_str()];
    assert_eq!(store["format"].as_str(), Some("inline-toml"));
    let envelope: Value = serde_json::from_str(
        store["contents"][id.as_str()]
            .as_str()
            .expect("should store the envelope under the store ID"),
    )
    .expect("should parse the stored envelope");
    assert_eq!(envelope["data"]["publisher"]["origin_url"], ORIGIN);
    assert!(
        parsed["local_server"]["config_stores"]
            .get("jwks_store")
            .is_some()
            && parsed["local_server"]["secret_stores"]
                .get("trusted_server_secrets")
                .is_some(),
        "should keep the rest of the integration template"
    );
    assert!(
        !generated
            .text
            .contains("GENERATED_TRUSTED_SERVER_CONFIG_STORES"),
        "should replace the marker"
    );

    let other = viceroy_config("http://127.0.0.1:9998").expect("should generate another");
    assert_ne!(other.sha256, generated.sha256, "should bind the origin");
    assert_eq!(
        other.fixture_sha256, generated.fixture_sha256,
        "should identify the same fixed fixture"
    );
}

fn html_response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![(
            "content-type".to_owned(),
            "text/html; charset=utf-8".to_owned(),
        )],
        body: body.as_bytes().to_vec(),
    }
}

fn injected_page() -> String {
    origin_page(MARKER).replacen("<head>", &format!("<head>{SCRIPT}"), 1)
}

#[test]
fn injected_origin_page_is_accepted() {
    let page = injected_page();
    let observed =
        check_publisher_response(&html_response(200, &page), MARKER).expect("should accept");
    assert_eq!(
        observed.script_src,
        "/static/tsjs=tsjs-unified.min.js?v=0123abcd"
    );
    assert_eq!(observed.body_size, page.len());
    assert_eq!(observed.status, 200);
}

#[test]
fn publisher_response_negatives_are_rejected() {
    let page = injected_page();
    let health = HttpResponse {
        status: 200,
        headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
        body: b"ok".to_vec(),
    };
    let cases: [(&str, HttpResponse, RuntimeRejection); 9] = [
        (
            "bad gateway",
            html_response(502, &page),
            RuntimeRejection::Status,
        ),
        ("health-only answer", health, RuntimeRejection::Injection),
        (
            "uninjected origin page",
            html_response(200, &origin_page(MARKER)),
            RuntimeRejection::Injection,
        ),
        (
            "duplicated script",
            html_response(200, &page.replacen("<body>", &format!("<body>{SCRIPT}"), 1)),
            RuntimeRejection::Injection,
        ),
        (
            "wrong script source",
            html_response(200, &page.replace("/static/tsjs=", "/static/other=")),
            RuntimeRejection::Injection,
        ),
        (
            "id on a non-script element",
            html_response(200, &page.replace("<script src", "<div src")),
            RuntimeRejection::Injection,
        ),
        (
            "script after the origin content",
            html_response(
                200,
                &origin_page(MARKER).replacen("</main>", &format!("</main>{SCRIPT}"), 1),
            ),
            RuntimeRejection::Injection,
        ),
        (
            "foreign origin page",
            html_response(200, &page.replace(MARKER, "another-origin")),
            RuntimeRejection::OriginMarker,
        ),
        (
            "repeated origin marker",
            html_response(
                200,
                &page.replacen("</main>", &format!("{MARKER}</main>"), 1),
            ),
            RuntimeRejection::OriginMarker,
        ),
    ];
    for (label, response, expected) in cases {
        assert_eq!(
            rejection(check_publisher_response(&response, MARKER)),
            expected,
            "should reject the {label}"
        );
    }
}

#[test]
fn health_answer_is_recognized_only_as_health() {
    let ok = HttpResponse {
        status: 200,
        headers: Vec::new(),
        body: b"ok".to_vec(),
    };
    assert!(is_health_ok(&ok), "should recognize the health answer");
    assert!(!is_health_ok(&html_response(200, "ready")));
    assert!(!is_health_ok(&HttpResponse { status: 503, ..ok }));
}

#[test]
fn responses_parse_strictly() {
    let fixed =
        parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Example: a:b\r\n\r\nhello")
            .expect("should parse a content-length body");
    assert_eq!((fixed.status, fixed.body.as_slice()), (200, &b"hello"[..]));
    assert_eq!(fixed.header("X-EXAMPLE"), Some("a:b"));
    let chunked = parse_response(
        b"HTTP/1.1 502 Bad Gateway\r\nTransfer-Encoding: chunked\r\n\r\n3;ext=1\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
    )
    .expect("should parse a chunked body");
    assert_eq!(
        (chunked.status, chunked.body.as_slice()),
        (502, &b"abcde"[..])
    );
    for raw in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n"[..],
        b"HTTP/2 200 OK\r\n\r\n",
        b"HTTP/1.1 2000 OK\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort",
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\nzz",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        b"HTTP/1.1 200 OK\r\nbad header\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nX-Ex\tample: a\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nX(Example): a\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\n\r\nab",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 5\r\n\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nab",
        b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nab",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n0\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n0\r\n\r\nextra",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Trailer: a\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+2\r\nab\r\n0\r\n\r\n",
    ] {
        assert_eq!(
            rejection(parse_response(raw)),
            RuntimeRejection::Exchange,
            "should reject {:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn origin_fixture_serves_its_page_and_records_requests() {
    let origin = OriginFixture::start(MARKER).expect("should start the loopback origin");
    let timeout = Duration::from_secs(10);
    let page = get(
        origin.address(),
        "/",
        &[("Sec-Fetch-Mode", "navigate")],
        timeout,
    )
    .expect("should fetch the origin page");
    let missing = get(origin.address(), "/missing", &[], timeout).expect("should fetch a 404");
    let requests = origin.stop();

    assert_eq!(page.status, 200);
    assert_eq!(
        page.body,
        origin_page(MARKER).into_bytes(),
        "should serve the fixed page"
    );
    assert_eq!(missing.status, 404, "should serve only the publisher page");
    assert_eq!(
        requests,
        ["GET / HTTP/1.1", "GET /missing HTTP/1.1"],
        "should record every request line in order"
    );
}

#[test]
fn client_refuses_non_loopback_addresses() {
    let documentation: SocketAddr = "192.0.2.1:80".parse().expect("should parse");
    assert_eq!(
        rejection(get(documentation, "/", &[], Duration::from_secs(1))),
        RuntimeRejection::Exchange,
        "should refuse before connecting"
    );
}

/// A minimal valid Fastly command with nonempty debug sections.
fn fastly_command() -> Vec<u8> {
    append_customs(
        command_module(
            &[
                ("wasi_snapshot_preview1", "fd_write"),
                ("fastly_http_req", "body_downstream_get"),
                ("fastly_http_resp", "send_downstream"),
            ],
            Entry::Nullary,
            true,
        ),
        &[(".debug_info", b"i"), (".debug_line", b"l")],
    )
}

#[test]
fn gate_validates_core_format_before_running_viceroy() {
    let never_run = SelectedViceroy::unverified_for_tests(
        std::path::Path::new("/nonexistent/viceroy"),
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let stripped = command_module(
        &[
            ("fastly_http_req", "body_downstream_get"),
            ("fastly_http_resp", "send_downstream"),
        ],
        Entry::Nullary,
        true,
    );
    let cases: [(&str, Vec<u8>, Violation); 6] = [
        (
            "non-Wasm bytes",
            b"#!/bin/sh\n".to_vec(),
            Violation::WasmEncoding,
        ),
        (
            "component",
            b"\0asm\x0d\0\x01\0".to_vec(),
            Violation::WasmEncoding,
        ),
        (
            "missing entry export",
            append_customs(
                command_module(
                    &[
                        ("fastly_http_req", "body_downstream_get"),
                        ("fastly_http_resp", "send_downstream"),
                    ],
                    Entry::Renamed,
                    true,
                ),
                &[(".debug_info", b"i"), (".debug_line", b"l")],
            ),
            Violation::WasmAbi,
        ),
        (
            "plain WASI command",
            append_customs(
                command_module(
                    &[("wasi_snapshot_preview1", "fd_write")],
                    Entry::Nullary,
                    true,
                ),
                &[(".debug_info", b"i"), (".debug_line", b"l")],
            ),
            Violation::WasmAbi,
        ),
        (
            "foreign host import",
            command_module(&[("env", "call")], Entry::Nullary, true),
            Violation::WasmImport,
        ),
        ("stripped module", stripped, Violation::WasmDebugInfo),
    ];
    for (label, bytes, expected) in cases {
        assert_eq!(
            violation(run_gate(&bytes, &never_run, OriginPresence::Served)),
            expected,
            "should reject a {label} before running Viceroy"
        );
    }
}

#[test]
fn gate_rejects_a_changed_viceroy_before_running_it() {
    let directory = tempfile::tempdir().expect("should create a directory");
    let path = directory.path().join("viceroy");
    std::fs::write(&path, b"example replaced executable").expect("should write a stand-in");
    let stale = SelectedViceroy::unverified_for_tests(
        &path,
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    assert_eq!(
        rejection(run_gate(&fastly_command(), &stale, OriginPresence::Served)),
        RuntimeRejection::Viceroy,
        "should rehash Viceroy before spawning it"
    );
}
