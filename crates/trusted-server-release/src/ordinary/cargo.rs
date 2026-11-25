//! Ordinary compiler path policy and bounded Cargo message parsing.

use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;
use error_stack::Report;
use serde_json::Value;
use std::io;
use std::path::Path;

const CARGO_CONFIG: &str = ".cargo/config.toml";
const LEGACY_CARGO_CONFIG: &str = ".cargo/config";
/// Fixed source prefix in the final wasm.
pub(crate) const REMAPPED_CHECKOUT: &str = "/trusted-server";
/// Fixed prefix for each role's private Cargo home.
pub(crate) const REMAPPED_CARGO_HOME: &str = "/cargo-home";
/// Fixed build-output prefix in the final wasm.
pub(crate) const REMAPPED_TARGET: &str = "/cargo-target";
const EXECUTION_MOUNT_PREFIX: &str = "/boundary/";
const MAX_FORBIDDEN: usize = 64;
const MAX_PREFIX_BYTES: usize = 4096;
fn recipe_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Recipe).attach(detail.to_string())
}

/// Encodes a target-scoped path-remapping argument without TOML escaping.
///
/// # Errors
///
/// Rejects unsafe target names and flags.
pub(crate) fn rustflags_arguments(
    triple: &str,
    flags: &[String],
) -> Result<Vec<String>, Report<ReleaseError>> {
    if triple.is_empty()
        || !triple
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || flags
            .iter()
            .any(|flag| flag.contains(['"', '\\']) || flag.chars().any(char::is_control))
    {
        return Err(recipe_error("target rustflags cannot be safely encoded"));
    }
    let flags: Vec<_> = flags.iter().map(|flag| format!("\"{flag}\"")).collect();
    Ok(vec![
        "--config".to_owned(),
        format!("target.\"{triple}\".rustflags=[{}]", flags.join(",")),
    ])
}

/// Refuses build flags that the target-scoped remapping would supersede.
///
/// # Errors
///
/// Rejects malformed configuration or a `build.rustflags` setting.
pub(crate) fn check_cargo_config(bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| recipe_error(format!("{CARGO_CONFIG} is not UTF-8: {error}")))?;
    let table: toml::Table = toml::from_str(text)
        .map_err(|error| recipe_error(format!("{CARGO_CONFIG} is not TOML: {error}")))?;
    let sets_build_rustflags = table
        .get("build")
        .and_then(toml::Value::as_table)
        .is_some_and(|build| build.contains_key("rustflags"));
    if sets_build_rustflags {
        return Err(recipe_error(format!(
            "{CARGO_CONFIG} sets build.rustflags, which the target path remap would supersede"
        )));
    }
    Ok(())
}

/// Refuses Cargo's unreviewed legacy configuration layer.
///
/// # Errors
///
/// Rejects a tracked, existing or uninspectable extensionless configuration.
pub(crate) fn check_no_extensionless_config(
    checkout: &Path,
    tracked: bool,
) -> Result<(), Report<ReleaseError>> {
    if tracked {
        return Err(recipe_error(format!(
            "the approved source tracks {LEGACY_CARGO_CONFIG}, which is refused"
        )));
    }
    match std::fs::symlink_metadata(checkout.join(LEGACY_CARGO_CONFIG)) {
        Ok(_) => Err(recipe_error(format!(
            "the checkout holds {LEGACY_CARGO_CONFIG}, which is refused"
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(recipe_error(format!(
            "{LEGACY_CARGO_CONFIG} cannot be inspected: {error}"
        ))),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Rejects any retained physical build root in final bytes.
///
/// # Errors
///
/// Rejects invalid prefix bounds or a matching physical/container path.
pub(crate) fn reject_execution_prefixes(
    bytes: &[u8],
    forbidden: &[&str],
) -> Result<(), Report<ReleaseError>> {
    if forbidden.len() > MAX_FORBIDDEN {
        return Err(recipe_error("too many forbidden prefixes for one scan"));
    }
    for prefix in forbidden.iter().copied().chain([EXECUTION_MOUNT_PREFIX]) {
        if prefix.is_empty() || prefix.len() > MAX_PREFIX_BYTES {
            return Err(recipe_error("forbidden prefix is empty or too long"));
        }
        if contains(bytes, prefix.as_bytes()) {
            return Err(recipe_error(format!(
                "final bytes still hold the unremapped path prefix {prefix:?}"
            )));
        }
    }
    Ok(())
}

fn output_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    recipe_error(format!("cargo output: {detail}"))
}

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

/// Parses a bounded Cargo stream ending in exactly one successful completion.
///
/// # Errors
///
/// Rejects malformed, oversized or incomplete Cargo output.
pub(crate) fn successful_messages(stdout: &[u8]) -> Result<Vec<Value>, Report<ReleaseError>> {
    let messages = messages(stdout)?;
    require_success(&messages)?;
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn lines(values: &[Value]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| {
                let mut bytes = serde_json::to_vec(value).expect("should encode Cargo message");
                bytes.push(b'\n');
                bytes
            })
            .collect()
    }

    #[test]
    fn build_requires_exactly_one_final_successful_completion() {
        let artifact = json!({"reason":"compiler-artifact"});
        let finished = json!({"reason":"build-finished","success":true});
        let good = lines(&[artifact.clone(), finished.clone()]);
        successful_messages(&good).expect("should accept a successful stream");
        for values in [
            vec![],
            vec![artifact.clone()],
            vec![json!({"reason":"build-finished","success":false})],
            vec![json!({"reason":"build-finished","success":"true"})],
            vec![finished.clone(), artifact],
            vec![finished.clone(), finished],
            vec![json!({"reason":"build-finished"})],
        ] {
            assert!(
                successful_messages(&lines(&values)).is_err(),
                "should reject incomplete or ambiguous completion"
            );
        }
    }

    #[test]
    fn malformed_unterminated_and_oversized_streams_are_rejected() {
        let good = lines(&[json!({"reason":"build-finished","success":true})]);
        for prefix in [
            b"Compiling example\n".as_slice(),
            b"\n",
            b"[]\n",
            b"null\n",
            b"{\"x\":1}\n",
            b"{\"reason\":1}\n",
            b"{\"reason\":\"a\",\"reason\":\"b\"}\n",
            b"{\"reason\":\"\xff\"}\n",
        ] {
            assert!(
                successful_messages(&[prefix, &good].concat()).is_err(),
                "should reject malformed Cargo line"
            );
        }
        assert!(
            successful_messages(&good[..good.len() - 1]).is_err(),
            "should require a final newline"
        );
        let oversized_line =
            lines(&[json!({"reason":"compiler-artifact","padding":"x".repeat(1024*1024)})]);
        assert!(
            successful_messages(&[oversized_line, good].concat()).is_err(),
            "should bound individual messages"
        );
        assert!(
            successful_messages(&vec![b' '; 16 * 1024 * 1024 + 1]).is_err(),
            "should bound the entire Cargo stream"
        );
    }
}
