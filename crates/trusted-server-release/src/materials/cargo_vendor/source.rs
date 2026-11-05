//! Cargo source identities of locked Git packages, for source replacement.

use error_stack::Report;

use crate::error::ReleaseError;
use crate::materials::material_error;

/// The Git reference a locked source was requested with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GitReference {
    /// No `?` selector in the lock: the repository's default branch.
    DefaultBranch,
    /// `?tag=<name>`.
    Tag(String),
    /// `?branch=<name>`.
    Branch(String),
    /// `?rev=<name>`.
    Rev(String),
}

/// One locked Git source as Cargo's `[source."<key>"]` table spells it.
///
/// The key is the exact lock source without its `#<commit>` suffix, which is
/// how `cargo vendor` names the table. Cargo also needs the `git` location
/// and the reference in the same table: a bare `replace-with` is rejected
/// with "no source location specified".
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitSourceConfig {
    key: String,
    url: String,
    reference: GitReference,
}

/// Characters a reference may carry; anything else would need escaping.
///
/// This deliberately rejects `/` (and so branch names such as `feature/x`,
/// which Cargo writes percent-encoded) and `%`: a locked source needing them
/// fails closed here until the encoding is reviewed and supported.
fn reference_text(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Requires an `https` URL that can sit unescaped inside a TOML basic string.
fn url_text(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && rest.bytes().all(|byte| {
                byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\' | b'#' | b'?' | b'%')
            })
    })
}

impl GitSourceConfig {
    /// Parses a lock source selector such as `git+https://example.com/org/repo?tag=v1`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Material`] for a selector that is not `git+https`,
    /// carries more than one reference, an empty or unusual reference value,
    /// or characters that would need escaping in a Cargo configuration.
    pub(crate) fn parse(selector: &str) -> Result<Self, Report<ReleaseError>> {
        let location = selector
            .strip_prefix("git+")
            .ok_or_else(|| material_error("Git source selector must start with git+"))?;
        let (url, query) = match location.split_once('?') {
            Some((url, query)) => (url, Some(query)),
            None => (location, None),
        };
        if !url_text(url) {
            return Err(material_error(
                "Git source URL must be plain https text without escapes",
            ));
        }
        let reference = match query {
            None => GitReference::DefaultBranch,
            Some(query) => match query.split_once('=') {
                Some((kind, value)) if reference_text(value) => match kind {
                    "tag" => GitReference::Tag(value.to_owned()),
                    "branch" => GitReference::Branch(value.to_owned()),
                    "rev" => GitReference::Rev(value.to_owned()),
                    _ => return Err(material_error("Git source reference kind is unsupported")),
                },
                _ => {
                    return Err(material_error(
                        "Git source must carry exactly one plain tag, branch or rev",
                    ));
                }
            },
        };
        Ok(Self {
            key: selector.to_owned(),
            url: url.to_owned(),
            reference,
        })
    }

    /// The exact locked selector, used as the Cargo source table name.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// The repository URL for the table's `git` key.
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// The reference as a Cargo key and value, or `None` for the default branch.
    pub(crate) fn reference(&self) -> Option<(&'static str, &str)> {
        match &self.reference {
            GitReference::DefaultBranch => None,
            GitReference::Tag(value) => Some(("tag", value)),
            GitReference::Branch(value) => Some(("branch", value)),
            GitReference::Rev(value) => Some(("rev", value)),
        }
    }
}
