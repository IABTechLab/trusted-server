//! Resolution rule for the deployed git version reported in `x-ts-version`.
//!
//! Shared by `build.rs` and `tests/git_version_resolve.rs` via `#[path]`, so it
//! must stay dependency-free.

use std::ffi::OsStr;

/// Returns the exact source commit in release mode, or `None` in development.
///
/// # Errors
///
/// Rejects any present release control except `1`, and release sources other
/// than exactly 40 lowercase ASCII hexadecimal bytes.
pub fn release_source(
    control: Option<&OsStr>,
    source: Option<&OsStr>,
) -> Result<Option<String>, &'static str> {
    match control {
        None => return Ok(None),
        Some(value) if value == OsStr::new("1") => {}
        Some(_) => return Err("TRUSTED_SERVER__RELEASE_BUILD must be exactly 1 when present"),
    }
    let source = source
        .and_then(OsStr::to_str)
        .ok_or("release builds require TRUSTED_SERVER__GIT_VERSION as a full lowercase commit")?;
    if source.len() != 40
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("release source must be exactly 40 lowercase ASCII hexadecimal bytes");
    }
    Ok(Some(source.to_owned()))
}

/// Resolve the actual build source, calling local Git only in development.
///
/// # Errors
///
/// Returns release input errors before invoking the fallback.
pub fn resolve_build_source(
    control: Option<&OsStr>,
    source: Option<&OsStr>,
    local_git: impl FnOnce() -> Option<String>,
) -> Result<Option<String>, &'static str> {
    if let Some(commit) = release_source(control, source)? {
        return Ok(Some(commit));
    }
    let override_value = source.and_then(OsStr::to_str);
    if override_value.is_some_and(is_usable) {
        Ok(resolve_git_version(&Candidates {
            override_value,
            exact_tag: None,
            branch: None,
            commit: None,
        }))
    } else {
        Ok(local_git())
    }
}

/// Raw candidates for the deployed git version, in priority order.
pub struct Candidates<'a> {
    /// `TRUSTED_SERVER__GIT_VERSION`, supplied by the deploy pipeline.
    pub override_value: Option<&'a str>,
    /// `git describe --tags --exact-match`.
    pub exact_tag: Option<&'a str>,
    /// `git symbolic-ref --short -q HEAD`.
    pub branch: Option<&'a str>,
    /// `git rev-parse HEAD`.
    pub commit: Option<&'a str>,
}

/// Whether `value`, once trimmed, is non-empty visible ASCII (`0x21`–`0x7E`).
///
/// Stricter than git, which also allows non-ASCII UTF-8 in ref names. Such a
/// ref is skipped so every compiled-in value is a `to_str()`-able header value.
#[must_use]
pub fn is_usable(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

/// Picks the first usable candidate: override, tag, branch, then the first 6
/// characters of the commit. `None` when nothing usable is known.
#[must_use]
pub fn resolve_git_version(candidates: &Candidates<'_>) -> Option<String> {
    usable(candidates.override_value)
        .or_else(|| usable(candidates.exact_tag))
        .or_else(|| usable(candidates.branch))
        .map(str::to_owned)
        .or_else(|| usable(candidates.commit).map(|c| c.chars().take(6).collect()))
}

/// The trimmed candidate, if it is usable.
fn usable(value: Option<&str>) -> Option<&str> {
    value.filter(|v| is_usable(v)).map(str::trim)
}
