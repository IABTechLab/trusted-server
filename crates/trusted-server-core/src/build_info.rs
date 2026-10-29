//! Immutable facts compiled into an owning product.
//!
//! These facts identify build inputs. They do not establish release provenance
//! or publishing authority; release tags, run IDs and artifact checksums remain
//! external to the compiled product.

use crate::constants::TS_GIT_VERSION;

/// The owning product's compile-time feature variant.
#[derive(Debug, Clone, Copy, Eq, PartialEq, derive_more::Display)]
pub enum BuildVariant {
    /// The product's default feature set.
    #[display("default")]
    Default,
    /// Fastly's opt-in reusable sandbox feature.
    #[display("reusable-sandbox")]
    ReusableSandbox,
}

/// Immutable package, source, target and variant facts for an owning product.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BuildInfo {
    package_version: &'static str,
    source: Option<&'static str>,
    target: &'static str,
    variant: BuildVariant,
}

impl BuildInfo {
    /// Defines owning product facts using the source compiled into core.
    ///
    /// Call this in an owning constant with Cargo's package version, its build
    /// script's target and its own feature variant.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::build_info::{BuildInfo, BuildVariant};
    /// const FACTS: BuildInfo = BuildInfo::new("1.2.3", "example-target", BuildVariant::Default);
    /// assert_eq!(FACTS.package_version(), "1.2.3");
    /// ```
    #[must_use]
    pub const fn new(
        package_version: &'static str,
        target: &'static str,
        variant: BuildVariant,
    ) -> Self {
        Self {
            package_version,
            source: TS_GIT_VERSION,
            target,
            variant,
        }
    }

    /// Returns the owning product's Cargo package version.
    #[must_use]
    pub const fn package_version(&self) -> &'static str {
        self.package_version
    }

    /// Returns the compiled source identity, when known.
    ///
    /// Release builds require a complete commit. Development builds retain
    /// the existing tag, branch and abbreviated commit resolution.
    #[must_use]
    pub const fn source(&self) -> Option<&'static str> {
        self.source
    }

    /// Returns the owning product's Cargo target triple.
    #[must_use]
    pub const fn target(&self) -> &'static str {
        self.target
    }

    /// Returns the owning product's feature variant.
    #[must_use]
    pub const fn variant(&self) -> BuildVariant {
        self.variant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_display_canonical_product_labels() {
        assert_eq!(
            BuildVariant::Default.to_string(),
            "default",
            "should label the default product variant"
        );
        assert_eq!(
            BuildVariant::ReusableSandbox.to_string(),
            "reusable-sandbox",
            "should label the reusable sandbox product variant"
        );
    }
}
