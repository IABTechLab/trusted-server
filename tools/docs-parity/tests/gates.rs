use docs_parity::gates::{
    check_link_only_consumer, check_owned_region, parse_manifest, render_region,
};

const MANIFEST: &str = r#"
version = 1
reviewed = true

[[gates]]
id = "rust"
title = "Rust"
owner = "documentation-maintainers"
commands = ["cargo fmt --all -- --check", "cargo test-fastly"]

[[gates]]
id = "docs"
title = "Documentation"
owner = "documentation-maintainers"
commands = ["cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all"]
"#;

#[test]
fn gate_manifest_renders_a_stable_owned_region() {
    let manifest = parse_manifest(MANIFEST.as_bytes()).expect("should parse gate fixture");
    let first = render_region(&manifest).expect("should render gates");
    let second = render_region(&manifest).expect("should rerender gates");

    assert_eq!(first, second, "gate region should be deterministic");
    assert!(first.contains("<!-- docs-parity:gates:start -->"));
    assert!(first.contains("<!-- docs-parity:owner:documentation-maintainers -->"));
    assert!(first.contains("`cargo test-fastly`"));
    assert!(first.ends_with("<!-- docs-parity:gates:end -->\n"));
}

#[test]
fn gate_schema_rejects_unknown_duplicate_and_unowned_records() {
    for invalid in [
        MANIFEST.replace("reviewed = true", "reviewed = true\nunknown = true"),
        MANIFEST.replace("id = \"docs\"", "id = \"rust\""),
        MANIFEST.replace("owner = \"documentation-maintainers\"", "owner = \"\""),
    ] {
        assert!(
            parse_manifest(invalid.as_bytes()).is_err(),
            "closed gate schema should reject invalid fixture"
        );
    }
}

#[test]
fn gate_schema_rejects_markdown_and_html_injection_fields() {
    for invalid in [
        MANIFEST.replace("title = \"Rust\"", "title = \"Rust [`injection`](x)\""),
        MANIFEST.replace(
            "owner = \"documentation-maintainers\"",
            "owner = \"documentation-maintainers --><script>\"",
        ),
        MANIFEST.replace("cargo test-fastly", "cargo `test-fastly`"),
    ] {
        assert!(
            parse_manifest(invalid.as_bytes()).is_err(),
            "rendered fields must not alter Markdown or HTML structure"
        );
    }
}

#[test]
fn link_only_consumers_require_one_canonical_link_and_no_copied_commands() {
    let consumer = "Run the [canonical gates](./CLAUDE.md#development-gates).\n";
    check_link_only_consumer(
        consumer,
        "./CLAUDE.md#development-gates",
        &["cargo fmt --all -- --check", "cargo test-fastly"],
    )
    .expect("one canonical link should pass");

    assert!(
        check_link_only_consumer(
            &format!("{consumer}`cargo test-fastly`\n"),
            "./CLAUDE.md#development-gates",
            &["cargo test-fastly"],
        )
        .is_err(),
        "copied gate commands should fail"
    );
    assert!(
        check_link_only_consumer(
            "[first](./CLAUDE.md#development-gates) [second](./CLAUDE.md#development-gates)",
            "./CLAUDE.md#development-gates",
            &[],
        )
        .is_err(),
        "duplicate canonical links should fail"
    );

    check_link_only_consumer(
        concat!(
            "Run the [canonical gates](./CLAUDE.md#development-gates).\n",
            "<!-- [decoy](./CLAUDE.md#development-gates) cargo test-fastly -->\n",
            "```text\n[copied](./CLAUDE.md#development-gates)\n",
            "cargo test-fastly\n```\n",
        ),
        "./CLAUDE.md#development-gates",
        &["cargo test-fastly"],
    )
    .expect("comments and fenced examples are not semantic link-only content");
    assert!(
        check_link_only_consumer(
            "Only a decoy <!-- [link](./CLAUDE.md#development-gates) -->.\n",
            "./CLAUDE.md#development-gates",
            &[],
        )
        .is_err(),
        "an HTML-comment decoy must not satisfy the canonical link"
    );
    assert!(
        check_link_only_consumer(
            "Run the [canonical gates](./CLAUDE.md#development-gates). cargo **test-fastly**\n",
            "./CLAUDE.md#development-gates",
            &["cargo test-fastly"],
        )
        .is_err(),
        "inline Markdown formatting must not hide copied canonical command text"
    );
}

#[test]
fn owned_gate_region_requires_exact_unique_markers_and_generated_bytes() {
    let manifest = parse_manifest(MANIFEST.as_bytes()).expect("should parse gate fixture");
    let rendered = render_region(&manifest).expect("should render gates");
    let anchor = "# Development gates\n\n";
    let document = format!("{anchor}{rendered}\nManual tail.\n");
    check_owned_region(&document, &manifest, anchor).expect("exact owned region should pass");

    for invalid in [
        document.replace("cargo test-fastly", "cargo test-axum"),
        document.replace("<!-- docs-parity:gates:start -->", ""),
        document.replace("<!-- docs-parity:gates:end -->", ""),
        format!("<!-- docs-parity:gates:end -->\n{document}"),
        format!("{document}{rendered}"),
        format!("{anchor}Manual tail.\n\n{rendered}"),
    ] {
        assert!(
            check_owned_region(&invalid, &manifest, anchor).is_err(),
            "missing, moved, duplicate, or drifted owned regions must fail"
        );
    }
}
