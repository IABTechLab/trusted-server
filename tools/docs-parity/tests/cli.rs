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

fn executable_on_path(name: &str) -> PathBuf {
    env::split_paths(&env::var_os("PATH").expect("PATH should be available"))
        .map(|directory| directory.join(name))
        .find_map(|candidate| fs::canonicalize(candidate).ok())
        .filter(|candidate| candidate.is_file())
        .expect("requested executable should resolve to a regular file")
}

fn node_executable() -> PathBuf {
    let result = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .expect("node should report its executable path");
    assert!(
        result.status.success(),
        "node executable lookup should pass"
    );
    let path = String::from_utf8(result.stdout).expect("node path should be UTF-8");
    fs::canonicalize(path.trim()).expect("node executable path should be canonicalizable")
}

fn git_status(repository: &Path) -> Vec<u8> {
    let result = Command::new(executable_on_path("git"))
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(repository)
        .output()
        .expect("git status should execute");
    assert!(result.status.success(), "git status should pass");
    result.stdout
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
fn expiry_rejects_invalid_calendar_and_clock_components() {
    assert!(
        Expiry::parse("2024-02-29T23:59:59Z").is_ok(),
        "leap day should be valid in a leap year"
    );
    for invalid in [
        "0000-01-01T00:00:00Z",
        "2024-00-01T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-00T00:00:00Z",
        "2024-04-31T00:00:00Z",
        "2023-02-29T00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:60:00Z",
        "2024-01-01T00:00:60Z",
    ] {
        assert!(
            Expiry::parse(invalid).is_err(),
            "invalid expiry should fail: {invalid}"
        );
    }
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
fn cli_help_command_tree_is_constructible() {
    let help = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .args(["cli-help", "--help"]),
    );

    assert_eq!(
        status_code(&help),
        SUCCESS,
        "the CLI-help subcommand must not panic while Clap builds its argument tree: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    let stdout = String::from_utf8(help.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("--check"));
    assert!(stdout.contains("capture"));
    assert!(stdout.contains("import-hosted"));
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
fn check_all_conflicts_with_single_record_mode() {
    let conflict = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .args(["check", "--all", "--tracked-paths-record", "record.txt"]),
    );
    assert_eq!(
        status_code(&conflict),
        ERROR,
        "aggregate and record modes must conflict"
    );
}

#[test]
fn capture_and_artifact_modes_are_not_reachable_from_check_all_syntax() {
    let help = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .args(["check", "--help"]),
    );
    let stdout = String::from_utf8(help.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("--all"));
    for forbidden in [
        "capture",
        "import-hosted",
        "--artifact",
        "dependency-snapshot",
    ] {
        assert!(
            !stdout.contains(forbidden),
            "offline check help must not expose {forbidden}"
        );
    }
}

#[test]
fn pages_command_tree_is_constructible() {
    let help = output(
        command_in(
            env::current_dir()
                .expect("should read current directory")
                .as_path(),
        )
        .args(["pages", "--help"]),
    );

    assert_eq!(status_code(&help), SUCCESS);
    assert!(String::from_utf8_lossy(&help.stdout).contains("--check"));
}

#[cfg(unix)]
#[test]
fn check_all_executes_clean_with_external_sentinel_and_without_repository_writes() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tool manifest should be nested under the repository");
    let restricted = tempfile::tempdir().expect("should create restricted PATH");
    let sentinel = restricted.path().join("external-transport-constructed");
    symlink(executable_on_path("git"), restricted.path().join("git"))
        .expect("should expose only Git to repository checks");
    symlink(node_executable(), restricted.path().join("node"))
        .expect("should expose only Node syntax validation");
    let before = git_status(repository);

    let checked = output(
        command_in(repository)
            .env_clear()
            .env("PATH", restricted.path())
            .env("TMPDIR", restricted.path())
            .env("DOCS_PARITY_TEST_EXTERNAL_SENTINEL", &sentinel)
            .args(["check", "--all"]),
    );

    assert_eq!(
        status_code(&checked),
        SUCCESS,
        "the offline aggregate must pass without cargo, gh, capture, import, issue, submission, or production external-transport execution: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        !sentinel.exists(),
        "the offline aggregate must not construct the production external transport"
    );
    assert_eq!(
        git_status(repository),
        before,
        "the offline aggregate must preserve every repository status byte"
    );
}

#[test]
fn explicit_external_check_trips_the_production_transport_sentinel() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tool manifest should be nested under the repository");
    let directory = tempfile::tempdir().expect("should create sentinel directory");
    let sentinel = directory.path().join("external-transport-constructed");

    let checked = output(
        command_in(repository)
            .env("DOCS_PARITY_TEST_EXTERNAL_SENTINEL", &sentinel)
            .args(["links", "--external", "--check"]),
    );

    assert_eq!(
        status_code(&checked),
        ERROR,
        "an active external sentinel must fail before network execution"
    );
    assert_eq!(
        fs::read(&sentinel).expect("external check should write its construction sentinel"),
        b"production external transport constructed\n"
    );
    assert!(
        String::from_utf8(checked.stderr)
            .expect("diagnostic should be UTF-8")
            .contains("production external transport forbidden by active test sentinel"),
        "the failure must identify the blocked production transport"
    );
}

#[test]
fn check_all_propagates_operational_errors_without_writing() {
    let repository = TestRepository::new(&["source.txt"]);
    let before = git_status(repository.path());

    let checked = output(repository.command().args(["check", "--all"]));

    assert_eq!(
        status_code(&checked),
        ERROR,
        "a materialized repository-check error must propagate"
    );
    assert_eq!(
        git_status(repository.path()),
        before,
        "a failed aggregate must not alter repository state"
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
fn repository_root_preserves_trailing_whitespace() {
    let parent = tempfile::tempdir().expect("should create repository parent");
    for root_name in ["repository ", "repository\r"] {
        let repository = parent.path().join(root_name);
        let nested = repository.join("nested");
        fs::create_dir_all(&nested).expect("should create trailing-whitespace repository");
        fs::write(repository.join("source.txt"), "source\n").expect("should write tracked file");
        run_git(&repository, &["init", "--quiet"]);
        run_git(&repository, &["add", "--all"]);

        let result = output(command_in(&nested).args([
            "update",
            "--tracked-paths-record",
            "tracked-paths.txt",
        ]));

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "Git root with trailing whitespace should be preserved: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            repository.join("tracked-paths.txt").is_file(),
            "record should be written inside the exact trailing-whitespace root"
        );
    }
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
fn git_admin_paths_are_rejected_without_writes() {
    let check_repository = TestRepository::new(&["source.txt"]);
    let update_repository = TestRepository::new(&["source.txt"]);
    let check_config = check_repository.path().join(".git/config");
    let update_config = update_repository.path().join(".git/config");
    let check_before = fs::read(&check_config).expect("should read check Git config");
    let update_before = fs::read(&update_config).expect("should read update Git config");

    let checked =
        output(
            check_repository
                .command()
                .args(["check", "--tracked-paths-record", ".git/config"]),
        );
    let updated = output(update_repository.command().args([
        "update",
        "--tracked-paths-record",
        ".git/config",
    ]));

    assert_eq!(
        status_code(&checked),
        ERROR,
        "check should reject the Git administrative directory"
    );
    assert_eq!(
        status_code(&updated),
        ERROR,
        "update should reject the Git administrative directory"
    );
    assert_eq!(
        fs::read(&check_config).expect("should reread check Git config"),
        check_before,
        "check should preserve exact Git config bytes"
    );
    assert_eq!(
        fs::read(&update_config).expect("should reread update Git config"),
        update_before,
        "update should preserve exact Git config bytes"
    );
}

#[test]
fn portable_ambiguous_paths_are_rejected() {
    let repository = TestRepository::new(&["source.txt"]);
    let git_config = repository.path().join(".git/config");
    let git_config_before = fs::read(&git_config).expect("should read Git config");
    let mut unsafe_paths = vec![
        "C:outside.txt".to_owned(),
        "directory\\outside.txt".to_owned(),
        "line\nbreak.txt".to_owned(),
        "control\u{001f}.txt".to_owned(),
        "record.txt:stream".to_owned(),
        "record<copy>.txt".to_owned(),
        "record>copy.txt".to_owned(),
        "record\"copy.txt".to_owned(),
        "record|copy.txt".to_owned(),
        "record?copy.txt".to_owned(),
        "record*copy.txt".to_owned(),
        "record.".to_owned(),
        "record ".to_owned(),
        ".GiT/config".to_owned(),
        "nested/.GIT/record.txt".to_owned(),
        "COM¹.txt".to_owned(),
        "com²".to_owned(),
        "CoM³.log".to_owned(),
        "LPT¹".to_owned(),
        "lpt².txt".to_owned(),
        "lpt³.log".to_owned(),
    ];
    for device in [
        "CON", "PRN", "AUX", "NUL", "CLOCK$", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
        "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
        "LPT9",
    ] {
        unsafe_paths.push(device.to_owned());
        unsafe_paths.push(format!("{}.txt", device.to_ascii_lowercase()));
    }

    for unsafe_path in unsafe_paths {
        let result =
            output(
                repository
                    .command()
                    .args(["update", "--tracked-paths-record", &unsafe_path]),
            );

        assert_eq!(
            status_code(&result),
            ERROR,
            "portable unsafe path should fail: {unsafe_path}"
        );
        if !unsafe_path.to_ascii_lowercase().contains(".git/") {
            assert!(
                !repository.path().join(&unsafe_path).exists(),
                "portable unsafe path should not be written: {unsafe_path}"
            );
        }
    }
    assert_eq!(
        fs::read(&git_config).expect("should reread Git config"),
        git_config_before,
        "case-insensitive Git administrative paths should preserve config bytes"
    );
}

#[test]
fn portable_regular_lookalikes_are_allowed() {
    let repository = TestRepository::new(&["source.txt"]);

    for record in [
        ".gitignore",
        ".gitmodules",
        "COM10.txt",
        "COM¹extra.txt",
        "lpt³more.log",
    ] {
        let result =
            output(
                repository
                    .command()
                    .args(["update", "--tracked-paths-record", record]),
            );

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "non-reserved portable file should be allowed: {record}"
        );
    }
}

#[test]
fn tracked_path_update_never_deletes_an_unknown_peer_stage() {
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
        SUCCESS,
        "an unrelated stage name must not block the owned update: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(&staged).expect("should retain unknown peer stage"),
        "interrupted partial record\n"
    );
    assert_ne!(
        fs::read_to_string(&record).expect("should read updated record"),
        "previous complete record\n",
        "the owned unique stage should replace the target"
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
