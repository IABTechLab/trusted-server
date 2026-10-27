use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{Captured, Pins, Request, capture, finish, pins, version_matches};
use crate::ordinary::inputs::tests::Fixture;
use crate::ordinary::inputs::{SourceIdentity, source_identity};
use crate::ordinary::{ASSET, Environment, Tool, Tools};

#[test]
fn reads_exact_source_owned_rust_and_node_pins() {
    let directory = tempfile::tempdir().expect("should create pinned source fixture");
    let root = fs::canonicalize(directory.path()).expect("should resolve fixture root");
    fs::write(
        root.join("rust-toolchain.toml"),
        b"[toolchain]\nchannel = \"1.98.1\"\n",
    )
    .expect("should write pinned Rust source");
    fs::write(
        root.join(".tool-versions"),
        b"nodejs 24.20.0\nfastly 16.0.0\n",
    )
    .expect("should write pinned Node source");
    assert_eq!(
        pins(&root).expect("should read exact immutable source pins"),
        Pins {
            rust: "1.98.1".to_owned(),
            node: "24.20.0".to_owned()
        },
        "should use source files rather than request-supplied tool versions"
    );
}

#[test]
fn rejects_floating_duplicate_and_malformed_source_tool_pins() {
    for (rust, node) in [
        ("stable", "nodejs 24.20.0\n"),
        ("1.98.1", "nodejs 24.20.0\nnodejs 24.20.0\n"),
        ("1.98.1", "nodejs 24.20.0 25.0.0\n"),
        ("1.98.1", "nodejs 024.20.0\n"),
        ("1.98.1", "other 24.20.0\n"),
    ] {
        let fixture = Fixture::new();
        fs::write(
            fixture.checkout.join("rust-toolchain.toml"),
            format!("[toolchain]\nchannel = \"{rust}\"\n"),
        )
        .expect("should write source pin");
        fs::write(fixture.checkout.join(".tool-versions"), node)
            .expect("should write source Node selection");
        assert!(
            pins(&fixture.checkout).is_err(),
            "should reject a floating, ambiguous or malformed source tool selection"
        );
    }
}

#[test]
fn requires_exact_safe_version_output_for_each_source_selected_tool() {
    assert!(
        version_matches("rustc", "1.98.1", "rustc 1.98.1 (abcdef123 2026-01-01)"),
        "should accept the exact compiler version line"
    );
    assert!(
        version_matches("node", "24.20.0", "v24.20.0"),
        "should accept the exact Node version line"
    );
    for (kind, pin, line) in [
        ("rustc", "1.98.1", "rustc 1.95.0 (abcdef123 2026-01-01)"),
        ("rustc", "1.98.1", "rustc 1.98.1 (example secret)"),
        ("rustc", "1.98.1", "rustc 1.98.1 (abcdef123 2026-01-01)\n"),
        ("node", "24.20.0", "v24.20.0\nexample"),
        ("node", "24.20.0", "v24.12.0"),
        ("node", "24.20.0", "v24.20.0-extra"),
        ("cargo", "1.98.1", "rustc 1.98.1 (abcdef123 2026-01-01)"),
    ] {
        assert!(
            !version_matches(kind, pin, line),
            "should reject mismatched, injected or unsupported version output"
        );
    }
}

struct CaptureFixture {
    files: Fixture,
    captured: Captured,
    initial: SourceIdentity,
    pins: Pins,
}

impl CaptureFixture {
    fn new() -> Self {
        let files = Fixture::new();
        let (metadata, messages) = files.private_js();
        fs::write(
            files.checkout.join("rust-toolchain.toml"),
            b"[toolchain]\nchannel = \"1.98.1\"\n",
        )
        .expect("should write Rust pin");
        fs::write(files.checkout.join(".tool-versions"), b"nodejs 24.20.0\n")
            .expect("should write Node pin");
        let tools = files.checkout.join("tools/bin");
        fs::create_dir_all(&tools).expect("should create fictional tool root");
        for name in ["rustc", "node"] {
            let file = tools.join(name);
            fs::write(&file, name.as_bytes()).expect("should write fictional tool identity");
            fs::set_permissions(file, fs::Permissions::from_mode(0o755))
                .expect("should set fictional executable mode");
        }
        fs::create_dir(files.checkout.join("release-assets")).expect("should create artifact root");
        fs::write(
            files.checkout.join("release-assets").join(ASSET),
            crate::ordinary::normalization::TEST_WASM,
        )
        .expect("should write final artifact");
        let cargo_metadata = files.target.join("metadata.json");
        let cargo_messages = files.target.join("messages.jsonl");
        fs::write(
            &cargo_metadata,
            serde_json::to_vec(&metadata).expect("should encode metadata"),
        )
        .expect("should retain metadata");
        let bytes: Vec<u8> = messages
            .iter()
            .flat_map(|message| {
                let mut bytes = serde_json::to_vec(message).expect("should encode message");
                bytes.push(b'\n');
                bytes
            })
            .collect();
        fs::write(&cargo_messages, bytes).expect("should retain Cargo messages");
        let rustflags = files.target.join("rustflags.json");
        let flags =
            crate::ordinary::normalization::plan(&files.checkout, &files.target, "canonical")
                .expect("should derive source-owned target flags");
        fs::write(&rustflags, flags.bytes).expect("should retain actual Cargo arguments");
        let uid = fs::metadata(&files.checkout)
            .expect("should inspect source owner")
            .uid();
        let request = Request {
            role: "canonical".to_owned(),
            source_commit: "a".repeat(40),
            environment: Environment {
                checkout_root: files
                    .checkout
                    .to_str()
                    .expect("should encode source root")
                    .to_owned(),
                target_root: files
                    .target
                    .to_str()
                    .expect("should encode target root")
                    .to_owned(),
                started_at_seconds: 1_700_000_000,
                umask: 0o022,
                jobs: 4,
                locale: "C".to_owned(),
                timezone: "UTC".to_owned(),
                uid,
            },
            rustc: tools.join("rustc"),
            node: tools.join("node"),
            cargo_messages,
            cargo_metadata,
            rustflags,
        };
        let captured =
            capture(&serde_json::to_vec(&request).expect("should encode explicit capture"))
                .expect("should retain initial root identities");
        let initial = source_identity(&files.checkout, &captured.request.source_commit)
            .expect("should measure initial source");
        let pins = pins(&files.checkout).expect("should bind source tool pins");
        Self {
            files,
            captured,
            initial,
            pins,
        }
    }

    fn tools(&self) -> Tools {
        let digest = |path| {
            hex::encode(Sha256::digest(
                fs::read(path).expect("should measure fictional tool bytes"),
            ))
        };
        Tools {
            rustc: Tool {
                version: "rustc 1.98.1 (abcdef123 2026-01-01)".to_owned(),
                sha256: digest(&self.captured.request.rustc),
            },
            node: Tool {
                version: "v24.20.0".to_owned(),
                sha256: digest(&self.captured.request.node),
            },
            acquisition: "ordinary-hosted-toolchain".to_owned(),
        }
    }

    fn value(&self) -> Value {
        serde_json::to_value(&self.captured.request)
            .expect("should encode only explicit capture fields")
    }
}

#[test]
fn assembles_only_whitelisted_ordinary_facts_from_retained_inputs() {
    let fixture = CaptureFixture::new();
    let observed = finish(
        &fixture.captured,
        &fixture.initial,
        &fixture.pins,
        &fixture.tools(),
    )
    .expect("should join measured ordinary inputs without executing a tool in this test");
    assert_eq!(
        observed.value()["release_qualification"],
        false,
        "should not grant qualification"
    );
    assert_eq!(
        observed.value()["gaps"]
            .as_array()
            .expect("should record gaps")
            .len(),
        4,
        "should preserve every unsupported assurance"
    );
    assert_eq!(
        observed.value()["environment"]
            .as_object()
            .expect("should record explicit environment")
            .len(),
        8,
        "should serialize only the explicit capture allowlist"
    );
    assert_eq!(
        observed.value()["artifact"]["sha256"],
        hex::encode(Sha256::digest(crate::ordinary::normalization::TEST_WASM)),
        "should bind final retained bytes"
    );
}

#[test]
fn rejects_wrong_source_versions_or_source_changes_before_recording() {
    let fixture = CaptureFixture::new();
    let mut tools = fixture.tools();
    tools.node.version = "v24.12.0".to_owned();
    assert!(
        finish(&fixture.captured, &fixture.initial, &fixture.pins, &tools).is_err(),
        "should reject a version differing from source pins"
    );
    fs::write(
        fixture.files.checkout.join("Cargo.lock"),
        b"changed source lock",
    )
    .expect("should change lock after initial measurement");
    assert!(
        finish(
            &fixture.captured,
            &fixture.initial,
            &fixture.pins,
            &fixture.tools()
        )
        .is_err(),
        "should reject source changes during measurement"
    );
}

#[test]
fn rejects_changed_retained_arguments_and_unmapped_or_stripped_final_wasm() {
    for change in ["arguments", "stripped", "physical-root"] {
        let fixture = CaptureFixture::new();
        if change == "arguments" {
            fs::write(
                &fixture.captured.request.rustflags,
                b"[\"--config\",\"wrong\"]\n",
            )
            .expect("should alter actual retained arguments");
        } else {
            let bytes = if change == "stripped" {
                b"\0asm\x01\0\0\0".to_vec()
            } else {
                let mut bytes = crate::ordinary::normalization::TEST_WASM.to_vec();
                let root = fixture
                    .files
                    .checkout
                    .to_str()
                    .expect("should encode physical root");
                let data = root.as_bytes();
                // A custom section leaves all existing debug sections intact.
                bytes.push(0);
                let mut length = data.len() + 2;
                loop {
                    let next = (length & 0x7f) as u8;
                    length >>= 7;
                    bytes.push(next | if length == 0 { 0 } else { 0x80 });
                    if length == 0 {
                        break;
                    }
                }
                bytes.extend([1, b'x']);
                bytes.extend(data);
                bytes
            };
            fs::write(
                fixture.files.checkout.join("release-assets").join(ASSET),
                bytes,
            )
            .expect("should alter final Wasm evidence");
        }
        assert!(
            finish(
                &fixture.captured,
                &fixture.initial,
                &fixture.pins,
                &fixture.tools()
            )
            .is_err(),
            "should refuse altered retained argument bytes or debug/path evidence"
        );
    }
}

#[test]
fn rejects_malformed_unknown_and_false_capture_contexts_without_tool_execution() {
    let fixture = CaptureFixture::new();
    for (pointer, value) in [
        ("/role", json!("other")),
        ("/source_commit", json!("main")),
        ("/environment/jobs", json!(2)),
        ("/environment/umask", json!(0o027)),
        ("/environment/locale", json!("example")),
        ("/environment/timezone", json!("example")),
        ("/environment/uid", json!(0)),
        ("/environment/started_at_seconds", json!(0)),
    ] {
        let mut request = fixture.value();
        *request
            .pointer_mut(pointer)
            .expect("should locate fixture capture field") = value;
        assert!(
            capture(&serde_json::to_vec(&request).expect("should encode context")).is_err(),
            "should reject false context {pointer} before tools could run"
        );
    }
    let mut request = fixture.value();
    request["credentials"] = json!("example-secret");
    assert!(
        capture(&serde_json::to_vec(&request).expect("should encode unknown capture")).is_err(),
        "should reject any additional inherited-environment field"
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("should create byte-identical replacement directory");
    for entry in fs::read_dir(source).expect("should enumerate retained fixture") {
        let entry = entry.expect("should read fixture entry");
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("should inspect fixture entry")
            .is_dir()
        {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("should copy identical fixture bytes");
        }
    }
}

#[test]
fn rejects_byte_identical_checkout_or_target_root_substitution() {
    for replace_target in [false, true] {
        let fixture = CaptureFixture::new();
        let tools = fixture.tools();
        let root = if replace_target {
            &fixture.files.target
        } else {
            &fixture.files.checkout
        };
        let retained = root.with_extension("retained");
        fs::rename(root, &retained).expect("should retain the originally captured root");
        copy_tree(&retained, root);
        assert!(
            finish(&fixture.captured, &fixture.initial, &fixture.pins, &tools).is_err(),
            "should reject replacing either captured root even when every file byte and owner matches"
        );
    }
}

#[test]
fn malformed_capture_errors_never_echo_untrusted_keys() {
    let key = "example-key".repeat(10000);
    let bytes = format!("{{\"{key}\":0,\"{key}\":1}}");
    let error = match capture(bytes.as_bytes()) {
        Ok(_) => panic!("should reject duplicate capture keys"),
        Err(error) => error,
    };
    // Caller-enabled backtraces may expand Report's debug rendering. Bound the
    // owned diagnostic itself and independently reject input in the full rendering.
    let diagnostic = error
        .downcast_ref::<&str>()
        .expect("should retain the fixed diagnostic");
    assert_eq!(
        *diagnostic, "ordinary measurement inputs or tool pins are invalid",
        "should discard the original parser diagnostic"
    );
    assert!(
        diagnostic.len() < 512 && !format!("{error:?}").contains("example-key"),
        "should retain a bounded fixed diagnostic for malformed captures"
    );
}
