use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::recipe::paths::PrefixPaths;

const MANIFEST: &str = "/ts/immutable/0/crates/example-adapter/Cargo.toml";
const RELEASE: &str = "/ts/writable/1/wasm32-example/release";

/// A message edit applied before serialization.
type Edit = fn(&mut Value);

fn expected() -> ExpectedBinary<'static> {
    ExpectedBinary {
        bin: "example-bin",
        manifest: Path::new(MANIFEST),
        release_dir: Path::new(RELEASE),
    }
}

fn artifact() -> Value {
    json!({
        "reason": "compiler-artifact",
        "package_id": "path+file:///example#0.1.0",
        "manifest_path": MANIFEST,
        "target": {"kind": ["bin"], "crate_types": ["bin"], "name": "example-bin"},
        "profile": {"opt_level": "3", "debuginfo": 1, "debug_assertions": false},
        "filenames": [format!("{RELEASE}/example-bin.wasm")],
        "executable": format!("{RELEASE}/example-bin.wasm"),
        "fresh": false
    })
}

fn build_script() -> Value {
    json!({
        "reason": "compiler-artifact",
        "manifest_path": MANIFEST,
        "target": {"kind": ["custom-build"], "crate_types": ["bin"], "name": "build-script-build"},
        "profile": {"debuginfo": 0},
        "executable": null,
        "fresh": false
    })
}

fn finished(success: &Value) -> Value {
    json!({"reason": "build-finished", "success": success})
}

fn lines(messages: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        out.extend_from_slice(message.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

fn rejection(stdout: &[u8]) -> String {
    match locate_binary(stdout, &expected()) {
        Ok(path) => panic!("should reject the output, got {path:?}"),
        Err(error) => format!("{error:?}"),
    }
}

fn modified(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut message = artifact();
    edit(&mut message);
    lines(&[build_script(), message, finished(&json!(true))])
}

#[test]
fn single_fresh_false_debuginfo_one_artifact_is_located() {
    let stdout = lines(&[
        build_script(),
        json!({"reason": "build-script-executed", "linked_libs": [], "env": []}),
        artifact(),
        finished(&json!(true)),
    ]);
    assert_eq!(
        locate_binary(&stdout, &expected()).expect("should locate the executable"),
        Path::new("/ts/writable/1/wasm32-example/release/example-bin.wasm"),
        "should return the container executable path"
    );
}

#[test]
fn build_must_end_with_successful_build_finished() {
    let cases: [(&str, Vec<u8>); 6] = [
        ("missing", lines(&[build_script(), artifact()])),
        ("empty", Vec::new()),
        ("failed", lines(&[artifact(), finished(&json!(false))])),
        (
            "non-boolean success",
            lines(&[artifact(), finished(&json!("true"))]),
        ),
        ("not last", lines(&[finished(&json!(true)), artifact()])),
        (
            "twice",
            lines(&[artifact(), finished(&json!(true)), finished(&json!(true))]),
        ),
    ];
    for (label, stdout) in cases {
        assert!(
            rejection(&stdout).contains("build-finished"),
            "should reject a {label} build-finished"
        );
    }
    assert!(
        rejection(&lines(&[artifact(), json!({"reason": "build-finished"})]))
            .contains("build-finished"),
        "should reject build-finished without success"
    );
}

#[test]
fn zero_or_duplicate_matching_artifacts_are_rejected() {
    let none = lines(&[build_script(), finished(&json!(true))]);
    assert!(
        rejection(&none).contains("exactly one"),
        "should reject zero binary artifacts"
    );
    let twice = lines(&[artifact(), artifact(), finished(&json!(true))]);
    assert!(
        rejection(&twice).contains("exactly one"),
        "should reject two binary artifacts"
    );
    let mut stray = artifact();
    stray["target"]["name"] = json!("other-bin");
    stray["executable"] = json!(format!("{RELEASE}/other-bin.wasm"));
    let extra = lines(&[artifact(), stray, finished(&json!(true))]);
    assert!(
        rejection(&extra).contains("unexpected"),
        "should reject an additional binary artifact"
    );
}

#[test]
fn wrong_kind_name_or_manifest_is_rejected() {
    let edits: [(&str, Edit); 6] = [
        ("lib kind", |m| m["target"]["kind"] = json!(["lib"])),
        ("bin and lib kind", |m| {
            m["target"]["kind"] = json!(["bin", "lib"])
        }),
        ("string kind", |m| m["target"]["kind"] = json!("bin")),
        ("other name", |m| m["target"]["name"] = json!("other-bin")),
        ("other manifest", |m| {
            m["manifest_path"] = json!("/ts/immutable/0/crates/other/Cargo.toml");
        }),
        ("relative manifest", |m| {
            m["manifest_path"] = json!("crates/example-adapter/Cargo.toml");
        }),
    ];
    for (label, edit) in edits {
        assert!(
            rejection(&modified(edit)).contains("exactly one"),
            "should reject an artifact with {label}"
        );
    }
}

#[test]
fn fresh_or_unmeasured_artifact_is_rejected() {
    let edits: [(&str, Edit); 4] = [
        ("fresh true", |m| m["fresh"] = json!(true)),
        ("fresh missing", |m| {
            m.as_object_mut().expect("should be object").remove("fresh");
        }),
        ("fresh string", |m| m["fresh"] = json!("false")),
        ("fresh null", |m| m["fresh"] = Value::Null),
    ];
    for (label, edit) in edits {
        assert!(
            rejection(&modified(edit)).contains("fresh"),
            "should reject an artifact with {label}"
        );
    }
}

#[test]
fn debuginfo_must_be_exactly_one() {
    let edits: [(&str, Edit); 6] = [
        ("zero", |m| m["profile"]["debuginfo"] = json!(0)),
        ("two", |m| m["profile"]["debuginfo"] = json!(2)),
        ("string one", |m| m["profile"]["debuginfo"] = json!("1")),
        ("float one", |m| m["profile"]["debuginfo"] = json!(1.5)),
        ("missing", |m| {
            m["profile"]
                .as_object_mut()
                .expect("should be object")
                .remove("debuginfo");
        }),
        ("no profile", |m| {
            m.as_object_mut()
                .expect("should be object")
                .remove("profile");
        }),
    ];
    for (label, edit) in edits {
        assert!(
            rejection(&modified(edit)).contains("debuginfo"),
            "should reject an artifact with {label} debuginfo"
        );
    }
}

#[test]
fn executable_must_be_a_direct_child_of_the_release_directory() {
    let cases: [(&str, Value); 9] = [
        ("null", Value::Null),
        ("number", json!(7)),
        ("relative", json!("release/example-bin.wasm")),
        ("outside", json!("/ts/immutable/0/example-bin.wasm")),
        (
            "sibling prefix",
            json!("/ts/writable/1/wasm32-example/release-evil/x"),
        ),
        ("nested", json!(format!("{RELEASE}/deps/example-bin.wasm"))),
        ("dotdot", json!(format!("{RELEASE}/../example-bin.wasm"))),
        ("directory itself", json!(RELEASE)),
        (
            "trailing slash",
            json!(format!("{RELEASE}/example-bin.wasm/")),
        ),
    ];
    for (label, value) in cases {
        let stdout = modified(|m| m["executable"] = value);
        assert!(
            rejection(&stdout).contains("executable"),
            "should reject a {label} executable"
        );
    }
}

#[test]
fn malformed_lines_are_rejected() {
    let good = lines(&[artifact(), finished(&json!(true))]);
    let mut oversize = br#"{"reason":"build-script-executed","pad":""#.to_vec();
    oversize.resize(oversize.len() + 1024 * 1024, b'a');
    oversize.extend_from_slice(b"\"}\n");
    oversize.extend_from_slice(&good);
    let cases: [(&str, Vec<u8>); 9] = [
        (
            "non-JSON",
            [b"Compiling example\n".as_slice(), &good].concat(),
        ),
        ("blank line", [b"\n".as_slice(), &good].concat()),
        (
            "duplicate keys",
            [
                br#"{"reason":"build-script-executed","reason":"compiler-artifact"}"#.as_slice(),
                b"\n",
                &good,
            ]
            .concat(),
        ),
        ("array line", [b"[]\n".as_slice(), &good].concat()),
        ("null line", [b"null\n".as_slice(), &good].concat()),
        (
            "missing reason",
            [b"{\"x\":1}\n".as_slice(), &good].concat(),
        ),
        ("oversize", oversize),
        (
            "invalid UTF-8",
            [b"{\"reason\":\"\xff\"}\n".as_slice(), &good].concat(),
        ),
        ("unterminated final line", good[..good.len() - 1].to_vec()),
    ];
    for (label, stdout) in cases {
        let message = rejection(&stdout);
        assert!(
            message.contains("line") || message.contains("Document"),
            "should reject {label} output: {message}"
        );
    }
}

#[test]
fn host_path_maps_only_through_writable_roots() {
    let host = Path::new("/host/target");
    let paths = PrefixPaths::new()
        .with(Path::new("/host/checkout"), "/ts/immutable/0", false)
        .with(host, "/ts/writable/1", true);
    let located = locate_host_binary(
        &lines(&[artifact(), finished(&json!(true))]),
        &expected(),
        &paths,
    )
    .expect("should locate and map the executable");
    assert_eq!(
        located.execution,
        Path::new("/ts/writable/1/wasm32-example/release/example-bin.wasm"),
        "should keep the container path"
    );
    assert_eq!(
        located.host,
        Path::new("/host/target/wasm32-example/release/example-bin.wasm"),
        "should map back into the writable host root"
    );
    let immutable_only =
        PrefixPaths::new().with(Path::new("/host/target"), "/ts/writable/1", false);
    assert!(
        locate_host_binary(
            &lines(&[artifact(), finished(&json!(true))]),
            &expected(),
            &immutable_only
        )
        .is_err(),
        "should reject an executable that maps only through a non-writable root"
    );
}
