//! Bounded, isolated gh runner tests using real OS fake children.
//!
//! Each fake `gh` is a POSIX `sh` script in a private tempdir. The scripts
//! exercise real pipes, process groups, signals and reaping rather than mocks.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use error_stack::Report;
use tempfile::TempDir;

use crate::release::bounds::{AttemptDeadline, Cancellation, ProcessBounds};
use crate::release::documents::{ApprovedCommit, ApprovedRelease, ReleaseTag};
use crate::release::environment::CallerEnvironment;
use crate::release::error::ReleaseError;
use crate::release::gh::{
    GhLocation, GhRunner, PredicateKind, attestation_verify_args, release_verify_args,
    release_verify_asset_args,
};

mod argv;
mod environment;
mod lifecycle;

const TOKEN: &str = "fictional-gh-token-1111";
const FALLBACK_TOKEN: &str = "fictional-github-token-2222";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const VERSION_PRELUDE: &str = "if [ \"$1\" = \"--version\" ]; then\n  echo 'gh version 2.97.0 (2026-01-01)'\n  echo 'https://github.com/cli/cli/releases/tag/v2.97.0'\n  exit 0\nfi\n";
/// Spawn attempts tolerated for Linux `ETXTBSY` on a freshly written script.
const SPAWN_ATTEMPTS: u32 = 5;

/// A fake `gh` whose script is written exactly once.
struct Fake {
    directory: TempDir,
    gh: PathBuf,
}

impl Fake {
    /// Creates the private directory; [`Self::install`] writes the script.
    fn empty() -> Self {
        let directory = tempfile::tempdir().expect("should create fake gh directory");
        let gh = directory.path().join("gh");
        Self { directory, gh }
    }

    /// Creates a fake that answers `--version` with 2.97.0, then runs `body`.
    fn script(body: &str) -> Self {
        let fake = Self::empty();
        fake.install(body);
        fake
    }

    /// Creates a fake with a custom version prelude.
    fn with_version(prelude: &str, body: &str) -> Self {
        let fake = Self::empty();
        fake.install_with(prelude, body);
        fake
    }

    fn install(&self, body: &str) {
        self.install_with(VERSION_PRELUDE, body);
    }

    // Write under a temporary name, sync, mark executable and rename into
    // place, so the executable path never refers to a file open for writing.
    fn install_with(&self, prelude: &str, body: &str) {
        let staging = self.file(".gh.staging");
        let mut file = File::create(&staging).expect("should create staged fake gh");
        file.write_all(format!("#!/bin/sh\n{prelude}{body}\n").as_bytes())
            .expect("should write staged fake gh");
        file.sync_all().expect("should sync staged fake gh");
        drop(file);
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
            .expect("should mark fake gh executable");
        fs::rename(&staging, &self.gh).expect("should install fake gh");
    }

    fn file(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn runner(&self, bounds: ProcessBounds) -> Result<GhRunner, Report<ReleaseError>> {
        resolve(
            GhLocation::Explicit(&self.gh),
            &caller(&[("GH_TOKEN", TOKEN)]),
        )
        .map(|runner| runner.with_bounds(bounds))
    }
}

/// Resolves gh with test bounds, retrying only Linux `ETXTBSY`: another test
/// thread may have forked while this script was briefly open for writing.
fn resolve(
    location: GhLocation<'_>,
    caller: &CallerEnvironment,
) -> Result<GhRunner, Report<ReleaseError>> {
    let mut attempt = 1;
    loop {
        let result = GhRunner::resolve(
            location,
            caller,
            test_bounds(),
            AttemptDeadline::start(),
            &Cancellation::default(),
        );
        let busy = result
            .as_ref()
            .err()
            .is_some_and(|report| format!("{report:?}").contains("ExecutableFileBusy"));
        if !busy || attempt >= SPAWN_ATTEMPTS {
            return result;
        }
        attempt += 1;
        thread::sleep(Duration::from_millis(20));
    }
}

fn caller(pairs: &[(&str, &str)]) -> CallerEnvironment {
    CallerEnvironment::from_pairs(pairs.iter().copied())
}

fn test_bounds() -> ProcessBounds {
    ProcessBounds {
        child_timeout: Duration::from_secs(10),
        stdout_bytes: 64 * 1024,
        stderr_bytes: 16 * 1024,
        kill_grace: Duration::from_millis(500),
    }
}

fn release() -> ApprovedRelease {
    ApprovedRelease::new(
        ReleaseTag::parse("v1.2.0").expect("should parse fictional tag"),
        ApprovedCommit::parse(COMMIT).expect("should parse fictional commit"),
    )
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn run(
    runner: &GhRunner,
    deadline: AttemptDeadline,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, Report<ReleaseError>> {
    runner
        .run(&args(&["attestation", "verify"]), deadline, cancellation)
        .map(|output| output.stdout)
}

fn read_pid(path: &Path) -> libc::pid_t {
    let started = Instant::now();
    loop {
        if let Some(pid) = fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .filter(|pid: &libc::pid_t| *pid > 0)
        {
            return pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should observe child pid file {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Returns whether `pid` names a process, including an unreaped zombie.
fn process_exists(pid: libc::pid_t) -> bool {
    assert!(pid > 0, "should only probe a single positive pid");
    // SAFETY: signal 0 performs only existence and permission checks.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Kills one escaped test process, never a group or broadcast target.
fn terminate_process(pid: libc::pid_t) {
    assert!(pid > 0, "should only kill a single positive pid");
    // SAFETY: `kill` only reads its two integer arguments.
    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
}

fn assert_dead(pid: libc::pid_t, what: &str) {
    let started = Instant::now();
    while process_exists(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should kill and reap {what} (pid {pid})"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn context<T>(result: Result<T, Report<ReleaseError>>) -> (ReleaseError, String) {
    let report = result.err().expect("should fail");
    (*report.current_context(), format!("{report:?}"))
}
