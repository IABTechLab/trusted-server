//! Bounded read-only Git plumbing under a cleared, hardened environment.

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;

use error_stack::Report;

use super::{checked_sha1, source_error};
use crate::error::ReleaseError;
use crate::preflight::AuditedEnvironment;

const STDERR_LIMIT: u64 = 64 * 1024;
const STDERR_SHOWN: usize = 4 * 1024;
/// Largest single object read; far above any tracked source file.
const OBJECT_LIMIT: u64 = 64 * 1024 * 1024;
/// Total bytes of all objects read in one authentication. The tracked tree is
/// currently tens of MiB; 2 GiB leaves ample growth while bounding the work a
/// hostile repository can cause, since objects are held one at a time.
const AGGREGATE_OBJECT_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// Read-only Git plumbing under a cleared, hardened environment.
pub(super) struct Git {
    program: PathBuf,
    /// Child `PATH`: only the resolved git executable's own directory.
    child_path: String,
    root: PathBuf,
    home: tempfile::TempDir,
    /// Remaining bytes of the aggregate object budget.
    object_budget: Cell<u64>,
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
        let child_path = program
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| source_error("git directory must be UTF-8"))?
            .to_owned();
        let home = tempfile::Builder::new()
            .prefix("release-git-home-")
            .tempdir()
            .map_err(|error| source_error(format!("cannot create private git HOME: {error}")))?;
        Ok(Self {
            program,
            child_path,
            root: root.to_path_buf(),
            home,
            object_budget: Cell::new(AGGREGATE_OBJECT_BUDGET),
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
            .env("PATH", &self.child_path)
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

    /// Replaces the aggregate object budget so tests can exercise exhaustion.
    #[cfg(test)]
    pub(super) fn with_object_budget(self, bytes: u64) -> Self {
        self.object_budget.set(bytes);
        self
    }

    /// Runs one plumbing command whose stdout must fit `stdout_limit` bytes.
    pub(super) fn run(
        &self,
        args: &[&str],
        stdout_limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        let mut child = self.spawn(args, false)?;
        let stderr = capture_stderr(&mut child);
        let mut output = Vec::new();
        let read = match child.stdout.take() {
            Some(stdout) => stdout
                .take(stdout_limit + 1)
                .read_to_end(&mut output)
                .map(|_| ())
                .map_err(|error| source_error(format!("cannot read stdout: {error}"))),
            None => Err(source_error("stdout is unavailable")),
        };
        let read = read.and_then(|()| {
            if output.len() as u64 > stdout_limit {
                Err(source_error("stdout exceeds its byte limit"))
            } else {
                Ok(())
            }
        });
        finish(child, stderr, args, read)?;
        Ok(output)
    }

    /// Reads objects of one `kind` through `cat-file --batch`, rehashing each.
    ///
    /// Every returned object must hash to its requested id as
    /// `sha1("<kind> <length>\0" + bytes)` with collision detection; Git's
    /// loose-object reads do not verify this themselves.
    pub(super) fn read_objects(
        &self,
        kind: &'static str,
        oids: &[&str],
        mut visit: impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
    ) -> Result<(), Report<ReleaseError>> {
        let args = ["cat-file", "--batch"];
        let mut child = self.spawn(&args, true)?;
        let stderr = capture_stderr(&mut child);
        let request: Vec<u8> = oids
            .iter()
            .flat_map(|oid| oid.bytes().chain(std::iter::once(b'\n')))
            .collect();
        let writer = child
            .stdin
            .take()
            .map(|mut stdin| thread::spawn(move || stdin.write_all(&request)));
        let read = match child.stdout.take() {
            Some(stdout) => read_batch(
                &mut BufReader::new(stdout),
                &Batch {
                    kind,
                    oids,
                    budget: &self.object_budget,
                },
                &mut visit,
            ),
            None => Err(source_error("stdout is unavailable")),
        };
        if read.is_err() {
            let _ = child.kill();
        }
        let written = match writer.map(thread::JoinHandle::join) {
            Some(Ok(Ok(()))) => Ok(()),
            Some(Ok(Err(error))) => Err(source_error(format!("cannot write stdin: {error}"))),
            Some(Err(_)) => Err(source_error("stdin writer panicked")),
            None => Err(source_error("stdin is unavailable")),
        };
        finish(child, stderr, &args, read.and(written))
    }
}

/// Collision-checked Git SHA-1 object id of `bytes` stored as an object of `kind`.
fn object_id(kind: &str, bytes: &[u8]) -> Result<String, Report<ReleaseError>> {
    let header = format!("{kind} {}\0", bytes.len());
    checked_sha1(&[header.as_bytes(), bytes])
}

/// One `cat-file --batch` request and the shared aggregate byte budget.
struct Batch<'a> {
    kind: &'a str,
    oids: &'a [&'a str],
    budget: &'a Cell<u64>,
}

fn read_batch(
    reader: &mut impl BufRead,
    batch: &Batch<'_>,
    visit: &mut impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    let io_error = |error: io::Error| source_error(format!("cannot read batch output: {error}"));
    let kind = batch.kind;
    for (index, oid) in batch.oids.iter().enumerate() {
        let size = read_header(reader, kind, oid)?;
        let remaining = batch.budget.get();
        if size > OBJECT_LIMIT || size > remaining {
            return Err(source_error(format!(
                "object {oid} exceeds the per-object or aggregate object byte budget"
            )));
        }
        batch.budget.set(remaining - size);
        let length = usize::try_from(size).map_err(|_| source_error("object size overflows"))?;
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).map_err(io_error)?;
        let mut terminator = [0];
        reader.read_exact(&mut terminator).map_err(io_error)?;
        if terminator != *b"\n" {
            return Err(source_error(format!("batch output for {oid} is malformed")));
        }
        if object_id(kind, &bytes)? != *oid {
            return Err(source_error(format!(
                "object identity mismatch: {kind} {oid} content does not hash to its id"
            )));
        }
        visit(index, &bytes)?;
    }
    Ok(())
}

/// Reads one `<oid> <kind> <size>` header and returns the object size.
fn read_header(
    reader: &mut impl BufRead,
    kind: &str,
    oid: &str,
) -> Result<u64, Report<ReleaseError>> {
    let mut header = Vec::new();
    reader
        .take(128)
        .read_until(b'\n', &mut header)
        .map_err(|error| source_error(format!("cannot read batch header: {error}")))?;
    let header =
        std::str::from_utf8(&header).map_err(|_| source_error("batch header is not UTF-8"))?;
    let fields: Vec<&str> = header.trim_end_matches('\n').split(' ').collect();
    match fields.as_slice() {
        [returned, returned_kind, size] if *returned == oid && *returned_kind == kind => size
            .parse::<u64>()
            .map_err(|_| source_error("batch object size is malformed")),
        _ => Err(source_error(format!(
            "object {oid} is not a readable {kind}"
        ))),
    }
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

/// Reaps the child and attaches its bounded stderr to any failure.
fn finish(
    mut child: Child,
    stderr: StderrCapture,
    args: &[&str],
    read: Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    if read.is_err() {
        let _ = child.kill();
    }
    // Task 7 bounded process module replaces this unbounded wait.
    let status = child
        .wait()
        .map_err(|error| source_error(format!("cannot wait for git: {error}")))?;
    let detail = bounded_detail(stderr);
    let context = format!("git {}; stderr: {detail}", args.join(" "));
    read.map_err(|report| report.attach(context.clone()))?;
    if !status.success() {
        return Err(source_error(format!("{context}; exit {status}")));
    }
    Ok(())
}

fn bounded_detail(stderr: StderrCapture) -> String {
    let detail = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let mut detail = String::from_utf8_lossy(&detail).trim().to_owned();
    if detail.len() > STDERR_SHOWN {
        let mut cut = STDERR_SHOWN;
        while !detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail.truncate(cut);
        detail.push_str(" [truncated]");
    }
    detail
}
