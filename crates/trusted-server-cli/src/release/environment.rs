//! Caller environment snapshots and the explicit GitHub credential.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;

use error_stack::Report;

use super::error::{ReleaseError, report};

/// Text substituted for the credential in every diagnostic.
pub(crate) const REDACTED: &str = "[redacted]";

/// A snapshot of the caller's environment, taken once.
///
/// The gh runner reads only explicit entries from it and never lets the child
/// inherit it.
#[derive(Clone, Debug, Default)]
pub(crate) struct CallerEnvironment(BTreeMap<OsString, OsString>);

impl CallerEnvironment {
    /// Snapshots the current process environment.
    pub(crate) fn from_process() -> Self {
        Self(std::env::vars_os().collect())
    }

    /// Builds a snapshot from explicit pairs.
    #[cfg(test)]
    pub(crate) fn from_pairs<K, V, I>(pairs: I) -> Self
    where
        K: Into<OsString>,
        V: Into<OsString>,
        I: IntoIterator<Item = (K, V)>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    /// Returns one variable's value.
    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.0.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    /// Iterates over variable names.
    pub(crate) fn names(&self) -> impl Iterator<Item = &OsStr> {
        self.0.keys().map(OsString::as_os_str)
    }
}

/// An explicit read-only GitHub credential.
///
/// Its [`fmt::Debug`] output never includes the secret.
#[derive(Clone)]
pub(crate) struct GithubToken(String);

impl fmt::Debug for GithubToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "GithubToken({REDACTED})")
    }
}

impl GithubToken {
    /// Selects explicit `GH_TOKEN`, else `GITHUB_TOKEN`, from the snapshot.
    ///
    /// Stored `gh auth login` credentials are deliberately not used.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Environment`] when neither variable holds a
    /// nonempty, printable, whitespace-free value.
    pub(crate) fn select(environment: &CallerEnvironment) -> Result<Self, Report<ReleaseError>> {
        let selected = ["GH_TOKEN", "GITHUB_TOKEN"]
            .into_iter()
            .find_map(|name| environment.get(name).filter(|value| !value.is_empty()))
            .ok_or_else(|| {
                report(
                    ReleaseError::Environment,
                    "release verification requires an explicit read-only GH_TOKEN \
                     (or GITHUB_TOKEN); stored `gh auth login` credentials are not used",
                )
            })?;
        let token = selected
            .to_str()
            .filter(|token| token.bytes().all(|byte| byte.is_ascii_graphic()))
            .ok_or_else(|| {
                report(
                    ReleaseError::Environment,
                    "the GitHub token must be printable ASCII without whitespace",
                )
            })?;
        Ok(Self(token.to_owned()))
    }

    /// Returns the secret for the single place it is transmitted.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Replaces every occurrence of the secret in `text`.
    pub(crate) fn redact(&self, text: &str) -> String {
        text.replace(&self.0, REDACTED)
    }
}
