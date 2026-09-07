use std::path::Path;

use docs_parity::snippets::{SnippetExecution, SnippetRunner, SnippetSource, check_snippets};
use sha2::{Digest as _, Sha256};

#[derive(Default)]
struct FakeRunner {
    requests: Vec<(String, String, String)>,
}

impl SnippetRunner for FakeRunner {
    fn run(
        &mut self,
        runner: &str,
        target: &str,
        command: &str,
        _contents: &str,
        working_directory: &Path,
    ) -> Result<SnippetExecution, String> {
        assert!(
            working_directory.is_dir(),
            "isolated directory should exist"
        );
        self.requests
            .push((runner.to_owned(), target.to_owned(), command.to_owned()));
        if runner == "rust-syntax" {
            Ok(SnippetExecution {
                success: false,
                phase: "validation".to_owned(),
                diagnostic: "Rust syntax is invalid".to_owned(),
            })
        } else {
            Ok(SnippetExecution {
                success: true,
                phase: "execute".to_owned(),
                diagnostic: String::new(),
            })
        }
    }
}

fn fingerprint(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn manifest(illustrative: &str) -> String {
    format!(
        r#"
version = 1
reviewed = true

[[commands]]
id = "shell-ok"
runner = "shell-syntax"
target = "host"
language = "bash"
mode = "executable"
command = "validate syntax"

[[commands]]
id = "rust-invalid"
runner = "rust-syntax"
target = "host"
language = "rust"
mode = "expected_failure"
command = "validate syntax"
expected_phase = "validation"
expected_diagnostic = "Rust syntax is invalid"

[[commands]]
id = "fragment"
runner = "none"
target = "none"
language = "toml"
mode = "illustrative"
owner = "documentation-maintainers"
rationale = "Partial configuration requires surrounding operator values"
expires_at = "2027-01-01T00:00:00Z"

[[snippets]]
path = "docs/guide/example.md"
selector = "fence:1"
command = "shell-ok"
fingerprint = "{}"

[[snippets]]
path = "docs/guide/example.md"
selector = "fence:2"
command = "rust-invalid"
fingerprint = "{}"

[[snippets]]
path = "docs/guide/example.md"
selector = "fence:3"
command = "fragment"
fingerprint = "{}"
"#,
        fingerprint("echo ready\n"),
        fingerprint("let value = missing;\n"),
        fingerprint(illustrative),
    )
}

fn source(illustrative: &str) -> SnippetSource {
    SnippetSource {
        path: "docs/guide/example.md".to_owned(),
        markdown: format!(
            "# Example\n\n```bash\necho ready\n```\n\n```rust\nlet value = missing;\n```\n\n```toml\n{illustrative}```\n"
        ),
    }
}

#[test]
fn every_fence_is_classified_and_modes_execute_in_isolation() {
    let illustrative = "publisher.origin_url = \"https://origin.example.com\"\n";
    let mut runner = FakeRunner::default();

    check_snippets(
        manifest(illustrative).as_bytes(),
        &[source(illustrative)],
        "2026-09-06T00:00:00Z",
        &mut runner,
    )
    .expect("complete snippet fixture should pass");

    assert_eq!(
        runner.requests.len(),
        2,
        "illustrative fence must not execute"
    );
    assert_eq!(
        runner.requests[0],
        (
            "shell-syntax".to_owned(),
            "host".to_owned(),
            "validate syntax".to_owned()
        )
    );
    assert_eq!(runner.requests[1].0, "rust-syntax");
}

#[test]
fn snippet_check_rejects_missing_stale_and_duplicate_classification() {
    let illustrative = "value = true\n";
    let source = source(illustrative);
    let valid = manifest(illustrative);
    for invalid in [
        valid.replacen("[[snippets]]", "[[removed]]", 1),
        valid.replace("selector = \"fence:3\"", "selector = \"fence:4\""),
        format!(
            "{valid}\n[[snippets]]\npath = \"docs/guide/example.md\"\nselector = \"fence:1\"\ncommand = \"shell-ok\"\nfingerprint = \"{}\"\n",
            fingerprint("echo ready\n")
        ),
    ] {
        assert!(
            check_snippets(
                invalid.as_bytes(),
                std::slice::from_ref(&source),
                "2026-09-06T00:00:00Z",
                &mut FakeRunner::default(),
            )
            .is_err(),
            "classification mismatch should fail"
        );
    }
}

#[test]
fn expected_failure_requires_the_exact_phase_and_stable_diagnostic() {
    let illustrative = "value = true\n";
    let source = source(illustrative);
    let valid = manifest(illustrative);
    for invalid in [
        valid.replace(
            "expected_phase = \"validation\"",
            "expected_phase = \"execute\"",
        ),
        valid.replace("Rust syntax is invalid", "different diagnostic"),
    ] {
        assert!(
            check_snippets(
                invalid.as_bytes(),
                std::slice::from_ref(&source),
                "2026-09-06T00:00:00Z",
                &mut FakeRunner::default(),
            )
            .is_err(),
            "wrong failure oracle should fail"
        );
    }
}

#[test]
fn command_manifest_accepts_only_closed_syntax_validation_tokens() {
    let illustrative = "value = true\n";
    let source = source(illustrative);
    let valid = manifest(illustrative);
    for invalid in [
        valid.replacen(
            "command = \"validate syntax\"",
            "command = \"echo owned\"",
            1,
        ),
        valid.replacen("runner = \"shell-syntax\"", "runner = \"shell\"", 1),
        valid.replacen("target = \"host\"", "target = \"remote\"", 1),
    ] {
        assert!(
            check_snippets(
                invalid.as_bytes(),
                std::slice::from_ref(&source),
                "2026-09-06T00:00:00Z",
                &mut FakeRunner::default(),
            )
            .is_err(),
            "arbitrary command grammar must fail before execution"
        );
    }
}

#[test]
fn illustrative_waivers_expire_and_all_modes_reject_stale_fingerprints() {
    let illustrative = "value = true\n";
    let source = source(illustrative);
    assert!(
        check_snippets(
            manifest(illustrative).as_bytes(),
            std::slice::from_ref(&source),
            "2027-01-01T00:00:00Z",
            &mut FakeRunner::default(),
        )
        .is_err(),
        "waiver should fail at its expiry instant"
    );
    assert!(
        check_snippets(
            manifest("different\n").as_bytes(),
            &[source],
            "2026-09-06T00:00:00Z",
            &mut FakeRunner::default(),
        )
        .is_err(),
        "changed fence bytes should fail fingerprint binding"
    );
}
