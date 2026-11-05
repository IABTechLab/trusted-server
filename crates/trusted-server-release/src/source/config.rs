//! Strict allowlist parser for the repository's `.git/config`.
//!
//! Git would execute or redirect through many configuration keys, so only a
//! small set of inert keys and values is accepted. Values are never echoed
//! because remote URLs may embed credentials.

use error_stack::Report;

use super::source_error;
use crate::error::ReleaseError;

/// Accepts only allowlisted repository configuration keys and values.
///
/// # Errors
///
/// Fails on any key outside the allowlist, escapes or continuations, unknown
/// section syntax, or unreviewed values.
pub(super) fn check_git_config(text: &str) -> Result<(), Report<ReleaseError>> {
    let mut section: Option<(String, bool)> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let number = index + 1;
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.contains('\\') {
            return Err(source_error(format!(
                "git config line {number} uses escapes or continuations"
            )));
        }
        if let Some(header) = line.strip_prefix('[') {
            section = Some(config_section(header, number)?);
            continue;
        }
        let (name, subsection) = section.as_ref().ok_or_else(|| {
            source_error(format!("git config line {number} is outside a section"))
        })?;
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim().to_ascii_lowercase(), Some(value.trim())),
            None => (line.to_ascii_lowercase(), None),
        };
        if !config_value_allowed(name, *subsection, &key, value) {
            return Err(source_error(format!(
                "git config key {name}.{key} on line {number} is not allowlisted or has an unreviewed value"
            )));
        }
    }
    Ok(())
}

fn config_section(header: &str, number: usize) -> Result<(String, bool), Report<ReleaseError>> {
    let invalid = || {
        source_error(format!(
            "git config line {number} has an unsupported section"
        ))
    };
    let header = header.strip_suffix(']').ok_or_else(invalid)?;
    let (name, subsection) = match header.split_once(' ') {
        Some((name, quoted)) => {
            let inner = quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .ok_or_else(invalid)?;
            if inner.contains('"') {
                return Err(invalid());
            }
            (name, true)
        }
        None => (header, false),
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid());
    }
    Ok((name.to_ascii_lowercase(), subsection))
}

fn config_value_allowed(section: &str, subsection: bool, key: &str, value: Option<&str>) -> bool {
    let boolean = matches!(value, Some("true" | "false"));
    let plain = value.is_some_and(|value| !value.is_empty() && !value.contains(['"', '#', ';']));
    match (section, subsection, key) {
        ("core", false, "repositoryformatversion") => value == Some("0"),
        ("core", false, "bare") => value == Some("false"),
        (
            "core",
            false,
            "filemode" | "logallrefupdates" | "ignorecase" | "precomposeunicode" | "symlinks",
        ) => boolean,
        ("remote", true, "url" | "fetch") | ("branch", true, "remote" | "merge") => plain,
        _ => false,
    }
}
