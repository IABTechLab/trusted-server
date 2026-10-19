//! Real `sandbox-exec` boundary tests (macOS only).
//!
//! These must run outside an enclosing application sandbox: the unconfined
//! control needs local socket permission, and without it no proof is
//! constructed and the tests fail instead of passing vacuously.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::time::SystemTime;

use tempfile::{TempDir, tempdir};

use super::super::macos::{MacProfile, RunIdentity};
use super::super::probe::LoopbackListener;
use super::super::survivors::{RunSignature, matching};
use super::*;
use crate::process::ProcessOutcome;

/// A sandbox fixture with one immutable root, a tool root holding a staged
/// copy of the test binary and one private writable root.
struct Fixture {
    _directory: TempDir,
    immutable: PathBuf,
    writable: PathBuf,
    probe: ProbeProgram,
    boundary: Boundary,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().expect("should create fixture directory");
        let base = fs::canonicalize(directory.path()).expect("should canonicalize fixture");
        let immutable = base.join("immutable");
        let writable = base.join("writable");
        let tools = base.join("tools");
        fs::create_dir_all(immutable.join("src")).expect("should create immutable root");
        fs::create_dir(&writable).expect("should create writable root");
        fs::create_dir(&tools).expect("should create tool root");
        fs::write(immutable.join("src/a.txt"), b"original").expect("should write source");
        let probe = staged_probe(&tools);
        let roots = BoundaryRoots::new(
            std::slice::from_ref(&immutable),
            &[tools],
            std::slice::from_ref(&writable),
        )
        .expect("should accept fixture roots");
        let boundary = Boundary::mac_sandbox_for_tests(roots).expect("should configure sandbox");
        Self {
            _directory: directory,
            immutable,
            writable,
            probe,
            boundary,
        }
    }

    fn prove(&self) -> DenialProof {
        self.boundary
            .establish_denial(&self.probe, limits())
            .expect("should prove denial with a real control and denied probe")
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

fn path_text(path: &Path) -> &str {
    path.to_str().expect("should be UTF-8")
}

#[test]
fn selected_cargo_starts_without_reading_ambient_openssl_configuration() {
    let directory = tempdir().expect("should create startup fixture");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize startup fixture");
    let immutable = base.join("source");
    let writable = base.join("private");
    fs::create_dir(&immutable).expect("should create source");
    fs::create_dir(&writable).expect("should create private state");
    let cargo = fs::canonicalize(env!("CARGO")).expect("should locate selected Cargo");
    let roots = BoundaryRoots::new(
        &[immutable],
        std::slice::from_ref(&cargo),
        std::slice::from_ref(&writable),
    )
    .expect("should bind startup roots");
    let profile = MacProfile::generate(&roots).expect("should generate actual profile");
    let run = RunIdentity::new(&profile).expect("should create private run markers");
    // These direct version/read commands create no descendants. Use the actual
    // profile without the global survivor scan exercised by separate tests.
    let execute = |program: &Path, arguments: &[&str]| {
        let command = ConfinedCommand::new(program, &writable, limits())
            .expect("should describe harmless command")
            .arguments(arguments.iter().map(OsString::from))
            .environment("OPENSSL_CONF", "/dev/null")
            .expect("should normalize optional OpenSSL configuration")
            .environment("LC_ALL", "C")
            .expect("should normalize diagnostic locale")
            .environment("HOME", &writable)
            .expect("should use private home")
            .environment("CARGO_HOME", &writable)
            .expect("should use private Cargo home");
        profile
            .wrap(&command, &run)
            .expect("should wrap command")
            .run()
            .expect("should finish bounded command")
    };
    let version = execute(&cargo, &["--version"])
        .require_success()
        .expect("should initialize selected Cargo without ambient configuration");
    assert!(
        version.starts_with(b"cargo "),
        "should run actual selected Cargo"
    );
    for alias in ["/private/etc/ssl/openssl.cnf", "/etc/ssl/openssl.cnf"] {
        let denied = execute(Path::new("/bin/cat"), &[alias]);
        assert_ne!(
            denied.outcome(),
            ProcessOutcome::Exited { code: 0 },
            "should deny configuration contents"
        );
        assert!(
            denied.stdout().is_empty(),
            "should expose no configuration bytes"
        );
        assert!(
            String::from_utf8_lossy(denied.stderr()).contains("No such file or directory"),
            "should report optional configuration as absent"
        );
    }
    let unrelated = execute(Path::new("/bin/cat"), &["/private/etc/hosts"]);
    assert_ne!(
        unrelated.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should retain unrelated read denial"
    );
    assert!(
        unrelated.stdout().is_empty(),
        "should expose no unrelated host input"
    );
    assert!(
        String::from_utf8_lossy(unrelated.stderr()).contains("Operation not permitted"),
        "should retain ordinary denial outside the exact aliases"
    );
}

#[test]
fn confined_probe_is_denied_with_eperm_and_no_accept() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    assert_eq!(
        proof.denied_errno(),
        libc::EPERM,
        "should observe real EPERM"
    );

    let listener = LoopbackListener::bind().expect("should bind listener");
    let address = listener.address().to_string();
    let report = fixture.run(&proof, "connect", &[&address]);
    assert_eq!(report, "denied errno=1", "should deny confined connect");
    assert_eq!(
        listener.drain_accepts().expect("should drain"),
        0,
        "should observe no accept from the confined probe"
    );
}

#[test]
fn grandchild_inherits_denial() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let listener = LoopbackListener::bind().expect("should bind listener");
    let address = listener.address().to_string();
    let report = fixture.run(
        &proof,
        "nest",
        &["2", &address, path_text(&fixture.writable)],
    );
    assert_eq!(
        report, "denied errno=1",
        "should deny the grandchild's connect"
    );
    assert_eq!(
        listener.drain_accepts().expect("should drain"),
        0,
        "should see no accept"
    );
    let pids: Vec<String> = (0..=2)
        .map(|depth| {
            fs::read_to_string(fixture.writable.join(format!("nest-{depth}")))
                .expect("should record every nesting level")
        })
        .collect();
    assert!(
        pids[0] != pids[1] && pids[1] != pids[2] && pids[0] != pids[2],
        "should run each level as a distinct process: {pids:?}"
    );
}

#[test]
fn private_unix_exchange_succeeds_while_inet_stays_denied() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let socket = fixture.writable.join("x.sock");
    assert_eq!(
        fixture.run(&proof, "unix-exchange", &[path_text(&socket)]),
        "exchanged"
    );
    let outside = fixture.immutable.join("x.sock");
    assert_eq!(
        fixture.run(&proof, "unix-exchange", &[path_text(&outside)]),
        "denied errno=1",
        "should deny Unix sockets outside the writable roots"
    );
    let listener = LoopbackListener::bind().expect("should bind listener");
    let address = listener.address().to_string();
    assert_eq!(
        fixture.run(&proof, "connect", &[&address]),
        "denied errno=1"
    );
}

#[test]
fn writes_are_denied_in_read_only_roots_and_allowed_in_private_roots() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let denied = fixture.immutable.join("src/new.txt");
    let existing = fixture.immutable.join("src/a.txt");
    for target in [&denied, &existing, &fixture.probe.program().to_path_buf()] {
        assert_eq!(
            fixture.run(&proof, "write", &[path_text(target)]),
            "denied errno=1",
            "should deny writing {target:?}"
        );
    }
    assert!(
        !denied.exists(),
        "should not create files in the immutable root"
    );
    assert_eq!(
        fs::read(&existing).expect("should read"),
        b"original",
        "should keep source"
    );

    let private = fixture.writable.join("out.txt");
    assert_eq!(
        fixture.run(&proof, "write", &[path_text(&private)]),
        "written"
    );
    assert_eq!(
        fs::read(&private).expect("should read"),
        b"tampered",
        "should allow private writes"
    );
}

#[test]
fn signals_are_limited_to_the_same_sandbox() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    assert_eq!(
        fixture.run(&proof, "signal-own-child", &[]),
        "signalled",
        "should signal its own child"
    );

    let mut sibling = spawn_sibling();
    let sibling_pid = sibling.id().to_string();
    let kill = libc::SIGKILL.to_string();
    assert_eq!(
        fixture.run(&proof, "signal", &[&sibling_pid, &kill]),
        "denied errno=1",
        "should not signal an outside same-user sibling"
    );
    let alive = sibling.try_wait().expect("should poll sibling").is_none();
    sibling.kill().expect("should stop sibling");
    sibling.wait().expect("should reap sibling");
    assert!(alive, "should leave the sibling running");

    let orchestrator = std::process::id().to_string();
    assert_eq!(
        fixture.run(&proof, "signal", &[&orchestrator, "0"]),
        "denied errno=1",
        "should not signal the orchestrator"
    );
}

#[test]
fn setsid_and_setpgid_are_denied() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    for mode in ["setsid", "setpgid"] {
        assert_eq!(
            fixture.run(&proof, mode, &[]),
            "denied errno=1",
            "should deny {mode} inside the sandbox"
        );
    }
}

#[test]
fn same_group_orphan_is_killed_with_the_run() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let pid_file = fixture.writable.join("orphan.pid");
    assert_eq!(
        fixture.run(&proof, "orphan", &[path_text(&pid_file)]),
        "spawned"
    );
    let pid = read_pid(&pid_file);
    assert!(wait_gone(pid), "should kill the same-group orphan {pid}");
}

#[test]
fn group_escape_keeping_the_run_nonce_is_killed_and_rejected() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let pid_file = fixture.writable.join("escape.pid");
    let error = fixture
        .boundary
        .run_confined(
            &fixture.command("escape", &[path_text(&pid_file), "keep"]),
            &proof,
        )
        .expect_err("should reject a run whose descendant escaped the group");
    assert!(
        format!("{error:?}").contains("escaped"),
        "should name the escape: {error:?}"
    );
    let pid = read_pid(&pid_file);
    assert!(wait_gone(pid), "should kill the escaped descendant {pid}");
}

#[test]
fn group_escape_with_scrubbed_environment_is_killed_and_rejected() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let pid_file = fixture.writable.join("scrubbed.pid");
    let outcome = fixture.boundary.run_confined(
        &fixture.command("escape", &[path_text(&pid_file), "scrub"]),
        &proof,
    );
    let pid = read_pid(&pid_file);
    let survived = process_exists(pid);
    if survived {
        // SAFETY: `pid` is the fixture's own escaped helper (> 1).
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let error = outcome.expect_err("should reject a run whose scrubbed descendant escaped");
    assert!(
        format!("{error:?}").contains("escaped"),
        "should name the escape: {error:?}"
    );
    assert!(
        !survived,
        "should kill the scrubbed escaped descendant {pid} before returning"
    );
}

/// Collect-only: lists matches without signalling anything.
#[test]
fn fresh_run_signature_matches_no_existing_process() {
    let directory = tempdir().expect("should create marker directory");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize markers");
    let allow = base.join("allow");
    let deny = base.join("deny");
    fs::write(&allow, b"").expect("should write allow marker");
    fs::write(&deny, b"").expect("should write deny marker");
    let matches = matching(&RunSignature {
        variable: "TS_RELEASE_BOUNDARY_RUN",
        nonce: "fictional-unused-nonce",
        allow: &allow,
        deny: &deny,
        started_after_micros: 0,
    })
    .expect("should query every process without signalling it");
    assert!(
        matches.is_empty(),
        "should match no unrelated sandboxed or unsandboxed process: {matches:?}"
    );
}

/// Collect-only: the single carrier is this test's own child.
#[test]
fn processes_started_before_the_run_are_never_matched() {
    let directory = tempdir().expect("should create marker directory");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize markers");
    let allow = base.join("allow");
    let deny = base.join("deny");
    fs::write(&allow, b"").expect("should write allow marker");
    fs::write(&deny, b"").expect("should write deny marker");
    let nonce = "fictional-start-guard-nonce";
    // Apple platform binaries hide their environment from KERN_PROCARGS2.
    let sleeper = base.join("sleep");
    fs::copy("/bin/sleep", &sleeper).expect("should copy a visible sleeper");
    let mut carrier = std::process::Command::new(&sleeper)
        .arg("30")
        .env("TS_RELEASE_BOUNDARY_RUN", nonce)
        .spawn()
        .expect("should start an own nonce carrier");
    let carrier_pid = libc::pid_t::try_from(carrier.id()).expect("should fit a pid");
    std::thread::sleep(Duration::from_millis(100));
    let signature = |started_after_micros| RunSignature {
        variable: "TS_RELEASE_BOUNDARY_RUN",
        nonce,
        allow: &allow,
        deny: &deny,
        started_after_micros,
    };
    let future = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|now| now + Duration::from_secs(3600))
        .ok()
        .and_then(|future| u64::try_from(future.as_micros()).ok());
    let eligible = matching(&signature(0));
    let later = future.map(|future| matching(&signature(future)));
    // Stop the own carrier before any assertion can unwind.
    carrier.kill().expect("should stop own carrier");
    carrier.wait().expect("should reap own carrier");
    let eligible = eligible.expect("should list eligible carriers");
    let later = later
        .expect("should compute a later run start")
        .expect("should list carriers started after a later run");
    assert_eq!(
        eligible,
        vec![carrier_pid],
        "should match exactly the own carrier"
    );
    assert!(
        later.is_empty(),
        "should never match a process that started before the run: {later:?}"
    );
}

#[test]
fn concurrent_run_of_another_instance_is_not_reaped() {
    let first = Fixture::new();
    let first_proof = first.prove();
    let second = Fixture::new();
    let second_proof = second.prove();
    std::thread::scope(|scope| {
        let long = scope.spawn(|| {
            second
                .boundary
                .run_confined(&second.command("sleep", &["1500"]), &second_proof)
        });
        std::thread::sleep(Duration::from_millis(300));
        let short = first
            .boundary
            .run_confined(&first.command("sleep", &["0"]), &first_proof)
            .expect("should not count another instance's live run as a survivor");
        assert_eq!(
            short.outcome(),
            ProcessOutcome::Exited { code: 0 },
            "should complete the short run"
        );
        let long = long
            .join()
            .expect("should join the concurrent run")
            .expect("should leave the other instance's run untouched");
        assert_eq!(
            long.outcome(),
            ProcessOutcome::Exited { code: 0 },
            "should let the other instance finish normally"
        );
    });
}

#[test]
fn escaped_scrubbed_descendant_inherits_denial() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let listener = LoopbackListener::bind().expect("should bind listener");
    let address = listener.address().to_string();
    assert_eq!(
        fixture.run(&proof, "escape-connect", &[&address]),
        "denied errno=1",
        "should deny a new-group descendant with a scrubbed environment"
    );
    assert_eq!(
        listener.drain_accepts().expect("should drain"),
        0,
        "should see no accept from the escaped descendant"
    );
}

#[test]
fn runtime_hard_link_from_immutable_root_is_denied() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let source = fixture.immutable.join("src/a.txt");
    let alias = fixture.writable.join("runtime-link");
    assert_eq!(
        fixture.run(&proof, "link", &[path_text(&source), path_text(&alias)]),
        "denied errno=1",
        "should deny creating a write-through alias during the run"
    );
    assert!(!alias.exists(), "should leave no alias behind");
    assert_eq!(
        fs::metadata(&source).expect("should stat source").nlink(),
        1,
        "should keep the immutable file single-linked"
    );
}

#[test]
fn pre_existing_hard_link_is_rejected_before_execution() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let source = fixture.immutable.join("src/a.txt");
    let alias = fixture.writable.join("prelink");
    fs::hard_link(&source, &alias).expect("should hard link into the private root");

    let rejected = fixture
        .boundary
        .run_confined(&fixture.command("write", &[path_text(&alias)]), &proof)
        .expect_err("should reject the hard-linked immutable file");
    assert!(
        format!("{rejected:?}").contains("hard links"),
        "should name the hard link"
    );
    assert_eq!(
        fs::read(&source).expect("should read"),
        b"original",
        "should not run the helper"
    );
}

#[test]
fn hard_linked_tool_is_rejected_before_execution() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    fs::hard_link(fixture.probe.program(), fixture.writable.join("tool-alias"))
        .expect("should hard link the tool");
    let rejected = fixture
        .boundary
        .run_confined(&fixture.command("sleep", &["0"]), &proof)
        .expect_err("should reject a hard-linked tool");
    assert!(
        format!("{rejected:?}").contains("hard links"),
        "should name the hard link"
    );
}

#[test]
fn hard_link_write_through_bypasses_path_denial_and_restoration_hides_it() {
    let fixture = Fixture::new();
    let source = fixture.immutable.join("src/a.txt");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).expect("should chmod");
    let before = fs::metadata(&source).expect("should stat source");
    let alias = fixture.writable.join("prelink");
    fs::hard_link(&source, &alias).expect("should hard link");
    fs::set_permissions(&alias, fs::Permissions::from_mode(0o644)).expect("should chmod alias");

    // Deliberately skip the boundary's nlink scan to show what it prevents.
    let profile = MacProfile::generate(&fixture.boundary.roots).expect("should generate");
    let run = RunIdentity::new(&profile).expect("should create run markers");
    let result = profile
        .wrap(&fixture.command("write", &[path_text(&alias)]), &run)
        .expect("should wrap")
        .run()
        .expect("should run unscanned helper");
    assert_eq!(
        helper_report(result.stdout()),
        "written",
        "should write through the link"
    );
    assert_eq!(
        fs::read(&source).expect("should read"),
        b"tampered",
        "should reach the source"
    );

    restore_and_compare(&source, &alias, &before);
}

/// Restores content, mtime and mode through `alias` and shows the source
/// then matches its pre-tamper content, mode and mtime.
fn restore_and_compare(source: &Path, alias: &Path, before: &fs::Metadata) {
    fs::write(alias, b"original").expect("should restore content");
    let modified = before.modified().expect("should read mtime");
    fs::File::options()
        .write(true)
        .open(alias)
        .and_then(|file| file.set_modified(modified))
        .expect("should restore mtime");
    fs::set_permissions(source, fs::Permissions::from_mode(0o444)).expect("should restore mode");
    let after = fs::metadata(source).expect("should stat source");
    assert_eq!(
        fs::read(source).expect("should read"),
        b"original",
        "should match content"
    );
    assert_eq!(
        after.permissions(),
        before.permissions(),
        "should match mode"
    );
    assert_eq!(after.modified().ok(), Some(modified), "should match mtime");
}

/// Mutates `path` from an unconfined thread while a confined helper sleeps.
fn mutate_during_run(fixture: &Fixture, proof: &DenialProof, path: PathBuf) -> String {
    let started = SystemTime::now();
    let mutator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        fs::write(&path, b"mutated").expect("should mutate outside the sandbox");
    });
    let outcome = fixture
        .boundary
        .run_confined(&fixture.command("sleep", &["1500"]), proof);
    mutator.join().expect("should join mutator");
    let elapsed = started.elapsed().expect("should measure");
    assert!(
        elapsed >= Duration::from_millis(300),
        "should overlap the mutation"
    );
    format!("{:?}", outcome.expect_err("should reject a changed root"))
}

#[test]
fn immutable_and_tool_mutation_during_run_is_detected() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let error = mutate_during_run(&fixture, &proof, fixture.immutable.join("src/a.txt"));
    assert!(
        error.contains("changed"),
        "should report the immutable change"
    );

    let fixture = Fixture::new();
    let proof = fixture.prove();
    let tool = fixture
        .probe
        .program()
        .parent()
        .expect("should have tool root")
        .join("extra-tool");
    let error = mutate_during_run(&fixture, &proof, tool);
    assert!(
        error.contains("changed"),
        "should report the tool-root change"
    );
}

#[test]
fn unsafe_writable_alias_left_by_a_run_is_rejected() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let writable = fixture.writable.clone();
    let creator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        symlink("/etc", writable.join("absolute-alias")).expect("should create alias");
    });
    let outcome = fixture
        .boundary
        .run_confined(&fixture.command("sleep", &["1000"]), &proof);
    creator.join().expect("should join creator");
    let error = outcome.expect_err("should reject an unsafe alias after the run");
    assert!(
        format!("{error:?}").contains("absolute"),
        "should name the alias"
    );
}

#[test]
fn proof_is_bound_to_its_instance() {
    let first = Fixture::new();
    let proof = first.prove();
    let twin = Boundary::mac_sandbox_for_tests(first.boundary.roots.clone())
        .expect("should configure an identical twin");
    let error = twin
        .run_confined(&first.command("sleep", &["0"]), &proof)
        .expect_err("should reject a proof from another instance");
    assert!(
        format!("{error:?}").contains("different boundary"),
        "should explain"
    );
    let result = first
        .boundary
        .run_confined(&first.command("sleep", &["0"]), &proof)
        .expect("should accept its own proof");
    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should run"
    );
}

#[test]
fn programs_outside_the_roots_are_rejected() {
    let fixture = Fixture::new();
    let proof = fixture.prove();
    let command = ConfinedCommand::new(Path::new("/usr/bin/true"), &fixture.writable, limits())
        .expect("should describe command");
    assert!(
        fixture.boundary.run_confined(&command, &proof).is_err(),
        "should reject a program outside every root"
    );
}

/// Reads the umask a helper inherits from this process, outside any sandbox.
fn unconfined_umask() -> String {
    let mut command = StdCommand::new(test_binary());
    command
        .args(helper_arguments(&[]))
        .env_clear()
        .stdin(std::process::Stdio::null());
    for (name, value) in helper_environment("umask") {
        command.env(name, value);
    }
    let output = command.output().expect("should run the unconfined helper");
    helper_report(&output.stdout)
}

#[test]
fn confined_command_reports_the_requested_umask() {
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
    assert_eq!(
        report(None),
        unconfined_umask(),
        "should inherit the orchestrator umask without a request"
    );
    assert_eq!(report(Some(0o027)), "027", "should apply umask 027");
    assert_eq!(report(Some(0o077)), "077", "should apply umask 077");
}
