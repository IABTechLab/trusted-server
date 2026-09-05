use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docs_parity::markdown::{
    GeneratedRegion, GeneratedRow, OwnershipRecord, render_generated_document,
};
use tempfile::TempDir;

const SUCCESS: i32 = 0;
const DRIFT: i32 = 1;
const ERROR: i32 = 2;

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn new(document: &str, target: &str) -> Self {
        let directory = tempfile::tempdir().expect("should create test repository");
        run_git(directory.path(), &["init", "--quiet"]);
        write(directory.path(), target, document);
        let manifest = format!(
            "version = 1\nreviewed = true\nsite_root = \"docs\"\n\
             vitepress_config = \"docs/.vitepress/config.mts\"\n\n\
             [[regions]]\nname = \"adapter-support\"\npath = \"{}\"\n\
             columns = [\"Adapter\", \"Health\"]\n\n\
             [[regions.rows]]\nkey = \"spin\"\ncells = [\"Spin\", \"yes\"]\n\n\
             [[regions.rows]]\nkey = \"axum\"\ncells = [\"Axum\", \"yes\"]\n",
            target
        );
        write(
            directory.path(),
            "tools/docs-parity/manifests/pages.toml",
            &manifest,
        );
        run_git(directory.path(), &["add", "--all"]);
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(binary());
        command.current_dir(self.path());
        command
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_docs-parity"))
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("path should have a parent"))
        .expect("should create parent");
    fs::write(path, contents).expect("should write fixture");
}

fn run_git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .expect("should execute git");
    assert!(status.success(), "git command should succeed");
}

fn output(command: &mut Command) -> Output {
    command.output().expect("should execute docs-parity")
}

fn status_code(output: &Output) -> i32 {
    output.status.code().expect("should exit normally")
}

fn region() -> GeneratedRegion {
    GeneratedRegion {
        name: "adapter-support".to_owned(),
        columns: vec!["Adapter".to_owned(), "Health".to_owned()],
        rows: vec![
            GeneratedRow {
                key: "spin".to_owned(),
                cells: vec!["Spin".to_owned(), "yes".to_owned()],
            },
            GeneratedRow {
                key: "axum".to_owned(),
                cells: vec!["Axum".to_owned(), "yes".to_owned()],
            },
        ],
    }
}

fn render(source: &str, regions: &[GeneratedRegion], ownership: &[OwnershipRecord]) -> String {
    String::from_utf8(
        render_generated_document(source.as_bytes(), regions, ownership)
            .expect("document should render"),
    )
    .expect("rendered document should be UTF-8")
}

fn error(source: &str, regions: &[GeneratedRegion], ownership: &[OwnershipRecord]) -> String {
    format!(
        "{:?}",
        render_generated_document(source.as_bytes(), regions, ownership)
            .expect_err("document should fail")
    )
}

#[test]
fn generated_regions_sort_rows_and_preserve_every_outside_byte() {
    let source = concat!(
        "prefix\r\n",
        "<!-- docs-parity:start adapter-support -->\r\n",
        "hand drift\r\n",
        "<!-- docs-parity:end adapter-support -->\r\n",
        "suffix without newline",
    );

    let rendered = render(source, &[region()], &[]);

    assert!(rendered.starts_with("prefix\r\n<!-- docs-parity:start adapter-support -->\r\n"));
    assert!(
        rendered.ends_with("<!-- docs-parity:end adapter-support -->\r\nsuffix without newline")
    );
    assert!(
        rendered
            .find("| Axum | yes |")
            .expect("Axum row should exist")
            < rendered
                .find("| Spin | yes |")
                .expect("Spin row should exist"),
        "rows should use stable key ordering"
    );
    assert_eq!(
        render(&rendered, &[region()], &[]),
        rendered,
        "a second update should be byte-identical"
    );
}

#[test]
fn generated_marker_grammar_fails_closed() {
    let fixtures = [
        (
            "duplicate",
            "<!-- docs-parity:start adapter-support -->\nold\n<!-- docs-parity:end adapter-support -->\n<!-- docs-parity:start adapter-support -->\nold\n<!-- docs-parity:end adapter-support -->\n",
        ),
        (
            "missing end",
            "<!-- docs-parity:start adapter-support -->\nold\n",
        ),
        (
            "mismatched",
            "<!-- docs-parity:start adapter-support -->\nold\n<!-- docs-parity:end another -->\n",
        ),
        (
            "nested",
            "<!-- docs-parity:start adapter-support -->\n<!-- docs-parity:start another -->\n<!-- docs-parity:end another -->\n<!-- docs-parity:end adapter-support -->\n",
        ),
        (
            "unsafe placement",
            "prefix <!-- docs-parity:start adapter-support -->\nold\n<!-- docs-parity:end adapter-support -->\n",
        ),
    ];

    for (name, source) in fixtures {
        assert!(
            !error(source, &[region()], &[]).is_empty(),
            "{name} should fail"
        );
    }
}

#[test]
fn unknown_marker_and_unknown_record_names_fail_closed() {
    let unknown_marker = concat!(
        "<!-- docs-parity:start unknown -->\n",
        "old\n",
        "<!-- docs-parity:end unknown -->\n",
    );
    assert!(error(unknown_marker, &[region()], &[]).contains("unknown"));

    let known_marker = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let mut unbound = region();
    unbound.name = "unbound-record".to_owned();
    assert!(error(known_marker, &[region(), unbound], &[]).contains("unbound"));
}

#[test]
fn generated_rows_require_unique_keys_and_exact_cell_counts() {
    let source = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let mut duplicate = region();
    duplicate.rows[1].key = duplicate.rows[0].key.clone();
    assert!(error(source, &[duplicate], &[]).contains("duplicate"));

    let mut wrong_width = region();
    wrong_width.rows[0].cells.pop();
    assert!(error(source, &[wrong_width], &[]).contains("cells"));
}

#[test]
fn adjacent_manual_prose_requires_an_exact_owned_marker() {
    let source = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
        "<!-- docs-parity:ownership endpoint-contract owner=docs-team -->\n",
        "Manual endpoint contract.\n",
    );
    let ownership = OwnershipRecord {
        name: "endpoint-contract".to_owned(),
        owner: "docs-team".to_owned(),
    };
    assert_eq!(
        render(source, &[region()], std::slice::from_ref(&ownership))
            .lines()
            .last(),
        Some("Manual endpoint contract.")
    );

    let missing = source.replace(
        "<!-- docs-parity:ownership endpoint-contract owner=docs-team -->\n",
        "",
    );
    assert!(error(&missing, &[region()], std::slice::from_ref(&ownership)).contains("ownership"));

    let wrong_owner = source.replace("owner=docs-team", "owner=other-team");
    assert!(error(&wrong_owner, &[region()], &[ownership]).contains("owner"));
}

#[test]
fn generate_check_never_writes_and_update_is_atomic_and_idempotent() {
    let document = concat!(
        "before\n",
        "<!-- docs-parity:start adapter-support -->\n",
        "hand edit\n",
        "<!-- docs-parity:end adapter-support -->\n",
        "after\n",
    );
    let repository = TestRepository::new(document, "docs/generated.md");
    let target = repository.path().join("docs/generated.md");
    let before = fs::read(&target).expect("should read original");

    let checked = output(repository.command().args(["generate", "--check"]));
    assert_eq!(status_code(&checked), DRIFT, "hand drift should fail check");
    assert_eq!(fs::read(&target).expect("should reread target"), before);

    let updated = output(repository.command().args(["generate", "--update"]));
    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "update should pass: {}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let first = fs::read(&target).expect("should read updated target");
    let clean = output(repository.command().args(["generate", "--check"]));
    let second = output(repository.command().args(["generate", "--update"]));
    assert_eq!(status_code(&clean), SUCCESS, "updated bytes should check");
    assert_eq!(status_code(&second), SUCCESS, "second update should pass");
    assert_eq!(fs::read(&target).expect("should reread target"), first);
}

#[test]
fn interrupted_generated_write_preserves_the_complete_original() {
    let document = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let repository = TestRepository::new(document, "docs/generated.md");
    let target = repository.path().join("docs/generated.md");
    let staged = repository.path().join("docs/.generated.md.docs-parity.tmp");
    fs::write(&staged, "interrupted stage\n").expect("should create stale stage");
    let before = fs::read(&target).expect("should read original");

    let result = output(repository.command().args(["generate", "--update"]));

    assert_eq!(
        status_code(&result),
        ERROR,
        "stale stage should fail closed"
    );
    assert_eq!(fs::read(target).expect("should reread original"), before);
    assert!(
        !staged.exists(),
        "failed atomic update should clean its stale stage"
    );
}

#[cfg(unix)]
#[test]
fn generated_atomic_update_preserves_the_original_safe_mode() {
    let document = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let repository = TestRepository::new(document, "docs/generated.md");
    let target = repository.path().join("docs/generated.md");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644))
        .expect("should set safe original mode");

    let result = output(repository.command().args(["generate", "--update"]));

    assert_eq!(status_code(&result), SUCCESS);
    assert_eq!(
        fs::metadata(target)
            .expect("should inspect updated target")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[cfg(unix)]
#[test]
fn generated_targets_reject_symlinks_unsafe_modes_and_path_escape() {
    let document = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let symlink_repository = TestRepository::new(document, "docs/real.md");
    symlink(
        "real.md",
        symlink_repository.path().join("docs/generated.md"),
    )
    .expect("should create internal symlink");
    let manifest = fs::read_to_string(
        symlink_repository
            .path()
            .join("tools/docs-parity/manifests/pages.toml"),
    )
    .expect("should read pages manifest")
    .replace("docs/real.md", "docs/generated.md");
    fs::write(
        symlink_repository
            .path()
            .join("tools/docs-parity/manifests/pages.toml"),
        manifest,
    )
    .expect("should update pages manifest");
    let symlink_result = output(symlink_repository.command().args(["generate", "--update"]));
    assert_eq!(status_code(&symlink_result), ERROR);

    let unsafe_repository = TestRepository::new(document, "docs/generated.md");
    fs::set_permissions(
        unsafe_repository.path().join("docs/generated.md"),
        fs::Permissions::from_mode(0o666),
    )
    .expect("should make target unsafe");
    let unsafe_result = output(unsafe_repository.command().args(["generate", "--update"]));
    assert_eq!(status_code(&unsafe_result), ERROR);

    let escape_repository = TestRepository::new(document, "docs/generated.md");
    let manifest_path = escape_repository
        .path()
        .join("tools/docs-parity/manifests/pages.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .expect("should read pages manifest")
        .replace("docs/generated.md", "../outside.md");
    fs::write(manifest_path, manifest).expect("should update pages manifest");
    let escape_result = output(escape_repository.command().args(["generate", "--update"]));
    assert_eq!(status_code(&escape_result), ERROR);
}

#[test]
fn generated_target_rejects_oversized_input_before_writing() {
    let document = format!(
        "<!-- docs-parity:start adapter-support -->\n{}<!-- docs-parity:end adapter-support -->\n",
        "x".repeat(4 * 1024 * 1024),
    );
    let repository = TestRepository::new(&document, "docs/generated.md");
    let before = fs::metadata(repository.path().join("docs/generated.md"))
        .expect("should inspect original")
        .len();

    let result = output(repository.command().args(["generate", "--update"]));

    assert_eq!(status_code(&result), ERROR);
    assert_eq!(
        fs::metadata(repository.path().join("docs/generated.md"))
            .expect("should inspect preserved original")
            .len(),
        before
    );
}

#[test]
fn generated_target_rejects_oversized_rendered_output_before_writing() {
    let source = concat!(
        "<!-- docs-parity:start adapter-support -->\n",
        "old\n",
        "<!-- docs-parity:end adapter-support -->\n",
    );
    let mut oversized = region();
    oversized.rows[0].cells[0] = "x".repeat(4 * 1024 * 1024);

    let result = render_generated_document(source.as_bytes(), &[oversized], &[]);

    assert!(
        format!("{:?}", result.expect_err("oversized output should fail"))
            .contains("rendered document exceeds")
    );
}
