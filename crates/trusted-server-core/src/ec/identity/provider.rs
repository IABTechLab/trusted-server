//! The identity provider trait.

use error_stack::Report;

use super::descriptor::ProviderDescriptor;
use super::exchange::{ProviderRequest, ProviderResponse};
use super::input::{CaptureInput, ResolveInput};
use super::resolution::{IdentityProviderError, Resolution};
use crate::settings::IntegrationConfig;

/// A vendor that turns server-side request signals into EIDs.
///
/// Only [`descriptor`](Self::descriptor) is required. A provider adds a
/// capability by declaring it in its descriptor and overriding the matching
/// methods:
///
/// - active resolution: [`Resolves`](super::descriptor::Resolves), plus [`request`](Self::request) and
///   [`response`](Self::response)
/// - passive capture: [`Captures`](super::descriptor::Captures), plus [`capture`](Self::capture)
///
/// Methods added to this trait later get default bodies, so existing
/// providers keep compiling. No method takes generic parameters, so the
/// trait stays object-safe. Every default body returns the
/// [`IdentityProviderError::not_implemented`] marker, so a capability that is
/// declared but not implemented is caught by the harness.
pub trait IdentityProvider: Send + Sync {
    /// What this provider is, what it may write, what it needs and which
    /// consent it requires.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Builds the call for this request, or `None` to skip it this time.
    ///
    /// Skipping is not a failure: the coordinator waits `refresh_after_sec`
    /// before asking again (section 8).
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::UndeclaredInput`] when it reads
    /// something its descriptor did not declare, or another
    /// [`IdentityProviderError`] when the request cannot be built.
    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("request"))
    }

    /// Turns the buffered response to [`request`](Self::request) into a
    /// resolution.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Rejected`] for a refusal and
    /// [`IdentityProviderError::Malformed`] for a body it cannot parse.
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = (response, input);
        Err(IdentityProviderError::not_implemented("response"))
    }

    /// Reads IDs from a response TS proxied for this provider, with no
    /// outbound call. [`Resolution::no_match`] means the response held
    /// nothing to capture, and costs nothing: capture never backs off.
    ///
    /// # Errors
    ///
    /// Returns an [`IdentityProviderError`] only for a provider bug. A body
    /// it does not recognise is `Ok(Resolution::no_match())`, not an error.
    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("capture"))
    }
}

/// Implemented by each identity integration's config type.
///
/// Builds the provider's descriptor from config alone, with no secrets, so
/// `ts config validate` checks ownership against the same descriptor the
/// running provider uses.
pub trait IdentityProviderConfig: IntegrationConfig {
    /// Describes the provider this config enables.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>;
}

// Keeps the trait object-safe. Fails to compile if a method gains generics.
const _: Option<&dyn IdentityProvider> = None;
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use http::Method;

    use super::*;
    use crate::ec::identity::descriptor::Captures;
    use crate::ec::identity::exchange::ProviderResponse;
    use crate::ec::identity::ids::UnixTime;
    use crate::ec::identity::input::{CaptureInput, ProviderConsent};

    struct DescriptorOnly(ProviderDescriptor);

    impl IdentityProvider for DescriptorOnly {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    #[test]
    fn default_capability_methods_return_the_not_implemented_marker() {
        let provider = DescriptorOnly(
            ProviderDescriptor::builder("p".parse().expect("should parse the provider id"))
                .with_source("example.com".parse().expect("should parse the source"))
                .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                .build()
                .expect("should build"),
        );
        let boxed: Box<dyn IdentityProvider> = Box::new(provider);
        let response = ProviderResponse::for_test(http::StatusCode::OK, b"{}");
        let consent = ProviderConsent::default();
        let stored = BTreeMap::new();
        let input = CaptureInput::new(
            boxed.descriptor(),
            "/integrations/p/api",
            &response,
            &stored,
            &consent,
            "www.example.com",
            UnixTime::from_secs(1),
        );

        let error = boxed
            .capture(&input)
            .expect_err("should return the not-implemented marker");
        assert!(
            error.current_context().is_not_implemented(),
            "should carry the marker"
        );
        assert!(
            !IdentityProviderError::config("other")
                .current_context()
                .is_not_implemented(),
            "should not treat an ordinary Config error as the marker"
        );
    }
}
