use std::env;
use std::path::Path;
use std::process::Command;

use serde_json::json;

use super::{Observation, json_bytes};

pub(super) fn observations() -> Vec<Observation> {
    let binary = env::var_os("INTEGRATION_SPLIT_BASELINE_TS")
        .expect("should select the freshly built candidate CLI binary");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("should resolve the repository root");
    let source = root.join(
        "crates/trusted-server-integration-tests/fixtures/integration_split_baseline/source.toml",
    );
    let cases: [(&str, &[&str]); 8] = [
        ("lint", &["lint"]),
        ("match-details", &["match", "/article", "--details"]),
        ("match-none", &["match", "/unmatched", "--details"]),
        ("explain", &["explain", "/article"]),
        ("explain-prefetch", &["explain", "/article", "--prefetch"]),
        (
            "explain-consent",
            &["explain", "/article", "--consent-denied"],
        ),
        (
            "check",
            &[
                "check",
                "/article",
                "--expected-slot",
                "baseline-banner",
                "--expected-slot",
                "baseline-footer",
            ],
        ),
        (
            "check-fails",
            &["check", "/article", "--expected-slot", "missing-slot"],
        ),
    ];
    let rows: Vec<_> = cases
        .into_iter()
        .map(|(id, arguments)| {
            let output = Command::new(&binary)
                .current_dir(&root)
                .args(["config", "ad-templates"])
                .args(arguments)
                .arg("--app-config")
                .arg(&source)
                .arg("--no-env")
                .env("NO_COLOR", "1")
                .env("CLICOLOR", "0")
                .env("RUST_LOG", "off")
                .output()
                .expect("should execute the candidate CLI read model");
            let normalize = |bytes: Vec<u8>| {
                String::from_utf8(bytes)
                    .expect("should produce UTF-8 CLI output")
                    .replace(
                        source.to_str().expect("should have a UTF-8 source path"),
                        "@baseline-source@",
                    )
            };
            json!({
                "id": id,
                "status": output.status.code().expect("should exit without a signal"),
                "stdout": normalize(output.stdout),
                "stderr": normalize(output.stderr),
            })
        })
        .collect();
    vec![Observation {
        name: "cli-views.json",
        bytes: json_bytes(json!(rows)),
    }]
}
