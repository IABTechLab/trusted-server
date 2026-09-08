use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const GUIDE: &str = include_str!("../../../docs/guide/integration-guide.md");
const START: &str = "<!-- documentation-snippet:runtime-services:start -->";
const END: &str = "<!-- documentation-snippet:runtime-services:end -->";

#[test]
fn integration_guide_runtime_services_fixture_compiles() {
    let marked = GUIDE
        .split_once(START)
        .and_then(|(_, tail)| tail.split_once(END).map(|(body, _)| body))
        .expect("should contain one bounded runtime-services snippet");
    let source = marked
        .trim()
        .strip_prefix("```rust\n")
        .and_then(|body| body.strip_suffix("\n```"))
        .expect("should contain exactly one Rust fence inside the snippet markers");

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should have a current system clock")
        .as_nanos();
    let fixture_root = std::env::temp_dir().join(format!(
        "trusted-server-documentation-snippet-{}-{nonce}",
        std::process::id()
    ));
    let source_dir = fixture_root.join("src");
    fs::create_dir_all(&source_dir).expect("should create isolated fixture directory");

    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("should resolve repository root")
        .canonicalize()
        .expect("should canonicalize repository root");
    let core_path = repository_root.join("crates/trusted-server-core");
    let manifest = format!(
        "[package]\nname = \"documentation-snippet\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n\n[dependencies]\nerror-stack = \"0.6\"\ntrusted-server-core = {{ path = {:?} }}\n",
        core_path
    );
    fs::write(fixture_root.join("Cargo.toml"), manifest)
        .expect("should write isolated fixture manifest");
    fs::write(source_dir.join("lib.rs"), source).expect("should write exact documented source");

    let output = Command::new("cargo")
        .args(["check", "--offline", "--quiet"])
        .current_dir(&fixture_root)
        .env(
            "CARGO_TARGET_DIR",
            repository_root.join("target/documentation-snippets"),
        )
        .output()
        .expect("should execute cargo check for documented source");
    fs::remove_dir_all(&fixture_root).expect("should remove isolated fixture directory");

    assert!(
        output.status.success(),
        "documented integration fixture must compile:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
