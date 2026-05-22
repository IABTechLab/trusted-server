//! `ts dev install-hooks` — installs the pre-commit hook that runs
//! `ts dev lint domains --staged`.
//!
//! Design: docs/superpowers/specs/2026-05-18-check-domains-design.md
//!
//! All git operations go through `gix` / `gix-config` — no
//! subprocess. The hook file itself is a tiny shell wrapper (git's
//! hook contract requires an executable artifact); it carries the
//! absolute path of the `ts` binary so it works from GUI git tools
//! that do not inherit the shell `PATH`.

use core::error::Error;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use derive_more::Display;
use error_stack::{Report, ResultExt as _};
use gix::bstr::BStr;
use gix_config::File as GixConfigFile;

use crate::dev::InstallHooksArgs;
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
    /// Writing the git config failed.
    #[display("failed to write git config")]
    ConfigWrite,
    /// An existing, unmanaged pre-commit hook would be overwritten.
    #[display("refusing to overwrite existing hook at `{}`", path.display())]
    WouldClobber {
        /// The existing hook file.
        path: PathBuf,
    },
    /// `core.hooksPath` is already set to a foreign value.
    #[display("refusing to override existing core.hooksPath `{current}` (would set `{proposed}`)")]
    ForeignHooksPath {
        /// The current `core.hooksPath` value.
        current: String,
        /// The value `install-hooks` would set.
        proposed: String,
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

/// Whether `hook_path` is a hook this tool previously installed —
/// detected by the [`MANAGED_MARKER`] line near the top of the file.
fn is_managed(hook_path: &Path) -> Result<bool, Report<InstallHooksError>> {
    let content = match fs::read_to_string(hook_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(Report::new(InstallHooksError::WriteHook).attach(e.to_string()));
        }
    };
    Ok(content
        .lines()
        .take(10)
        .any(|line| line.trim() == MANAGED_MARKER))
}

/// Write `content` to `path` atomically: write a sibling temp file,
/// then rename it over the target (atomic on the same filesystem).
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), Report<InstallHooksError>> {
    let dir = path
        .parent()
        .ok_or_else(|| Report::new(InstallHooksError::WriteHook))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".ts-install-hooks.tmp.{}.{nanos}", std::process::id()));
    fs::write(&tmp, content).change_context(InstallHooksError::WriteHook)?;
    fs::rename(&tmp, path).change_context(InstallHooksError::WriteHook)?;
    Ok(())
}

/// Read a single dotted-key value from the local repo config.
/// Returns `Ok(None)` if the config file or key is absent.
fn read_local_config_value(
    repo: &gix::Repository,
    dotted_key: &str,
) -> Result<Option<String>, Report<InstallHooksError>> {
    let config_path = repo.git_dir().join("config");
    let file = match GixConfigFile::from_path_no_includes(config_path, gix_config::Source::Local) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    Ok(file
        .raw_value(dotted_key)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

/// Set a dotted-key value in the local repo config, writing the file
/// back atomically.
fn set_local_config_value(
    repo: &gix::Repository,
    dotted_key: &str,
    value: &str,
) -> Result<(), Report<InstallHooksError>> {
    let config_path = repo.git_dir().join("config");
    let mut file =
        match GixConfigFile::from_path_no_includes(config_path.clone(), gix_config::Source::Local) {
            Ok(f) => f,
            Err(_) => GixConfigFile::new(gix_config::file::Metadata::from(gix_config::Source::Local)),
        };
    let value_bstr: &BStr = value.into();
    file.set_raw_value(dotted_key, value_bstr)
        .change_context(InstallHooksError::ConfigWrite)?;
    let serialized = file.to_bstring();
    write_atomic(&config_path, serialized.as_slice()).change_context(InstallHooksError::ConfigWrite)
}

/// Install the pre-commit hook into the repository at `repo_path`.
///
/// Writes `.githooks/pre-commit` and sets `core.hooksPath` to
/// `.githooks`. Refuses to clobber an unmanaged hook or a foreign
/// `core.hooksPath` unless `force` is set.
///
/// # Errors
///
/// Returns [`InstallHooksError`] on any failure; see the variants.
pub fn install_hooks(repo_path: &Path, force: bool) -> Result<(), Report<InstallHooksError>> {
    let repo = gix::open(repo_path).change_context(InstallHooksError::OpenRepo)?;
    let work_dir = repo
        .workdir()
        .ok_or_else(|| Report::new(InstallHooksError::NoWorkdir))?
        .to_path_buf();
    let ts_path = env::current_exe().change_context(InstallHooksError::CurrentExe)?;

    // Preflight: refuse to override a foreign core.hooksPath.
    let existing_hooks_path = read_local_config_value(&repo, "core.hooksPath")?;
    let displaced_hooks_path = match existing_hooks_path.as_deref() {
        None | Some("") | Some(".githooks") => None,
        Some(other) if !force => {
            return Err(Report::new(InstallHooksError::ForeignHooksPath {
                current: other.to_string(),
                proposed: ".githooks".to_string(),
            }));
        }
        Some(other) => Some(other.to_string()),
    };

    let hooks_dir = work_dir.join(".githooks");
    let hook_path = hooks_dir.join("pre-commit");
    fs::create_dir_all(&hooks_dir).change_context(InstallHooksError::WriteHook)?;

    // Refuse to clobber an unmanaged hook.
    if hook_path.exists() && !is_managed(&hook_path)? && !force {
        return Err(Report::new(InstallHooksError::WouldClobber {
            path: hook_path.clone(),
        }));
    }
    // Under --force, back up any existing hook before replacing it.
    if hook_path.exists() && force {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = hook_path.with_extension(format!("bak.{secs}"));
        fs::rename(&hook_path, &backup).change_context(InstallHooksError::WriteHook)?;
    }

    write_atomic(&hook_path, render_hook(&ts_path).as_bytes())?;
    set_executable(&hook_path)?;
    set_local_config_value(&repo, "core.hooksPath", ".githooks")?;

    write_stdout_line(format!(
        "Installed: pre-commit hook -> {} (runs {})",
        hook_path.display(),
        ts_path.display(),
    ))
    .change_context(InstallHooksError::WriteHook)?;
    if let Some(prev) = displaced_hooks_path {
        write_stderr_line(format!(
            "note: previous core.hooksPath was `{prev}`. \
             To restore: git config --local core.hooksPath {prev}"
        ))
        .change_context(InstallHooksError::WriteHook)?;
    }
    Ok(())
}

/// Set the executable bit on `path` (Unix only; a no-op elsewhere).
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), Report<InstallHooksError>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = fs::metadata(path)
        .change_context(InstallHooksError::WriteHook)?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).change_context(InstallHooksError::WriteHook)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), Report<InstallHooksError>> {
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

        let absent = temp.path().join("absent");
        assert!(!is_managed(&absent).expect("absent hook reads as not managed"));
    }

    #[test]
    fn write_atomic_writes_and_leaves_no_temp() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let target = temp.path().join("file");
        write_atomic(&target, b"hello").expect("should write atomically");
        assert_eq!(
            fs::read(&target).expect("should read written file"),
            b"hello"
        );
        let leftovers: Vec<_> = fs::read_dir(temp.path())
            .expect("should read tempdir")
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".ts-install-hooks.tmp.")
            })
            .collect();
        assert!(leftovers.is_empty(), "no temp file should remain");
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::dev::lint::test_support;

    #[test]
    fn read_returns_none_when_unset() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        let value =
            read_local_config_value(&repo, "core.hooksPath").expect("should read config");
        assert!(value.is_none(), "unset key reads as None: {value:?}");
    }

    #[test]
    fn write_then_read_round_trips_and_persists() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());

        set_local_config_value(&repo, "core.hooksPath", ".githooks")
            .expect("should write config");
        let value =
            read_local_config_value(&repo, "core.hooksPath").expect("should read config back");
        assert_eq!(value.as_deref(), Some(".githooks"));

        let on_disk = fs::read_to_string(repo.git_dir().join("config"))
            .expect("should read .git/config");
        assert!(
            on_disk.contains("[core]") && on_disk.contains("hooksPath"),
            "on-disk config should carry core/hooksPath: {on_disk}"
        );
    }
}

#[cfg(test)]
mod install_hooks_tests {
    use super::*;
    use crate::dev::lint::test_support;

    fn hooks_path_value(repo_path: &Path) -> Option<String> {
        let repo = gix::open(repo_path).expect("should reopen repo");
        read_local_config_value(&repo, "core.hooksPath").expect("should read hooksPath")
    }

    #[test]
    fn fresh_repo_installs_hook_and_sets_config() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());

        install_hooks(temp.path(), false).expect("should install into a fresh repo");

        let hook = temp.path().join(".githooks/pre-commit");
        assert!(hook.is_file(), "hook file should exist");
        let content = fs::read_to_string(&hook).expect("should read hook");
        assert!(content.contains(MANAGED_MARKER), "hook should carry the marker");
        assert!(
            content.contains("dev lint domains --staged"),
            "hook should exec the linter"
        );
        assert_eq!(hooks_path_value(temp.path()).as_deref(), Some(".githooks"));
    }

    #[test]
    fn re_running_is_idempotent() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());

        install_hooks(temp.path(), false).expect("first install should succeed");
        install_hooks(temp.path(), false).expect("re-install should be idempotent");
        assert_eq!(hooks_path_value(temp.path()).as_deref(), Some(".githooks"));
    }

    #[test]
    fn overwrites_managed_hook_silently() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());

        install_hooks(temp.path(), false).expect("first install should succeed");
        // A managed hook is present; a second non-forced install must
        // still succeed (silent overwrite).
        install_hooks(temp.path(), false).expect("managed hook should be overwritten silently");
    }

    #[test]
    fn refuses_to_clobber_unmanaged_hook() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());

        let hooks_dir = temp.path().join(".githooks");
        fs::create_dir_all(&hooks_dir).expect("should create .githooks");
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho custom\n")
            .expect("should write unmanaged hook");

        let err = install_hooks(temp.path(), false)
            .expect_err("should refuse to clobber an unmanaged hook");
        assert!(
            matches!(err.current_context(), InstallHooksError::WouldClobber { .. }),
            "should be WouldClobber: {err:?}"
        );
    }

    #[test]
    fn force_backs_up_unmanaged_hook() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let _repo = test_support::init_repo(temp.path());

        let hooks_dir = temp.path().join(".githooks");
        fs::create_dir_all(&hooks_dir).expect("should create .githooks");
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho custom\n")
            .expect("should write unmanaged hook");

        install_hooks(temp.path(), true).expect("force should overwrite");

        // The new hook is managed; a backup of the old one exists.
        let content =
            fs::read_to_string(hooks_dir.join("pre-commit")).expect("should read new hook");
        assert!(content.contains(MANAGED_MARKER));
        let has_backup = fs::read_dir(&hooks_dir)
            .expect("should read hooks dir")
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("pre-commit.bak.")
            });
        assert!(has_backup, "the displaced hook should be backed up");
    }

    #[test]
    fn refuses_foreign_hooks_path() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        set_local_config_value(&repo, "core.hooksPath", "hooks")
            .expect("should seed foreign hooksPath");

        let err = install_hooks(temp.path(), false)
            .expect_err("should refuse a foreign core.hooksPath");
        assert!(
            matches!(err.current_context(), InstallHooksError::ForeignHooksPath { .. }),
            "should be ForeignHooksPath: {err:?}"
        );
    }

    #[test]
    fn force_overrides_foreign_hooks_path() {
        let temp = tempfile::tempdir().expect("should create tempdir");
        let repo = test_support::init_repo(temp.path());
        set_local_config_value(&repo, "core.hooksPath", "hooks")
            .expect("should seed foreign hooksPath");

        install_hooks(temp.path(), true).expect("force should override foreign hooksPath");
        assert_eq!(hooks_path_value(temp.path()).as_deref(), Some(".githooks"));
    }
}
