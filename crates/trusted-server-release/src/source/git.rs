//! Bounded read-only Git plumbing under a cleared, hardened environment.

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Cursor, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use error_stack::Report;

use super::{checked_sha1, source_error};
use crate::error::ReleaseError;
use crate::preflight::AuditedEnvironment;
use crate::process::{BoundedCommand, ProcessLimits, ProcessResult};

const STDERR_LIMIT: u64 = 64 * 1024;
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
    wall_limit: Duration,
    attempt_deadline: Instant,
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
            wall_limit: Duration::from_secs(120),
            attempt_deadline: Instant::now() + Duration::from_secs(900),
        })
    }

    #[cfg(test)]
    pub(super) fn with_wall_limit(mut self, limit: Duration) -> Self {
        self.wall_limit = limit;
        self
    }

    fn command(
        &self,
        args: &[&str],
        stdout_limit: u64,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(self.root.join(".git"));
        let mut work_tree = OsString::from("--work-tree=");
        work_tree.push(&self.root);
        let remaining = self
            .attempt_deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| source_error("Git authentication attempt exceeded its deadline"))?;
        let limits = ProcessLimits {
            wall: self.wall_limit.min(remaining),
            stdout_max: stdout_limit,
            stderr_max: STDERR_LIMIT,
        };
        let mut arguments = vec![git_dir, work_tree, OsString::from("--no-replace-objects")];
        arguments.extend(
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "protocol.allow=never",
            ]
            .into_iter()
            .chain(args.iter().copied())
            .map(OsString::from),
        );
        // HOME is fresh and private and XDG_CONFIG_HOME is absent. This
        // disables user configuration without an arbitrary Git config path.
        BoundedCommand::new(&self.program, &self.root, limits)?
            .arguments(arguments)
            .environment("PATH", &self.child_path)?
            .environment("HOME", self.home.path())?
            .environment("GIT_CONFIG_NOSYSTEM", "1")?
            .environment("GIT_NO_REPLACE_OBJECTS", "1")?
            .environment("GIT_TERMINAL_PROMPT", "0")?
            .environment("GIT_OPTIONAL_LOCKS", "0")?
            .environment("LC_ALL", "C")
    }

    fn execute(
        &self,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        stdout_limit: u64,
    ) -> Result<Vec<u8>, Report<ReleaseError>> {
        let mut command = self.command(args, stdout_limit)?;
        if let Some(bytes) = stdin {
            command = command.stdin_bytes(bytes);
        }
        command
            .run()
            .and_then(ProcessResult::require_success)
            .map_err(|report| {
                report
                    .change_context(ReleaseError::Source)
                    .attach(format!("git {}", args.join(" ")))
            })
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
        self.execute(args, None, stdout_limit)
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
        let sizes = self.object_sizes(kind, oids)?;
        let mut start = 0;
        while start < oids.len() {
            let mut end = start;
            let mut limit = 0;
            while end < oids.len() && limit + sizes[end] + 128 <= OBJECT_LIMIT + 128 {
                limit += sizes[end] + 128;
                end += 1;
            }
            let output = self.execute(
                &["cat-file", "--batch"],
                Some(request(&oids[start..end])),
                limit,
            )?;
            let mut reader = Cursor::new(output.as_slice());
            read_batch(
                &mut reader,
                &Batch {
                    kind,
                    oids: &oids[start..end],
                    sizes: &sizes[start..end],
                    budget: &self.object_budget,
                },
                &mut |index, bytes| visit(start + index, bytes),
            )?;
            if reader.position() != output.len() as u64 {
                return Err(source_error("unexpected trailing Git batch output"));
            }
            start = end;
        }
        Ok(())
    }

    fn object_sizes(&self, kind: &str, oids: &[&str]) -> Result<Vec<u64>, Report<ReleaseError>> {
        let limit = (oids.len() as u64)
            .checked_mul(128)
            .ok_or_else(|| source_error("object header budget overflow"))?;
        let output = self.execute(&["cat-file", "--batch-check"], Some(request(oids)), limit)?;
        let mut reader = Cursor::new(output.as_slice());
        let mut total = 0_u64;
        let mut sizes = Vec::with_capacity(oids.len());
        for oid in oids {
            let size = read_header(&mut reader, kind, oid)?;
            total = total
                .checked_add(size)
                .ok_or_else(|| source_error("object byte budget overflow"))?;
            if size > OBJECT_LIMIT || total > self.object_budget.get() {
                return Err(source_error(
                    "object exceeds the per-object or aggregate object byte budget",
                ));
            }
            sizes.push(size);
        }
        if reader.position() != output.len() as u64 {
            return Err(source_error("unexpected trailing Git object metadata"));
        }
        Ok(sizes)
    }
}

fn request(oids: &[&str]) -> Vec<u8> {
    oids.iter()
        .flat_map(|oid| oid.bytes().chain(std::iter::once(b'\n')))
        .collect()
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
    sizes: &'a [u64],
    budget: &'a Cell<u64>,
}

fn read_batch(
    reader: &mut impl BufRead,
    batch: &Batch<'_>,
    visit: &mut impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    let io_error = |error: io::Error| source_error(format!("cannot read batch output: {error}"));
    let kind = batch.kind;
    if batch.sizes.len() != batch.oids.len() {
        return Err(source_error(
            "Git preflight size count differs from requested object count",
        ));
    }
    for (index, oid) in batch.oids.iter().enumerate() {
        let size = read_header(reader, kind, oid)?;
        if size != batch.sizes[index] {
            return Err(source_error(
                "Git object body size differs from preflight metadata",
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_body_size_must_match_the_preflight_size() {
        let bytes = b"fictional object";
        let oid = object_id("blob", bytes).expect("should hash fixture blob");
        let mut raw = format!("{oid} blob {}\n", bytes.len()).into_bytes();
        raw.extend_from_slice(bytes);
        raw.push(b'\n');
        let budget = Cell::new(1024);
        let batch = Batch {
            kind: "blob",
            oids: &[oid.as_str()],
            sizes: &[1],
            budget: &budget,
        };
        let mut delivered = false;
        let result = read_batch(&mut Cursor::new(raw), &batch, &mut |_, _| {
            delivered = true;
            Ok(())
        });
        assert!(
            result.is_err(),
            "should reject body header inconsistent with preflight metadata"
        );
        assert!(
            !delivered,
            "should reject before delivering any object bytes"
        );
        assert_eq!(
            budget.get(),
            1024,
            "should reject before consuming object budget"
        );
    }
}
