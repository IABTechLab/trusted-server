//! Clean-container startup acceptance of the final Linux `ts` bytes.
//!
//! The exact final executable is copied once into a private read-only
//! directory and started as `ts --version` in each reviewed clean
//! distribution: the glibc 2.34 floor (UBI 9) and two newer mainstream
//! distributions. Each container runs by local image ID with no network, a
//! read-only root, an unprivileged numeric user, every capability dropped,
//! no privilege escalation and bounded processes, memory and CPU time. No
//! operator configuration exists in the container, and `--version` must
//! print exactly the owning build facts and exit 0, so the startup proves
//! the loader floor and that no configuration fallback runs before
//! argument parsing. The copy must still hash to the accepted digest after
//! every run.

mod accepted;
mod lifecycle;
#[cfg(test)]
pub(crate) use accepted::NativeStartupAccepted;
pub(crate) mod native;

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::artifact::EXECUTABLE_LIMIT;
use super::compare::read_artifact;
use super::{RecipeCompleted, recipe_error};
use crate::boundary::daemon::{ContainerId, DockerClient, host_arguments};
use crate::documents::hash_bounded_with_check;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;
use crate::process::{ProcessLimits, ProcessOutcome, ProcessResult};
use crate::source::CommitId;

/// The only asset the Linux startup gate accepts.
pub(crate) const LINUX_CLI_ASSET: &str = "ts-x86_64-unknown-linux-gnu";
/// Numeric unprivileged account inside every startup container.
const STARTUP_USER: &str = "65534:65534";
/// Read-only mount point of the private copy inside the container.
const MOUNT_POINT: &str = "/opt/ts";
/// File name of the private copy.
const EXECUTABLE_NAME: &str = "ts";
/// Wall limit of attached execution; stopped-container creation is separately bounded.
const STARTUP_WALL: Duration = Duration::from_secs(120);
/// Captured stdout limit; the version line is short.
const STARTUP_STDOUT_MAX: u64 = 4096;
/// Captured stderr limit; the Docker client may warn about the kernel.
const STARTUP_STDERR_MAX: u64 = 64 * 1024;
/// Stderr bytes shown when a run is rejected.
const STDERR_TAIL_BYTES: usize = 4096;

/// One reviewed clean distribution the Linux executable must start on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupDistribution {
    /// Red Hat UBI 9, the glibc 2.34 floor.
    Ubi9,
    /// Ubuntu 22.04 LTS (glibc 2.35).
    Ubuntu2204,
    /// Debian 12 (glibc 2.36).
    Debian12,
}

impl StartupDistribution {
    /// Every reviewed distribution, in run order.
    pub(crate) const ALL: [Self; 3] = [Self::Ubi9, Self::Ubuntu2204, Self::Debian12];

    /// Stable label used by fixtures and measurements.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ubi9 => "ubi9",
            Self::Ubuntu2204 => "ubuntu-22.04",
            Self::Debian12 => "debian-12",
        }
    }

    /// The tag-pinned image reference the ordinary CI fixture pulls.
    ///
    /// A tag is not an identity: every run selects the pulled image by its
    /// local image ID, which the measurement records.
    pub(crate) fn reference(self) -> &'static str {
        match self {
            Self::Ubi9 => "registry.access.redhat.com/ubi9/ubi-minimal:9.6",
            Self::Ubuntu2204 => "docker.io/library/ubuntu:22.04",
            Self::Debian12 => "docker.io/library/debian:12",
        }
    }

    /// Parses exactly one [`Self::label`].
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] for any other text.
    pub(crate) fn parse(label: &str) -> Result<Self, Report<ReleaseError>> {
        Self::ALL
            .into_iter()
            .find(|distribution| distribution.label() == label)
            .ok_or_else(|| recipe_error(format!("unknown startup distribution {label:?}")))
    }
}

/// Reports whether `text` is a local image ID: `sha256:` and 64 lowercase
/// hex digits, never a tag or repository reference.
pub(crate) fn is_local_image_id(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// A reviewed distribution selected by its local image ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupImage {
    distribution: StartupDistribution,
    id: String,
}

impl StartupImage {
    /// Selects `distribution` by the local image ID `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] unless `id` is `sha256:` followed by
    /// 64 lowercase hex digits.
    pub(crate) fn new(
        distribution: StartupDistribution,
        id: &str,
    ) -> Result<Self, Report<ReleaseError>> {
        if !is_local_image_id(id) {
            return Err(recipe_error(format!(
                "startup image for {} is not a local image ID",
                distribution.label()
            )));
        }
        Ok(Self {
            distribution,
            id: id.to_owned(),
        })
    }

    /// The selected distribution.
    pub(crate) fn distribution(&self) -> StartupDistribution {
        self.distribution
    }

    /// The local image ID.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

/// A Docker client whose host inputs are pinned and checked before every command.
#[derive(Debug)]
pub(crate) struct StartupClient {
    client: DockerClient,
}

impl StartupClient {
    /// Pins the Docker client and fresh private host inputs outside mounted roots.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid, overlapping or unpinnable host inputs.
    pub(crate) fn new(
        docker: &Path,
        state_parent: &Path,
        mounted: &[&Path],
    ) -> Result<Self, Report<ReleaseError>> {
        Ok(Self {
            client: DockerClient::new(
                docker,
                state_parent,
                mounted,
                ProcessLimits {
                    wall: Duration::from_secs(15),
                    stdout_max: 4096,
                    stderr_max: STARTUP_STDERR_MAX,
                },
            )?,
        })
    }

    /// Requires creation acknowledgement before any attached start request.
    fn execute_created(
        &self,
        name: &str,
        create_arguments: Vec<OsString>,
        limits: ProcessLimits,
        before_start: impl FnOnce() -> Result<(), Report<ReleaseError>>,
        accept: impl FnOnce(&ProcessResult) -> Result<(), Report<ReleaseError>>,
    ) -> Result<ProcessResult, Report<ReleaseError>> {
        lifecycle::run_created(
            || {
                self.client.create(
                    create_arguments,
                    ProcessLimits {
                        wall: Duration::from_secs(15),
                        stdout_max: 4096,
                        stderr_max: STARTUP_STDERR_MAX,
                    },
                )
            },
            |identity| {
                before_start()?;
                let result = self
                    .client
                    .command(
                        startup_attach_arguments(self.client.inputs().config(), identity),
                        limits,
                    )?
                    .run()?;
                accept(&result)?;
                Ok(result)
            },
            |identity, step| self.cleanup(identity, name, step),
        )
    }

    /// Stops and removes only the uniquely named container owned by this run.
    fn cleanup(
        &self,
        identity: Option<&ContainerId>,
        name: &str,
        step: &str,
    ) -> Result<bool, Report<ReleaseError>> {
        let target = identity.map_or(name, ContainerId::as_str);
        let filter = cleanup_filter(identity, name);
        let arguments: &[&str] = match step {
            "stop" => &["stop", "--time", "2", target],
            "rm" => &["rm", "--force", target],
            "ps" => &["ps", "--all", "--quiet", "--filter", &filter],
            _ => return Err(recipe_error("unknown startup cleanup step")),
        };
        let mut failures = Vec::new();
        let result = self.client.cleanup_host(arguments, &mut failures);
        if !failures.is_empty() {
            let mut report = failures.remove(0);
            for failure in failures {
                report = report.attach(format!("cleanup also failed: {failure:?}"));
            }
            return Err(report);
        }
        Ok(result.is_some_and(|result| {
            result.outcome() == (ProcessOutcome::Exited { code: 0 })
                && (step != "ps" || result.stdout().is_empty())
        }))
    }
}

/// Selects the acknowledged immutable identity, or an unacknowledged owned name.
fn cleanup_filter(identity: Option<&ContainerId>, name: &str) -> String {
    identity.map_or_else(
        || format!("name=^/{name}$"),
        |id| format!("id={}", id.as_str()),
    )
}

/// Formats fixture version output using the current producer's workspace version.
///
/// Owning [`accepted::NativeStartupAccepted`] gates derive the CLI version from their
/// sealed selected-package observations instead of this fixture formatter.
pub(crate) fn expected_version_output(commit: &CommitId, target: ReleaseTarget) -> String {
    format!(
        "ts package={} source={} target={} variant=default\n",
        env!("CARGO_PKG_VERSION"),
        commit.as_str(),
        target.triple()
    )
}

/// Returns the Docker client arguments of one startup run.
pub(crate) fn startup_arguments(
    config: &Path,
    image: &StartupImage,
    copy_directory: &Path,
    name: &str,
) -> Vec<OsString> {
    let mut arguments = host_arguments(config);
    arguments.extend([
        "create".into(),
        "--init".into(),
        format!("--name={name}").into(),
        "--pull=never".into(),
        "--network=none".into(),
        "--read-only".into(),
        format!("--user={STARTUP_USER}").into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--pids-limit=64".into(),
        "--memory=268435456".into(),
        "--cpus=1".into(),
        "--ulimit=cpu=30:30".into(),
        "--workdir=/".into(),
    ]);
    let mut mount = OsString::from("--mount=type=bind,source=");
    mount.push(copy_directory.as_os_str());
    mount.push(format!(",target={MOUNT_POINT},readonly"));
    arguments.push(mount);
    arguments.push(format!("--entrypoint={MOUNT_POINT}/{EXECUTABLE_NAME}").into());
    arguments.push(image.id.clone().into());
    arguments.push("--version".into());
    arguments
}

/// Returns attached-start arguments for one acknowledged immutable identity.
pub(crate) fn startup_attach_arguments(config: &Path, identity: &ContainerId) -> Vec<OsString> {
    let mut arguments = host_arguments(config);
    arguments.extend(["start".into(), "--attach".into(), identity.as_str().into()]);
    arguments
}

/// Accepts one startup run's outcome and stdout.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] unless the run exited 0 and printed
/// exactly `expected`.
pub(crate) fn check_startup_output(
    distribution: StartupDistribution,
    outcome: ProcessOutcome,
    stdout: &[u8],
    expected: &str,
) -> Result<(), Report<ReleaseError>> {
    let label = distribution.label();
    if outcome != (ProcessOutcome::Exited { code: 0 }) {
        return Err(recipe_error(format!("ts --version on {label} {outcome}")));
    }
    if stdout != expected.as_bytes() {
        return Err(recipe_error(format!(
            "ts --version on {label} printed {:?}, expected {expected:?}",
            String::from_utf8_lossy(stdout)
        )));
    }
    Ok(())
}

/// One accepted startup run.
#[derive(Clone, Debug)]
pub(crate) struct StartupObserved {
    /// The image the run used.
    pub(crate) image: StartupImage,
    /// The exact accepted stdout.
    pub(crate) stdout: String,
    /// SHA-256 of the captured Docker client stderr.
    pub(crate) stderr_sha256: String,
    /// Wall time of the Docker client.
    pub(crate) elapsed: Duration,
}

/// A private read-only copy of the accepted final bytes.
#[derive(Debug)]
pub(crate) struct StartupCopy {
    directory: PathBuf,
    sha256: String,
    size: u64,
}

impl StartupCopy {
    /// Copies the accepted Linux executable into the fresh `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when `completed` is not the Linux
    /// CLI asset or no longer rehashes, `directory` is not absolute UTF-8
    /// free of the `,` and `=` that would split the bind mount, `directory`
    /// exists, or the copy cannot be written read-only.
    pub(crate) fn create(
        completed: &RecipeCompleted,
        directory: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        if completed.asset().artifact().name != LINUX_CLI_ASSET {
            return Err(recipe_error(format!(
                "the startup gate accepts only {LINUX_CLI_ASSET}"
            )));
        }
        let mountable = directory.is_absolute()
            && directory
                .to_str()
                .is_some_and(|text| !text.contains([',', '=']));
        if !mountable {
            return Err(recipe_error(
                "startup copy directory cannot be bind-mounted verbatim",
            ));
        }
        Self::write_private(completed, directory)
    }

    /// Writes the accepted bytes as the only `0555` file of the fresh
    /// `0555` `directory` and rehashes the copy.
    fn write_private(
        completed: &RecipeCompleted,
        directory: &Path,
    ) -> Result<Self, Report<ReleaseError>> {
        let bytes = read_artifact(completed)?;
        let io = |error: std::io::Error| recipe_error(format!("cannot copy for startup: {error}"));
        fs::create_dir(directory).map_err(io)?;
        let copy = Self {
            directory: directory.to_owned(),
            sha256: completed.artifact().sha256().to_owned(),
            size: completed.artifact().size(),
        };
        let path = directory.join(EXECUTABLE_NAME);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o555)
            .open(&path)
            .map_err(io)?;
        file.write_all(&bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).map_err(io)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).map_err(io)?;
        copy.verify()?;
        Ok(copy)
    }

    /// Requires the copy to still hash to the accepted digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when it cannot be read or changed.
    pub(crate) fn verify(&self) -> Result<(), Report<ReleaseError>> {
        let directory = fs::symlink_metadata(&self.directory)
            .map_err(|error| recipe_error(format!("cannot inspect startup directory: {error}")))?;
        if !directory.is_dir()
            || fs::canonicalize(&self.directory).ok().as_deref() != Some(&self.directory)
        {
            return Err(recipe_error(
                "startup directory is aliased or not a directory",
            ));
        }
        let mut entries = fs::read_dir(&self.directory)
            .map_err(|error| recipe_error(format!("cannot list startup directory: {error}")))?;
        let entry = entries
            .next()
            .transpose()
            .map_err(|error| recipe_error(format!("cannot inspect startup entry: {error}")))?;
        if entry.is_none_or(|entry| entry.file_name() != EXECUTABLE_NAME)
            || entries.next().is_some()
        {
            return Err(recipe_error(
                "startup directory must hold only the executable",
            ));
        }
        let digest = hash_bounded_with_check(
            &self.directory.join(EXECUTABLE_NAME),
            EXECUTABLE_LIMIT,
            &|| Ok(()),
        )?;
        if digest.sha256 != self.sha256 || digest.size != self.size {
            return Err(recipe_error("the startup copy changed"));
        }
        Ok(())
    }

    /// Starts the copy as `ts --version` in `image` and accepts the output.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Process`] when the Docker client cannot run,
    /// or [`ReleaseError::Recipe`] for a rejected outcome or output, or a
    /// copy that changed during the run.
    pub(crate) fn start(
        &self,
        client: &StartupClient,
        image: &StartupImage,
        expected: &str,
    ) -> Result<StartupObserved, Report<ReleaseError>> {
        let limits = ProcessLimits {
            wall: STARTUP_WALL,
            stdout_max: STARTUP_STDOUT_MAX,
            stderr_max: STARTUP_STDERR_MAX,
        };
        self.verify()?;
        client.client.require_no_image_volumes(image.id())?;
        let name = format!(
            "ts-release-startup-{}",
            hex::encode(crate::boundary::random_bytes::<12>()?)
        );
        self.verify()?;
        let result = client.execute_created(
            &name,
            startup_arguments(
                client.client.inputs().config(),
                image,
                &self.directory,
                &name,
            ),
            limits,
            || self.verify(),
            |result| {
                check_startup_output(
                    image.distribution,
                    result.outcome(),
                    result.stdout(),
                    expected,
                )
                .map_err(|error| {
                    let stderr = result.stderr();
                    let tail = &stderr[stderr.len().saturating_sub(STDERR_TAIL_BYTES)..];
                    error.attach(format!(
                        "docker stderr tail: {}",
                        String::from_utf8_lossy(tail)
                    ))
                })
            },
        );
        let after = self.verify();
        let result = lifecycle::combine(result, after)?;
        Ok(StartupObserved {
            image: image.clone(),
            stdout: expected.to_owned(),
            stderr_sha256: hex::encode(Sha256::digest(result.stderr())),
            elapsed: result.elapsed(),
        })
    }

    /// Restores write permission and removes the copy.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Recipe`] when the directory cannot be removed.
    pub(crate) fn remove(self) -> Result<(), Report<ReleaseError>> {
        self.remove_inner()
    }

    fn remove_inner(&self) -> Result<(), Report<ReleaseError>> {
        let io = |error: std::io::Error| recipe_error(format!("cannot remove the copy: {error}"));
        let metadata = fs::symlink_metadata(&self.directory).map_err(io)?;
        if !metadata.is_dir()
            || fs::canonicalize(&self.directory).ok().as_deref() != Some(&self.directory)
        {
            return Err(recipe_error("cannot remove an aliased startup directory"));
        }
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700)).map_err(io)?;
        fs::remove_dir_all(&self.directory).map_err(io)
    }
}

impl Drop for StartupCopy {
    fn drop(&mut self) {
        if self.directory.exists() {
            let _ = self.remove_inner();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_client_pins_its_executable_and_empty_config() {
        let scratch = tempfile::tempdir().expect("should create client scratch space");
        let base = fs::canonicalize(scratch.path()).expect("should resolve client scratch space");
        let program = base.join("docker");
        fs::write(&program, b"fictional Docker client bytes").expect("should write client bytes");
        let state = base.join("state");
        fs::create_dir(&state).expect("should create disjoint host state");
        let client = StartupClient::new(&program, &state, &[&base.join("mount")])
            .expect("should pin host inputs without launching a process");
        client
            .client
            .inputs()
            .verify()
            .expect("should verify fresh inputs");
        fs::write(&program, b"changed Docker client bytes").expect("should change selected client");
        assert!(
            client.client.inputs().verify().is_err(),
            "should reject a changed client before execution"
        );
        fs::write(&program, b"fictional Docker client bytes").expect("should restore client bytes");
        let config = client.client.inputs().config();
        fs::set_permissions(config, fs::Permissions::from_mode(0o700))
            .expect("should reopen the configuration for a tamper case");
        fs::write(config.join("config.json"), b"fictional configuration")
            .expect("should add unreviewed configuration");
        assert!(
            client.client.inputs().verify().is_err(),
            "should reject changed configuration before execution"
        );
    }

    #[test]
    fn acknowledged_container_is_attached_and_checked_by_immutable_id() {
        let text = "a".repeat(64);
        let identity = ContainerId::parse(text.as_bytes()).expect("should parse a full ID");
        let arguments = startup_attach_arguments(Path::new("/example/config"), &identity);
        let expected: Vec<OsString> = [
            "--config",
            "/example/config",
            "--host",
            "unix:///var/run/docker.sock",
            "start",
            "--attach",
            &text,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            arguments, expected,
            "should attach only to the acknowledged identity"
        );
        assert_eq!(
            cleanup_filter(Some(&identity), "ts-release-startup-example"),
            format!("id={text}"),
            "should confirm ID absence using the ID filter rather than a name filter"
        );
        assert_eq!(
            cleanup_filter(None, "ts-release-startup-example"),
            "name=^/ts-release-startup-example$",
            "should restrict unresolved creation cleanup to its owned name"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires explicit ordinary CI CLI fixture and Linux Docker; starts only its own named container"]
    fn startup_timeout_removes_only_its_owned_container() {
        let root = PathBuf::from(
            std::env::var_os("TS_RELEASE_CLI_FIXTURE_ROOT")
                .expect("should explicitly supply the fixture root"),
        );
        let raw = crate::documents::read_bounded(
            &root.join("cli-fixture.json"),
            crate::documents::Role::Metadata,
        )
        .expect("should read bounded fixture description");
        let document = crate::documents::parse_json(&raw.bytes, crate::documents::Role::Metadata)
            .expect("should parse fixture description");
        let id = document["startup_images"][1]["image_id"]
            .as_str()
            .expect("should name Ubuntu image");
        let image = StartupImage::new(StartupDistribution::Ubuntu2204, id)
            .expect("should select local image");
        let scratch = tempfile::tempdir().expect("should create private host state");
        let state = fs::canonicalize(scratch.path()).expect("should resolve host state");
        let client = StartupClient::new(Path::new("/usr/bin/docker"), &state, &[])
            .expect("should pin Docker host inputs");
        let name = format!(
            "ts-release-startup-{}",
            hex::encode(
                crate::boundary::random_bytes::<12>().expect("should choose an owned name")
            )
        );
        let mut arguments = host_arguments(client.client.inputs().config());
        arguments.extend(
            [
                "create",
                "--init",
                "--pull=never",
                "--network=none",
                "--read-only",
                "--user=65534:65534",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--pids-limit=16",
                "--memory=67108864",
                "--cpus=1",
            ]
            .map(OsString::from),
        );
        arguments.extend([
            format!("--name={name}").into(),
            "--entrypoint=/bin/sleep".into(),
            image.id().into(),
            "60".into(),
        ]);
        let result = client
            .execute_created(
                &name,
                arguments,
                ProcessLimits {
                    wall: Duration::from_millis(500),
                    stdout_max: 4096,
                    stderr_max: STARTUP_STDERR_MAX,
                },
                || Ok(()),
                |_| Ok(()),
            )
            .expect("should clean up after the bounded client timeout");
        assert_eq!(
            result.outcome(),
            ProcessOutcome::TimedOut,
            "should exercise a real client timeout"
        );
        assert!(
            client
                .cleanup(None, &name, "ps")
                .expect("should independently confirm absence"),
            "should leave no owned container"
        );
    }
}
