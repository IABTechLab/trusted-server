//! Completion of synthetic Darwin builds: Mach-O acceptance, the measured
//! SDK and the recorded descendant cleanup.

use object::macho as macho_format;

use super::complete::built;
use super::*;
use crate::boundary::DescendantCleanup;
use crate::inspect::Violation;
use crate::inspect::tests::builders::{
    Command, MachoSpec, Symbol, debug_map_symbols, macho, macos,
};

/// A conforming linked arm64 `ts` whose payload holds the remapped source
/// prefix the final-path gate requires.
pub(super) fn darwin_executable() -> Vec<u8> {
    darwin_executable_with(b"")
}

/// The same executable with `extra` bytes appended after its signature.
pub(super) fn darwin_executable_with(extra: &[u8]) -> Vec<u8> {
    let mut bytes = macho(&darwin_spec());
    bytes.extend_from_slice(extra);
    bytes
}

/// Reports whether a completion error rejected an embedded path: by the
/// final-path gate, or first by the Mach-O inspector, whose runner-path rule
/// also matches temporary host roots and any `/home/` segment.
pub(super) fn rejects_embedded_path(error: &str) -> bool {
    error.contains("unremapped path prefix") || error.contains("runner-only path")
}

fn darwin_spec() -> MachoSpec {
    MachoSpec {
        payload: b"/trusted-server/src/main.rs",
        ..MachoSpec::conforming()
    }
}

/// Completes a synthetic build of `asset` holding exactly `bytes`.
fn finish(asset: &'static str, bytes: &[u8]) -> Result<RecipeCompleted, Report<ReleaseError>> {
    let mut built = built(asset, Role::Canonical, bytes);
    let stdout = built.stdout(|_| {});
    built.finish(stdout)
}

fn replaced(index: usize, command: Command) -> Vec<u8> {
    let mut spec = darwin_spec();
    spec.commands[index] = command;
    macho(&spec)
}

fn position(matches: impl Fn(&Command) -> bool) -> usize {
    darwin_spec().position(matches)
}

#[test]
fn conforming_darwin_executables_complete_with_post_run_reaped_cleanup() {
    let completed = finish("ts-aarch64-apple-darwin", &darwin_executable())
        .expect("should accept a conforming arm64 executable");
    assert_eq!(
        completed.descendant_cleanup(),
        DescendantCleanup::ReapedAfterRun,
        "should record that macOS survivors are reaped after the run"
    );
    let measurement = super::candidate::cargo_measurement(&completed);
    assert_eq!(
        measurement["descendant_cleanup"], "post-run-reaped",
        "should report the cleanup mode in the candidate measurement"
    );

    let mut x86_64 = darwin_spec();
    x86_64.cputype = macho_format::CPU_TYPE_X86_64;
    x86_64.cpusubtype = macho_format::CPU_SUBTYPE_X86_64_ALL;
    finish("ts-x86_64-apple-darwin", &macho(&x86_64))
        .expect("should accept a conforming x86_64 executable");
}

#[test]
fn linux_completion_records_container_owned_cleanup() {
    let wasm = super::complete::wasm_with(&[(".debug_info", b"i"), (".debug_line", b"l")]);
    let completed =
        finish("trusted-server-fastly.wasm", &wasm).expect("should accept the Wasm module");
    assert_eq!(
        super::candidate::cargo_measurement(&completed)["descendant_cleanup"],
        "owned-by-container",
        "should report container-owned cleanup"
    );
}

#[test]
fn darwin_executables_outside_the_policy_are_rejected_by_rule() {
    let build_version = position(|command| matches!(command, Command::BuildVersion { .. }));
    let symtab = position(|command| matches!(command, Command::Symtab(_)));
    let mut absolute = debug_map_symbols();
    absolute.push(Symbol {
        n_type: macho_format::N_OSO,
        name: "/opt/example/cargo-target/release/deps/ts.o",
        value: 0,
    });
    let mut unsigned = darwin_spec();
    unsigned
        .commands
        .retain(|command| !matches!(command, Command::CodeSignature(_)));
    let cases: [(&str, &'static str, Vec<u8>, Violation); 5] = [
        (
            "minimum macOS 11",
            "ts-aarch64-apple-darwin",
            replaced(
                build_version,
                Command::BuildVersion {
                    platform: macho_format::PLATFORM_MACOS,
                    minos: macos(11, 0),
                },
            ),
            Violation::DeploymentTarget,
        ),
        (
            "an absolute debug-map object",
            "ts-aarch64-apple-darwin",
            replaced(symtab, Command::Symtab(absolute)),
            Violation::MachoDebugMap,
        ),
        (
            "a stripped debug map",
            "ts-aarch64-apple-darwin",
            replaced(symtab, Command::Symtab(Vec::new())),
            Violation::MachoDebugMap,
        ),
        (
            "an unsigned arm64 executable",
            "ts-aarch64-apple-darwin",
            macho(&unsigned),
            Violation::CodeSignature,
        ),
        (
            "an arm64 executable for the x86_64 asset",
            "ts-x86_64-apple-darwin",
            darwin_executable(),
            Violation::Architecture,
        ),
    ];
    for (label, asset, bytes, expected) in cases {
        let error = finish(asset, &bytes).expect_err("should reject the executable");
        assert_eq!(
            error.downcast_ref::<Violation>(),
            Some(&expected),
            "should reject {label} by its rule: {error:?}"
        );
    }
}

#[test]
fn darwin_executables_must_record_the_measured_sdk() {
    let build_version = position(|command| matches!(command, Command::BuildVersion { .. }));
    let other_sdk = replaced(
        build_version,
        Command::BuildVersionSdk {
            platform: macho_format::PLATFORM_MACOS,
            minos: macos(13, 0),
            sdk: macos(26, 1),
        },
    );

    let error = finish("ts-aarch64-apple-darwin", &other_sdk)
        .expect_err("should reject an executable linked against another SDK");

    assert!(
        format!("{error:?}").contains("but the bound Apple SDK is 15.5.0"),
        "should name the measured SDK: {error:?}"
    );
}
