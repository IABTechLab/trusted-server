//! Platform-independent seccomp, Docker argv, daemon-socket and host-input
//! tests on real files.

use std::ffi::CString;
use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use super::super::daemon::DockerClient;
use super::super::daemon::check_daemon_socket;
use super::super::docker::{
    DockerSettings, RunPlan, container_path, mounts, run_arguments, validate_image_id,
};
use super::super::host_inputs::HostInputs;
use super::super::seccomp;
use super::unit::{canonical_tempdir, sample_roots};
use super::*;

#[test]
fn effective_seccomp_denies_inet_sockets_and_preserves_baseline() {
    let baseline = seccomp::baseline().expect("should match reviewed digest");
    let original: Value = serde_json::from_slice(baseline).expect("should parse baseline");
    let effective: Value =
        serde_json::from_slice(&seccomp::effective(baseline).expect("should derive profile"))
            .expect("should parse effective profile");
    for key in ["defaultAction", "defaultErrnoRet"] {
        assert_eq!(
            original.get(key),
            effective.get(key),
            "should preserve {key}"
        );
    }
    let rules = effective["syscalls"]
        .as_array()
        .expect("should have syscalls");
    let socket_rules: Vec<&Value> = rules
        .iter()
        .filter(|rule| {
            rule["names"].as_array().is_some_and(|names| {
                names.iter().any(|name| {
                    matches!(name.as_str(), Some("socket" | "socketpair" | "socketcall"))
                })
            })
        })
        .collect();
    assert_eq!(socket_rules.len(), 1, "should keep exactly one socket rule");
    assert_eq!(
        socket_rules[0]["args"],
        serde_json::json!([{"index": 0, "value": 1, "op": "SCMP_CMP_EQ"}]),
        "should allow only AF_UNIX"
    );
    let original_rules = original["syscalls"]
        .as_array()
        .expect("should have syscalls");
    assert_eq!(
        rules.len(),
        original_rules.len() - 5 + 1,
        "should drop three socket-only and two process-memory rules, then add the Unix socket rule"
    );
    let mut allowing = original.clone();
    allowing["defaultAction"] = Value::from("SCMP_ACT_ALLOW");
    let bytes = serde_json::to_vec(&allowing).expect("should serialize");
    assert!(
        seccomp::effective(&bytes).is_err(),
        "should reject a permissive baseline"
    );
}

fn docker_settings(client: &Path) -> DockerSettings {
    DockerSettings {
        client: client.to_path_buf(),
        cpus: 1,
        memory_bytes: 256 * 1024 * 1024,
        pids_limit: 128,
        tmpfs_bytes: 16 * 1024 * 1024,
        user: (1001, 1001),
        stop_timeout_seconds: 5,
        host_limits: limits(),
    }
}

#[test]
fn docker_run_arguments_are_exact() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let mounts = mounts(&roots).expect("should build mounts");
    let image = format!("sha256:{}", "b".repeat(64));
    let environment = [(
        OsString::from("HOME"),
        OsString::from("/boundary/writable/0"),
    )];
    let arguments = [OsString::from("--flag")];
    let plan = RunPlan {
        config: Path::new("/host/config"),
        seccomp: Path::new("/host/seccomp.json"),
        name: "ts-release-boundary-fixture",
        image: &image,
        mounts: &mounts,
        environment: &environment,
        working_directory: Path::new("/boundary/writable/0"),
        program: Path::new("/boundary/tool/0"),
        arguments: &arguments,
        interactive: false,
        umask: None,
    };
    let actual: Vec<String> = run_arguments(&docker_settings(Path::new("/usr/bin/docker")), &plan)
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    let root = base.to_str().expect("should be UTF-8");
    assert_eq!(
        actual,
        expected_run_arguments(root, &image),
        "should build the exact fixed argv"
    );
    assert!(
        !actual.iter().any(|part| part == "--pid"
            || part.starts_with("--pid=")
            || part == "-t"
            || part == "--tty"),
        "should never pass --pid or a TTY"
    );
}

/// The exact argv expected for the fixture plan rooted at `root`.
/// Fixed argv prefix expected before the mounts.
const EXPECTED_PREFIX: [&str; 33] = [
    "--config",
    "/host/config",
    "--host",
    "unix:///var/run/docker.sock",
    "run",
    "--name",
    "ts-release-boundary-fixture",
    "--rm",
    "--init",
    "--pull",
    "never",
    "--network",
    "none",
    "--ipc",
    "private",
    "--read-only",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges=true",
    "--security-opt",
    "seccomp=/host/seccomp.json",
    "--pids-limit",
    "128",
    "--cpus",
    "1",
    "--memory",
    "268435456b",
    "--user",
    "1001:1001",
    "--tmpfs",
    "/tmp:rw,noexec,nosuid,nodev,size=16777216",
    "--mount",
];

fn expected_run_arguments(root: &str, image: &str) -> Vec<String> {
    EXPECTED_PREFIX
    .iter()
    .map(|part| (*part).to_owned())
    .chain([
        format!("type=bind,source={root}/immutable,target=/boundary/immutable/0,readonly,bind-propagation=rprivate,bind-recursive=readonly"),
        String::from("--mount"),
        format!("type=bind,source={root}/tool,target=/boundary/tool/0,readonly,bind-propagation=rprivate,bind-recursive=readonly"),
        String::from("--mount"),
        format!("type=bind,source={root}/writable,target=/boundary/writable/0"),
    ])
    .chain(
        [
            "--env",
            "HOME=/boundary/writable/0",
            "--workdir",
            "/boundary/writable/0",
            image,
            "/boundary/tool/0",
            "--flag",
        ]
        .iter()
        .map(|part| (*part).to_owned()),
    )
    .collect()
}

#[test]
fn docker_paths_translate_and_reject_mount_metacharacters() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    assert_eq!(
        container_path(&roots, &base.join("writable/out/x")).expect("should translate"),
        PathBuf::from("/boundary/writable/0/out/x"),
        "should map into the writable mount"
    );
    assert!(
        container_path(&roots, Path::new("/elsewhere")).is_err(),
        "should reject paths outside every root"
    );
    let comma = base.join("with,comma");
    fs::create_dir(&comma).expect("should create directory");
    let roots = BoundaryRoots::new(&[], &[], &[comma]).expect("should accept roots");
    assert!(
        mounts(&roots).is_err(),
        "should reject ',' in mount sources"
    );
}

#[test]
fn docker_settings_fit_host_capacity() {
    let client = Path::new("/usr/bin/docker");
    assert!(
        docker_settings(client).validate().is_ok(),
        "should accept modest limits"
    );
    let parallelism = std::thread::available_parallelism().expect("should read parallelism");
    let mut settings = docker_settings(client);
    settings.cpus = u32::try_from(parallelism.get()).expect("should fit") + 1;
    assert!(
        settings.validate().is_err(),
        "should reject CPUs above host capacity"
    );
    for mutate in [
        (|settings: &mut DockerSettings| settings.cpus = 0) as fn(&mut DockerSettings),
        |settings| settings.memory_bytes = u64::MAX,
        |settings| settings.user = (0, 0),
        |settings| settings.stop_timeout_seconds = 0,
        |settings| settings.stop_timeout_seconds = 3600,
    ] {
        let mut settings = docker_settings(client);
        mutate(&mut settings);
        assert!(settings.validate().is_err(), "should reject {settings:?}");
    }
}

#[test]
fn image_must_be_pinned_by_content_id() {
    assert!(validate_image_id(&format!("sha256:{}", "c".repeat(64))).is_ok());
    for rejected in [
        "ubuntu:24.04",
        "sha256:abc",
        &format!("sha256:{}", "C".repeat(64)),
    ] {
        assert!(
            validate_image_id(rejected).is_err(),
            "should reject {rejected}"
        );
    }
}

fn host_inputs_fixture() -> (TempDir, PathBuf, HostInputs) {
    let (directory, base) = canonical_tempdir();
    let client = base.join("docker-client");
    fs::write(&client, b"fictional client").expect("should write client");
    let state = base.join("state");
    fs::create_dir(&state).expect("should create state parent");
    let inputs = HostInputs::create(&state, &client, &[]).expect("should create inputs");
    (directory, client, inputs)
}

fn make_writable(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("should chmod");
}

#[test]
fn host_inputs_rehash_detects_tampering_between_commands() {
    let (_directory, _client, inputs) = host_inputs_fixture();
    inputs.verify().expect("should verify fresh inputs");
    let effective = inputs.effective_profile().to_path_buf();
    make_writable(&effective, 0o644);
    fs::write(&effective, b"{}").expect("should tamper profile");
    assert!(
        inputs.verify().is_err(),
        "should detect a changed effective profile"
    );

    let (_directory, _client, inputs) = host_inputs_fixture();
    let baseline = inputs.directory().join("seccomp-baseline.json");
    make_writable(&baseline, 0o644);
    fs::write(&baseline, b"{}").expect("should tamper baseline");
    assert!(
        inputs.verify().is_err(),
        "should detect a changed baseline profile"
    );

    let (_directory, _client, inputs) = host_inputs_fixture();
    make_writable(inputs.config(), 0o700);
    fs::write(inputs.config().join("config.json"), b"{}").expect("should add config");
    assert!(inputs.verify().is_err(), "should detect a non-empty config");

    let (_directory, client, inputs) = host_inputs_fixture();
    fs::write(&client, b"substituted client").expect("should replace client");
    assert!(inputs.verify().is_err(), "should detect a changed client");

    let (_directory, _client, inputs) = host_inputs_fixture();
    fs::write(inputs.directory().join("extra"), b"x").expect("should add entry");
    assert!(inputs.verify().is_err(), "should detect an added entry");
}

#[test]
fn host_inputs_must_be_disjoint_from_mounted_roots() {
    let (_directory, base) = canonical_tempdir();
    let client = base.join("docker-client");
    fs::write(&client, b"fictional client").expect("should write client");
    let mounted = base.join("mounted");
    fs::create_dir_all(mounted.join("inner")).expect("should create mount");
    for parent in [base.clone(), mounted.clone(), mounted.join("inner")] {
        assert!(
            HostInputs::create(&parent, &client, &[&mounted]).is_err(),
            "should reject state parent {parent:?} overlapping a mount"
        );
    }
    let alias = base.join("client-alias");
    symlink(&client, &alias).expect("should create client alias");
    let state = base.join("state");
    fs::create_dir(&state).expect("should create state");
    assert!(
        HostInputs::create(&state, &alias, &[&mounted]).is_err(),
        "should reject an aliased client path"
    );
}

#[test]
fn docker_client_must_be_outside_every_mounted_root() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let state = base.join("state");
    fs::create_dir(&state).expect("should create state directory");
    let mounted: Vec<&Path> = roots.entries().map(|(_, _, path)| path).collect();
    for root in &mounted {
        let client = if root.is_dir() {
            root.join("docker-client")
        } else {
            root.to_path_buf()
        };
        fs::write(&client, b"fictional client").expect("should create fixture client");
        assert!(
            HostInputs::create(&state, &client, &mounted).is_err(),
            "should reject a host executable exposed through a mounted root"
        );
    }
}

#[test]
fn host_input_fifo_is_rejected_without_waiting_for_a_writer() {
    let (_directory, base) = canonical_tempdir();
    let client = base.join("docker-client");
    let state = base.join("state");
    fs::create_dir(&state).expect("should create state directory");
    let name = CString::new(client.as_os_str().as_bytes()).expect("should encode FIFO path");
    // SAFETY: The path is NUL-terminated and remains valid during mkfifo.
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
        0,
        "should create FIFO"
    );
    let (sender, receiver) = mpsc::channel();
    let reader_path = client.clone();
    let reader = thread::spawn(move || {
        let rejected = HostInputs::create(&state, &reader_path, &[]).is_err();
        let _ = sender.send(rejected);
    });
    let result = receiver.recv_timeout(Duration::from_millis(200));
    // Release a regressed blocking open before joining, so failure cannot hang CI.
    let _keeper = File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&client)
        .expect("should open nonblocking FIFO cleanup handle");
    reader.join().expect("should reap input reader");
    assert_eq!(result, Ok(true), "should reject FIFO without a writer");
}

#[test]
fn cleanup_never_retries_through_changed_host_inputs() {
    let (_directory, base) = canonical_tempdir();
    let client_path = base.join("docker-client");
    fs::write(&client_path, b"fictional client").expect("should create fixture client");
    let client =
        DockerClient::new(&client_path, &base, &[], limits()).expect("should create pinned client");
    fs::write(&client_path, b"substituted client").expect("should replace fixture client");
    let mut failures = Vec::new();
    assert!(
        client.cleanup_host(&["version"], &mut failures).is_none(),
        "should reject cleanup after integrity failure"
    );
    assert_eq!(
        failures.len(),
        1,
        "should stop at integrity failure before endpoint inspection or execution"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn container_boundary_is_refused_off_linux() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let settings = docker_settings(Path::new("/usr/bin/docker"));
    let image = super::super::docker::ImageSelection::Test {
        id: format!("sha256:{}", "a".repeat(64)),
    };
    assert!(
        Boundary::linux_container(roots, settings, image, &base).is_err(),
        "should refuse the container boundary off Linux"
    );
}

#[test]
fn daemon_socket_rejects_user_owned_aliased_and_shared_paths() {
    let (_directory, base) = canonical_tempdir();
    let socket = base.join("d.sock");
    let _listener = UnixListener::bind(&socket).expect("should bind fixture socket");
    let error = check_daemon_socket(&socket).expect_err("should reject a user-owned path");
    assert!(
        format!("{error:?}").contains("root-owned"),
        "should explain ownership"
    );
    let alias = base.join("alias.sock");
    symlink(&socket, &alias).expect("should alias socket");
    assert!(
        check_daemon_socket(&alias).is_err(),
        "should reject an alias"
    );
    assert!(
        check_daemon_socket(&base.join("missing.sock")).is_err(),
        "should reject a missing socket"
    );
    if cfg!(target_os = "macos") {
        // Root-owned socket under the root-owned but group-writable
        // /private/var/run: rejected for its shared-writable ancestor.
        assert!(
            check_daemon_socket(Path::new("/var/run/mDNSResponder")).is_err(),
            "should reject a socket under a group-writable directory"
        );
    }
}

#[test]
fn effective_seccomp_admits_only_native_architectures() {
    let baseline = seccomp::baseline().expect("should match reviewed digest");
    let original: Value = serde_json::from_slice(baseline).expect("should parse baseline");
    let effective: Value =
        serde_json::from_slice(&seccomp::effective(baseline).expect("should derive profile"))
            .expect("should parse effective profile");
    let native = |profile: &Value| -> Vec<Value> {
        profile["archMap"]
            .as_array()
            .expect("should have archMap")
            .iter()
            .map(|entry| entry["architecture"].clone())
            .collect()
    };
    assert_eq!(
        native(&original),
        native(&effective),
        "should keep native ABIs"
    );
    for entry in effective["archMap"]
        .as_array()
        .expect("should have archMap")
    {
        assert_eq!(
            entry["subArchitectures"],
            serde_json::json!([]),
            "should admit no non-native ABI (x86/x32 socketcall)"
        );
    }
}

#[test]
fn effective_seccomp_has_no_process_memory_or_descriptor_transfer_allowance() {
    let effective: Value = serde_json::from_slice(
        &seccomp::effective(seccomp::baseline().expect("should authenticate baseline"))
            .expect("should derive effective profile"),
    )
    .expect("should parse profile");
    for rule in effective["syscalls"].as_array().expect("should list rules") {
        for name in rule["names"].as_array().expect("should list syscall names") {
            assert!(
                ![
                    "ptrace",
                    "process_vm_readv",
                    "process_vm_writev",
                    "pidfd_getfd",
                    "kcmp",
                    "process_madvise"
                ]
                .contains(&name.as_str().expect("should have syscall name")),
                "should remove process-memory and descriptor access from every allowance"
            );
        }
    }
}

/// Returns the argv tail after the image for a fixture plan.
fn argv_after_image(umask: Option<Umask>, program: &str, arguments: &[&str]) -> Vec<String> {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let mounts = mounts(&roots).expect("should build mounts");
    let image = format!("sha256:{}", "b".repeat(64));
    let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let plan = RunPlan {
        config: Path::new("/host/config"),
        seccomp: Path::new("/host/seccomp.json"),
        name: "ts-release-boundary-fixture",
        image: &image,
        mounts: &mounts,
        environment: &[],
        working_directory: Path::new("/boundary/writable/0"),
        program: Path::new(program),
        arguments: &arguments,
        interactive: false,
        umask,
    };
    let argv: Vec<String> = run_arguments(&docker_settings(Path::new("/usr/bin/docker")), &plan)
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    let position = argv
        .iter()
        .position(|part| *part == image)
        .expect("should place the image in argv");
    argv[position + 1..].to_vec()
}

#[test]
fn umask_validation_accepts_only_permission_bits() {
    for valid in [0, 0o022, 0o027, 0o777] {
        assert_eq!(
            Umask::new(valid)
                .expect("should accept a permission mask")
                .mask(),
            valid,
            "should round-trip {valid:#o}"
        );
    }
    for invalid in [0o1000, 0o4777, 0x1_0000, u32::MAX] {
        assert!(
            Umask::new(invalid).is_err(),
            "should reject {invalid:#o} outside 0..=0o777"
        );
    }
}

#[test]
fn umask_displays_as_three_octal_digits() {
    let rendered: Vec<String> = [0, 0o7, 0o022, 0o027, 0o777]
        .into_iter()
        .map(|mask| Umask::new(mask).expect("should accept mask").to_string())
        .collect();
    assert_eq!(
        rendered,
        ["000", "007", "022", "027", "777"],
        "should render zero-padded three-digit octal"
    );
}

#[test]
fn docker_argv_without_umask_runs_the_program_directly() {
    assert_eq!(
        argv_after_image(None, "/boundary/tool/0", &["--flag"]),
        ["/boundary/tool/0", "--flag"],
        "should not involve a shell without a umask"
    );
}

#[test]
fn docker_argv_with_umask_uses_the_fixed_shell_idiom() {
    let umask = Umask::new(0o027).expect("should accept mask");
    assert_eq!(
        argv_after_image(Some(umask), "/boundary/tool/0", &["--flag", "value"]),
        [
            "/bin/sh",
            "-c",
            "umask 027 && exec \"$@\"",
            "sh",
            "/boundary/tool/0",
            "--flag",
            "value"
        ],
        "should wrap the program in the fixed umask script with positional arguments"
    );
}

#[test]
fn docker_argv_keeps_shell_metacharacters_positional() {
    let umask = Umask::new(0o022).expect("should accept mask");
    let hostile = [
        "$(touch /boundary/writable/0/pwned)",
        "`id`",
        "a; rm -rf /",
        "'quoted' \"double\" && || | > <",
        "-c",
        "$@",
        "",
    ];
    let tail = argv_after_image(Some(umask), "/boundary/tool/0;$(id)", &hostile);
    assert_eq!(
        tail[..4],
        ["/bin/sh", "-c", "umask 022 && exec \"$@\"", "sh"],
        "should keep the script fixed whatever the program and arguments are"
    );
    assert_eq!(
        tail[4], "/boundary/tool/0;$(id)",
        "should pass the program as one positional word"
    );
    assert_eq!(
        tail[5..],
        hostile,
        "should pass every hostile argument verbatim as its own word"
    );
}
