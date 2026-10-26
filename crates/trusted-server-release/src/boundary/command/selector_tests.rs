//! Pure tests of confined tool-selector variables; no process is started.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use super::*;

fn command() -> ConfinedCommand {
    ConfinedCommand::new(
        Path::new("/example/bin/cargo"),
        Path::new("/example/checkout"),
        ProcessLimits {
            wall: Duration::from_secs(1),
            stdout_max: 1024,
            stderr_max: 1024,
        },
    )
    .expect("should describe a confined command")
}

#[test]
fn rustc_needs_the_explicit_tool_method_and_survives_later_variables() {
    assert!(
        command()
            .environment("RUSTC", "/example/bin/rustc")
            .is_err(),
        "should reject RUSTC through the generic environment"
    );
    let described = command()
        .tool_environment("RUSTC", "/example/bin/rustc")
        .expect("should accept the explicit tool selector")
        .environment("TZ", "UTC")
        .expect("should accept a later ordinary variable")
        .tool_environment("RUSTC_WRAPPER", "/example/wrapper");
    assert!(described.is_err(), "should reject any other tool selector");

    let described = command()
        .tool_environment("RUSTC", "/example/bin/rustc")
        .expect("should accept the explicit tool selector")
        .environment("TZ", "UTC")
        .expect("should accept a later ordinary variable");
    assert_eq!(
        described.environment,
        [
            (
                OsString::from("RUSTC"),
                OsString::from("/example/bin/rustc")
            ),
            (OsString::from("TZ"), OsString::from("UTC")),
        ],
        "should carry both variables to the platform runner in order"
    );
    assert!(
        command()
            .tool_environment("RUSTC", "/example/bin/rustc")
            .and_then(|command| command.environment("RUSTC", "/example/other"))
            .is_err(),
        "should reject a second RUSTC through the generic environment"
    );
}
