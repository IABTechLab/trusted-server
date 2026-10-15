//! Native OS network-denial boundary (`source-owned-os-denial-v1`).
//!
//! A [`Boundary`] runs a [`ConfinedCommand`] under operating-system enforced
//! network denial: on macOS a generated deny-default `sandbox-exec` profile,
//! on Linux a Docker container with `--network none`, an effective seccomp
//! profile denying non-Unix sockets, and `--init` in Docker's default private
//! PID namespace. [`Boundary::run_confined`] requires a [`DenialProof`],
//! which [`Boundary::establish_denial`] constructs only after an unconfined
//! positive socket control connected and the confined probe was denied with
//! a real errno, for this boundary instance, in this process run.
//!
//! Every fingerprint includes a fresh per-instance random nonce, so a proof
//! never transfers to another [`Boundary`], even one with identical roots.
//!
//! # Root checks around every confined execution
//!
//! - Immutable and tool roots: every regular file must have `nlink == 1` and
//!   every alias must be relative, inside its root and not dangling. Full file
//!   facts (content hash, mode, inode, times) are recorded before and
//!   compared after; any change rejects the run. Tool executables must
//!   therefore be private single-link copies ([`ProbeProgram::staged`]), not
//!   Cargo's hard-linked `target/` outputs.
//! - Writable roots: `nlink == 1` and safe aliases before the run; safe
//!   aliases again after it.
//!
//! # Descendants
//!
//! On Linux the container's private PID namespace with `--init` ends every
//! descendant when the run's container is stopped and removed. On macOS:
//!
//! - The profile denies the `setsid` and `setpgid` system calls, so a
//!   descendant cannot leave the supervised process group that way, and the
//!   process module's group kill reaps it.
//! - `posix_spawn` with `POSIX_SPAWN_SETPGROUP`/`POSIX_SPAWN_SETSID` (Rust's
//!   `Command::process_group`) bypasses that syscall filter (observed). As a
//!   fail-closed backstop, after each run every process that started during
//!   it and belongs to that run's sandbox instance (identified by per-run
//!   marker rules through `sandbox_check`) or carries its environment nonce is
//!   killed and the run rejected. Scrubbing the environment does not hide a
//!   descendant, and a confined process cannot re-apply a sandbox.
//! - Survivors are found only after the run exits, so the macOS backend
//!   still does not own descendant cleanup the way a container does
//!   ([`Boundary::has_owned_descendant_cleanup`] stays false).
//!
//! # What this module does not prove
//!
//! - It probes one IPv4 TCP loopback connect; the UDP, IPv6 and
//!   documentation-address matrix belongs to Task 8 qualification.
//! - No sandbox profile or container image is qualified:
//!   [`QualifiedSandboxProfile`] and [`QualifiedImage`](docker::QualifiedImage)
//!   are uninhabited, so
//!   production execution stays closed.
//! - The Docker client is trust-on-first-use pinned by digest for the life of
//!   a boundary, not authenticated against an independent trust root.

mod command;
mod daemon;
mod docker;
mod host_inputs;
mod macos;
mod probe;
mod proof;
mod roots;
mod seccomp;
#[cfg(target_os = "macos")]
mod survivors;
#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

pub(crate) use command::{ConfinedCommand, Umask};
pub(crate) use docker::{DockerSettings, ImageSelection};
pub(crate) use probe::{DENIED_STATUS, ProbeProgram, ProbeReport, connect_probe, parse_loopback};
pub(crate) use proof::DenialProof;
pub(crate) use roots::BoundaryRoots;

use crate::error::ReleaseError;
use crate::process::{ProcessLimits, ProcessResult};
use roots::{RootRole, ScanPolicy, TreeFacts};

/// Domain separating the instance fingerprint from platform fingerprints.
const INSTANCE_DOMAIN: &[u8] = b"trusted-server-boundary-instance-v1\n";

fn boundary_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Returns `N` bytes from the kernel random source.
fn random_bytes<const N: usize>() -> Result<[u8; N], Report<ReleaseError>> {
    let mut bytes = [0_u8; N];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| boundary_error(format!("cannot read kernel randomness: {error}")))?;
    Ok(bytes)
}

/// A macOS sandbox profile that passed Task 8 qualification.
///
/// None has, so this type is uninhabited and production macOS selection
/// stays closed; tests use a `#[cfg(test)]` constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedSandboxProfile {}

/// The platform mechanism enforcing denial.
#[derive(Debug)]
enum Platform {
    MacSandbox(macos::MacProfile),
    LinuxContainer(Box<docker::DockerBoundary>),
}

/// A configured OS network-denial boundary.
#[derive(Debug)]
pub(crate) struct Boundary {
    roots: BoundaryRoots,
    platform: Platform,
    fingerprint: [u8; 32],
}

/// Binds a platform fingerprint to a fresh per-instance nonce.
fn instance_fingerprint(platform: &[u8; 32]) -> Result<[u8; 32], Report<ReleaseError>> {
    let nonce: [u8; 16] = random_bytes()?;
    let mut hasher = Sha256::new();
    hasher.update(INSTANCE_DOMAIN);
    hasher.update(platform);
    hasher.update(nonce);
    Ok(hasher.finalize().into())
}

/// Before-run facts of the immutable and tool roots.
struct FixedFacts(Vec<TreeFacts>);

impl Boundary {
    /// Returns the current instance's measured roots, controls and nonce binding.
    ///
    /// This binds an executed candidate; it does not qualify that candidate.
    pub(crate) fn execution_fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    /// Requires exact mount roles and placement for an owning recipe.
    pub(crate) fn matches_roots(&self, roots: &BoundaryRoots) -> bool {
        self.roots == *roots
    }

    /// Reports whether container teardown owns every run descendant.
    ///
    /// The experimental macOS backend deliberately cannot satisfy this gate.
    pub(crate) fn has_owned_descendant_cleanup(&self) -> bool {
        matches!(self.platform, Platform::LinuxContainer(_))
    }

    /// Translates a selected host path into the command's execution namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is not inside this boundary's roots.
    pub(crate) fn execution_path(
        &self,
        path: &Path,
    ) -> Result<std::path::PathBuf, Report<ReleaseError>> {
        match &self.platform {
            Platform::LinuxContainer(_) => docker::container_path(&self.roots, path),
            Platform::MacSandbox(_) if self.roots.locate(path).is_some() => Ok(path.to_owned()),
            _ => Err(boundary_error("execution path is outside boundary roots")),
        }
    }
    /// Configures the qualified macOS `sandbox-exec` boundary.
    ///
    /// # Errors
    ///
    /// Never returns: [`QualifiedSandboxProfile`] is uninhabited until a
    /// profile is qualified.
    pub(crate) fn mac_sandbox(
        _roots: BoundaryRoots,
        qualification: QualifiedSandboxProfile,
    ) -> Result<Self, Report<ReleaseError>> {
        match qualification {}
    }

    /// Configures the unqualified macOS boundary for tests.
    ///
    /// # Errors
    ///
    /// As [`Self::mac_sandbox_unqualified`].
    #[cfg(test)]
    pub(crate) fn mac_sandbox_for_tests(
        roots: BoundaryRoots,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::mac_sandbox_unqualified(roots)
    }

    /// Generates the profile and binds the instance fingerprint.
    fn mac_sandbox_unqualified(roots: BoundaryRoots) -> Result<Self, Report<ReleaseError>> {
        if !cfg!(target_os = "macos") {
            return Err(boundary_error("the sandbox-exec boundary is macOS-only"));
        }
        let profile = macos::MacProfile::generate(&roots)?;
        Ok(Self {
            fingerprint: instance_fingerprint(&profile.fingerprint())?,
            platform: Platform::MacSandbox(profile),
            roots,
        })
    }

    /// Configures the Linux Docker container boundary.
    ///
    /// Host-owned seccomp profiles and the empty client configuration are
    /// created in a fresh directory under `state_parent`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] on any target other than Linux, or
    /// as [`docker::DockerBoundary::new`] does.
    pub(crate) fn linux_container(
        roots: BoundaryRoots,
        settings: DockerSettings,
        image: ImageSelection,
        state_parent: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        if !cfg!(target_os = "linux") {
            return Err(boundary_error(
                "the Docker container boundary is Linux-only",
            ));
        }
        let container = docker::DockerBoundary::new(&roots, settings, image, state_parent)?;
        Ok(Self {
            fingerprint: instance_fingerprint(&container.fingerprint())?,
            platform: Platform::LinuxContainer(Box::new(container)),
            roots,
        })
    }

    /// Runs the unconfined control and the confined probe for this instance
    /// and returns the proof only when both behave.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the control or denial fails,
    /// or [`ReleaseError::Process`] when a probe cannot be run.
    pub(crate) fn establish_denial(
        &self,
        probe: &ProbeProgram,
        limits: ProcessLimits,
    ) -> Result<DenialProof, Report<ReleaseError>> {
        proof::establish(self, probe, limits)
    }

    /// Runs `command` confined, requiring a proof for this instance.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for a proof from another instance, a
    /// rejected or changed root, a surviving escaped descendant, or a command
    /// outside the roots, or [`ReleaseError::Process`] when the child cannot
    /// be run.
    pub(crate) fn run_confined(
        &self,
        command: &ConfinedCommand,
        proof: &DenialProof,
    ) -> Result<ProcessResult, Report<ReleaseError>> {
        if !proof.matches(&self.fingerprint) {
            return Err(boundary_error(
                "denial proof belongs to a different boundary instance",
            ));
        }
        self.execute(command)
    }

    /// Checks every root, runs `command` confined, then rechecks them.
    fn execute(&self, command: &ConfinedCommand) -> Result<ProcessResult, Report<ReleaseError>> {
        self.check_placement(command)?;
        let before = self.scan_fixed()?;
        self.scan_writable(ScanPolicy::SingleLinks)?;
        let outcome = match &self.platform {
            Platform::MacSandbox(profile) => profile.run(command),
            Platform::LinuxContainer(container) => container.run(&self.roots, command),
        };
        let after = self.verify_after(&before);
        match (outcome, after) {
            (Ok(result), Ok(())) => Ok(result),
            (Ok(_), Err(changed)) => Err(changed),
            (Err(run), Ok(())) => Err(run),
            (Err(run), Err(changed)) => {
                Err(run.attach(format!("roots also failed their checks: {changed:?}")))
            }
        }
    }

    fn fixed_roots(&self) -> impl Iterator<Item = &Path> {
        self.roots
            .immutable()
            .iter()
            .chain(self.roots.tools())
            .map(std::path::PathBuf::as_path)
    }

    fn scan_fixed(&self) -> Result<FixedFacts, Report<ReleaseError>> {
        self.fixed_roots()
            .map(|root| TreeFacts::scan(root, ScanPolicy::Immutable))
            .collect::<Result<_, _>>()
            .map(FixedFacts)
    }

    fn scan_writable(&self, policy: ScanPolicy) -> Result<(), Report<ReleaseError>> {
        for root in self.roots.writable() {
            TreeFacts::scan(root, policy)?;
        }
        Ok(())
    }

    fn verify_after(&self, before: &FixedFacts) -> Result<(), Report<ReleaseError>> {
        let after = self.scan_fixed()?;
        for (before, after) in before.0.iter().zip(&after.0) {
            before.ensure_unchanged(after)?;
        }
        self.scan_writable(ScanPolicy::AliasesOnly)
    }

    fn check_placement(&self, command: &ConfinedCommand) -> Result<(), Report<ReleaseError>> {
        if self.roots.locate(&command.program).is_none() {
            return Err(boundary_error(
                "confined program is outside every boundary root",
            ));
        }
        match self.roots.locate(&command.working_directory) {
            Some((RootRole::Immutable | RootRole::Writable, _, _)) => Ok(()),
            _ => Err(boundary_error(
                "confined working directory is outside the immutable and writable roots",
            )),
        }
    }
}
