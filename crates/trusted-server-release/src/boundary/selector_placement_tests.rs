//! Pure tests of tool-selector placement and sandbox wrapping; nothing runs.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::macos::{MacProfile, RunIdentity};
use super::*;

struct Layout {
    _directory: tempfile::TempDir,
    base: PathBuf,
    roots: BoundaryRoots,
}

fn layout() -> Layout {
    let directory = tempfile::tempdir().expect("should create layout");
    let base = fs::canonicalize(directory.path()).expect("should canonicalize layout");
    for relative in ["tools/bin", "checkout", "state"] {
        fs::create_dir_all(base.join(relative)).expect("should create root");
    }
    let roots = BoundaryRoots::new(
        &[base.join("checkout")],
        &[base.join("tools")],
        &[base.join("state")],
    )
    .expect("should build roots");
    Layout {
        _directory: directory,
        base,
        roots,
    }
}

fn command(layout: &Layout, rustc: &std::path::Path) -> ConfinedCommand {
    ConfinedCommand::new(
        &layout.base.join("tools/bin/cargo"),
        &layout.base.join("checkout"),
        ProcessLimits {
            wall: Duration::from_secs(1),
            stdout_max: 1024,
            stderr_max: 1024,
        },
    )
    .expect("should describe the command")
    .tool_environment("RUSTC", rustc)
    .expect("should accept an absolute RUSTC")
}

#[cfg(target_os = "macos")]
#[test]
fn rustc_must_be_the_tool_roots_own_bin_rustc() {
    let layout = layout();
    let boundary =
        Boundary::mac_sandbox_for_tests(layout.roots.clone()).expect("should configure, not run");
    boundary
        .check_placement(&command(&layout, &layout.base.join("tools/bin/rustc")))
        .expect("should accept the tool root's rustc");
    for (label, other) in [
        (
            "another file in the tool root",
            layout.base.join("tools/bin/other"),
        ),
        (
            "a nested lookalike",
            layout.base.join("tools/bin/rustc/rustc"),
        ),
        ("a file in the checkout", layout.base.join("checkout/rustc")),
        ("a file outside every root", PathBuf::from("/usr/bin/rustc")),
        ("the tool root itself", layout.base.join("tools")),
    ] {
        assert!(
            boundary.check_placement(&command(&layout, &other)).is_err(),
            "should reject RUSTC naming {label}"
        );
    }
}

#[test]
fn sandbox_wrap_carries_rustc_but_still_rejects_the_nonce_variable() {
    let layout = layout();
    let profile = MacProfile::generate(&layout.roots).expect("should generate the profile");
    let run = RunIdentity::new(&profile).expect("should create run markers");
    profile
        .wrap(
            &command(&layout, &layout.base.join("tools/bin/rustc")),
            &run,
        )
        .expect("should wrap a command carrying the explicit compiler");
    let forged = command(&layout, &layout.base.join("tools/bin/rustc"))
        .environment(super::macos::RUN_NONCE_VARIABLE, "x")
        .expect("should hold the variable until wrapping");
    assert!(
        profile.wrap(&forged, &run).is_err(),
        "should reject a caller variable named like the run nonce"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn mac_host_path_maps_only_lexically_normal_writable_paths() {
    let layout = layout();
    let boundary =
        Boundary::mac_sandbox_for_tests(layout.roots.clone()).expect("should configure, not run");
    let state = layout.base.join("state");
    assert_eq!(
        boundary
            .host_path(&state.join("release/ts"))
            .expect("should accept a normal writable path"),
        state.join("release/ts"),
        "should map a writable path to itself"
    );
    let text = |relative: &str| PathBuf::from(format!("{}/{relative}", state.display()));
    for path in [
        text("release/./ts"),
        text("release//ts"),
        text("release/ts/"),
        text("release/../state/ts"),
        layout.base.join("checkout/ts"),
        layout.base.join("tools/bin/rustc"),
    ] {
        assert!(
            boundary.host_path(&path).is_err(),
            "should reject {path:?} as a host path"
        );
    }
}
