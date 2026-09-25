//! Resolution rule for the deployed git version reported in `x-ts-version`.
//!
//! Shared by `build.rs` and `tests/git_version_resolve.rs` via `#[path]`, so it
//! must stay dependency-free.

/// Raw candidates for the deployed git version, in priority order.
pub struct Candidates<'a> {
    /// `TRUSTED_SERVER_GIT_VERSION`, supplied by the deploy pipeline.
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
