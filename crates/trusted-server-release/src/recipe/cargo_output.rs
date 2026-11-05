//! Strict parser for Cargo's `--message-format=json` build stream.
//!
//! Cargo prints one JSON object per line. The recipe accepts the stream only
//! when every line is a strict, bounded JSON object, the stream ends in one
//! successful `build-finished`, and exactly one freshly compiled, debug-info
//! carrying binary artifact of the requested package and binary names was
//! produced, whose executable is a direct child of the target's release
//! directory. The parser never opens a file: it returns where Cargo says the
//! executable is, and the caller still re-reads and hashes those bytes.

use std::path::{Component, Path, PathBuf};

use error_stack::Report;
use serde_json::Value;

use super::fresh::normal_absolute;
use super::paths::ExecutionPaths;
use super::recipe_error;
use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;

/// `profile.debuginfo` the release profile must report (limited debug info).
const REQUIRED_DEBUGINFO: u64 = 1;

/// What the single binary artifact must be, in the execution namespace.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExpectedBinary<'a> {
    /// Cargo binary target name.
    pub(crate) bin: &'a str,
    /// Checkout path of the package manifest, as the child sees it.
    pub(crate) manifest: &'a Path,
    /// The `<target>/<triple>/release` directory, as the child sees it.
    pub(crate) release_dir: &'a Path,
}

/// The located executable in both namespaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocatedBinary {
    /// Executable path as the confined child reported it.
    pub(crate) execution: PathBuf,
    /// The same file on the host, inside a writable root.
    pub(crate) host: PathBuf,
}

fn output_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("cargo output: {detail}"))
}

/// Splits `stdout` into strict JSON objects, one per newline-terminated line.
fn messages(stdout: &[u8]) -> Result<Vec<Value>, Report<ReleaseError>> {
    if stdout.len() > 16 * 1024 * 1024 {
        return Err(output_error("Cargo stream exceeds the build output bound"));
    }
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    let body = stdout
        .strip_suffix(b"\n")
        .ok_or_else(|| output_error("final line is not newline-terminated"))?;
    body.split(|byte| *byte == b'\n')
        .enumerate()
        .map(|(index, line)| {
            let value = parse_json(line, Role::CargoMessage).map_err(|error| {
                error.change_context(ReleaseError::Recipe).attach(format!(
                    "cargo output line {} is not strict JSON",
                    index + 1
                ))
            })?;
            let has_reason = value
                .as_object()
                .is_some_and(|object| object.get("reason").is_some_and(Value::is_string));
            if !has_reason {
                return Err(output_error(format!(
                    "line {} is not an object with a string reason",
                    index + 1
                )));
            }
            Ok(value)
        })
        .collect()
}

/// Requires the last message to be the only successful `build-finished`.
fn require_success(messages: &[Value]) -> Result<(), Report<ReleaseError>> {
    let reason = |message: &Value| message["reason"].as_str().map(str::to_owned);
    let finished = messages
        .iter()
        .filter(|message| reason(message).as_deref() == Some("build-finished"))
        .count();
    let last = messages.last();
    let ends_finished =
        last.is_some_and(|message| reason(message).as_deref() == Some("build-finished"));
    if finished != 1 || !ends_finished {
        return Err(output_error(
            "stream must end with exactly one build-finished message",
        ));
    }
    let success = last
        .and_then(|message| message.get("success"))
        .and_then(Value::as_bool);
    if success != Some(true) {
        return Err(output_error("build-finished did not report success true"));
    }
    Ok(())
}

fn is_artifact(message: &Value) -> bool {
    message["reason"].as_str() == Some("compiler-artifact")
}

fn kinds(message: &Value) -> Option<Vec<&str>> {
    message["target"]["kind"]
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect()
}

fn is_requested(message: &Value, expected: &ExpectedBinary<'_>) -> bool {
    kinds(message).as_deref() == Some(&["bin"][..])
        && message["target"]["name"].as_str() == Some(expected.bin)
        && message["manifest_path"].as_str().map(Path::new) == Some(expected.manifest)
}

/// Validates the one requested artifact and returns its executable path.
fn executable_of(
    message: &Value,
    expected: &ExpectedBinary<'_>,
) -> Result<PathBuf, Report<ReleaseError>> {
    if message["fresh"].as_bool() != Some(false) {
        return Err(output_error(
            "binary artifact is not reported fresh false; a cached artifact proves nothing",
        ));
    }
    if message["profile"]["debuginfo"].as_u64() != Some(REQUIRED_DEBUGINFO) {
        return Err(output_error(format!(
            "binary artifact profile debuginfo is not {REQUIRED_DEBUGINFO}"
        )));
    }
    let text = message["executable"]
        .as_str()
        .ok_or_else(|| output_error("binary artifact has no executable path"))?;
    let executable = PathBuf::from(text);
    normal_absolute("cargo executable", &executable)
        .map_err(|_| output_error("executable path is not an absolute normal path"))?;
    let relative = executable
        .strip_prefix(expected.release_dir)
        .map_err(|_| output_error("executable is outside the release directory"))?;
    let mut parts = relative.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => Ok(executable),
        _ => Err(output_error(
            "executable is not a direct child of the release directory",
        )),
    }
}

/// Parses the build stream and returns the container path of the executable.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for a non-strict or oversize line, a
/// stream not ending in exactly one successful `build-finished`, zero or more
/// than one artifact with `target.kind == ["bin"]`, the requested name and
/// manifest, an additional binary artifact, a fresh or unreported-freshness
/// artifact, `profile.debuginfo != 1`, or an executable that is missing,
/// relative, dotted, outside or nested below the release directory.
pub(crate) fn locate_binary(
    stdout: &[u8],
    expected: &ExpectedBinary<'_>,
) -> Result<PathBuf, Report<ReleaseError>> {
    let messages = successful_messages(stdout)?;
    let artifacts: Vec<&Value> = messages
        .iter()
        .filter(|message| is_artifact(message))
        .collect();
    let matching: Vec<&&Value> = artifacts
        .iter()
        .filter(|message| is_requested(message, expected))
        .collect();
    let [requested] = matching.as_slice() else {
        return Err(output_error(format!(
            "expected exactly one binary artifact for {}, found {}",
            expected.bin,
            matching.len()
        )));
    };
    if artifacts.iter().any(|message| {
        !std::ptr::eq(*message, **requested)
            && kinds(message).is_none_or(|kinds| kinds.contains(&"bin"))
    }) {
        return Err(output_error("unexpected additional binary artifact"));
    }
    executable_of(requested, expected)
}

/// Parses bounded strict JSONL ending in one successful build completion.
///
/// # Errors
///
/// Returns an error for malformed, oversized, duplicate-key or incomplete output.
pub(crate) fn successful_messages(stdout: &[u8]) -> Result<Vec<Value>, Report<ReleaseError>> {
    let messages = messages(stdout)?;
    require_success(&messages)?;
    Ok(messages)
}

/// Parses the build stream and maps the executable into a writable host root.
///
/// # Errors
///
/// As [`locate_binary`], or [`ReleaseError::Recipe`] when the executable does
/// not map back through a writable root.
pub(crate) fn locate_host_binary(
    stdout: &[u8],
    expected: &ExpectedBinary<'_>,
    paths: &dyn ExecutionPaths,
) -> Result<LocatedBinary, Report<ReleaseError>> {
    let execution = locate_binary(stdout, expected)?;
    let host = paths
        .host_path(&execution)
        .map_err(|error| error.change_context(ReleaseError::Recipe))
        .map_err(|error| error.attach("executable is outside every writable root"))?;
    Ok(LocatedBinary { execution, host })
}

#[cfg(test)]
mod tests;
