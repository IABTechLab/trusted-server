//! Real Docker container boundary tests (Linux only).
//!
//! On Linux these REQUIRE a reachable Docker daemon at the fixed socket and
//! fail otherwise; they never skip. The image comes from
//! `TS_RELEASE_TEST_IMAGE` (default `ubuntu:24.04`, whose glibc matches the
//! `ubuntu-24.04` runner that builds this test binary). A missing image is
//! pulled once under a bounded deadline; a failed pull fails the test.
//!
//! What the confined probe shows: a staged copy of the test binary,
//! bind-mounted read-only, attempts a TCP connect to `127.0.0.1:<port>`
//! inside the container. Under `--network none` that is the container's own
//! loopback, so without the effective seccomp profile the result would be
//! `ECONNREFUSED`; the proof requires `EPERM`, which only the seccomp socket
//! denial produces. It shows nothing about routes to other hosts.

use std::fs;
use std::os::unix::fs::symlink;

use tempfile::{TempDir, tempdir};

use super::super::daemon::DockerClient;
use super::super::docker::{DockerSettings, ImageSelection, container_path};
use super::super::probe::LoopbackListener;
use super::*;
use crate::process::{ProcessOutcome, ProcessResult};

/// The fixed Docker client on GitHub-hosted Ubuntu runners.
const DOCKER_CLIENT: &str = "/usr/bin/docker";
const TEST_IMAGE_VARIABLE: &str = "TS_RELEASE_TEST_IMAGE";
const DEFAULT_TEST_IMAGE: &str = "ubuntu:24.04";
/// `EROFS`: a write into a read-only bind mount.
const READ_ONLY_FILESYSTEM: &str = "denied errno=30";
/// `ESRCH`: the host pid does not exist in the container's PID namespace.
const NO_SUCH_PROCESS: &str = "denied errno=3";

fn host_limits() -> ProcessLimits {
    ProcessLimits {
        wall: Duration::from_secs(300),
        stdout_max: 1024 * 1024,
        stderr_max: 1024 * 1024,
    }
}

fn settings() -> DockerSettings {
    // SAFETY: `getuid`/`getgid` have no preconditions.
    let user = unsafe { (libc::getuid(), libc::getgid()) };
    DockerSettings {
        client: PathBuf::from(DOCKER_CLIENT),
        cpus: 1,
        memory_bytes: 256 * 1024 * 1024,
        pids_limit: 128,
        tmpfs_bytes: 16 * 1024 * 1024,
        user,
        stop_timeout_seconds: 5,
        host_limits: host_limits(),
    }
}

fn succeeded(result: &ProcessResult) -> bool {
    result.outcome() == (ProcessOutcome::Exited { code: 0 })
}

/// Resolves the test image to its local content ID, pulling it if absent.
fn test_image(client: &DockerClient) -> ImageSelection {
    let reference =
        std::env::var(TEST_IMAGE_VARIABLE).unwrap_or_else(|_| String::from(DEFAULT_TEST_IMAGE));
    let inspect = [
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        reference.as_str(),
    ];
    let mut result = client
        .host(&inspect)
        .expect("should run docker image inspect");
    if !succeeded(&result) {
        let pull = client
            .host(&["pull", reference.as_str()])
            .expect("should run docker pull");
        assert!(
            succeeded(&pull),
            "should pull test image {reference}: {pull:?}"
        );
        result = client
            .host(&inspect)
            .expect("should rerun docker image inspect");
    }
    assert!(succeeded(&result), "should inspect test image {reference}");
    let id = String::from_utf8_lossy(result.stdout()).trim().to_owned();
    ImageSelection::Test { id }
}

struct Fixture {
    _roots: TempDir,
    _state: TempDir,
    immutable: PathBuf,
    tools: PathBuf,
    writable: PathBuf,
    probe: ProbeProgram,
    boundary: Boundary,
}

/// Creates fixture roots under `base` and returns them.
fn create_roots(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let immutable = base.join("immutable");
    let tools = base.join("tools");
    let writable = base.join("writable");
    fs::create_dir_all(immutable.join("src")).expect("should create immutable root");
    fs::create_dir(&tools).expect("should create tool root");
    fs::create_dir(&writable).expect("should create writable root");
    fs::write(immutable.join("src/a.txt"), b"original").expect("should write source");
    (immutable, tools, writable)
}

/// Requires the daemon and resolves the test image.
fn reachable_image(state: &Path) -> ImageSelection {
    let client = DockerClient::new(Path::new(DOCKER_CLIENT), state, &[], host_limits())
        .expect("should pin docker client inputs");
    let version = client
        .host(&["version", "--format", "{{.Server.Version}}"])
        .expect("should run docker version");
    assert!(
        succeeded(&version),
        "should reach the Docker daemon at the fixed socket: {version:?}"
    );
    test_image(&client)
}

impl Fixture {
    fn new() -> Self {
        let roots_directory = tempdir().expect("should create roots directory");
        let state = tempdir().expect("should create state directory");
        let base = fs::canonicalize(roots_directory.path()).expect("should canonicalize");
        let (immutable, tools, writable) = create_roots(&base);
        let probe = staged_probe(&tools);
        let roots = BoundaryRoots::new(
            std::slice::from_ref(&immutable),
            std::slice::from_ref(&tools),
            std::slice::from_ref(&writable),
        )
        .expect("should accept roots");
        let image = reachable_image(state.path());
        let boundary = Boundary::linux_container(roots, settings(), image, state.path())
            .expect("should configure container boundary");
        Self {
            _roots: roots_directory,
            _state: state,
            immutable,
            tools,
            writable,
            probe,
            boundary,
        }
    }

    fn prove(&self) -> DenialProof {
        self.boundary
            .establish_denial(&self.probe, limits())
            .expect("should prove denial with a real control and denied container probe")
    }

    fn container(&self, host: &Path) -> String {
        container_path(&self.boundary.roots, host)
            .expect("should translate path")
            .to_string_lossy()
            .into_owned()
    }

    fn command(&self, mode: &str, arguments: &[&str]) -> ConfinedCommand {
        helper_command(self.probe.program(), &self.writable, mode, arguments)
    }

    fn run(&self, proof: &DenialProof, mode: &str, arguments: &[&str]) -> String {
        let result = self
            .boundary
            .run_confined(&self.command(mode, arguments), proof)
            .expect("should run confined helper");
        helper_report(result.stdout())
    }
}

#[test]
fn container_denies_process_memory_access() {
    assert_eq!(
        super::modes::process_memory_read(),
        0,
        "should permit the real self-memory operation outside confinement"
    );
    let fixture = Fixture::new();
    let proof = fixture.prove();
    assert_eq!(
        fixture.run(&proof, "process-memory-read", &[]),
        "denied errno=1",
        "should deny the same process-memory syscall with real seccomp EPERM"
    );
}

#[test]
fn container_denial_proof_and_confined_execution() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    assert_eq!(
        proof.denied_errno(),
        libc::EPERM,
        "should observe seccomp EPERM"
    );

    let listener = LoopbackListener::bind().expect("should bind listener");
    let address = listener.address().to_string();
    let marker = fixture.container(&fixture.writable);
    let report = fixture.run(&proof, "nest", &["2", &address, &marker]);
    assert_eq!(
        report, "denied errno=1",
        "should deny the grandchild's connect"
    );
    assert_eq!(
        listener.drain_accepts().expect("should drain"),
        0,
        "should see no accept"
    );
    for depth in 0..=2 {
        assert!(
            fixture.writable.join(format!("nest-{depth}")).exists(),
            "should record nesting level {depth}"
        );
    }

    let source = fixture.container(&fixture.immutable.join("src/a.txt"));
    assert_eq!(
        fixture.run(&proof, "write", &[&source]),
        READ_ONLY_FILESYSTEM
    );
    assert_eq!(
        fs::read(fixture.immutable.join("src/a.txt")).expect("should read"),
        b"original",
        "should keep the immutable source"
    );
    let private = fixture.container(&fixture.writable.join("out.txt"));
    assert_eq!(fixture.run(&proof, "write", &[&private]), "written");
    assert_eq!(
        fs::read(fixture.writable.join("out.txt")).expect("should read"),
        b"tampered",
        "should allow private writes"
    );
}

#[test]
fn container_denies_every_non_unix_socket_family() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    for domain in ["inet", "inet6", "netlink", "packet"] {
        assert_eq!(
            fixture.run(&proof, "socket", &[domain]),
            "denied errno=1",
            "should deny {domain} sockets with seccomp EPERM"
        );
    }
    assert_eq!(fixture.run(&proof, "socket", &["unix"]), "created");
    let socket = fixture.container(&fixture.writable.join("x.sock"));
    assert_eq!(
        fixture.run(&proof, "unix-exchange", &[&socket]),
        "exchanged"
    );
}

#[test]
fn container_cannot_signal_a_host_sibling() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let mut sibling = spawn_sibling();
    let pid = sibling.id().to_string();
    let kill = libc::SIGKILL.to_string();
    let report = fixture.run(&proof, "signal", &[&pid, &kill]);
    let alive = sibling.try_wait().expect("should poll sibling").is_none();
    sibling.kill().expect("should stop sibling");
    sibling.wait().expect("should reap sibling");
    assert_eq!(report, NO_SUCH_PROCESS, "should not see host pids");
    assert!(alive, "should leave the host sibling running");
}

#[test]
fn container_is_removed_after_a_timeout() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let mut command = fixture.command("sleep", &["30000"]);
    command.limits.wall = Duration::from_secs(3);
    let result = fixture
        .boundary
        .run_confined(&command, &proof)
        .expect("should confirm the container removed after the timeout");
    assert_eq!(
        result.outcome(),
        ProcessOutcome::TimedOut,
        "should time out"
    );
}

#[test]
fn container_rejects_unsafe_aliases_and_hard_links_in_mounts() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let cases: [(&Path, &str, &str); 4] = [
        (&fixture.writable, "absolute", "/etc"),
        (&fixture.immutable, "escaping", "../../outside"),
        (&fixture.immutable, "dangling", "src/missing"),
        (&fixture.tools, "dangling", "missing-tool"),
    ];
    for (root, name, target) in cases {
        let link = root.join(name);
        symlink(target, &link).expect("should create alias");
        assert!(
            fixture
                .boundary
                .run_confined(&fixture.command("sleep", &["0"]), &proof)
                .is_err(),
            "should reject {name} alias {target} in {root:?}"
        );
        fs::remove_file(&link).expect("should remove alias");
    }
    let alias = fixture.writable.join("prelink");
    fs::hard_link(fixture.immutable.join("src/a.txt"), &alias).expect("should hard link");
    let error = fixture
        .boundary
        .run_confined(&fixture.command("sleep", &["0"]), &proof)
        .expect_err("should reject a pre-existing hard link");
    assert!(
        format!("{error:?}").contains("hard links"),
        "should name the hard link"
    );
}

#[test]
fn docker_client_rehash_rejects_tampering_between_commands() {
    let state = tempdir().expect("should create state directory");
    let client = DockerClient::new(Path::new(DOCKER_CLIENT), state.path(), &[], host_limits())
        .expect("should pin inputs");
    let first = client
        .host(&["version", "--format", "{{.Client.Version}}"])
        .expect("should run first host command");
    assert!(succeeded(&first), "should run the pinned client");
    let profile = client.inputs().effective_profile().to_path_buf();
    fs::set_permissions(
        &profile,
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .expect("should chmod profile");
    fs::write(&profile, b"{}").expect("should tamper profile");
    assert!(
        client.host(&["version"]).is_err(),
        "should refuse the next host command after tampering"
    );
}

#[test]
fn container_command_reports_the_requested_umask() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let report = |umask: Option<u32>| {
        let mut command = fixture.command("umask", &[]);
        if let Some(mask) = umask {
            command = command.umask(Umask::new(mask).expect("should accept mask"));
        }
        let result = fixture
            .boundary
            .run_confined(&command, &proof)
            .expect("should run confined helper");
        helper_report(result.stdout())
    };
    let default = report(None);
    assert!(
        default.len() == 3 && default.bytes().all(|digit| (b'0'..=b'7').contains(&digit)),
        "should report the runtime default as three octal digits: {default}"
    );
    assert_eq!(report(Some(0o027)), "027", "should apply umask 027");
    assert_eq!(report(Some(0o077)), "077", "should apply umask 077");
}
