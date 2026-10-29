//! Linux Docker container boundary.
//!
//! Every container runs with `--init` in Docker's default private PID
//! namespace (never the invalid `--pid=private`), `--network none`, the
//! effective seccomp profile (every non-Unix socket denied with `EPERM`), a read-only
//! root filesystem, all capabilities dropped, `no-new-privileges`, private
//! IPC without a `/dev/shm` mount, an empty read-only `/tmp`, a non-root user
//! and CPU, memory and process limits that fit the
//! actual host. The client is a fixed absolute executable invoked with an
//! explicit empty `--config` and the fixed Unix daemon socket, a cleared
//! environment (no `DOCKER_HOST`, `DOCKER_CONTEXT` or `DOCKER_CONFIG`), no TTY
//! and `--pull never`.
//!
//! Mounts use `--mount type=bind`, which fails on a missing source instead of
//! creating it the way `-v` does. Immutable roots and tools are mounted
//! read-only and recursively read-only (`bind-recursive=readonly`), writable
//! roots read-write, at fixed `/boundary/<role>/<index>` targets.
//!
//! Creation must acknowledge a full container ID before a separate attached
//! start can execute the guest. A failed creation remains unresolved and fatal;
//! a later name lookup cannot exclude a delayed daemon-side creation.
//! Every acknowledged start is followed, on success and failure alike, by bounded
//! `docker stop --time <n>`, `docker rm --force` and a listing that must show
//! the acknowledged container ID gone. Each step is attempted even when an earlier one
//! failed, and every failure is reported. If the pinned host inputs fail
//! their integrity check during cleanup, cleanup still proceeds through the
//! pinned client path (a left-running container is the worse outcome) and the
//! integrity failure is returned.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::daemon::{
    ContainerId, DOCKER_SOCKET, DockerClient, ImageConfiguration, check_daemon_socket,
    host_arguments,
};
use super::roots::{BoundaryRoots, RootRole, validate_path_text};
use super::{ConfinedCommand, Umask};
use crate::error::ReleaseError;
use crate::process::{ProcessLimits, ProcessOutcome, ProcessResult};

/// Container-namespace prefix for every mount target.
const MOUNT_PREFIX: &str = "/boundary";
/// Configuration fingerprint domain.
const FINGERPRINT_DOMAIN: &[u8] = b"trusted-server-boundary-docker-image-config-v3\n";
/// Fixed absolute shell of the selected image that applies a requested umask.
const SHELL: &str = "/bin/sh";
/// Fixed `argv[0]` of [`SHELL`]; the program and its arguments follow it.
const SHELL_NAME: &str = "sh";

fn docker_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

fn fingerprint_parts(
    parts: impl IntoIterator<Item = OsString>,
    image: &ImageConfiguration,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    let configuration = image.canonical();
    hasher.update((configuration.len() as u64).to_be_bytes());
    hasher.update(configuration);
    for part in parts {
        let bytes = part.as_encoded_bytes();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod image_configuration_tests {
    use serde_json::json;

    use super::{ImageConfiguration, fingerprint_parts};

    #[test]
    fn fingerprint_binds_actual_entrypoint_and_environment_fields() {
        let initial = json!({"Volumes":null,"Entrypoint":null,"Env":null});
        let fingerprint = |value| {
            let bytes = serde_json::to_vec(&value).expect("should encode retained configuration");
            let configuration =
                ImageConfiguration::parse(&bytes).expect("should accept reviewed fields");
            fingerprint_parts(
                ["same-image".into(), "same-arguments".into()],
                &configuration,
            )
        };
        let original = fingerprint(initial.clone());
        for (key, value) in [
            ("Entrypoint", json!([])),
            (
                "Env",
                json!(["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]),
            ),
            ("Env", json!(["container=oci"])),
        ] {
            let mut changed = initial.clone();
            changed[key] = value;
            assert_ne!(
                original,
                fingerprint(changed),
                "should bind each inspected image field even with identical arguments and image ID"
            );
        }
        assert_eq!(
            original,
            fingerprint(initial),
            "should retain stable fingerprints for identical inspected values"
        );
    }
}

/// A production OCI image that passed Task 8 qualification.
///
/// No image is qualified yet (`release-tools.json` keeps the GNU builder
/// `image_reference` null), so this type is uninhabited and production
/// selection stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualifiedImage {}

/// The image a container boundary runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageSelection {
    /// A qualified production image (currently impossible to construct).
    Qualified(QualifiedImage),
    /// A test-only local image resolved to its content ID.
    #[cfg(test)]
    Test { id: String },
}

impl ImageSelection {
    fn into_id(self) -> String {
        match self {
            Self::Qualified(image) => match image {},
            #[cfg(test)]
            Self::Test { id } => id,
        }
    }
}

/// Validates a local image content ID (`sha256:<64 lowercase hex>`).
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] for anything else.
pub(crate) fn validate_image_id(id: &str) -> Result<(), Report<ReleaseError>> {
    let valid = id.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    });
    if !valid {
        return Err(docker_error("image is not pinned by a sha256 content ID"));
    }
    Ok(())
}

/// Container resource and cleanup settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockerSettings {
    /// Absolute fixed Docker client executable.
    pub(crate) client: PathBuf,
    /// Whole CPUs; never more than the host's available parallelism.
    pub(crate) cpus: u32,
    /// Memory limit in bytes; never more than host physical memory.
    pub(crate) memory_bytes: u64,
    /// Maximum container processes.
    pub(crate) pids_limit: u32,
    /// Size bound of the empty read-only `/tmp` tmpfs in bytes.
    pub(crate) tmpfs_bytes: u64,
    /// Non-root container user and group IDs.
    pub(crate) user: (u32, u32),
    /// `docker stop -t` grace in seconds.
    pub(crate) stop_timeout_seconds: u32,
    /// Limits for Docker creation and host cleanup commands.
    pub(crate) host_limits: ProcessLimits,
}

/// Returns host physical memory in bytes, when the OS reports it.
fn physical_memory() -> Option<u64> {
    // SAFETY: `sysconf` has no preconditions and only reads system values.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let pages = u64::try_from(pages).ok()?;
    let page_size = u64::try_from(page_size).ok()?;
    pages.checked_mul(page_size)
}

impl DockerSettings {
    /// Checks the settings against the actual host capacity.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for zero limits, CPUs above
    /// [`std::thread::available_parallelism`], memory above physical memory,
    /// a root user, or a stop grace not below the host command deadline.
    pub(crate) fn validate(&self) -> Result<(), Report<ReleaseError>> {
        let parallelism = std::thread::available_parallelism()
            .map_err(|error| docker_error(format!("cannot read host parallelism: {error}")))?;
        let parallelism = u32::try_from(parallelism.get()).unwrap_or(u32::MAX);
        if self.cpus == 0 || self.cpus > parallelism {
            return Err(docker_error(format!(
                "container CPUs {} must be between 1 and host parallelism {parallelism}",
                self.cpus
            )));
        }
        let memory =
            physical_memory().ok_or_else(|| docker_error("cannot read host physical memory"))?;
        if self.memory_bytes == 0 || self.memory_bytes > memory {
            return Err(docker_error(format!(
                "container memory {} must be nonzero and at most host memory {memory}",
                self.memory_bytes
            )));
        }
        if self.pids_limit == 0 || self.tmpfs_bytes == 0 || self.user.0 == 0 {
            return Err(docker_error(
                "pids, tmpfs and non-root user must all be set",
            ));
        }
        let stop = std::time::Duration::from_secs(u64::from(self.stop_timeout_seconds));
        if self.stop_timeout_seconds == 0 || stop >= self.host_limits.wall {
            return Err(docker_error(
                "stop grace must be nonzero and below the host deadline",
            ));
        }
        Ok(())
    }
}

/// One bind mount into the container namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    source: PathBuf,
    target: String,
    read_only: bool,
}

fn role_directory(role: RootRole) -> &'static str {
    match role {
        RootRole::Immutable => "immutable",
        RootRole::Tool => "tool",
        RootRole::Writable => "writable",
        RootRole::System => "system",
    }
}

/// Returns the container path of `host`, which must lie inside a root.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when `host` is outside every root.
pub(crate) fn container_path(
    roots: &BoundaryRoots,
    host: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    let (role, index, relative) = roots.locate(host).ok_or_else(|| {
        docker_error(format!("{} is outside every boundary root", host.display()))
    })?;
    let base = PathBuf::from(format!("{MOUNT_PREFIX}/{}/{index}", role_directory(role)));
    Ok(if relative.as_os_str().is_empty() {
        base
    } else {
        base.join(relative)
    })
}

/// Returns the host path of a container path inside a writable root.
///
/// Only writable roots map back: a build must never name an immutable or tool
/// file as its output. Dot segments are rejected rather than resolved.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when `container` is not an absolute
/// path below `/boundary/writable/<index>` with a canonical decimal index of
/// an existing writable root and only normal components after it.
pub(crate) fn host_path(
    roots: &BoundaryRoots,
    container: &Path,
) -> Result<PathBuf, Report<ReleaseError>> {
    // `components` silently drops `.`, doubled and trailing separators, so
    // lexical normal form is checked against the original text.
    if container.components().collect::<PathBuf>().as_os_str() != container.as_os_str() {
        return Err(docker_error("container path is not in lexical normal form"));
    }
    let prefix = Path::new(MOUNT_PREFIX).join(role_directory(RootRole::Writable));
    let relative = container.strip_prefix(&prefix).map_err(|_| {
        docker_error(format!(
            "{} is not below the writable mount prefix",
            container.display()
        ))
    })?;
    let mut components = relative.components();
    let index = components
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(text) => text.to_str(),
            _ => None,
        })
        .filter(|text| {
            text.bytes().all(|byte| byte.is_ascii_digit())
                && (*text == "0" || !text.starts_with('0'))
        })
        .and_then(|text| text.parse::<usize>().ok())
        .ok_or_else(|| docker_error("container path has no canonical writable root index"))?;
    let root = roots
        .writable()
        .get(index)
        .ok_or_else(|| docker_error(format!("no writable root has index {index}")))?;
    let mut host = root.clone();
    for component in components {
        match component {
            std::path::Component::Normal(part) => host.push(part),
            _ => {
                return Err(docker_error(
                    "container path has a dot segment after its root",
                ));
            }
        }
    }
    Ok(host)
}

/// Builds the bind mounts for `roots`.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when a source path contains `,` or `=`,
/// which the `--mount` field syntax cannot carry unambiguously, or when any
/// system root is present: the container exposes only reviewed image tools,
/// never a host system directory.
pub(crate) fn mounts(roots: &BoundaryRoots) -> Result<Vec<Mount>, Report<ReleaseError>> {
    if !roots.system().is_empty() {
        return Err(docker_error(
            "the container boundary refuses host system roots",
        ));
    }
    roots
        .entries()
        .map(|(role, _, source)| {
            let text = validate_path_text(source)?;
            if text.contains(',') || text.contains('=') {
                return Err(docker_error(format!(
                    "mount source {text:?} contains ',' or '='"
                )));
            }
            Ok(Mount {
                source: source.to_path_buf(),
                target: container_path(roots, source)?
                    .to_string_lossy()
                    .into_owned(),
                read_only: role != RootRole::Writable,
            })
        })
        .collect()
}

/// Everything one `docker run` invocation needs besides settings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunPlan<'a> {
    pub(crate) config: &'a Path,
    pub(crate) seccomp: &'a Path,
    pub(crate) name: &'a str,
    pub(crate) image: &'a str,
    pub(crate) mounts: &'a [Mount],
    pub(crate) environment: &'a [(OsString, OsString)],
    pub(crate) working_directory: &'a Path,
    pub(crate) program: &'a Path,
    pub(crate) arguments: &'a [OsString],
    pub(crate) interactive: bool,
    /// Mask applied before the program through [`SHELL`], when set.
    pub(crate) umask: Option<Umask>,
}

fn mount_argument(mount: &Mount) -> OsString {
    let mut value = OsString::from("type=bind,source=");
    value.push(&mount.source);
    value.push(",target=");
    value.push(&mount.target);
    if mount.read_only {
        value.push(",readonly,bind-propagation=rprivate,bind-recursive=readonly");
    }
    value
}

/// Fixed isolation flags; deliberately no `--pid` (the default namespace is
/// already private) and no `--tty`.
const ISOLATION_ARGUMENTS: [&str; 12] = [
    "--init",
    "--pull",
    "never",
    "--network",
    "none",
    "--ipc",
    "none",
    "--read-only",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges=true",
];

fn limit_arguments(settings: &DockerSettings) -> [String; 10] {
    [
        String::from("--pids-limit"),
        settings.pids_limit.to_string(),
        String::from("--cpus"),
        settings.cpus.to_string(),
        String::from("--memory"),
        format!("{}b", settings.memory_bytes),
        String::from("--user"),
        format!("{}:{}", settings.user.0, settings.user.1),
        String::from("--tmpfs"),
        format!("/tmp:ro,noexec,nosuid,nodev,size={}", settings.tmpfs_bytes),
    ]
}

/// Returns the fixed shell words that apply `umask` before `exec "$@"`.
///
/// The script is built only from the validated octal digits; the program and
/// its arguments follow as positional parameters and are never interpolated.
fn umask_prefix(umask: Option<Umask>) -> Vec<OsString> {
    umask
        .map(|umask| {
            [
                String::from(SHELL),
                String::from("-c"),
                format!("umask {umask} && exec \"$@\""),
                String::from(SHELL_NAME),
            ]
            .into_iter()
            .map(OsString::from)
            .collect()
        })
        .unwrap_or_default()
}

/// Returns the complete fixed `docker create` argument vector for `plan`.
#[must_use]
pub(crate) fn create_arguments(settings: &DockerSettings, plan: &RunPlan<'_>) -> Vec<OsString> {
    let mut arguments = host_arguments(plan.config);
    arguments.extend(["create", "--name", plan.name].iter().map(OsString::from));
    arguments.push(OsString::from("--entrypoint="));
    arguments.push(OsString::from("--no-healthcheck"));
    arguments.extend(ISOLATION_ARGUMENTS.iter().map(OsString::from));
    let mut seccomp = OsString::from("seccomp=");
    seccomp.push(plan.seccomp);
    arguments.extend([OsString::from("--security-opt"), seccomp]);
    arguments.extend(limit_arguments(settings).into_iter().map(OsString::from));
    if plan.interactive {
        arguments.push(OsString::from("--interactive"));
    }
    for mount in plan.mounts {
        arguments.extend([OsString::from("--mount"), mount_argument(mount)]);
    }
    for (name, value) in plan.environment {
        let mut assignment = name.clone();
        assignment.push("=");
        assignment.push(value);
        arguments.extend([OsString::from("--env"), assignment]);
    }
    arguments.extend([
        OsString::from("--workdir"),
        plan.working_directory.as_os_str().to_os_string(),
        OsString::from(plan.image),
    ]);
    arguments.extend(umask_prefix(plan.umask));
    arguments.push(plan.program.as_os_str().to_os_string());
    arguments.extend(plan.arguments.iter().cloned());
    arguments
}

fn container_name() -> Result<String, Report<ReleaseError>> {
    Ok(format!(
        "ts-release-boundary-{}",
        hex::encode(super::random_bytes::<12>()?)
    ))
}

/// Folds `failures` into one report, or `Ok` when there are none.
fn aggregate(mut failures: Vec<Report<ReleaseError>>) -> Result<(), Report<ReleaseError>> {
    if failures.is_empty() {
        return Ok(());
    }
    let first = failures.remove(0);
    Err(failures.into_iter().fold(first, |report, other| {
        report.attach(format!("additional cleanup failure: {other:?}"))
    }))
}

/// A configured container boundary.
#[derive(Debug)]
pub(crate) struct DockerBoundary {
    client: DockerClient,
    settings: DockerSettings,
    /// Content ID of the [`ImageSelection`] this boundary was configured with.
    image_id: String,
    image_configuration: ImageConfiguration,
    mounts: Vec<Mount>,
}

impl DockerBoundary {
    /// Validates settings, the daemon socket and mounts, then pins inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for invalid settings, an unusable
    /// daemon socket, an unpinned image, unmountable roots or host inputs
    /// that overlap a mounted root, or an image with declared volumes,
    /// a nonempty entrypoint or an unapproved environment entry.
    /// Container creation always disables inherited image healthchecks.
    pub(crate) fn new(
        roots: &BoundaryRoots,
        settings: DockerSettings,
        image: ImageSelection,
        state_parent: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        Self::configure(roots, settings, image.into_id(), state_parent)
    }

    /// Configures a boundary for an image ID taken from an [`ImageSelection`].
    fn configure(
        roots: &BoundaryRoots,
        settings: DockerSettings,
        image_id: String,
        state_parent: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        settings.validate()?;
        validate_image_id(&image_id)?;
        check_daemon_socket(Path::new(DOCKER_SOCKET))?;
        let mounts = mounts(roots)?;
        let mounted: Vec<&Path> = mounts.iter().map(|mount| mount.source.as_path()).collect();
        let client = DockerClient::new(
            &settings.client,
            state_parent,
            &mounted,
            settings.host_limits,
        )?;
        let image_configuration = client.builder_image_configuration(&image_id)?;
        Ok(Self {
            client,
            settings,
            image_id,
            image_configuration,
            mounts,
        })
    }

    /// Configures a sibling container with the same image and settings over
    /// `roots`, with fresh host inputs beside this boundary's inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] as [`Self::new`] does, or when the
    /// sibling pinned a different client or seccomp profile.
    pub(crate) fn sibling(&self, roots: &BoundaryRoots) -> Result<Self, Report<ReleaseError>> {
        let state_parent = self
            .client
            .inputs()
            .directory()
            .parent()
            .ok_or_else(|| docker_error("Docker host input directory has no parent"))?;
        let sibling = Self::configure(
            roots,
            self.settings.clone(),
            self.image_id.clone(),
            state_parent,
        )?;
        if sibling.client.inputs().digests() != self.client.inputs().digests()
            || sibling.image_configuration != self.image_configuration
        {
            return Err(docker_error(
                "sibling container pinned different host inputs or image configuration",
            ));
        }
        Ok(sibling)
    }

    /// Returns the configuration fingerprint (before the instance nonce).
    #[must_use]
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        let plan = RunPlan {
            config: self.client.inputs().config(),
            seccomp: self.client.inputs().effective_profile(),
            name: "",
            image: &self.image_id,
            mounts: &self.mounts,
            environment: &[],
            working_directory: Path::new("/"),
            program: Path::new("/"),
            arguments: &[],
            interactive: false,
            umask: None,
        };
        let digests = self.client.inputs().digests();
        let parts = create_arguments(&self.settings, &plan)
            .into_iter()
            .chain(digests.iter().map(OsString::from));
        fingerprint_parts(parts, &self.image_configuration)
    }

    /// Runs `command` in a fresh container, then always cleans it up.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] for changed host inputs, a failed
    /// daemon-socket check or failed cleanup, or [`ReleaseError::Process`]
    /// when the client cannot be run.
    pub(crate) fn run(
        &self,
        roots: &BoundaryRoots,
        command: &ConfinedCommand,
    ) -> Result<ProcessResult, Report<ReleaseError>> {
        let name = container_name()?;
        let working_directory = container_path(roots, &command.working_directory)?;
        let program = container_path(roots, &command.program)?;
        let plan = RunPlan {
            config: self.client.inputs().config(),
            seccomp: self.client.inputs().effective_profile(),
            name: &name,
            image: &self.image_id,
            mounts: &self.mounts,
            environment: &command.environment,
            working_directory: &working_directory,
            program: &program,
            arguments: &command.arguments,
            interactive: command.stdin.is_some(),
            umask: command.umask,
        };
        let arguments = create_arguments(&self.settings, &plan);
        complete_created(
            self.client.create(arguments, self.settings.host_limits),
            |id| {
                self.client
                    .command(
                        start_arguments(self.client.inputs().config(), id, command.stdin.is_some()),
                        command.limits,
                    )
                    .and_then(|bounded| command.finish(bounded).run())
            },
            |id| self.cleanup(&name, id),
        )
    }

    /// Attempts bounded stop, remove and a confirming listing independently,
    /// and aggregates every failure, including a host-input integrity
    /// failure observed during cleanup.
    fn cleanup(&self, name: &str, id: Option<&ContainerId>) -> Result<(), Report<ReleaseError>> {
        let grace = self.settings.stop_timeout_seconds.to_string();
        let (reference, filter) = cleanup_target(name, id);
        let mut failures = Vec::new();
        // A stopped or already removed container can make stop or rm fail;
        // the final listing confirms absence of the exact acknowledged ID.
        // After failed creation, name absence never resolves the creation error.
        let _ = self
            .client
            .cleanup_host(&["stop", "--time", &grace, reference], &mut failures);
        let _ = self
            .client
            .cleanup_host(&["rm", "--force", reference], &mut failures);
        let listing = self.client.cleanup_host(
            &["ps", "--all", "--quiet", "--filter", &filter],
            &mut failures,
        );
        let gone = listing.as_ref().is_some_and(|listing| {
            listing.outcome() == (ProcessOutcome::Exited { code: 0 }) && listing.stdout().is_empty()
        });
        if !gone {
            failures.push(docker_error(format!(
                "container {reference} was not confirmed removed"
            )));
        }
        aggregate(failures)
    }
}

#[cfg(test)]
mod host_path_tests;

/// Attaches to a confirmed container, preserving its configured stdin behavior.
#[must_use]
pub(crate) fn start_arguments(config: &Path, id: &ContainerId, interactive: bool) -> Vec<OsString> {
    let mut arguments = host_arguments(config);
    arguments.extend([OsString::from("start"), OsString::from("--attach")]);
    if interactive {
        arguments.push(OsString::from("--interactive"));
    }
    arguments.push(OsString::from(id.as_str()));
    arguments
}

/// Starts only acknowledged creations and always attempts the matching cleanup.
///
/// # Errors
///
/// Returns the creation or start failure together with any cleanup failure.
/// Observing absence after failed creation never clears the creation error.
pub(crate) fn complete_created<T>(
    created: Result<ContainerId, Report<ReleaseError>>,
    start: impl FnOnce(&ContainerId) -> Result<T, Report<ReleaseError>>,
    cleanup: impl FnOnce(Option<&ContainerId>) -> Result<(), Report<ReleaseError>>,
) -> Result<T, Report<ReleaseError>> {
    let (outcome, cleanup) = match created {
        Ok(id) => (start(&id), cleanup(Some(&id))),
        Err(error) => (Err(error), cleanup(None)),
    };
    match (outcome, cleanup) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.attach(format!("cleanup also failed: {cleanup:?}")))
        }
    }
}

/// Selects ID-based cleanup after acknowledgment, or name lookup for unresolved creation.
pub(crate) fn cleanup_target<'a>(name: &'a str, id: Option<&'a ContainerId>) -> (&'a str, String) {
    match id {
        Some(id) => (id.as_str(), format!("id={}", id.as_str())),
        None => (name, format!("name=^/{name}$")),
    }
}
