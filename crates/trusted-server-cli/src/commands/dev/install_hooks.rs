//! `ts dev install-hooks` — installs the pre-commit hook that runs
//! `ts dev lint domains --staged`.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md
//!
//! The hook is written into git's own hook directory
//! (`<common git dir>/hooks/pre-commit`), never into the working tree,
//! and the command never edits git configuration. Versioned content can
//! therefore never become an executable hook through this tool, and a
//! linked worktree shares the hook with its main checkout the same way
//! it shares every other hook. All git access goes through `gix`; the
//! hook file itself is a tiny shell wrapper (git's hook contract
//! requires an executable artifact) that carries the absolute path of
//! the `ts` binary so it works from GUI git tools that do not inherit
//! the shell `PATH`.

use core::error::Error;
use std::env;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use derive_more::Display;
use error_stack::{Report, ResultExt as _};

use crate::commands::dev::InstallHooksArgs;
use crate::error::CliError;
use crate::output::write_stderr_line;
use crate::output::write_stdout_line;

/// Marker line written into managed hook files. `is_managed` looks
/// for this to decide whether overwriting is safe.
const MANAGED_MARKER: &str = "# ts-install-hooks: managed";

/// Errors raised by `ts dev install-hooks`.
#[derive(Debug, Display)]
pub enum InstallHooksError {
    /// Opening the git repository failed.
    #[display("failed to open git repository")]
    OpenRepo,
    /// The repository has no working directory (bare repo).
    #[display("repository has no working directory")]
    NoWorkdir,
    /// The path of the running executable could not be determined.
    #[display("failed to determine the path of the ts executable")]
    CurrentExe,
    /// Writing the hook file failed.
    #[display("failed to write the pre-commit hook")]
    WriteHook,
    /// An existing, unmanaged pre-commit hook would be overwritten.
    #[display("refusing to overwrite existing hook at `{}`", path.display())]
    WouldClobber {
        /// The existing hook file.
        path: PathBuf,
    },
    /// `core.hooksPath` is set in some config scope, so git would not
    /// run a hook installed into `.git/hooks`.
    #[display(
        "core.hooksPath is set to `{current}`; git runs hooks from there, not from .git/hooks"
    )]
    ForeignHooksPath {
        /// The effective `core.hooksPath` value.
        current: String,
    },
    /// The hook directory is a symlink; writing through it could land
    /// the hook anywhere on the filesystem.
    #[display("refusing to install into symlinked hooks directory `{}`", path.display())]
    SymlinkedHooksDir {
        /// The symlinked directory.
        path: PathBuf,
    },
}

impl Error for InstallHooksError {}

/// POSIX single-quote escaping: wrap in `'...'`, and replace every
/// embedded single quote with `'\''` (close, escaped quote, reopen).
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Render the pre-commit hook script that runs the linter against
/// staged changes. The `ts` path is shell-quoted and absolute.
fn render_hook(ts_path: &Path) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # Installed by `ts dev install-hooks`. DO NOT EDIT.\n\
         {MANAGED_MARKER}\n\
         exec {} dev lint domains --staged\n",
        shell_quote(&ts_path.to_string_lossy()),
    )
}

/// Whether the regular file at `hook_path` is a hook this tool
/// previously installed, detected by the [`MANAGED_MARKER`] line near
/// the top of the file. Non-UTF-8 content is simply not managed.
fn is_managed(hook_path: &Path) -> Result<bool, Report<InstallHooksError>> {
    let bytes = fs::read(hook_path).change_context(InstallHooksError::WriteHook)?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .take(10)
        .any(|line| line.trim() == MANAGED_MARKER))
}

/// Unique sibling temp / backup suffix. Nanosecond precision keeps two
/// installs within the same second from colliding.
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}.{nanos}", std::process::id())
}

/// Write the executable hook `content` to `path` atomically: create a
/// sibling temp file with mode `0755`, fsync it, then rename it over
/// the target. The temp file is removed if any step fails.
fn write_hook(path: &Path, content: &[u8]) -> Result<(), Report<InstallHooksError>> {
    let dir = path
        .parent()
        .ok_or_else(|| Report::new(InstallHooksError::WriteHook))?;
    let tmp = dir.join(format!(".ts-install-hooks.tmp.{}", unique_suffix()));
    let written = write_then_rename(&tmp, path, content);
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.change_context(InstallHooksError::WriteHook)
}

fn write_then_rename(tmp: &Path, path: &Path, content: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;
    file.write_all(content)?;
    file.sync_all()?;
    set_executable(&file)?;
    drop(file);
    fs::rename(tmp, path)
}

/// Mark `file` executable (Unix `0755`; a no-op elsewhere, where git
/// treats every hook file as executable).
#[cfg(unix)]
fn set_executable(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

/// The effective `core.hooksPath`, from any config scope git would
/// consult (system, global, includes, repo, worktree). An empty value
/// is a configured override, not an unset key: git still stops reading
/// `.git/hooks`, so it is reported like any other foreign path.
fn effective_hooks_path(repo: &gix::Repository) -> Option<String> {
    repo.config_snapshot()
        .string("core.hooksPath")
        .map(|value| value.to_string())
}

/// Make sure `dir` is a real directory: create it if absent, refuse a
/// symlink, and fail on any other kind of entry.
fn ensure_hooks_dir(dir: &Path) -> Result<(), Report<InstallHooksError>> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(Report::new(InstallHooksError::SymlinkedHooksDir {
                path: dir.to_path_buf(),
            }))
        }
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(Report::new(InstallHooksError::WriteHook)
            .attach(format!("`{}` exists but is not a directory", dir.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).change_context(InstallHooksError::WriteHook)
        }
        Err(e) => Err(Report::new(InstallHooksError::WriteHook).attach(e.to_string())),
    }
}

/// Decide what to do with whatever already sits at `hook_path`.
///
/// Nothing there: proceed. Under `--force`: rename it to a backup
/// without inspecting it (it may be a symlink, a binary, or non-UTF-8)
/// and return the backup path. Otherwise only a regular file carrying
/// the managed marker may be overwritten; anything else is refused.
fn displace_existing_hook(
    hook_path: &Path,
    force: bool,
) -> Result<Option<PathBuf>, Report<InstallHooksError>> {
    let meta = match fs::symlink_metadata(hook_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Report::new(InstallHooksError::WriteHook).attach(e.to_string()));
        }
    };
    if force {
        let backup = hook_path.with_extension(format!("bak.{}", unique_suffix()));
        fs::rename(hook_path, &backup).change_context(InstallHooksError::WriteHook)?;
        return Ok(Some(backup));
    }
    if meta.is_file() && is_managed(hook_path)? {
        return Ok(None);
    }
    Err(Report::new(InstallHooksError::WouldClobber {
        path: hook_path.to_path_buf(),
    })
    .attach("re-run with --force to replace it (the existing hook is backed up)"))
}

/// Install the pre-commit hook into the repository containing
/// `repo_path`.
///
/// Writes `<common git dir>/hooks/pre-commit`. Refuses to clobber an
/// unmanaged hook unless `force` is set, and refuses outright when
/// `core.hooksPath` is set anywhere, since git would then never run
/// the installed hook.
///
/// # Errors
///
/// Returns [`InstallHooksError`] on any failure; see the variants.
pub fn install_hooks(repo_path: &Path, force: bool) -> Result<(), Report<InstallHooksError>> {
    // `gix::discover` walks upward to the repository root, so the command
    // works from any subdirectory (git invokes hooks at the worktree root,
    // but a developer running `ts dev install-hooks` may be anywhere).
    let repo = gix::discover(repo_path).change_context(InstallHooksError::OpenRepo)?;
    install_into(&repo, force)
}

/// [`install_hooks`] on an already-opened repository.
fn install_into(repo: &gix::Repository, force: bool) -> Result<(), Report<InstallHooksError>> {
    if repo.workdir().is_none() {
        return Err(Report::new(InstallHooksError::NoWorkdir));
    }
    let ts_path = env::current_exe().change_context(InstallHooksError::CurrentExe)?;

    if let Some(current) = effective_hooks_path(repo) {
        return Err(
            Report::new(InstallHooksError::ForeignHooksPath { current }).attach(format!(
                "add `exec {} dev lint domains --staged` to the pre-commit hook in that \
                 directory, or unset core.hooksPath and re-run",
                shell_quote(&ts_path.to_string_lossy())
            )),
        );
    }

    // `common_dir` is the main repository's git dir even from a linked
    // worktree; git reads hooks from there for every worktree.
    let hooks_dir = repo.common_dir().join("hooks");
    ensure_hooks_dir(&hooks_dir)?;
    let hook_path = hooks_dir.join("pre-commit");
    let backup = displace_existing_hook(&hook_path, force)?;
    write_hook(&hook_path, render_hook(&ts_path).as_bytes())?;

    write_stdout_line(format!(
        "Installed: pre-commit hook -> {} (runs {})",
        hook_path.display(),
        ts_path.display(),
    ))
    .change_context(InstallHooksError::WriteHook)?;
    if let Some(backup) = backup {
        write_stderr_line(format!(
            "note: previous pre-commit hook backed up to {}",
            backup.display()
        ))
        .change_context(InstallHooksError::WriteHook)?;
    }
    Ok(())
}

/// `ts dev install-hooks` entry point.
///
/// # Errors
///
/// Returns [`CliError::EnvironmentError`] on any install failure —
/// every install-hooks failure is an environment / configuration
/// issue.
pub fn run(args: &InstallHooksArgs) -> Result<(), Report<CliError>> {
    install_hooks(Path::new("."), args.force).change_context(CliError::EnvironmentError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_plain_path() {
        assert_eq!(shell_quote("/usr/bin/ts"), "'/usr/bin/ts'");
    }

    #[test]
    fn shell_quote_path_with_spaces() {
        assert_eq!(
            shell_quote("/Users/Alice Q/.cargo/bin/ts"),
            "'/Users/Alice Q/.cargo/bin/ts'"
        );
    }

    #[test]
    fn shell_quote_path_with_single_quote() {
        // close, escaped quote, reopen
        assert_eq!(shell_quote("/path/o'brien/ts"), r"'/path/o'\''brien/ts'");
    }

    #[test]
    fn shell_quote_path_with_dollar_backtick_backslash() {
        // $, backtick, backslash are all literal inside single quotes.
        assert_eq!(shell_quote("/opt/$HOME/ts"), "'/opt/$HOME/ts'");
        assert_eq!(shell_quote("/opt/`x`/ts"), "'/opt/`x`/ts'");
        assert_eq!(shell_quote(r"/opt/a\b/ts"), r"'/opt/a\b/ts'");
    }

    #[test]
    fn render_hook_quotes_path_and_carries_marker() {
        let hook = render_hook(Path::new("/Users/Alice Q/.cargo/bin/ts"));
        assert!(
            hook.contains("exec '/Users/Alice Q/.cargo/bin/ts' dev lint domains --staged"),
            "hook should exec the quoted ts path: {hook}"
        );
        assert!(
            hook.lines().any(|l| l == MANAGED_MARKER),
            "hook should carry the managed marker: {hook}"
        );
        assert!(hook.starts_with("#!/usr/bin/env bash\n"));
    }

    #[test]
    fn is_managed_detects_marker() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let managed = temp.path().join("managed");
        fs::write(&managed, render_hook(Path::new("/usr/bin/ts")))
            .expect("should write managed hook");
        assert!(is_managed(&managed).expect("should read managed hook"));

        let foreign = temp.path().join("foreign");
        fs::write(&foreign, "#!/bin/sh\necho hi\n").expect("should write foreign hook");
        assert!(!is_managed(&foreign).expect("should read foreign hook"));

        let binary = temp.path().join("binary");
        fs::write(&binary, [0xff, 0xfe, 0x00, b'\n']).expect("should write binary hook");
        assert!(!is_managed(&binary).expect("non-UTF-8 hook reads as not managed"));
    }

    #[test]
    fn write_hook_writes_executable_and_leaves_no_temp() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let target = temp.path().join("file");
        write_hook(&target, b"hello").expect("should write atomically");
        assert_eq!(
            fs::read(&target).expect("should read written file"),
            b"hello"
        );
        assert!(no_temp_files(temp.path()), "no temp file should remain");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&target)
                .expect("should stat hook")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "hook should be mode 0755");
        }
    }

    #[test]
    fn write_hook_removes_temp_when_rename_fails() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        // Renaming a file over a non-empty directory fails.
        let target = temp.path().join("dir");
        fs::create_dir_all(target.join("child")).expect("should create blocking dir");
        assert!(
            write_hook(&target, b"hello").is_err(),
            "rename over a non-empty directory should fail"
        );
        assert!(no_temp_files(temp.path()), "temp file should be cleaned up");
    }

    pub(super) fn no_temp_files(dir: &Path) -> bool {
        !fs::read_dir(dir)
            .expect("should read dir")
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".ts-install-hooks.tmp.")
            })
    }
}

#[cfg(test)]
mod install_hooks_tests {
    use super::tests::no_temp_files;
    use super::*;
    use crate::commands::dev::lint::test_support;

    fn hook_path(repo: &gix::Repository) -> PathBuf {
        repo.common_dir().join("hooks").join("pre-commit")
    }

    fn backups(repo: &gix::Repository) -> Vec<PathBuf> {
        fs::read_dir(repo.common_dir().join("hooks"))
            .expect("should read hooks dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("pre-commit.bak."))
            })
            .collect()
    }

    fn write_unmanaged_hook(path: &Path) {
        fs::write(path, "#!/bin/sh\necho custom\n").expect("should write unmanaged hook");
    }

    #[test]
    fn fresh_repo_installs_into_git_hooks_without_touching_config_or_worktree() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        let config_path = repo.git_dir().join("config");
        let config_before = fs::read(&config_path).expect("should read config");

        install_hooks(temp.path(), false).expect("should install into a fresh repo");

        let content = fs::read_to_string(hook_path(&repo)).expect("should read hook");
        assert!(
            content.contains(MANAGED_MARKER),
            "hook should carry the marker"
        );
        assert!(
            content.contains("dev lint domains --staged"),
            "hook should exec the linter"
        );
        assert_eq!(
            fs::read(&config_path).expect("should re-read config"),
            config_before,
            ".git/config must be byte-identical after install"
        );
        assert!(
            !temp.path().join(".githooks").exists(),
            "nothing may be written into the working tree"
        );
        let reopened = gix::open(temp.path()).expect("should reopen repo");
        assert_eq!(
            effective_hooks_path(&reopened),
            None,
            "install must never set core.hooksPath"
        );
    }

    #[test]
    fn re_running_is_idempotent() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());

        install_hooks(temp.path(), false).expect("first install should succeed");
        install_hooks(temp.path(), false).expect("re-install should be idempotent");
        assert!(
            backups(&repo).is_empty(),
            "managed re-install makes no backup"
        );
        assert!(no_temp_files(&repo.common_dir().join("hooks")));
    }

    #[test]
    fn refuses_to_clobber_unmanaged_hook() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        fs::create_dir_all(repo.common_dir().join("hooks")).expect("should create hooks dir");
        write_unmanaged_hook(&hook_path(&repo));

        let err = install_hooks(temp.path(), false)
            .expect_err("should refuse to clobber an unmanaged hook");
        assert!(
            matches!(
                err.current_context(),
                InstallHooksError::WouldClobber { .. }
            ),
            "should be WouldClobber: {err:?}"
        );
        assert_eq!(
            fs::read_to_string(hook_path(&repo)).expect("should read hook"),
            "#!/bin/sh\necho custom\n",
            "refused install leaves the existing hook untouched"
        );
    }

    #[test]
    fn force_backs_up_unmanaged_hook() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        fs::create_dir_all(repo.common_dir().join("hooks")).expect("should create hooks dir");
        write_unmanaged_hook(&hook_path(&repo));

        install_hooks(temp.path(), true).expect("force should overwrite");

        let content = fs::read_to_string(hook_path(&repo)).expect("should read new hook");
        assert!(content.contains(MANAGED_MARKER));
        let backups = backups(&repo);
        assert_eq!(backups.len(), 1, "the displaced hook should be backed up");
        assert_eq!(
            fs::read_to_string(&backups[0]).expect("should read backup"),
            "#!/bin/sh\necho custom\n"
        );
    }

    /// `--force` must not read the existing hook: a non-UTF-8 binary
    /// hook is backed up byte-for-byte, not parsed.
    #[test]
    fn force_backs_up_non_utf8_hook_without_parsing_it() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        fs::create_dir_all(repo.common_dir().join("hooks")).expect("should create hooks dir");
        let binary = [0x7f, b'E', b'L', b'F', 0xff, 0xfe, 0x00];
        fs::write(hook_path(&repo), binary).expect("should write binary hook");

        let err = install_hooks(temp.path(), false)
            .expect_err("a non-UTF-8 hook is unmanaged and refused without --force");
        assert!(matches!(
            err.current_context(),
            InstallHooksError::WouldClobber { .. }
        ));

        install_hooks(temp.path(), true).expect("force should back up and replace");
        let backups = backups(&repo);
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read(&backups[0]).expect("should read backup"),
            binary,
            "backup must be byte-identical"
        );
    }

    /// A dangling symlink is still "something there": refused without
    /// `--force`, moved aside (as a symlink) with it.
    #[cfg(unix)]
    #[test]
    fn broken_symlink_hook_is_refused_then_backed_up_under_force() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        fs::create_dir_all(repo.common_dir().join("hooks")).expect("should create hooks dir");
        std::os::unix::fs::symlink(temp.path().join("missing"), hook_path(&repo))
            .expect("should create dangling symlink");

        let err = install_hooks(temp.path(), false).expect_err("dangling symlink is refused");
        assert!(matches!(
            err.current_context(),
            InstallHooksError::WouldClobber { .. }
        ));

        install_hooks(temp.path(), true).expect("force should displace the symlink");
        assert!(
            hook_path(&repo).is_file(),
            "a regular managed hook replaces the symlink"
        );
        let backups = backups(&repo);
        assert_eq!(backups.len(), 1);
        assert!(
            fs::symlink_metadata(&backups[0])
                .expect("should stat backup")
                .file_type()
                .is_symlink(),
            "the backup is the moved symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_hooks_directory() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        let elsewhere = tempfile::tempdir().expect("should create link target");
        let hooks_dir = repo.common_dir().join("hooks");
        if hooks_dir.exists() {
            fs::remove_dir_all(&hooks_dir).expect("should remove default hooks dir");
        }
        std::os::unix::fs::symlink(elsewhere.path(), &hooks_dir).expect("should symlink hooks dir");

        let err = install_hooks(temp.path(), false).expect_err("symlinked hooks dir is refused");
        assert!(
            matches!(
                err.current_context(),
                InstallHooksError::SymlinkedHooksDir { .. }
            ),
            "should be SymlinkedHooksDir: {err:?}"
        );
        assert!(
            !elsewhere.path().join("pre-commit").exists(),
            "nothing may be written through the symlink"
        );
    }

    #[test]
    fn refuses_when_repo_config_sets_hooks_path() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        let config_path = repo.git_dir().join("config");
        let mut config = fs::read_to_string(&config_path).expect("should read config");
        config.push_str("[core]\n\thooksPath = .husky\n");
        fs::write(&config_path, config).expect("should write config");

        let err = install_hooks(temp.path(), false).expect_err("foreign core.hooksPath is refused");
        match err.current_context() {
            InstallHooksError::ForeignHooksPath { current } => assert_eq!(current, ".husky"),
            other => panic!("should be ForeignHooksPath, got {other:?}"),
        }
        assert!(!hook_path(&repo).exists(), "refused install writes nothing");
    }

    /// An explicitly empty `core.hooksPath` is a configured override,
    /// not an unset key: git stops reading `.git/hooks` entirely, so
    /// installing there would claim success while doing nothing.
    #[test]
    fn refuses_when_hooks_path_is_explicitly_empty() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        let mut config =
            fs::read_to_string(temp.path().join(".git/config")).expect("should read repo config");
        config.push_str("[core]\n\thooksPath = \n");
        fs::write(temp.path().join(".git/config"), config).expect("should write repo config");

        let err = install_hooks(temp.path(), false).expect_err("empty core.hooksPath is refused");
        match err.current_context() {
            InstallHooksError::ForeignHooksPath { current } => assert_eq!(current, ""),
            other => panic!("should be ForeignHooksPath, got {other:?}"),
        }
        assert!(!hook_path(&repo).exists(), "refused install writes nothing");

        let err = install_hooks(temp.path(), true).expect_err("--force does not override config");
        assert!(matches!(
            err.current_context(),
            InstallHooksError::ForeignHooksPath { .. }
        ));
    }

    /// `core.hooksPath` from outside the repo config (global or
    /// included) is just as effective; the preflight must see it too.
    #[test]
    fn refuses_when_hooks_path_comes_from_another_scope() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());
        let repo = gix::open_opts(
            temp.path(),
            gix::open::Options::isolated().config_overrides(["core.hooksPath=/tmp/globalhooks"]),
        )
        .expect("should open with overrides");

        let err = install_into(&repo, false).expect_err("global core.hooksPath is refused");
        match err.current_context() {
            InstallHooksError::ForeignHooksPath { current } => {
                assert_eq!(current, "/tmp/globalhooks");
            }
            other => panic!("should be ForeignHooksPath, got {other:?}"),
        }
        let err = install_into(&repo, true).expect_err("--force does not override config");
        assert!(matches!(
            err.current_context(),
            InstallHooksError::ForeignHooksPath { .. }
        ));
    }
}
