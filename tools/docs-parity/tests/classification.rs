use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const SUCCESS: i32 = 0;
const ERROR: i32 = 2;

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("should create test repository");
        run_git(directory.path(), &["init", "--quiet"]);
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn track(&self, path: &str, contents: &[u8]) {
        let absolute = self.path().join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("should create tracked file parent");
        }
        fs::write(&absolute, contents).expect("should write tracked file");
        run_git(self.path(), &["add", "--", path]);
    }

    fn manifests(&self, tracked: &str, maintained: &str) {
        let directory = self.path().join("tools/docs-parity/manifests");
        fs::create_dir_all(&directory).expect("should create manifest directory");
        fs::write(directory.join("tracked-files.toml"), tracked)
            .expect("should write tracked-files manifest");
        fs::write(directory.join("maintained-sources.toml"), maintained)
            .expect("should write maintained-sources manifest");
    }

    fn classify(&self) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(["classify", "--check"])
            .output()
            .expect("should execute docs-parity")
    }

    fn update_classification(&self) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(["classify", "--update"])
            .output()
            .expect("should execute docs-parity")
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_docs-parity"))
}

fn run_git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .expect("should execute git");
    assert!(status.success(), "git command should succeed");
}

fn status_code(output: &Output) -> i32 {
    output.status.code().expect("should exit normally")
}

fn diagnostic(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("diagnostic should be UTF-8")
}

fn text_manifest(path: &str, maximum: usize) -> String {
    format!(
        "version = 1\nreviewed = true\nmax_text_bytes = {maximum}\n\n[[files]]\npath = \"{path}\"\nkind = \"text\"\n"
    )
}

fn binary_manifest(path: &str) -> String {
    format!(
        "version = 1\nreviewed = true\nmax_text_bytes = 1024\n\n[[files]]\npath = \"{path}\"\nkind = \"binary\"\n"
    )
}

fn whole_source(path: &str) -> String {
    format!(
        "version = 1\nreviewed = true\n\n[[sources]]\npath = \"{path}\"\nmode = \"whole\"\ndisposition = \"include\"\n"
    )
}

fn comment_source(path: &str, grammar: &str, comments: &[(&str, &str)]) -> String {
    let mut manifest = format!(
        "version = 1\nreviewed = true\n\n[[sources]]\npath = \"{path}\"\nmode = \"comments\"\ngrammar = \"{grammar}\"\n"
    );
    for (selector, contents) in comments {
        manifest.push_str(&format!(
            "\n[[comments]]\npath = \"{path}\"\nselector = \"{selector}\"\nfingerprint = \"{}\"\ndisposition = \"include\"\n",
            fingerprint(contents.as_bytes())
        ));
    }
    manifest
}

fn fingerprint(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

#[test]
fn tracked_manifest_requires_explicit_review_attestation() {
    let repository = TestRepository::new();
    repository.track("notes.txt", b"reviewed text\n");
    repository.manifests(
        "version = 1\nmax_text_bytes = 1024\n\n[[files]]\npath = \"notes.txt\"\nkind = \"text\"\n",
        &whole_source("notes.txt"),
    );
    let result = repository.classify();
    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("reviewed"));
}

#[test]
fn maintained_manifest_requires_explicit_review_attestation() {
    let repository = TestRepository::new();
    repository.track("notes.txt", b"reviewed text\n");
    repository.manifests(
        &text_manifest("notes.txt", 1024),
        "version = 1\n\n[[sources]]\npath = \"notes.txt\"\nmode = \"whole\"\ndisposition = \"include\"\n",
    );
    let result = repository.classify();
    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("reviewed"));
}

#[test]
fn complete_text_and_binary_classification_passes() {
    let repository = TestRepository::new();
    repository.track("notes.txt", b"reviewed text\n");
    repository.track("image.bin", &[0, 1, 2, 255]);
    repository.manifests(
        "version = 1\nreviewed = true\nmax_text_bytes = 1024\n\n[[files]]\npath = \"image.bin\"\nkind = \"binary\"\n\n[[files]]\npath = \"notes.txt\"\nkind = \"text\"\n",
        &whole_source("notes.txt"),
    );

    let result = repository.classify();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "complete classification should pass: {}",
        diagnostic(&result)
    );
}

#[test]
fn unknown_text_extension_fails_closed() {
    assert_unclassified("notes.unknown", b"human text\n");
}

#[test]
fn unknown_binary_fails_closed() {
    assert_unclassified("blob.dat", &[0, 159, 146, 150]);
}

#[test]
fn new_dockerfile_fails_closed() {
    assert_unclassified("nested/Dockerfile", b"FROM scratch\n");
}

#[test]
fn new_mjs_file_fails_closed() {
    assert_unclassified("scripts/new-build.mjs", b"export default {};\n");
}

#[test]
fn new_proto_file_fails_closed() {
    assert_unclassified("schema/new.proto", b"syntax = \"proto3\";\n");
}

fn assert_unclassified(path: &str, contents: &[u8]) {
    let repository = TestRepository::new();
    repository.track("known.txt", b"known\n");
    repository.track(path, contents);
    repository.manifests(
        &text_manifest("known.txt", 1024),
        &whole_source("known.txt"),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "unknown path should fail");
    assert!(
        diagnostic(&result).contains(&format!("unclassified tracked path: {path}")),
        "diagnostic should identify the unknown path: {}",
        diagnostic(&result)
    );
}

#[test]
fn invalid_utf8_in_expected_text_fails_closed() {
    let repository = TestRepository::new();
    repository.track("notes.txt", &[0xff, 0xfe]);
    repository.manifests(
        &text_manifest("notes.txt", 1024),
        &whole_source("notes.txt"),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "invalid UTF-8 should fail");
    assert!(
        diagnostic(&result).contains("expected text is not valid UTF-8: notes.txt"),
        "diagnostic should identify invalid UTF-8: {}",
        diagnostic(&result)
    );
}

#[test]
fn oversized_expected_text_fails_closed() {
    let repository = TestRepository::new();
    repository.track("notes.txt", b"nine bytes");
    repository.manifests(&text_manifest("notes.txt", 4), &whole_source("notes.txt"));

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "oversized text should fail");
    assert!(
        diagnostic(&result).contains("expected text exceeds 4 bytes: notes.txt"),
        "diagnostic should identify the size boundary: {}",
        diagnostic(&result)
    );
}

#[test]
fn human_facing_comment_outside_selector_fails_closed() {
    let repository = TestRepository::new();
    repository.track("script.sh", b"# reviewed\necho ok\n# newly added\n");
    repository.manifests(
        &text_manifest("script.sh", 1024),
        &comment_source("script.sh", "shell", &[("bytes:0-10", "# reviewed")]),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "new comment should fail");
    assert!(
        diagnostic(&result).contains("unclassified comment span: script.sh:bytes:19-32"),
        "diagnostic should identify the new span: {}",
        diagnostic(&result)
    );
}

#[test]
fn comment_syntax_without_an_extractor_fails_closed() {
    let repository = TestRepository::new();
    repository.track("workflow.weird", b"%% operator guidance\n");
    repository.manifests(
        &text_manifest("workflow.weird", 1024),
        &comment_source("workflow.weird", "percent-pairs", &[]),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "unknown grammar should fail");
    assert!(
        diagnostic(&result).contains("unsupported comment grammar: percent-pairs"),
        "diagnostic should identify the grammar: {}",
        diagnostic(&result)
    );
}

#[test]
fn an_unclassified_extracted_comment_span_fails_closed() {
    let repository = TestRepository::new();
    repository.track("config.toml", b"# operator guidance\nkey = \"value\"\n");
    repository.manifests(
        &text_manifest("config.toml", 1024),
        &comment_source("config.toml", "toml", &[]),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "unclassified span should fail");
    assert!(
        diagnostic(&result).contains("unclassified comment span: config.toml:bytes:0-19"),
        "diagnostic should identify the span: {}",
        diagnostic(&result)
    );
}

#[test]
fn trailing_comments_are_extracted_without_treating_string_markers_as_comments() {
    for (path, grammar, contents, comment) in [
        (
            "script.sh",
            "shell",
            "value=\"# literal\" # shell note\n",
            "# shell note",
        ),
        (
            "config.toml",
            "toml",
            "value = \"# literal\" # toml note\n",
            "# toml note",
        ),
        (
            "config.yaml",
            "yaml",
            "value: \"# literal\" # yaml note\n",
            "# yaml note",
        ),
        (
            "script.js",
            "javascript",
            "const x = \"// literal\"; // js note\n",
            "// js note",
        ),
        (
            "schema.proto",
            "protobuf",
            "string x = 1; // proto note\n",
            "// proto note",
        ),
    ] {
        let start = contents.find(comment).expect("comment should exist");
        let selector = format!("bytes:{start}-{}", start + comment.len());
        let repository = TestRepository::new();
        repository.track(path, contents.as_bytes());
        repository.manifests(
            &text_manifest(path, 1024),
            &comment_source(path, grammar, &[(&selector, comment)]),
        );
        let result = repository.classify();
        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{grammar}: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn multiple_block_comments_on_one_line_have_distinct_byte_selectors() {
    let contents = "let x = /* first */ 1 + /* second */ 2;\n";
    let repository = TestRepository::new();
    repository.track("script.js", contents.as_bytes());
    repository.manifests(
        &text_manifest("script.js", 1024),
        &comment_source(
            "script.js",
            "javascript",
            &[
                ("bytes:8-19", "/* first */"),
                ("bytes:24-36", "/* second */"),
            ],
        ),
    );
    let result = repository.classify();
    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
}

#[test]
fn a_stale_comment_fingerprint_fails_closed() {
    let repository = TestRepository::new();
    repository.track("script.sh", b"# changedx\necho ok\n");
    repository.manifests(
        &text_manifest("script.sh", 1024),
        &comment_source("script.sh", "shell", &[("bytes:0-10", "# reviewed")]),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "stale selector should fail");
    assert!(
        diagnostic(&result).contains("comment fingerprint mismatch: script.sh:bytes:0-10"),
        "diagnostic should identify stale content: {}",
        diagnostic(&result)
    );
}

#[cfg(unix)]
#[test]
fn tracked_symlink_escape_fails_closed() {
    let repository = TestRepository::new();
    let outside = tempfile::tempdir().expect("should create outside directory");
    fs::write(outside.path().join("outside.txt"), "outside\n")
        .expect("should write outside target");
    symlink(
        outside.path().join("outside.txt"),
        repository.path().join("escape.txt"),
    )
    .expect("should create escaping symlink");
    run_git(repository.path(), &["add", "escape.txt"]);
    repository.manifests(
        &text_manifest("escape.txt", 1024),
        &whole_source("escape.txt"),
    );

    let result = repository.classify();

    assert_eq!(status_code(&result), ERROR, "symlink escape should fail");
    assert!(
        diagnostic(&result).contains("repository path escapes through a symlink"),
        "diagnostic should identify the repository boundary: {}",
        diagnostic(&result)
    );
}

#[test]
fn binary_entries_do_not_use_utf8_sniffing_as_authority() {
    let repository = TestRepository::new();
    repository.track("plain-looking.bin", b"valid UTF-8 is still binary\n");
    repository.manifests(
        &binary_manifest("plain-looking.bin"),
        "version = 1\nreviewed = true\n",
    );

    let result = repository.classify();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "manifest binary authority should pass: {}",
        diagnostic(&result)
    );
}

#[test]
fn update_marks_new_and_moved_comment_spans_for_review() {
    let repository = TestRepository::new();
    repository.track("script.sh", b"\n# reviewed\necho ok\n# new\n");
    repository.manifests(
        &text_manifest("script.sh", 1024),
        &comment_source("script.sh", "shell", &[("bytes:0-10", "# reviewed")]),
    );

    let updated = repository.update_classification();
    let maintained = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/maintained-sources.toml"),
    )
    .expect("should read refreshed maintained manifest");
    let checked = repository.classify();

    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "candidate update should succeed: {}",
        diagnostic(&updated)
    );
    assert!(
        maintained.contains("selector = \"bytes:1-11\"")
            && maintained.contains("selector = \"bytes:20-25\"")
            && maintained.contains("reviewed = false")
            && maintained.matches("disposition = \"include\"").count() == 2,
        "moved and new spans should require review: {maintained}"
    );
    assert_eq!(
        status_code(&checked),
        ERROR,
        "review-required comment candidates should fail check"
    );
}

#[test]
fn update_never_silently_approves_a_sniffed_binary_kind() {
    let repository = TestRepository::new();
    repository.track("unknown.payload", &[0xff, 0xfe, 0xfd]);
    repository.manifests(
        "version = 1\nreviewed = true\nmax_text_bytes = 1024\n",
        "version = 1\nreviewed = true\n",
    );

    let updated = repository.update_classification();
    let tracked = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/tracked-files.toml"),
    )
    .expect("should read refreshed tracked manifest");
    let checked = repository.classify();

    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "candidate update should succeed: {}",
        diagnostic(&updated)
    );
    assert!(
        tracked.contains("path = \"unknown.payload\"")
            && tracked.contains("kind = \"binary\"")
            && tracked.contains("reviewed = false"),
        "sniffed kind should be an explicit unreviewed candidate: {tracked}"
    );
    assert_eq!(
        status_code(&checked),
        ERROR,
        "unreviewed kind should fail check"
    );
    assert!(
        diagnostic(&checked).contains("tracked-file candidates require review"),
        "diagnostic should require manual path classification: {}",
        diagnostic(&checked)
    );
}

#[test]
fn real_maintained_manifest_preserves_reader_facing_sets_and_typed_excludes() {
    let manifest = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("manifests/maintained-sources.toml"),
    )
    .expect("should read real maintained-source manifest");

    for path in [
        ".claude/agents/build-validator.md",
        ".claude/skills/deploying-trusted-server-to-fastly/SKILL.md",
        ".github/pull_request_template.md",
        "README.md",
        "docs/guide/index.md",
    ] {
        let record = source_record(&manifest, path);
        assert!(
            record.contains("mode = \"whole\"") && record.contains("disposition = \"include\""),
            "reader-facing Markdown must be whole-file included: {path}\n{record}"
        );
    }

    for (path, kind) in [
        ("Cargo.lock", "machine_data"),
        ("crates/trusted-server-core/src/lib.rs", "source_code"),
        (
            "crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package.json",
            "test_fixture",
        ),
        (
            "docs/superpowers/specs/2026-08-19-documentation-refresh-design.md",
            "historical",
        ),
    ] {
        let record = source_record(&manifest, path);
        assert!(
            record.contains("disposition = \"exclude\"")
                && record.contains(&format!("exclude_kind = \"{kind}\"")),
            "non-maintained text must carry its precise typed exclusion: {path}\n{record}"
        );
    }

    for (path, grammar) in [
        (
            "crates/trusted-server-openrtb/proto/openrtb.proto",
            "protobuf",
        ),
        ("crates/trusted-server-js/lib/build-all.mjs", "javascript"),
        ("scripts/test-cli.sh", "shell"),
    ] {
        let record = source_record(&manifest, path);
        assert!(
            record.contains("mode = \"comments\"")
                && record.contains(&format!("grammar = \"{grammar}\"")),
            "operational comment surface must use an extractor: {path}\n{record}"
        );
    }
}

fn source_record<'a>(manifest: &'a str, path: &str) -> &'a str {
    let marker = format!("[[sources]]\npath = \"{path}\"\n");
    manifest
        .split_once(&marker)
        .and_then(|(_before, after)| after.split("\n[[sources]]").next())
        .expect("source record should exist")
}
