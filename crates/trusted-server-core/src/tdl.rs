//! Terms Document Locators, the labels saying what terms the data offered for
//! a request is available under.
//!
//! A locator is the address of a published document a person can read, stating
//! the basis on which the data at hand may be collected, shared and used. A
//! recipient reads the locators alongside the data, decides whether those terms
//! are ones it accepts, and decides on the same basis whether it may pass the
//! data on. No locator means no terms are declared, which a recipient must not
//! read as permission.
//!
//! Nothing here interprets a document. Core carries the locators a
//! [`PermissionSignalProvider`](crate::permission_signal::PermissionSignalProvider)
//! declares for the request and makes them visible to what consumes the data,
//! and the provider for a terms scheme decides which document applies. Model
//! Terms for Marketing (MTM) is the first such scheme to arrive and one of many
//! rather than the only one, because a publisher, a trade body or a regulator
//! can each publish terms and each set becomes a provider.
//!
//! # The document must not change
//!
//! A locator has to point at a document that is never edited once published,
//! which is why a version belongs in its address. A document that can be
//! rewritten tomorrow means a recipient can never prove what it agreed to, and
//! one edit silently rewrites the basis of every transaction already sent under
//! it. That is a property of how the document is published, so no code here can
//! check it, and it is the reason this type refuses nothing but an address that
//! could not be fetched at all.

use core::str::FromStr;

use error_stack::Report;
use url::Url;

use crate::error::TrustedServerError;

/// The address of a published terms document.
///
/// Absolute, and `http` or `https`, because a recipient has to be able to
/// fetch and read the document. See the module documentation for why the
/// document itself must be immutable and versioned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub struct Tdl(String);

impl Tdl {
    /// Builds a locator from an address.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when the address is not an
    /// absolute `http` or `https` URL with a host, because a recipient given
    /// one of those has nothing it can fetch.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::tdl::Tdl;
    ///
    /// let tdl = Tdl::new("https://terms.example.com/marketing/2.txt")
    ///     .expect("should accept an absolute https address");
    /// assert_eq!(tdl.as_str(), "https://terms.example.com/marketing/2.txt");
    ///
    /// assert!(Tdl::new("/marketing/2.txt").is_err());
    /// ```
    pub fn new(locator: &str) -> Result<Self, Report<TrustedServerError>> {
        let parsed = Url::parse(locator).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Terms document locator `{locator}` is not an absolute URL: {error}"
                ),
            })
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Terms document locator `{locator}` must be http or https, so a \
                     recipient can read the document, and not `{}`",
                    parsed.scheme()
                ),
            }));
        }
        if parsed.host().is_none() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("Terms document locator `{locator}` names no host"),
            }));
        }
        Ok(Self(locator.to_owned()))
    }

    /// The address, as it is carried to a recipient.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Tdl {
    type Err = Report<TrustedServerError>;

    fn from_str(locator: &str) -> Result<Self, Self::Err> {
        Self::new(locator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_https_address_is_accepted() {
        let tdl = Tdl::new("https://terms.example.com/marketing/2.txt")
            .expect("should accept an absolute https address");
        assert_eq!(
            tdl.as_str(),
            "https://terms.example.com/marketing/2.txt",
            "should carry the address unchanged"
        );
    }

    #[test]
    fn an_absolute_http_address_is_accepted() {
        assert!(
            Tdl::new("http://terms.example.com/marketing/2.txt").is_ok(),
            "should accept http, because a document served over http is still readable"
        );
    }

    #[test]
    fn a_relative_address_is_refused() {
        let error = Tdl::new("/marketing/2.txt")
            .expect_err("should refuse an address a recipient cannot resolve");
        assert!(
            format!("{error:?}").contains("absolute"),
            "should say the address is not absolute"
        );
    }

    #[test]
    fn another_scheme_is_refused() {
        let error =
            Tdl::new("mailto:terms@example.com").expect_err("should refuse a scheme with no page");
        assert!(
            format!("{error:?}").contains("http or https"),
            "should name the schemes a recipient can read"
        );
    }

    #[test]
    fn an_empty_address_is_refused() {
        assert!(
            Tdl::new("").is_err(),
            "should refuse an empty address rather than carry it to a recipient"
        );
    }

    #[test]
    fn parsing_from_a_string_gives_the_same_answer() {
        let parsed: Tdl = "https://terms.example.com/marketing/2.txt"
            .parse()
            .expect("should parse an absolute https address");
        assert_eq!(
            parsed,
            Tdl::new("https://terms.example.com/marketing/2.txt")
                .expect("should accept an absolute https address"),
            "should match the constructor"
        );
    }

    #[test]
    fn two_locators_for_the_same_document_compare_equal() {
        let one = Tdl::new("https://terms.example.com/marketing/2.txt")
            .expect("should accept an absolute https address");
        let two = Tdl::new("https://terms.example.com/marketing/2.txt")
            .expect("should accept an absolute https address");
        assert_eq!(
            one, two,
            "should compare equal so the same document is carried once"
        );
    }

    #[test]
    fn versions_of_one_document_are_different_locators() {
        let two = Tdl::new("https://terms.example.com/marketing/2.txt")
            .expect("should accept an absolute https address");
        let three = Tdl::new("https://terms.example.com/marketing/3.txt")
            .expect("should accept an absolute https address");
        assert_ne!(
            two, three,
            "should distinguish versions, because a recipient agreed to one of them"
        );
    }
}
