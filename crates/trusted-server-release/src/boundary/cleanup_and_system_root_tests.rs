//! Pure tests of descendant-cleanup reporting and system roots; nothing runs.

use std::fs;
use std::path::PathBuf;

use super::macos::MacProfile;
use super::*;

struct Layout {
    _directory: tempfile::TempDir,
    base: PathBuf,
}

fn layout() -> Layout {
    let directory = tempfile::tempdir().expect("should create layout");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize layout");
    for relative in [
        "checkout",
        "tools/bin",
        "state",
        "developer",
        "developer-file",
    ] {
        fs::create_dir_all(base.join(relative)).expect("should create root");
    }
    Layout {
        _directory: directory,
        base,
    }
}

impl Layout {
    fn roots(&self) -> BoundaryRoots {
        BoundaryRoots::new(
            &[self.base.join("checkout")],
            &[self.base.join("tools")],
            &[self.base.join("state")],
        )
        .expect("should build roots")
    }

    fn with_developer(&self) -> BoundaryRoots {
        self.roots()
            .with_system(&[self.base.join("developer")])
            .expect("should add the system root")
    }
}

#[test]
fn mac_sandbox_reports_post_run_reaped_cleanup() {
    let layout = layout();
    let profile = MacProfile::generate(&layout.roots()).expect("should generate the profile");

    let cleanup = Platform::MacSandbox(profile).descendant_cleanup();

    assert_eq!(
        cleanup,
        DescendantCleanup::ReapedAfterRun,
        "should report that macOS survivors are reaped only after the run"
    );
    assert_eq!(
        cleanup.to_string(),
        "post-run-reaped",
        "should label the cleanup mode for measurements"
    );
    assert_eq!(
        DescendantCleanup::OwnedByContainer.to_string(),
        "owned-by-container",
        "should label container-owned cleanup for measurements"
    );
}

#[test]
fn system_roots_are_read_and_execute_only_in_the_profile() {
    let layout = layout();
    let profile =
        MacProfile::generate(&layout.with_developer()).expect("should generate the profile");
    let text = profile.text();

    assert!(
        text.contains("(allow file-read* process-exec (subpath (param \"SYSTEM_0\")))"),
        "should allow only reading and executing below the system root: {text}"
    );
    assert!(
        text.contains("(deny file-write* (subpath (param \"SYSTEM_0\")))"),
        "should explicitly deny writes below the system root"
    );
    assert!(
        !text.contains("file-write* process-exec (subpath (param \"SYSTEM_0\"))"),
        "should never allow writes below the system root"
    );
    assert!(
        !text.contains("unix-socket (subpath (param \"SYSTEM_0\"))"),
        "should never allow sockets below the system root"
    );
    for denied in [
        "/usr/bin/sudo",
        "/usr/bin/su",
        "/usr/bin/login",
        "/usr/bin/newgrp",
        "/usr/bin/xcrun",
        "/usr/bin/xcodebuild",
        "/usr/bin/xcode-select",
    ] {
        assert!(
            text.contains(&format!("(deny process-exec (literal \"{denied}\"))")),
            "should refuse to execute {denied}"
        );
    }
    for allowed in ["/usr/bin/cc", "/usr/bin/clang", "/usr/bin/ld"] {
        assert!(
            !text.contains(&format!("(literal \"{allowed}\")")),
            "should leave the {allowed} shim to build scripts"
        );
    }
    assert!(
        profile
            .defines()
            .iter()
            .any(|(name, value)| name == "SYSTEM_0"
                && Some(value.as_str()) == layout.base.join("developer").to_str()),
        "should bind the system root through a define"
    );
}

#[test]
fn system_roots_take_part_in_equality_and_the_fingerprint() {
    let layout = layout();
    let plain = layout.roots();
    let with_developer = layout.with_developer();

    assert_ne!(
        plain, with_developer,
        "should not treat roots without the system root as the same roots"
    );
    let plain_profile = MacProfile::generate(&plain).expect("should generate the profile");
    let developer_profile =
        MacProfile::generate(&with_developer).expect("should generate the profile");
    assert_ne!(
        plain_profile.fingerprint(),
        developer_profile.fingerprint(),
        "should bind the system root into the profile fingerprint"
    );
    assert_eq!(
        with_developer.system(),
        [layout.base.join("developer")],
        "should keep the canonical system root"
    );
    assert!(
        with_developer
            .locate(&layout.base.join("developer/usr/bin/clang"))
            .is_some_and(|(role, _, _)| role == roots::RootRole::System),
        "should locate tools below the system root"
    );
}

#[test]
fn system_roots_must_be_disjoint_directories() {
    let layout = layout();
    fs::write(layout.base.join("developer-file/tool"), b"fictional").expect("should write file");
    let cases = [
        ("nested in the checkout", layout.base.join("checkout/inner")),
        ("containing the tools", layout.base.clone()),
        ("a regular file", layout.base.join("developer-file/tool")),
        ("missing", layout.base.join("absent")),
    ];
    fs::create_dir_all(layout.base.join("checkout/inner")).expect("should create nested root");
    for (label, system) in cases {
        assert!(
            layout.roots().with_system(&[system]).is_err(),
            "should reject a system root {label}"
        );
    }
}

#[test]
fn container_boundary_refuses_system_roots() {
    let layout = layout();

    let error = docker::mounts(&layout.with_developer()).expect_err("should refuse system roots");

    assert!(
        format!("{error:?}").contains("refuses host system roots"),
        "should name the refused system root: {error:?}"
    );
    docker::mounts(&layout.roots()).expect("should mount roots without a system root");
}
