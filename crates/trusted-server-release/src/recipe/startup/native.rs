//! Actual macOS startup acceptance of the final Darwin `ts` bytes.
//!
//! The policy is pure and compiled everywhere; only measuring the host and
//! executing the copy are macOS-only. Before anything runs, the host must be
//! the reviewed cohort: macOS 15 on the asset's own CPU. An arm64 host must
//! report `sysctl.proc_translated == 0`. An Intel host must not be a
//! translated process on Apple silicon: `sysctl.proc_translated` absent or
//! `0` and `hw.optional.arm64` absent or `0`, so Rosetta can never stand in
//! for real Intel hardware.
//!
//! The exact final bytes are copied once into a fresh `0555` directory
//! outside every build root and started as `ts --version` with exactly
//! `PATH=/usr/bin:/bin`, a private `HOME` and `TMPDIR` and `LC_ALL=C`. No
//! operator configuration exists, so the output must be exactly the owning
//! build facts. The copy must still hash to the accepted digest afterwards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use error_stack::Report;
#[cfg(target_os = "macos")]
use sha2::{Digest as _, Sha256};

use super::super::fresh::{disjoint, normal_absolute};
use super::super::{RecipeCompleted, recipe_error};
use super::StartupCopy;
use crate::error::ReleaseError;
use crate::inspect::MacosVersion;
use crate::policy::ReleaseTarget;
#[cfg(target_os = "macos")]
use crate::process::{BoundedCommand, ProcessLimits, ProcessOutcome};

/// The only macOS major version the startup cohort accepts.
pub(crate) const REQUIRED_MACOS_MAJOR: u32 = 15;
/// The fixed search path of the startup child.
const STARTUP_PATH: &str = "/usr/bin:/bin";
/// The executable name inside the copy directory.
const EXECUTABLE_NAME: &str = "ts";

/// Host facts measured before the startup runs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HostIdentity {
    /// `kern.osproductversion`, such as `15.5`.
    pub(crate) product_version: String,
    /// Architecture of the running producer: `aarch64` or `x86_64`.
    pub(crate) architecture: String,
    /// `sysctl.proc_translated`, or `None` when the name is absent.
    pub(crate) proc_translated: Option<i32>,
    /// `hw.optional.arm64`, or `None` when the name is absent.
    pub(crate) hw_optional_arm64: Option<i32>,
}

/// Facts retained from the same actual native host and successful startup run.
#[derive(Debug)]
pub(crate) struct NativeStartupObserved {
    pub(super) host: HostIdentity,
    pub(super) stdout: String,
    pub(super) stderr_sha256: String,
    pub(super) elapsed: std::time::Duration,
}

/// Returns the major version of dotted decimal `text`.
fn major_version(text: &str) -> Option<u32> {
    let mut parts = text.split('.');
    let major = parts.next()?;
    let canonical = parts.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && !major.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && !major.starts_with('0');
    canonical.then(|| major.parse().ok()).flatten()
}

/// Requires the host to be the reviewed macOS 15 cohort for `target`.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for a non-Darwin target, another or
/// malformed macOS version, another architecture, a translated arm64
/// process, or an Intel target on a translated process or Apple silicon.
pub(crate) fn check_host_identity(
    target: ReleaseTarget,
    host: &HostIdentity,
) -> Result<(), Report<ReleaseError>> {
    if major_version(&host.product_version) != Some(REQUIRED_MACOS_MAJOR) {
        return Err(recipe_error(format!(
            "startup host runs macOS {:?}, expected macOS {REQUIRED_MACOS_MAJOR}",
            host.product_version
        )));
    }
    let native = match target {
        ReleaseTarget::Aarch64AppleDarwin => {
            host.architecture == "aarch64" && host.proc_translated == Some(0)
        }
        ReleaseTarget::X86_64AppleDarwin => {
            host.architecture == "x86_64"
                && matches!(host.proc_translated, None | Some(0))
                && matches!(host.hw_optional_arm64, None | Some(0))
        }
        ReleaseTarget::Wasm32Wasip1 | ReleaseTarget::X86_64UnknownLinuxGnu => {
            return Err(recipe_error(format!(
                "native macOS startup does not accept {}",
                target.triple()
            )));
        }
    };
    if !native {
        return Err(recipe_error(format!(
            "startup host {host:?} is not a native {} machine",
            target.triple()
        )));
    }
    Ok(())
}

/// Requires the actual host to satisfy the accepted artifact's deployment floor.
fn check_host_minimum(
    target: ReleaseTarget,
    host: &HostIdentity,
    minimum: MacosVersion,
) -> Result<(), Report<ReleaseError>> {
    check_host_identity(target, host)?;
    let current = MacosVersion::parse(&host.product_version).ok_or_else(|| {
        recipe_error("native startup host version cannot be compared to the artifact minimum")
    })?;
    if current < minimum {
        return Err(recipe_error(
            "native startup host is below the accepted deployment minimum",
        ));
    }
    Ok(())
}

/// Where the startup copy and its private state live.
pub(crate) struct NativeStartupLayout<'a> {
    /// Fresh directory receiving the `0555` copy; must not exist yet.
    pub(crate) copy_directory: &'a Path,
    /// Private `HOME` of the child.
    pub(crate) home: &'a Path,
    /// Private `TMPDIR` of the child.
    pub(crate) tmpdir: &'a Path,
    /// Every root the build used; none may contain or sit inside the above.
    pub(crate) build_roots: &'a [&'a Path],
}

/// The exact `ts --version` invocation of the native startup.
///
/// Its fields are private: only [`native_startup_command`] constructs it,
/// after checking the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeStartupCommand {
    /// The copied executable.
    program: PathBuf,
    /// Exactly `--version`.
    arguments: Vec<String>,
    /// Exactly `PATH`, `HOME`, `TMPDIR` and `LC_ALL`.
    environment: BTreeMap<String, String>,
    /// The copy directory.
    working_directory: PathBuf,
}

/// Derives the startup invocation after checking the layout.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when a path is not absolute normal UTF-8
/// or the copy directory, `HOME`, `TMPDIR` and build roots are not pairwise
/// disjoint.
pub(crate) fn native_startup_command(
    layout: &NativeStartupLayout<'_>,
) -> Result<NativeStartupCommand, Report<ReleaseError>> {
    let mut labelled = vec![
        ("startup copy directory", layout.copy_directory),
        ("startup HOME", layout.home),
        ("startup TMPDIR", layout.tmpdir),
    ];
    labelled.extend(layout.build_roots.iter().map(|root| ("build root", *root)));
    for (label, path) in &labelled {
        normal_absolute(label, path)?;
    }
    disjoint(&labelled)?;
    let text = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| recipe_error("startup path is not UTF-8"))
    };
    let environment = BTreeMap::from([
        ("PATH".to_owned(), STARTUP_PATH.to_owned()),
        ("HOME".to_owned(), text(layout.home)?),
        ("TMPDIR".to_owned(), text(layout.tmpdir)?),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ]);
    Ok(NativeStartupCommand {
        program: layout.copy_directory.join(EXECUTABLE_NAME),
        arguments: vec!["--version".to_owned()],
        environment,
        working_directory: layout.copy_directory.to_owned(),
    })
}

impl StartupCopy {
    /// Copies an accepted Darwin executable into the fresh `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when `completed` is not a Darwin
    /// asset or no longer rehashes, `directory` is not absolute normal UTF-8
    /// or exists, or the copy cannot be written read-only.
    pub(crate) fn create_native(
        completed: &RecipeCompleted,
        directory: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        if !completed.asset().artifact().target.is_darwin() {
            return Err(recipe_error(
                "the native startup gate accepts only Darwin assets",
            ));
        }
        normal_absolute("startup copy directory", directory)?;
        Self::write_private(completed, directory)
    }

    /// Measures the host, derives the command from `layout` and starts the
    /// copy natively, accepting exactly `expected`.
    ///
    /// Nothing runs unless the measured host passes
    /// [`check_host_identity`] for `target`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] when the child cannot run, or
    /// [`ReleaseError::Recipe`] when the host cannot be measured or is not
    /// the reviewed cohort for `target`, the layout is rejected, the command
    /// is not this copy's, the outcome or output is rejected, or the copy
    /// changed.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_native(
        &self,
        layout: &NativeStartupLayout<'_>,
        target: ReleaseTarget,
        expected: &str,
        minimum: MacosVersion,
    ) -> Result<NativeStartupObserved, Report<ReleaseError>> {
        let host = measure_host_identity()?;
        check_host_minimum(target, &host, minimum)?;
        let command = native_startup_command(layout)?;
        if command.program != self.directory.join(EXECUTABLE_NAME) {
            return Err(recipe_error("startup command does not run this copy"));
        }
        self.verify()?;
        let mut bounded = BoundedCommand::new(
            &command.program,
            &command.working_directory,
            ProcessLimits {
                wall: super::STARTUP_WALL,
                stdout_max: super::STARTUP_STDOUT_MAX,
                stderr_max: super::STARTUP_STDERR_MAX,
            },
        )?
        .arguments(command.arguments.iter().map(std::ffi::OsString::from));
        for (name, value) in &command.environment {
            bounded = bounded.environment(name, value)?;
        }
        let result = bounded.run();
        self.verify()?;
        let result = result?;
        if result.outcome() != (ProcessOutcome::Exited { code: 0 }) {
            return Err(recipe_error(format!(
                "native ts --version {}",
                result.outcome()
            )));
        }
        if result.stdout() != expected.as_bytes() {
            return Err(recipe_error(format!(
                "native ts --version printed {:?}, expected {expected:?}",
                String::from_utf8_lossy(result.stdout())
            )));
        }
        Ok(NativeStartupObserved {
            host,
            stdout: expected.to_owned(),
            stderr_sha256: hex::encode(Sha256::digest(result.stderr())),
            elapsed: result.elapsed(),
        })
    }
}

/// Measures the running host with `sysctlbyname`; nothing is executed.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the product version cannot be read.
#[cfg(target_os = "macos")]
pub(crate) fn measure_host_identity() -> Result<HostIdentity, Report<ReleaseError>> {
    Ok(HostIdentity {
        product_version: sysctl_text("kern.osproductversion")?,
        architecture: std::env::consts::ARCH.to_owned(),
        proc_translated: sysctl_integer("sysctl.proc_translated"),
        hw_optional_arm64: sysctl_integer("hw.optional.arm64"),
    })
}

/// Reads one NUL-terminated string sysctl.
#[cfg(target_os = "macos")]
fn sysctl_text(name: &str) -> Result<String, Report<ReleaseError>> {
    let key = std::ffi::CString::new(name).map_err(|_| recipe_error("sysctl name holds NUL"))?;
    let mut buffer = [0_u8; 64];
    let mut length: libc::size_t = buffer.len();
    // SAFETY: `key` is a NUL-terminated C string, `buffer` is writable for
    // `length` bytes, `length` points to its capacity, and no new value is set.
    let status = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    let bytes = buffer
        .get(..length)
        .filter(|_| status == 0)
        .ok_or_else(|| recipe_error(format!("cannot read sysctl {name}")))?;
    let text = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
    String::from_utf8(text.to_vec())
        .map_err(|_| recipe_error(format!("sysctl {name} is not UTF-8")))
}

/// Reads one 32-bit integer sysctl, or `None` when it is absent.
#[cfg(target_os = "macos")]
fn sysctl_integer(name: &str) -> Option<i32> {
    let key = std::ffi::CString::new(name).ok()?;
    let mut value: i32 = 0;
    let mut length: libc::size_t = std::mem::size_of::<i32>();
    // SAFETY: `key` is a NUL-terminated C string, `value` is a writable
    // `i32` whose size `length` states, and no new value is set.
    let status = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            (&raw mut value).cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && length == std::mem::size_of::<i32>()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::super::expected_version_output;
    use super::*;
    use crate::source::CommitId;

    fn host(
        version: &str,
        architecture: &str,
        translated: Option<i32>,
        arm64: Option<i32>,
    ) -> HostIdentity {
        HostIdentity {
            product_version: version.to_owned(),
            architecture: architecture.to_owned(),
            proc_translated: translated,
            hw_optional_arm64: arm64,
        }
    }

    #[test]
    fn host_identity_accepts_only_native_macos_15() {
        let arm = ReleaseTarget::Aarch64AppleDarwin;
        let intel = ReleaseTarget::X86_64AppleDarwin;
        let cases = [
            (
                "native arm64",
                arm,
                host("15.5", "aarch64", Some(0), Some(1)),
                true,
            ),
            (
                "arm64 point release",
                arm,
                host("15.6.1", "aarch64", Some(0), Some(1)),
                true,
            ),
            (
                "translated arm64",
                arm,
                host("15.5", "aarch64", Some(1), Some(1)),
                false,
            ),
            (
                "arm64 without translation report",
                arm,
                host("15.5", "aarch64", None, Some(1)),
                false,
            ),
            (
                "arm64 on macOS 14",
                arm,
                host("14.7", "aarch64", Some(0), Some(1)),
                false,
            ),
            (
                "arm64 on macOS 26",
                arm,
                host("26.0", "aarch64", Some(0), Some(1)),
                false,
            ),
            (
                "arm64 asset on Intel",
                arm,
                host("15.5", "x86_64", None, None),
                false,
            ),
            (
                "native Intel",
                intel,
                host("15.5", "x86_64", None, None),
                true,
            ),
            (
                "Intel reporting zeros",
                intel,
                host("15.5", "x86_64", Some(0), Some(0)),
                true,
            ),
            (
                "Intel under Rosetta",
                intel,
                host("15.5", "x86_64", Some(1), Some(1)),
                false,
            ),
            (
                "Intel process on Apple silicon",
                intel,
                host("15.5", "x86_64", Some(0), Some(1)),
                false,
            ),
            (
                "Intel asset on arm64",
                intel,
                host("15.5", "aarch64", Some(0), Some(1)),
                false,
            ),
            (
                "malformed version",
                intel,
                host("15.x", "x86_64", None, None),
                false,
            ),
            (
                "leading zero",
                intel,
                host("015.5", "x86_64", None, None),
                false,
            ),
            (
                "Linux target",
                ReleaseTarget::X86_64UnknownLinuxGnu,
                host("15.5", "x86_64", None, None),
                false,
            ),
        ];
        for (label, target, identity, accepted) in cases {
            assert_eq!(
                check_host_identity(target, &identity).is_ok(),
                accepted,
                "should decide the {label} host"
            );
        }
    }

    #[test]
    fn startup_host_must_satisfy_the_accepted_artifact_minimum_before_execution() {
        let host = host("15.5", "aarch64", Some(0), Some(1));
        check_host_minimum(
            crate::policy::ReleaseTarget::Aarch64AppleDarwin,
            &host,
            crate::inspect::MacosVersion::new(13, 0),
        )
        .expect("should accept native host above artifact minimum");
        assert!(
            check_host_minimum(
                crate::policy::ReleaseTarget::Aarch64AppleDarwin,
                &host,
                crate::inspect::MacosVersion::new(15, 6)
            )
            .is_err(),
            "should reject host below accepted artifact minimum"
        );
        assert!(
            check_host_minimum(
                crate::policy::ReleaseTarget::X86_64AppleDarwin,
                &host,
                crate::inspect::MacosVersion::new(13, 0)
            )
            .is_err(),
            "should still reject another target CPU"
        );
    }

    #[test]
    fn startup_command_is_exact_and_outside_every_build_root() {
        let layout = NativeStartupLayout {
            copy_directory: Path::new("/startup/example/copy"),
            home: Path::new("/startup/example/home"),
            tmpdir: Path::new("/startup/example/tmp"),
            build_roots: &[
                Path::new("/build/example/checkout"),
                Path::new("/build/example/target"),
            ],
        };

        let command = native_startup_command(&layout).expect("should derive the command");

        assert_eq!(command.program, Path::new("/startup/example/copy/ts"));
        assert_eq!(command.arguments, ["--version"]);
        assert_eq!(
            command.working_directory,
            Path::new("/startup/example/copy")
        );
        assert_eq!(
            command.environment,
            BTreeMap::from([
                ("HOME".to_owned(), "/startup/example/home".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("TMPDIR".to_owned(), "/startup/example/tmp".to_owned()),
            ]),
            "should pass exactly the fixed environment"
        );
    }

    #[test]
    fn startup_layout_rejects_overlap_and_relative_paths() {
        let roots = [
            Path::new("/build/example/checkout"),
            Path::new("/build/example/target"),
        ];
        let cases = [
            (
                "copy inside a build root",
                "/build/example/target/copy",
                "/startup/home",
                "/startup/tmp",
            ),
            (
                "build roots below the copy",
                "/build",
                "/startup/home",
                "/startup/tmp",
            ),
            (
                "home inside the copy",
                "/startup/copy",
                "/startup/copy/home",
                "/startup/tmp",
            ),
            (
                "shared HOME and TMPDIR",
                "/startup/copy",
                "/startup/state",
                "/startup/state",
            ),
            (
                "a relative copy",
                "startup/copy",
                "/startup/home",
                "/startup/tmp",
            ),
            (
                "a dotted TMPDIR",
                "/startup/copy",
                "/startup/home",
                "/startup/./tmp",
            ),
        ];
        for (label, copy, home, tmpdir) in cases {
            let layout = NativeStartupLayout {
                copy_directory: Path::new(copy),
                home: Path::new(home),
                tmpdir: Path::new(tmpdir),
                build_roots: &roots,
            };
            assert!(
                native_startup_command(&layout).is_err(),
                "should reject {label}"
            );
        }
    }

    #[test]
    fn darwin_version_output_names_the_darwin_target() {
        let commit =
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").expect("should parse");
        for target in [
            ReleaseTarget::Aarch64AppleDarwin,
            ReleaseTarget::X86_64AppleDarwin,
        ] {
            assert_eq!(
                expected_version_output(&commit, target),
                format!(
                    "ts package={} source=0123456789abcdef0123456789abcdef01234567 target={} \
                     variant=default\n",
                    env!("CARGO_PKG_VERSION"),
                    target.triple()
                ),
                "should expect exactly the owning build facts"
            );
        }
    }
}
