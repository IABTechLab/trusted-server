use super::*;

#[test]
fn rejects_anchor_and_alias_syntax() {
    for yaml in [
        "a: &value example\nb: *value\n",
        "{a: &value example, b: *value}",
        "a: &value [example]\nb: *value\n",
    ] {
        assert!(
            parse_document(yaml).is_err(),
            "should reject YAML references: {yaml}"
        );
    }
}

#[test]
fn rejects_merge_and_non_string_keys() {
    for yaml in [
        "'<<': {a: example}\n",
        "true: example\n",
        "? [example, other]\n: value\n",
    ] {
        assert!(
            parse_document(yaml).is_err(),
            "should reject ambiguous mappings: {yaml}"
        );
    }
}

#[test]
fn rejects_oversized_yaml_before_parsing() {
    let yaml = format!("a: {}", "x".repeat(1024 * 1024));
    assert!(
        parse_document(&yaml).is_err(),
        "should bound YAML before parsing"
    );
}

#[test]
fn rejects_duplicate_keys_and_multiple_documents() {
    for yaml in [
        "a: example\na: other\n",
        "a: example\n---\nb: other\n",
        "a: {nested: one, nested: two}\n",
    ] {
        assert!(
            parse_document(yaml).is_err(),
            "should reject ambiguous documents: {yaml}"
        );
    }
}

#[test]
fn accepts_shell_metacharacters_only_as_scalar_content() {
    let yaml = "run: |\n  test -f example && printf '%s' '*.json'\n";
    assert!(
        parse_document(yaml).is_ok(),
        "should accept inert scalar content: {yaml}"
    );
}

const STRUCTURAL: &str = "name: Example validation\non:\n  workflow_call:\npermissions:\n  contents: read\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@0123456789012345678901234567890123456789 # v4.0.0\n      - run: cargo fmt --all -- --check\n";

#[test]
fn rejects_ambient_triggers() {
    let yaml = STRUCTURAL.replace("  workflow_call:", "  push:");
    assert!(
        validate_structure(&parse_document(&yaml).expect("should parse fixture"), &yaml).is_err(),
        "should require workflow_call only"
    );
}

#[test]
fn rejects_permission_escalation() {
    for yaml in [
        STRUCTURAL.replace("contents: read", "contents: write"),
        STRUCTURAL.replace("  contents: read", "  contents: read\n  id-token: write"),
        STRUCTURAL.replace("  contents: read\n", ""),
    ] {
        assert!(
            validate_structure(&parse_document(&yaml).expect("should parse fixture"), &yaml)
                .is_err(),
            "should require exact read permissions"
        );
    }
}

#[test]
fn rejects_references_hidden_by_plain_scalar_quotes() {
    let yaml = "{a: foo'bar, b: &anchor example, c: *anchor, d: baz'}";
    assert!(
        parse_document(yaml).is_err(),
        "should not treat quotes inside plain scalars as YAML quoting"
    );
}

#[test]
fn rejects_unpinned_and_local_actions() {
    for action in [
        "actions/checkout@v4",
        "./.github/actions/example",
        "./.github/workflows/example.yml@main",
        "actions/checkout@012345678901234567890123456789012345678",
    ] {
        let yaml = STRUCTURAL.replace(
            "actions/checkout@0123456789012345678901234567890123456789",
            action,
        );
        assert!(
            validate_structure(&parse_document(&yaml).expect("should parse fixture"), &yaml)
                .is_err(),
            "should reject action {action}"
        );
    }
}

#[test]
fn accepts_reviewed_workflow_structure() {
    let text = include_str!("../../../../.github/workflows/release-validation.yml");
    let value = parse_document(text).expect("should parse reviewed workflow");
    validate_structure(&value, text)
        .expect("should accept reviewed source input and job permissions");
}

#[test]
fn rejects_reference_characters_in_quoted_scalars_conservatively() {
    for yaml in [
        "name: 'example & * literal'\n",
        "name: 'example ! literal'\n",
    ] {
        assert!(
            parse_document(yaml).is_err(),
            "should reject unsupported quoted reference spellings"
        );
    }
}

#[test]
fn rejects_multiline_quoted_scalars() {
    assert!(
        parse_document("name: \"example\n  run: |\n    other\"\n").is_err(),
        "should reject fake block headers in multiline quotes"
    );
}

#[test]
fn reads_bounded_regular_files() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let path = directory.path().join("example.yml");
    std::fs::write(&path, vec![b'x'; MAX_BYTES + 1]).expect("should write oversized fixture");
    assert!(
        read_bounded(&path).is_err(),
        "should reject held oversized file"
    );
    assert!(
        read_bounded(directory.path()).is_err(),
        "should reject directories"
    );
}

#[test]
fn accepts_only_reviewed_explicit_workflow() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows/release-validation.yml");
    check_files(&[path]).expect("should accept the reviewed explicit workflow and helper");
}

#[test]
fn rejects_each_missing_action_version_comment() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let path = directory.path().join("release-validation.yml");
    let text = include_str!("../../../../.github/workflows/release-validation.yml");
    let yaml = text.replacen(" # v5.0.0 (Node 24)", "", 1);
    std::fs::write(&path, yaml).expect("should write unannotated action fixture");
    assert!(
        check_files(&[path]).is_err(),
        "should reject even one unannotated occurrence of a repeated action"
    );
}

#[test]
fn rejects_unreviewed_workflow_authority_and_commands() {
    let reviewed = include_str!("../../../../.github/workflows/release-validation.yml");
    let mutations = [
        ("  workflow_call:", "  push:"),
        ("  workflow_call:", "  workflow_call:\n    secrets: inherit"),
        (
            "  workflow_call:",
            "  workflow_call:\n    paths: ['example']",
        ),
        ("      source_sha:", "      commands:"),
        ("  contents: read", "  contents: write"),
        ("  contents: read", "  contents: read\n  id-token: write"),
        (
            "  contents: read",
            "  contents: read\n  attestations: write",
        ),
        (
            "    needs: admission",
            "    needs: admission\n    if: false",
        ),
        (
            "    needs: admission",
            "    needs: admission\n    continue-on-error: true",
        ),
        (
            "      - name: Read pinned Node version",
            "      - continue-on-error: true\n        name: Read pinned Node version",
        ),
        (
            "actions/checkout@08c6903cd8c0fde910a37f88322edcfb5dd907a8",
            "actions/checkout@v5",
        ),
        (
            "actions/checkout@08c6903cd8c0fde910a37f88322edcfb5dd907a8",
            "./.github/actions/example",
        ),
        (
            "actions/checkout@08c6903cd8c0fde910a37f88322edcfb5dd907a8",
            "./.github/workflows/example.yml@main",
        ),
        ("cargo test-fastly", "cargo publish"),
        ("cargo test-fastly", "${{ inputs.commands }}"),
        ("persist-credentials: false", "persist-credentials: true"),
    ];
    for (needle, replacement) in mutations {
        assert!(
            reviewed.contains(needle),
            "should locate mutation target {needle}"
        );
        let directory = tempfile::tempdir().expect("should create fixture directory");
        let path = directory.path().join("release-validation.yml");
        std::fs::write(&path, reviewed.replacen(needle, replacement, 1))
            .expect("should write negative workflow fixture");
        assert!(
            check_files(&[path]).is_err(),
            "should reject unreviewed mutation {replacement}"
        );
    }
}

#[test]
fn rejects_empty_multiple_and_unreviewed_paths() {
    assert!(check_files(&[]).is_err(), "should require an explicit path");
    assert!(
        check_files(&[PathBuf::from("example.yml")]).is_err(),
        "should reject unreviewed filenames"
    );
    assert!(
        check_files(&[
            PathBuf::from("release-validation.yml"),
            PathBuf::from("example.yml")
        ])
        .is_err(),
        "should reject unreviewed invocation graphs"
    );
}

#[test]
fn rejects_leaf_symlink_inputs() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let target = directory.path().join("example.yml");
    let link = directory.path().join("release-validation.yml");
    std::fs::write(&target, "a: example\n").expect("should create regular fixture");
    std::os::unix::fs::symlink(&target, &link).expect("should create symlink fixture");
    assert!(
        read_bounded(&link).is_err(),
        "should reject a symlink without following it"
    );
}

#[test]
fn rejects_unreviewed_sibling_helper() {
    let directory = tempfile::tempdir().expect("should create fixture directory");
    let path = directory.path().join("release-validation.yml");
    std::fs::write(
        &path,
        include_str!("../../../../.github/workflows/release-validation.yml"),
    )
    .expect("should write reviewed workflow fixture");
    let helper = directory.path().join("release-validation.mjs");
    std::fs::write(&helper, "throw new Error('example unreviewed helper');\n")
        .expect("should write changed helper fixture");
    assert!(
        check_files(std::slice::from_ref(&path)).is_err(),
        "should bind helper commands to reviewed bytes"
    );
    std::fs::remove_file(&helper).expect("should remove helper fixture");
    assert!(
        check_files(&[path]).is_err(),
        "should require the fixed sibling helper"
    );
}
