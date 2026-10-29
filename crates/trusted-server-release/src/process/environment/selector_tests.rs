//! Pure tests of the explicit tool-selector allowance; no process is started.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::process::ProcessLimits;

fn fresh() -> BoundedCommand {
    BoundedCommand::new(
        Path::new("/example/tool"),
        Path::new("/example"),
        ProcessLimits {
            wall: Duration::from_secs(1),
            stdout_max: 1024,
            stderr_max: 1024,
        },
    )
    .expect("should describe a command")
}

#[test]
fn generic_environment_still_rejects_every_tool_selector() {
    for name in [
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTFLAGS",
        "RUSTDOC",
        "CARGO_BUILD_RUSTC",
    ] {
        assert!(
            fresh().environment(name, "/example/rustc").is_err(),
            "should keep rejecting {name} through the generic environment"
        );
    }
}

#[test]
fn only_rustc_with_an_absolute_path_is_a_selectable_tool() {
    let selected = fresh()
        .tool_selector_environment("RUSTC", "/example/bin/rustc")
        .expect("should accept an explicit absolute RUSTC");
    assert_eq!(
        selected
            .environment
            .get(OsStr::new("RUSTC"))
            .map(|entry| entry.value.clone()),
        Some(OsString::from("/example/bin/rustc")),
        "should carry the selected compiler"
    );
    for (name, value) in [
        ("RUSTC_WRAPPER", "/example/wrapper"),
        ("RUSTFLAGS", "/example/flags"),
        ("RUSTDOC", "/example/rustdoc"),
        ("CARGO_BUILD_RUSTC", "/example/rustc"),
        ("LD_PRELOAD", "/example/lib.so"),
        ("rustc", "/example/rustc"),
        ("RUSTC", "relative/rustc"),
        ("RUSTC", ""),
    ] {
        assert!(
            fresh().tool_selector_environment(name, value).is_err(),
            "should reject {name}={value:?} as a tool selector"
        );
    }
    assert!(
        fresh()
            .tool_selector_environment("RUSTC", "/example/bin/rustc")
            .and_then(|command| command.tool_selector_environment("RUSTC", "/example/other"))
            .is_err(),
        "should reject setting RUSTC twice"
    );
}
