//! Bounded read-only Git plumbing under a cleared, hardened environment.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;

use error_stack::Report;

use super::{TreeEntry, source_error};
use crate::error::ReleaseError;
use crate::preflight::AuditedEnvironment;

const STDOUT_LIMIT: u64 = 64 * 1024 * 1024;
const STDERR_LIMIT: u64 = 64 * 1024;
const STDERR_SHOWN: usize = 4 * 1024;
const BLOB_LIMIT: u64 = 64 * 1024 * 1024;

/// Read-only Git plumbing under a cleared, hardened environment.
pub(super) struct Git {
    program: PathBuf,
    search_path: String,
    root: PathBuf,
    home: tempfile::TempDir,
}

impl Git {
    pub(super) fn locate(
        root: &Path,
        audited: &AuditedEnvironment,
    ) -> Result<Self, Report<ReleaseError>> {
        let search_path = audited
            .search_path()
            .ok_or_else(|| source_error("PATH is required to locate git"))?;
        let program = search_path
            .split(':')
            .filter(|directory| directory.starts_with('/'))
            .map(|directory| Path::new(directory).join("git"))
            .find(|candidate| {
                fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| source_error("git was not found on an absolute PATH entry"))?;
        let home = tempfile::Builder::new()
            .prefix("release-git-home-")
            .tempdir()
            .map_err(|error| source_error(format!("cannot create private git HOME: {error}")))?;
        Ok(Self {
            program,
            search_path: search_path.to_owned(),
            root: root.to_path_buf(),
            home,
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(self.root.join(".git"));
        let mut work_tree = OsString::from("--work-tree=");
        work_tree.push(&self.root);
        let mut command = Command::new(&self.program);
        command
            .arg(git_dir)
            .arg(work_tree)
            .arg("--no-replace-objects")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "protocol.allow=never",
            ])
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", &self.search_path)
            .env("HOME", self.home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn spawn(&self, args: &[&str], stdin: bool) -> Result<Child, Report<ReleaseError>> {
        let mut command = self.command(args);
        if stdin {
            command.stdin(Stdio::piped());
        }
        command
            .spawn()
            .map_err(|error| source_error(format!("cannot run git {}: {error}", args.join(" "))))
    }

    pub(super) fn run(&self, args: &[&str]) -> Result<Vec<u8>, Report<ReleaseError>> {
        let mut child = self.spawn(args, false)?;
        let stderr = capture_stderr(&mut child);
        let mut output = Vec::new();
        let read = match child.stdout.take() {
            Some(stdout) => stdout
                .take(STDOUT_LIMIT + 1)
                .read_to_end(&mut output)
                .map_err(|error| error.to_string()),
            None => Err("stdout is unavailable".to_owned()),
        };
        let oversized = output.len() as u64 > STDOUT_LIMIT;
        finish(
            child,
            stderr,
            args,
            read.and_then(|_| {
                if oversized {
                    Err("stdout exceeds its byte limit".to_owned())
                } else {
                    Ok(())
                }
            }),
        )?;
        Ok(output)
    }

    pub(super) fn read_blobs(
        &self,
        entries: &[TreeEntry],
        mut visit: impl FnMut(&TreeEntry, &[u8]) -> Result<(), Report<ReleaseError>>,
    ) -> Result<(), Report<ReleaseError>> {
        let args = ["cat-file", "--batch"];
        let mut child = self.spawn(&args, true)?;
        let stderr = capture_stderr(&mut child);
        let request: Vec<u8> = entries
            .iter()
            .flat_map(|entry| entry.oid.bytes().chain(std::iter::once(b'\n')))
            .collect();
        let writer = child
            .stdin
            .take()
            .map(|mut stdin| thread::spawn(move || stdin.write_all(&request)));
        let mut visited = Ok(());
        let read = match child.stdout.take() {
            Some(stdout) => {
                let mut reader = BufReader::new(stdout);
                read_batch(&mut reader, entries, &mut |entry, bytes| {
                    visited = visit(entry, bytes);
                    visited.is_ok()
                })
            }
            None => Err("stdout is unavailable".to_owned()),
        };
        if read.is_err() {
            let _ = child.kill();
        }
        let written = match writer.map(thread::JoinHandle::join) {
            Some(Ok(Ok(()))) => Ok(()),
            Some(Ok(Err(error))) => Err(error.to_string()),
            Some(Err(_)) => Err("stdin writer panicked".to_owned()),
            None => Err("stdin is unavailable".to_owned()),
        };
        let finished = finish(child, stderr, &args, read.and(written));
        visited?;
        finished
    }
}

fn read_batch(
    reader: &mut impl BufRead,
    entries: &[TreeEntry],
    visit: &mut impl FnMut(&TreeEntry, &[u8]) -> bool,
) -> Result<(), String> {
    for entry in entries {
        let mut header = Vec::new();
        reader
            .take(128)
            .read_until(b'\n', &mut header)
            .map_err(|error| error.to_string())?;
        let header = std::str::from_utf8(&header).map_err(|_| "batch header is not UTF-8")?;
        let fields: Vec<&str> = header.trim_end_matches('\n').split(' ').collect();
        let size = match fields.as_slice() {
            [oid, "blob", size] if *oid == entry.oid => size
                .parse::<u64>()
                .map_err(|_| "batch blob size is malformed".to_owned())?,
            _ => return Err(format!("object for {} is not a readable blob", entry.path)),
        };
        if size > BLOB_LIMIT {
            return Err(format!(
                "{} exceeds the tracked blob byte limit",
                entry.path
            ));
        }
        let mut bytes = vec![0; usize::try_from(size).map_err(|_| "blob size overflows")?];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| error.to_string())?;
        let mut terminator = [0];
        reader
            .read_exact(&mut terminator)
            .map_err(|error| error.to_string())?;
        if terminator != *b"\n" || !visit(entry, &bytes) {
            return Err(format!("batch output for {} is rejected", entry.path));
        }
    }
    Ok(())
}

type StderrCapture = Option<thread::JoinHandle<Vec<u8>>>;

fn capture_stderr(child: &mut Child) -> StderrCapture {
    child.stderr.take().map(|stderr| {
        thread::spawn(move || {
            let mut captured = Vec::new();
            let mut stderr = stderr;
            let _ = (&mut stderr).take(STDERR_LIMIT).read_to_end(&mut captured);
            let _ = io::copy(&mut stderr, &mut io::sink());
            captured
        })
    })
}

fn finish(
    mut child: Child,
    stderr: StderrCapture,
    args: &[&str],
    read: Result<(), String>,
) -> Result<(), Report<ReleaseError>> {
    if read.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|error| source_error(format!("cannot wait for git: {error}")))?;
    let detail = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let mut detail = String::from_utf8_lossy(&detail).into_owned();
    if detail.len() > STDERR_SHOWN {
        let mut cut = STDERR_SHOWN;
        while !detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail.truncate(cut);
        detail.push_str(" [truncated]");
    }
    let command = format!("git {}", args.join(" "));
    if let Err(reason) = read {
        return Err(source_error(format!(
            "{command} output is rejected: {reason}; stderr: {}",
            detail.trim()
        )));
    }
    if !status.success() {
        return Err(source_error(format!(
            "{command} failed with {status}; stderr: {}",
            detail.trim()
        )));
    }
    Ok(())
}
