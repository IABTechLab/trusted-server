//! Platform-independent boundary tests on real files and processes.

use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use tempfile::{TempDir, tempdir};

use super::super::macos::{MacProfile, SYS_SETPGID, SYS_SETSID, define};
use super::super::probe::LoopbackListener;
use super::super::roots::validate_path_text;
use super::*;
use crate::process::{BoundedCommand, ProcessOutcome};

pub(super) fn canonical_tempdir() -> (TempDir, PathBuf) {
    let directory = tempdir().expect("should create temporary directory");
    let canonical = fs::canonicalize(directory.path()).expect("should canonicalize tempdir");
    (directory, canonical)
}

fn error_text(report: &Report<ReleaseError>) -> String {
    format!("{report:?}")
}

// ---------------------------------------------------------------------------
// Probe protocol
// ---------------------------------------------------------------------------

#[test]
fn probe_report_round_trips_and_rejects_malformed_lines() {
    for report in [ProbeReport::Connected, ProbeReport::Denied { errno: 1 }] {
        let line = format!("{report}\n");
        assert_eq!(
            ProbeReport::parse_line(line.as_bytes()),
            Some(report),
            "should round-trip {report}"
        );
    }
    for malformed in [
        &b"connected"[..],
        b"denied errno=\n",
        b"denied errno=-1\n",
        b"denied errno=1 \n",
        b"connected\nconnected\n",
        b"refused\n",
    ] {
        assert_eq!(
            ProbeReport::parse_line(malformed),
            None,
            "should reject {malformed:?}"
        );
    }
}

#[test]
fn probe_address_must_be_literal_loopback() {
    for accepted in ["127.0.0.1:80", "[::1]:443"] {
        assert!(parse_loopback(accepted).is_ok(), "should accept {accepted}");
    }
    for rejected in [
        "10.0.0.1:80",
        "192.0.2.1:80",
        "localhost:80",
        "127.0.0.1:0",
        "127.0.0.1",
    ] {
        assert!(
            parse_loopback(rejected).is_err(),
            "should reject {rejected}"
        );
    }
}

#[test]
fn unconfined_control_connects_and_listener_observes_accept() {
    let listener = LoopbackListener::bind().expect("should bind listener");
    let probe = helper_probe(&test_binary());
    let mut command = BoundedCommand::new(probe.program(), Path::new("/"), limits())
        .expect("should describe control")
        .arguments(probe.arguments(listener.address()));
    for (name, value) in probe.environment() {
        command = command.environment(name, value).expect("should set env");
    }
    let result = command.run().expect("should run unconfined control");
    assert_eq!(
        result.outcome(),
        ProcessOutcome::Exited { code: 0 },
        "should exit 0 when connected"
    );
    assert_eq!(
        probe.parse(result.stdout()),
        Some(ProbeReport::Connected),
        "should report connected"
    );
    assert_eq!(
        listener.drain_accepts().expect("should drain"),
        1,
        "should observe exactly one accept"
    );
}

// ---------------------------------------------------------------------------
// Roots, SBPL template and tree facts
// ---------------------------------------------------------------------------

#[test]
fn path_text_rejects_profile_metacharacters() {
    assert!(validate_path_text(Path::new("/private/tmp/ok-root")).is_ok());
    let nul = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/a\0b"));
    for rejected in [
        PathBuf::from("/tmp/quote\"root"),
        PathBuf::from("/tmp/back\\slash"),
        PathBuf::from("/tmp/new\nline"),
        PathBuf::from("/tmp/tab\tchar"),
        nul,
        PathBuf::from("relative/root"),
        PathBuf::from("/"),
    ] {
        assert!(
            validate_path_text(&rejected).is_err(),
            "should reject {rejected:?}"
        );
        assert!(
            define("WRITABLE_0", &rejected).is_err(),
            "should reject {rejected:?} as an SBPL define"
        );
    }
}

#[test]
fn roots_reject_metacharacter_directories_and_overlap() {
    let (_directory, base) = canonical_tempdir();
    for name in ["quote\"dir", "back\\dir", "new\ndir"] {
        let path = base.join(name);
        fs::create_dir(&path).expect("should create odd directory");
        assert!(
            BoundaryRoots::new(&[], &[], &[path]).is_err(),
            "should reject root named {name:?}"
        );
    }
    let outer = base.join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).expect("should create nested roots");
    assert!(
        BoundaryRoots::new(
            std::slice::from_ref(&inner),
            &[],
            std::slice::from_ref(&outer)
        )
        .is_err(),
        "should reject nested roots"
    );
    assert!(
        BoundaryRoots::new(
            std::slice::from_ref(&outer),
            &[],
            std::slice::from_ref(&outer)
        )
        .is_err(),
        "should reject duplicate roots"
    );
    assert!(
        BoundaryRoots::new(&[outer], &[], &[]).is_err(),
        "should require a writable root"
    );
    let many: Vec<PathBuf> = (0..=super::super::roots::MAX_ROOTS)
        .map(|index| {
            let path = base.join(format!("many-{index}"));
            fs::create_dir(&path).expect("should create root");
            path
        })
        .collect();
    assert!(
        BoundaryRoots::new(&[], &[], &many).is_err(),
        "should reject more roots than profile slots"
    );
}

pub(super) fn sample_roots(base: &Path) -> BoundaryRoots {
    let immutable = base.join("immutable");
    let writable = base.join("writable");
    let tool = base.join("tool");
    for path in [&immutable, &writable] {
        fs::create_dir(path).expect("should create root");
    }
    fs::write(&tool, b"tool").expect("should create tool");
    BoundaryRoots::new(&[immutable], &[tool], &[writable]).expect("should accept roots")
}

#[test]
fn sbpl_profile_hides_ambient_openssl_configuration_after_root_grants() {
    let (_directory, base) = canonical_tempdir();
    let profile = MacProfile::generate(&sample_roots(&base)).expect("should generate profile");
    let rule = "(deny file-read* (with errno ENOENT)\n  (literal \"/private/etc/ssl/openssl.cnf\")\n  (literal \"/etc/ssl/openssl.cnf\"))";
    let denial = profile
        .text()
        .find(rule)
        .expect("should hide both exact aliases");
    let grant = profile
        .text()
        .rfind("(allow file-read*")
        .expect("should have root grants");
    assert!(denial > grant, "should override every file read grant");
    assert!(
        !profile.text().contains("(subpath \"/private/etc/ssl\")")
            && !profile.text().contains("(subpath \"/etc/ssl\")"),
        "should not grant access to ambient SSL configuration"
    );
}

#[test]
fn sbpl_profile_carries_paths_only_as_defines() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let profile = MacProfile::generate(&roots).expect("should generate profile");
    let base_text = base.to_str().expect("should be UTF-8");
    assert!(
        !profile.text().contains(base_text),
        "should never splice root paths into profile text"
    );
    for required in [
        "(deny default)",
        "(deny network*)",
        "(allow signal (target same-sandbox))",
        "(deny file-write* (subpath (param \"IMMUTABLE_0\")))",
        "(allow file-read* file-write* process-exec (subpath (param \"WRITABLE_0\")))",
    ] {
        assert!(
            profile.text().contains(required),
            "should contain {required}"
        );
    }
    let names: Vec<&str> = profile
        .defines()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        names,
        ["IMMUTABLE_0", "TOOL_0", "WRITABLE_0"],
        "should bind every slot"
    );
    let arguments = profile.arguments();
    assert_eq!(
        arguments.first(),
        Some(&OsString::from("-p")),
        "should pass text with -p"
    );
    assert_eq!(
        arguments
            .iter()
            .filter(|argument| *argument == "-D")
            .count(),
        3,
        "should pass one define per root"
    );
}

#[test]
fn sbpl_fingerprint_depends_on_roots() {
    let (_first_directory, first) = canonical_tempdir();
    let (_second_directory, second) = canonical_tempdir();
    let first = MacProfile::generate(&sample_roots(&first)).expect("should generate");
    let second = MacProfile::generate(&sample_roots(&second)).expect("should generate");
    assert_ne!(
        first.fingerprint(),
        second.fingerprint(),
        "should bind root paths"
    );
    assert_eq!(
        first.fingerprint(),
        first.clone().fingerprint(),
        "should be stable"
    );
}

#[test]
fn tree_scan_rejects_hard_links_and_unsafe_aliases() {
    let (_directory, base) = canonical_tempdir();
    let root = base.join("root");
    fs::create_dir_all(root.join("sub")).expect("should create root");
    fs::write(root.join("sub/file.txt"), b"source").expect("should write file");
    symlink("sub/file.txt", root.join("inside")).expect("should create safe link");
    TreeFacts::scan(&root, ScanPolicy::Immutable).expect("should accept a safe tree");

    let hard = base.join("outside-link");
    fs::hard_link(root.join("sub/file.txt"), &hard).expect("should hard link");
    let rejected =
        TreeFacts::scan(&root, ScanPolicy::Immutable).expect_err("should reject nlink 2");
    assert!(
        error_text(&rejected).contains("hard links"),
        "should name the hard link"
    );
    fs::remove_file(&hard).expect("should remove link");

    for (name, target) in [
        ("absolute", "/etc/hosts"),
        ("escaping", "../../outside"),
        ("dangling", "sub/missing"),
    ] {
        let link = root.join(name);
        symlink(target, &link).expect("should create link");
        for policy in [ScanPolicy::Immutable, ScanPolicy::AliasesOnly] {
            assert!(
                TreeFacts::scan(&root, policy).is_err(),
                "should reject {name} alias under {policy:?}"
            );
        }
        fs::remove_file(&link).expect("should remove link");
    }
    let fifo = root.join("fifo");
    let fifo_text =
        std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("should be C string");
    // SAFETY: `fifo_text` is a valid NUL-terminated path.
    assert_eq!(
        unsafe { libc::mkfifo(fifo_text.as_ptr(), 0o600) },
        0,
        "should create fifo"
    );
    assert!(
        TreeFacts::scan(&root, ScanPolicy::AliasesOnly).is_err(),
        "should reject special files"
    );
}

#[test]
fn tree_facts_detect_content_mode_and_membership_changes() {
    let (_directory, base) = canonical_tempdir();
    let file = base.join("file.txt");
    fs::write(&file, b"one").expect("should write file");
    let before = TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should scan");
    assert!(
        before
            .ensure_unchanged(&TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should scan"))
            .is_ok(),
        "should accept an unchanged tree"
    );
    let mutations: [&dyn Fn(); 3] = [
        &|| fs::write(&file, b"two").expect("should rewrite"),
        &|| {
            fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).expect("should chmod");
        },
        &|| fs::write(base.join("added"), b"x").expect("should add"),
    ];
    for mutate in mutations {
        let snapshot = TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should scan");
        mutate();
        let after = TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should rescan");
        assert!(
            snapshot.ensure_unchanged(&after).is_err(),
            "should detect mutation"
        );
    }
    let snapshot = TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should scan");
    fs::remove_file(base.join("added")).expect("should remove");
    let after = TreeFacts::scan(&base, ScanPolicy::Immutable).expect("should rescan");
    assert!(
        snapshot.ensure_unchanged(&after).is_err(),
        "should detect removal"
    );
}

#[test]
fn confined_command_environment_rejects_injection_and_duplicates() {
    let command = ConfinedCommand::new(Path::new("/bin/tool"), Path::new("/work"), limits())
        .expect("should describe command");
    let command = command
        .environment("HOME", "/work")
        .expect("should accept HOME");
    assert!(
        ConfinedCommand::new(Path::new("/bin/tool"), Path::new("/work"), limits())
            .expect("should describe command")
            .environment("LD_PRELOAD", "/x.so")
            .is_err(),
        "should reject injection variables"
    );
    assert!(
        command.environment("HOME", "/other").is_err(),
        "should reject duplicates"
    );
    assert!(
        ConfinedCommand::new(Path::new("relative"), Path::new("/work"), limits()).is_err(),
        "should reject a relative program"
    );
}

#[test]
fn macos_boundary_is_test_only_until_qualified() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    assert_eq!(
        Boundary::mac_sandbox_for_tests(roots).is_ok(),
        cfg!(target_os = "macos"),
        "should allow sandbox-exec only on macOS"
    );
}

#[test]
fn instance_fingerprints_differ_for_identical_configurations() {
    let platform = [9; 32];
    let first = instance_fingerprint(&platform).expect("should draw a nonce");
    let second = instance_fingerprint(&platform).expect("should draw a nonce");
    assert_ne!(
        first, second,
        "should bind a fresh nonce into every instance"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn boundary_instances_never_share_a_fingerprint() {
    let (_directory, base) = canonical_tempdir();
    let roots = sample_roots(&base);
    let first = Boundary::mac_sandbox_for_tests(roots.clone()).expect("should configure");
    let second = Boundary::mac_sandbox_for_tests(roots).expect("should configure");
    assert_ne!(
        first.fingerprint, second.fingerprint,
        "should bind a fresh nonce into every instance"
    );
}

#[test]
fn confined_paths_reject_dot_components() {
    for (program, working_directory) in [
        ("/tools/../bin/tool", "/work"),
        ("/tools/./tool", "/work"),
        ("/tools/tool", "/work/../etc"),
        ("/tools/tool", "/work/."),
        ("tools/tool", "/work"),
    ] {
        assert!(
            ConfinedCommand::new(Path::new(program), Path::new(working_directory), limits())
                .is_err(),
            "should reject {program} in {working_directory}"
        );
    }
}

#[test]
fn tool_roots_require_single_link_files_and_staging_provides_them() {
    let (_directory, base) = canonical_tempdir();
    let tools = base.join("tools");
    fs::create_dir(&tools).expect("should create tools");
    let staged = staged_probe(&tools);
    let metadata = fs::metadata(staged.program()).expect("should stat staged probe");
    assert_eq!(metadata.nlink(), 1, "should stage a single-link copy");
    assert_eq!(metadata.mode() & 0o777, 0o555, "should stage read-only");
    TreeFacts::scan(&tools, ScanPolicy::Immutable).expect("should accept the staged tool");
    TreeFacts::scan(staged.program(), ScanPolicy::Immutable).expect("should scan a file root");
    assert!(
        helper_probe(&test_binary()).staged(&tools).is_err(),
        "should refuse to overwrite a staged tool"
    );
    fs::hard_link(staged.program(), base.join("alias")).expect("should hard link");
    assert!(
        TreeFacts::scan(&tools, ScanPolicy::Immutable).is_err(),
        "should reject a hard-linked tool"
    );
    assert!(
        TreeFacts::scan(staged.program(), ScanPolicy::Immutable).is_err(),
        "should reject a hard-linked file root"
    );
}

#[test]
fn writable_scans_reject_hard_links_before_runs() {
    let (_directory, base) = canonical_tempdir();
    let writable = base.join("writable");
    fs::create_dir(&writable).expect("should create writable root");
    fs::write(writable.join("file"), b"x").expect("should write");
    TreeFacts::scan(&writable, ScanPolicy::SingleLinks).expect("should accept single links");
    fs::hard_link(writable.join("file"), base.join("outside")).expect("should hard link");
    assert!(
        TreeFacts::scan(&writable, ScanPolicy::SingleLinks).is_err(),
        "should reject a hard link before a run"
    );
    TreeFacts::scan(&writable, ScanPolicy::AliasesOnly).expect("should allow links after a run");
}

#[test]
fn sbpl_profile_denies_group_escape_and_scopes_unix_sockets() {
    let (_directory, base) = canonical_tempdir();
    let profile = MacProfile::generate(&sample_roots(&base)).expect("should generate");
    let text = profile.text();
    let syscalls = format!("(deny syscall-unix (syscall-number {SYS_SETSID} {SYS_SETPGID}))");
    assert!(text.contains(&syscalls), "should deny setsid and setpgid");
    let network = text.find("(deny network*)").expect("should deny network");
    let unix = text
        .find("(local unix-socket (subpath (param \"WRITABLE_0\")))")
        .expect("should allow Unix sockets under the writable root");
    assert!(
        unix > network,
        "should re-allow only Unix sockets after the denial"
    );
    assert!(
        !text.contains("unix-socket (subpath (param \"IMMUTABLE_0\"))"),
        "should not allow Unix sockets in immutable roots"
    );
    for absent in ["(subpath \"/usr\")", "/usr/local"] {
        assert!(!text.contains(absent), "should not allow {absent}");
    }
    assert!(
        text.ends_with(
            "(allow file-read-data (literal (param \"RUN_MARK_ALLOW\")))\n\
             (deny file-read* (literal (param \"RUN_MARK_DENY\")))\n"
        ),
        "should end with the run instance marker rules so that they take precedence"
    );
}

#[test]
fn sibling_lifetime_is_owned_by_the_parent_pipe() {
    let mut sibling = spawn_sibling();
    let controlled = sibling.stdin.is_some();
    if controlled {
        drop(sibling.stdin.take());
    } else {
        sibling
            .kill()
            .expect("should stop the old timer-based fixture");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = sibling.try_wait().expect("should poll sibling") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            sibling.kill().expect("should stop a failed fixture");
            sibling.wait().expect("should reap a failed fixture");
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(controlled, "should retain a parent-owned lifetime pipe");
    assert!(
        status.is_some_and(|status| status.success()),
        "should exit successfully when the parent closes the pipe"
    );
}

#[test]
fn confined_debug_does_not_expose_private_stdin() {
    let private = b"fictional-private-parent-input";
    let command = ConfinedCommand::new(
        Path::new("/example/tool"),
        Path::new("/example/work"),
        limits(),
    )
    .expect("should describe command")
    .stdin_bytes(private.to_vec());
    let debug = format!("{command:?}");
    assert!(
        !debug.contains("fictional-private-parent-input"),
        "should omit private stdin from diagnostics"
    );
    assert!(
        !debug.contains("102, 105, 99, 116"),
        "should not render private stdin as decimal bytes"
    );
}
