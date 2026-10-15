//! The pinned Docker client and its daemon-socket checks.
//!
//! Before every Docker host command the client rehashes its pinned inputs
//! (see [`HostInputs`]) and rechecks the daemon socket: the socket itself
//! must be a root-owned socket reached without a final alias, and every
//! ancestor directory, both on the configured path and on its canonical
//! form (for example `/var/run` → `/run`), must be root-owned and not group-
//! or other-writable, so no unprivileged user can substitute the endpoint.
//!
//! The client executable is trust-on-first-use pinned by digest for the life
//! of a boundary; it is not authenticated against an independent root.
//! Production use stays closed through the uninhabited
//! [`QualifiedImage`](super::docker::QualifiedImage).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::Path;

use error_stack::Report;

use super::host_inputs::HostInputs;
use crate::error::ReleaseError;
use crate::process::{BoundedCommand, ProcessLimits, ProcessResult};

/// The fixed Docker daemon endpoint.
pub(crate) const DOCKER_HOST: &str = "unix:///var/run/docker.sock";
/// The daemon socket behind [`DOCKER_HOST`].
pub(crate) const DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// Group and other write permission bits.
const SHARED_WRITE: u32 = 0o022;

fn daemon_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Requires every ancestor of `path` to be root-owned, and every ancestor
/// directory not group- or other-writable.
fn check_ancestors(path: &Path) -> Result<(), Report<ReleaseError>> {
    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(ancestor).map_err(|error| {
            daemon_error(format!("cannot inspect {}: {error}", ancestor.display()))
        })?;
        let shared = metadata.is_dir() && metadata.mode() & SHARED_WRITE != 0;
        if metadata.uid() != 0 || shared {
            return Err(daemon_error(format!(
                "Docker socket ancestor {} is not root-owned and protected",
                ancestor.display()
            )));
        }
    }
    Ok(())
}

/// Checks the daemon socket at `path`.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when the socket is missing, is an
/// alias, is not a root-owned socket, or any ancestor (configured or
/// canonical) is not root-owned or is group- or other-writable.
pub(crate) fn check_daemon_socket(path: &Path) -> Result<(), Report<ReleaseError>> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(daemon_error("Docker socket path has no parent"));
    };
    check_ancestors(path)?;
    let canonical = fs::canonicalize(parent)
        .map_err(|error| daemon_error(format!("cannot resolve Docker socket parent: {error}")))?
        .join(name);
    check_ancestors(&canonical)?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| daemon_error(format!("Docker daemon socket unavailable: {error}")))?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 {
        return Err(daemon_error(
            "Docker daemon endpoint is not a root-owned socket (aliases are rejected)",
        ));
    }
    Ok(())
}

/// Returns the client-level arguments selecting the empty config and daemon.
#[must_use]
pub(crate) fn host_arguments(config: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--config"),
        config.as_os_str().to_os_string(),
        OsString::from("--host"),
        OsString::from(DOCKER_HOST),
    ]
}

/// A pinned Docker client that rehashes its inputs before every command.
#[derive(Debug)]
pub(crate) struct DockerClient {
    inputs: HostInputs,
    host_limits: ProcessLimits,
}

impl DockerClient {
    /// Creates pinned host inputs under `state_parent`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] as [`HostInputs::create`] does.
    pub(crate) fn new(
        client: &Path,
        state_parent: &Path,
        mounted: &[&Path],
        host_limits: ProcessLimits,
    ) -> Result<Self, Report<ReleaseError>> {
        Ok(Self {
            inputs: HostInputs::create(state_parent, client, mounted)?,
            host_limits,
        })
    }

    /// Returns the pinned inputs.
    #[must_use]
    pub(crate) fn inputs(&self) -> &HostInputs {
        &self.inputs
    }

    /// Builds a client command after rehashing every pinned input and
    /// rechecking the daemon socket.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when an input changed or the socket
    /// check fails, or [`ReleaseError::Process`] for an invalid command.
    pub(crate) fn command(
        &self,
        arguments: Vec<OsString>,
        limits: ProcessLimits,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        self.inputs.verify()?;
        self.control_command(arguments, limits)
    }

    /// Builds a command after checking its client, config and daemon inputs.
    fn control_command(
        &self,
        arguments: Vec<OsString>,
        limits: ProcessLimits,
    ) -> Result<BoundedCommand, Report<ReleaseError>> {
        self.inputs.verify_control()?;
        check_daemon_socket(Path::new(DOCKER_SOCKET))?;
        Ok(
            BoundedCommand::new(self.inputs.client(), self.inputs.directory(), limits)?
                .arguments(arguments),
        )
    }

    fn full_arguments(&self, arguments: &[&str]) -> Vec<OsString> {
        let mut full = host_arguments(self.inputs.config());
        full.extend(arguments.iter().map(OsString::from));
        full
    }

    /// Runs one host command (`docker --config … --host … <arguments>`).
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when an input changed or the socket
    /// check fails before the command, or [`ReleaseError::Process`] when the
    /// child cannot be run.
    pub(crate) fn host(&self, arguments: &[&str]) -> Result<ProcessResult, Report<ReleaseError>> {
        self.command(self.full_arguments(arguments), self.host_limits)?
            .run()
    }

    /// Runs one cleanup command, rehashing inputs first when they are still
    /// intact. Profile-only failures are recorded while safe cleanup is
    /// attempted; a changed client or config prevents all execution.
    pub(crate) fn cleanup_host(
        &self,
        arguments: &[&str],
        failures: &mut Vec<Report<ReleaseError>>,
    ) -> Option<ProcessResult> {
        if let Err(integrity) = self.inputs.verify_control() {
            failures.push(integrity);
            return None;
        }
        let full = self.full_arguments(arguments);
        if let Err(integrity) = self.inputs.verify() {
            failures.push(integrity);
        }
        let command = self.control_command(full, self.host_limits);
        match command.and_then(|command| command.run()) {
            Ok(result) => Some(result),
            Err(error) => {
                failures.push(error);
                None
            }
        }
    }
}
