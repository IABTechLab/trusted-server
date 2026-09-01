use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docs_parity::model::{Expiry, Governance, Owner, Rationale};
use tempfile::TempDir;

const SUCCESS: i32 = 0;
const DRIFT: i32 = 1;
const ERROR: i32 = 2;

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn new(paths: &[&str]) -> Self {
        let directory = tempfile::tempdir().expect("should create test repository directory");
        run_git(directory.path(), &["init", "--quiet"]);

        for path in paths {
            let absolute = directory.path().join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).expect("should create tracked file parent");
            }
            fs::write(&absolute, format!("contents for {path}\n"))
                .expect("should write tracked file");
        }

        run_git(directory.path(), &["add", "--all"]);
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn command(&self) -> Command {
        command_in(self.path())
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_docs-parity"))
}

fn command_in(directory: &Path) -> Command {
    let mut command = Command::new(binary());
    command.current_dir(directory);
    command
}

fn output(command: &mut Command) -> Output {
    command.output().expect("should execute docs-parity")
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

#[test]
fn governance_requires_typed_owner_rationale_and_expiry() {
    let owner = Owner::new("documentation-maintainers").expect("owner should be valid");
    let rationale = Rationale::new("Bounded exception for a checked fixture")
        .expect("rationale should be valid");
    let expiry = Expiry::parse("2027-01-02T03:04:05Z").expect("expiry should be valid");

    let governance = Governance::new(owner, rationale, expiry);

    assert_eq!(governance.owner().as_str(), "documentation-maintainers");
    assert_eq!(
        governance.rationale().as_str(),
        "Bounded exception for a checked fixture"
    );
    assert_eq!(governance.expiry().as_str(), "2027-01-02T03:04:05Z");
    assert!(Owner::new(" ").is_err(), "blank owner should fail");
    assert!(Rationale::new("").is_err(), "blank rationale should fail");
    assert!(
        Expiry::parse("2027-01-02").is_err(),
        "date-only expiry should fail"
    );
}

#[test]
fn help_is_deterministic() {
    let first = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .arg("--help"),
    );
    let second = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .arg("--help"),
    );

    assert_eq!(status_code(&first), SUCCESS, "help should succeed");
    assert_eq!(first.stdout, second.stdout, "help should be byte-stable");
    assert!(
        String::from_utf8(first.stdout)
            .expect("help should be UTF-8")
            .contains("check"),
        "help should list the check subcommand"
    );
}

#[test]
fn unknown_subcommand_uses_the_cli_error_exit_code() {
    let result = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .arg("unknown"),
    );

    assert_eq!(
        status_code(&result),
        ERROR,
        "unknown subcommand should fail"
    );
    assert!(
        String::from_utf8(result.stderr)
            .expect("diagnostic should be UTF-8")
            .contains("unrecognized subcommand"),
        "diagnostic should identify the unknown subcommand"
    );
}

#[test]
fn repository_root_is_discovered_from_a_nested_directory() {
    let repository = TestRepository::new(&["nested/deeper/source.txt"]);
    let nested = repository.path().join("nested/deeper");

    let updated = output(command_in(&nested).args([
        "update",
        "--tracked-paths-record",
        "generated/tracked-paths.txt",
    ]));
    let checked = output(command_in(&nested).args([
        "check",
        "--tracked-paths-record",
        "generated/tracked-paths.txt",
    ]));

    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "update should find the repository root"
    );
    assert_eq!(
        status_code(&checked),
        SUCCESS,
        "check should find the repository root"
    );
    assert!(
        repository
            .path()
            .join("generated/tracked-paths.txt")
            .is_file(),
        "record should be rooted at the repository, not the nested directory"
    );
}

#[test]
fn check_reports_drift_without_writing_and_update_repairs_it() {
    let repository = TestRepository::new(&["source.txt"]);
    let record = repository.path().join("tracked-paths.txt");

    let missing =
        output(
            repository
                .command()
                .args(["check", "--tracked-paths-record", "tracked-paths.txt"]),
        );
    assert_eq!(
        status_code(&missing),
        DRIFT,
        "missing record should be drift"
    );
    assert!(
        !record.exists(),
        "check mode should not create a missing record"
    );

    let updated = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "tracked-paths.txt",
    ]));
    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "update should write the record"
    );

    fs::write(&record, "hand edited\n").expect("should alter generated record");
    let before = fs::read(&record).expect("should read altered record");
    let drift =
        output(
            repository
                .command()
                .args(["check", "--tracked-paths-record", "tracked-paths.txt"]),
        );

    assert_eq!(status_code(&drift), DRIFT, "stale record should be drift");
    assert_eq!(
        fs::read(&record).expect("should reread altered record"),
        before,
        "check mode should not rewrite drift"
    );
}

#[test]
fn absolute_paths_outside_the_repository_are_rejected() {
    let repository = TestRepository::new(&["source.txt"]);
    let outside = tempfile::tempdir().expect("should create outside directory");
    let outside_record = outside.path().join("tracked-paths.txt");

    let result = output(
        repository.command().args([
            "update",
            "--tracked-paths-record",
            outside_record
                .to_str()
                .expect("outside path should be UTF-8"),
        ]),
    );

    assert_eq!(status_code(&result), ERROR, "outside path should fail");
    assert!(
        !outside_record.exists(),
        "outside path should not be written"
    );
}

#[test]
fn unsafe_relative_paths_are_rejected() {
    let repository = TestRepository::new(&["source.txt"]);
    let outside_record = repository
        .path()
        .parent()
        .expect("repository should have a parent")
        .join("outside.txt");

    let result =
        output(
            repository
                .command()
                .args(["update", "--tracked-paths-record", "../outside.txt"]),
        );

    assert_eq!(status_code(&result), ERROR, "parent traversal should fail");
    assert!(
        !outside_record.exists(),
        "parent traversal should not be written"
    );
}

#[test]
fn portable_drive_relative_and_backslash_paths_are_rejected() {
    let repository = TestRepository::new(&["source.txt"]);

    for unsafe_path in ["C:outside.txt", "directory\\outside.txt"] {
        let result =
            output(
                repository
                    .command()
                    .args(["update", "--tracked-paths-record", unsafe_path]),
            );

        assert_eq!(
            status_code(&result),
            ERROR,
            "portable unsafe path should fail: {unsafe_path}"
        );
        assert!(
            !repository.path().join(unsafe_path).exists(),
            "portable unsafe path should not be written: {unsafe_path}"
        );
    }
}

#[test]
fn interrupted_atomic_update_preserves_the_existing_record() {
    let repository = TestRepository::new(&["source.txt"]);
    let record = repository.path().join("tracked-paths.txt");
    let staged = repository.path().join(".tracked-paths.txt.docs-parity.tmp");
    fs::write(&record, "previous complete record\n").expect("should write existing record");
    fs::write(&staged, "interrupted partial record\n").expect("should stage interrupted write");

    let result = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "tracked-paths.txt",
    ]));

    assert_eq!(
        status_code(&result),
        ERROR,
        "stale atomic stage should fail closed"
    );
    assert_eq!(
        fs::read_to_string(&record).expect("should read existing record"),
        "previous complete record\n",
        "failed atomic update should preserve the previous complete record"
    );
}

#[test]
fn tracked_paths_are_written_in_stable_order() {
    let repository = TestRepository::new(&["z-last.txt", "middle/value.txt", "a-first.txt"]);
    let record = repository.path().join("tracked-paths.txt");

    let first = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "tracked-paths.txt",
    ]));
    let first_bytes = fs::read(&record).expect("should read first record");
    let second = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "tracked-paths.txt",
    ]));

    assert_eq!(status_code(&first), SUCCESS, "first update should succeed");
    assert_eq!(
        status_code(&second),
        SUCCESS,
        "second update should succeed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        fs::read(&record).expect("should read second record"),
        first_bytes,
        "second update should be byte-stable"
    );
    assert_eq!(
        String::from_utf8(first_bytes).expect("record should be UTF-8"),
        "a-first.txt\nmiddle/value.txt\nz-last.txt\n",
        "tracked paths should use lexical ordering"
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected_at_the_repository_boundary() {
    let repository = TestRepository::new(&["source.txt"]);
    let outside = tempfile::tempdir().expect("should create outside directory");
    symlink(outside.path(), repository.path().join("escape"))
        .expect("should create escaping symlink");

    let result = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "escape/tracked-paths.txt",
    ]));

    assert_eq!(status_code(&result), ERROR, "symlink escape should fail");
    assert!(
        !outside.path().join("tracked-paths.txt").exists(),
        "symlink escape should not be written"
    );
}

#[cfg(unix)]
#[test]
fn dangling_output_symlink_is_rejected_without_replacement() {
    let repository = TestRepository::new(&["source.txt"]);
    let update_record = repository.path().join("update-record.txt");
    let check_record = repository.path().join("check-record.txt");
    let missing_target = Path::new("missing-target.txt");
    symlink(missing_target, &update_record).expect("should create update symlink");
    symlink(missing_target, &check_record).expect("should create check symlink");

    let updated = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "update-record.txt",
    ]));
    let checked =
        output(
            repository
                .command()
                .args(["check", "--tracked-paths-record", "check-record.txt"]),
        );

    assert_eq!(
        status_code(&updated),
        ERROR,
        "update should reject a dangling final symlink"
    );
    assert_eq!(
        status_code(&checked),
        ERROR,
        "check should reject a dangling final symlink"
    );
    for record in [&update_record, &check_record] {
        assert!(
            fs::symlink_metadata(record)
                .expect("should inspect final entry")
                .file_type()
                .is_symlink(),
            "final entry should remain a symlink"
        );
        assert_eq!(
            fs::read_link(record).expect("should read final symlink"),
            missing_target,
            "final symlink target should remain unchanged"
        );
    }
    assert!(
        !repository.path().join(missing_target).exists(),
        "dangling target should not be created"
    );
}

#[cfg(unix)]
#[test]
fn tracked_symlink_to_an_internal_regular_file_is_allowed() {
    let repository = TestRepository::new(&["target.txt"]);
    symlink("target.txt", repository.path().join("link.txt"))
        .expect("should create internal symlink");
    run_git(repository.path(), &["add", "link.txt"]);

    let result = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "tracked-paths.txt",
    ]));

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "internal tracked symlink should be allowed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(unix)]
#[test]
fn unsafe_intermediate_parent_mode_is_rejected_for_tracked_and_output_paths() {
    let repository = TestRepository::new(&["unsafe/safe/source.txt"]);
    let unsafe_parent = repository.path().join("unsafe");
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777))
        .expect("should make intermediate parent unsafe");

    let checked = output(repository.command().arg("check"));
    let updated = output(repository.command().args([
        "update",
        "--tracked-paths-record",
        "unsafe/safe/tracked-paths.txt",
    ]));

    assert_eq!(
        status_code(&checked),
        ERROR,
        "unsafe tracked parent should fail"
    );
    assert_eq!(
        status_code(&updated),
        ERROR,
        "unsafe output parent should fail"
    );
    assert!(
        !repository
            .path()
            .join("unsafe/safe/tracked-paths.txt")
            .exists(),
        "unsafe output parent should not be written"
    );
}

#[cfg(unix)]
#[test]
fn unsafe_tracked_file_mode_is_rejected() {
    let repository = TestRepository::new(&["unsafe.txt"]);
    let path = repository.path().join("unsafe.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666))
        .expect("should make tracked file unsafe");

    let result = output(repository.command().arg("check"));

    assert_eq!(
        status_code(&result),
        ERROR,
        "world-writable tracked file should fail"
    );
}
