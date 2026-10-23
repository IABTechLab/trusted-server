//! Checks a deliberately closed, reviewed validation workflow without executing it.

use crate::error::ReleaseError;
use error_stack::Report;
use serde_yaml_ng::Value;
use sha2::{Digest as _, Sha256};
use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

// SHA-256 of compact sorted-key JSON produced from the reviewed YAML AST.
// Updating this value requires reviewing every command, input and action again.
const WORKFLOW_SHA256: &str = "5f5e965ad60ade5766d59162b7f89054fac00c5a7d634c1335eb1c8fc10e659f";

// Raw bytes additionally bind each version comment and the restricted YAML spelling.
const RAW_WORKFLOW_SHA256: &str =
    "59bb3a304f4865b45d2e1ddd74121910dbe1e763648cb0eea7b725d02fb344a7";

// The fixed sibling is executable validation policy and has its own reviewed pin.
const HELPER_SHA256: &str = "48a6728f812c1cc23ed78f02b90f0c32922ec4af750eabe8f4d94d4de1ac30dc";

const MAX_BYTES: usize = 1024 * 1024;

fn parse_document(text: &str) -> Result<Value, Report<ReleaseError>> {
    if text.len() > MAX_BYTES {
        return Err(reject("workflow exceeds 1 MiB"));
    }
    restricted_syntax(text)?;
    let value = serde_yaml_ng::from_str(text)
        .map_err(|error| Report::new(ReleaseError::Document).attach(error.to_string()))?;
    unambiguous_value(&value)?;
    Ok(value)
}

fn reject(message: &str) -> Report<ReleaseError> {
    Report::new(ReleaseError::Document).attach(message.to_owned())
}

// Only literal run blocks may contain reference indicators, including in quotes.
// Rejecting unsupported spellings is intentional: this is not a YAML formatter.
fn restricted_syntax(text: &str) -> Result<(), Report<ReleaseError>> {
    let mut block_indent = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if let Some(parent) = block_indent {
            if line.trim().is_empty() || indent > parent {
                continue;
            }
            block_indent = None;
        }
        let mut quote = None;
        let mut characters = line.chars().peekable();
        while let Some(character) = characters.next() {
            match (quote, character) {
                (Some('"'), '\\') => {
                    characters.next();
                }
                (Some('\''), '\'') if characters.peek() == Some(&'\'') => {
                    characters.next();
                }
                (Some(active), value) if active == value => quote = None,
                (Some(_), _) => {}
                (None, '#') => break,
                (None, '\'' | '"') => quote = Some(character),
                _ => {}
            }
        }
        if quote.is_some() {
            return Err(reject("multiline quoted scalars are unsupported"));
        }
        if line.contains(['&', '*', '!']) || line.contains('\t') {
            return Err(reject(
                "reference indicators, tags and tabs outside run blocks are unsupported",
            ));
        }
        if line.trim() == "run: |" {
            block_indent = Some(indent);
        }
    }
    Ok(())
}

fn unambiguous_value(value: &Value) -> Result<(), Report<ReleaseError>> {
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                let Some(key) = key.as_str() else {
                    return Err(reject("mapping keys must be strings"));
                };
                if key == "<<" {
                    return Err(reject("YAML merge keys are forbidden"));
                }
                unambiguous_value(value)?;
            }
        }
        Value::Sequence(sequence) => {
            for value in sequence {
                unambiguous_value(value)?;
            }
        }
        Value::Tagged(_) => return Err(reject("YAML tags are forbidden")),
        _ => {}
    }
    Ok(())
}

fn validate_structure(value: &Value, text: &str) -> Result<(), Report<ReleaseError>> {
    let triggers = value
        .get("on")
        .and_then(Value::as_mapping)
        .ok_or_else(|| reject("on must contain workflow_call only"))?;
    if triggers.len() != 1 || !triggers.contains_key("workflow_call") {
        return Err(reject("workflow_call is the only permitted trigger"));
    }
    if let Some(call) = triggers
        .get("workflow_call")
        .filter(|value| !value.is_null())
    {
        let call = call
            .as_mapping()
            .ok_or_else(|| reject("workflow_call must be a mapping"))?;
        let inputs = call
            .get("inputs")
            .and_then(Value::as_mapping)
            .ok_or_else(|| reject("workflow_call permits only the source_sha input"))?;
        let source = inputs
            .get("source_sha")
            .ok_or_else(|| reject("source_sha is required"))?;
        if call.len() != 1
            || inputs.len() != 1
            || source.get("type").and_then(Value::as_str) != Some("string")
            || source.get("required").and_then(Value::as_bool) != Some(true)
        {
            return Err(reject("only required string source_sha is permitted"));
        }
    }
    let permissions = value
        .get("permissions")
        .and_then(Value::as_mapping)
        .ok_or_else(|| reject("explicit read permissions are required"))?;
    if permissions.len() != 1 || permissions.get("contents").and_then(Value::as_str) != Some("read")
    {
        return Err(reject("only contents: read is permitted at workflow scope"));
    }
    validate_actions(value, text)?;
    Ok(())
}

/// Reads the explicit validation workflow and its fixed sibling helper.
///
/// This local check executes nothing and constructs no release authorization.
///
/// # Errors
///
/// Returns an error for unsupported paths, syntax, authority, or unreviewed bytes.
pub(crate) fn check_files(paths: &[PathBuf]) -> Result<(), Report<ReleaseError>> {
    let [path] = paths else {
        return Err(reject("exactly one reviewed workflow path is required"));
    };
    if path.file_name().and_then(|name| name.to_str()) != Some("release-validation.yml") {
        return Err(reject(
            "only release-validation.yml is reviewed; local invocation graphs remain unsupported",
        ));
    }
    let bytes = read_bounded(path)?;
    if hex::encode(Sha256::digest(&bytes)) != RAW_WORKFLOW_SHA256 {
        return Err(reject(
            "workflow bytes differ from the reviewed source including action annotations",
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| reject("workflow must be UTF-8"))?;
    let value = parse_document(text)?;
    validate_structure(&value, text)?;
    let json = serde_json::to_value(value)
        .map_err(|_| reject("workflow cannot be represented as JSON"))?;
    let canonical =
        serde_json::to_vec(&json).map_err(|_| reject("workflow canonicalization failed"))?;
    if hex::encode(Sha256::digest(&canonical)) != WORKFLOW_SHA256 {
        return Err(reject(
            "workflow differs from the reviewed structural command manifest",
        ));
    }
    let helper = read_bounded(&path.with_file_name("release-validation.mjs"))?;
    if hex::encode(Sha256::digest(&helper)) != HELPER_SHA256 {
        return Err(reject(
            "validation helper differs from the reviewed executable policy",
        ));
    }
    Ok(())
}

fn validate_actions(value: &Value, text: &str) -> Result<(), Report<ReleaseError>> {
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if key.as_str() == Some("uses") {
                    let action = value
                        .as_str()
                        .ok_or_else(|| reject("uses must be a string"))?;
                    let (repository, commit) = action
                        .rsplit_once('@')
                        .ok_or_else(|| reject("actions must have immutable pins"))?;
                    if repository.starts_with('.')
                        || !repository.contains('/')
                        || commit.len() != 40
                        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err(reject(
                            "local calls and non-commit action pins are forbidden",
                        ));
                    }
                    let annotated = text.lines().any(|line| {
                        let line = line.trim();
                        let Some(rest) = line
                            .strip_prefix("- uses: ")
                            .or_else(|| line.strip_prefix("uses: "))
                        else {
                            return false;
                        };
                        rest.strip_prefix(action)
                            .and_then(|rest| rest.strip_prefix(" # v"))
                            .is_some_and(|version| {
                                version.starts_with(|character: char| character.is_ascii_digit())
                            })
                    });
                    if !annotated {
                        return Err(reject("action pins require an adjacent version comment"));
                    }
                }
                validate_actions(value, text)?;
            }
        }
        Value::Sequence(sequence) => {
            for value in sequence {
                validate_actions(value, text)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, Report<ReleaseError>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| reject(&error.to_string()))?;
    if !file
        .metadata()
        .map_err(|error| reject(&error.to_string()))?
        .is_file()
    {
        return Err(reject("held workflow input must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| reject(&error.to_string()))?;
    if bytes.len() > MAX_BYTES {
        return Err(reject("workflow input exceeds 1 MiB"));
    }
    Ok(bytes)
}
