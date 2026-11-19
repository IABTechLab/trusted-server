//! Integration module registry and sample implementations.

use error_stack::Report;

use crate::error::TrustedServerError;
use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod datadome;
pub mod didomi;
pub mod google_tag_manager;
pub mod gpt;
pub mod gpt_diagnostics;
pub mod js_asset_proxy;
pub mod lockr;
pub mod nextjs;
pub mod osano;
pub mod permutive;
pub mod prebid;
mod registry;
mod sdk_url;
#[cfg(test)]
mod sdk_url_test_support;
pub mod sourcepoint;
pub mod testlight;

pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, HeaderMutation, HeaderMutationMode,
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationDocumentState,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext,
    IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory, IntegrationMetadata,
    IntegrationProxy, IntegrationRegistration, IntegrationRegistrationBuilder, IntegrationRegistry,
    IntegrationRequestFilter, IntegrationScriptContext, IntegrationScriptRewriter,
    ProxyDispatchInput, RequestFilterDecision, RequestFilterEffects, RequestFilterInput,
    RequestFilterRegistryInput, RequestFilterRegistryOutcome, ScriptRewriteAction,
};

/// Maximum body size accepted by integration proxy endpoints (256 KiB).
pub(crate) const INTEGRATION_MAX_BODY_BYTES: usize = 256 * 1024;

/// Maximum response body size from RTB providers (prebid, aps, mediator).
pub(crate) const UPSTREAM_RTB_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum response body size from SDK/proxy integrations.
pub(crate) const UPSTREAM_SDK_MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

type IntegrationBuilderFn =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

pub(crate) struct IntegrationBuilder {
    id: &'static str,
    build: IntegrationBuilderFn,
}

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[
        // This must remain first: attribute rewriters chain replacements and short-circuit removals.
        IntegrationBuilder {
            id: js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID,
            build: js_asset_proxy::register,
        },
        #[cfg(feature = "test-utils")]
        IntegrationBuilder {
            id: crate::test_support::async_html_finalization::ID,
            build: crate::test_support::async_html_finalization::register,
        },
        IntegrationBuilder {
            id: "testlight",
            build: testlight::register,
        },
        // Next.js must precede GTM: GTM reads the Flight-protection snapshot
        // that the Next.js script stage records for the same fragment.
        IntegrationBuilder {
            id: "nextjs",
            build: nextjs::register,
        },
        IntegrationBuilder {
            id: "permutive",
            build: permutive::register,
        },
        IntegrationBuilder {
            id: "lockr",
            build: lockr::register,
        },
        IntegrationBuilder {
            id: "didomi",
            build: didomi::register,
        },
        IntegrationBuilder {
            id: "sourcepoint",
            build: sourcepoint::register,
        },
        IntegrationBuilder {
            id: "osano",
            build: osano::register,
        },
        IntegrationBuilder {
            id: "google_tag_manager",
            build: google_tag_manager::register,
        },
        IntegrationBuilder {
            id: "datadome",
            build: datadome::register,
        },
        IntegrationBuilder {
            id: "gpt",
            build: gpt::register,
        },
        IntegrationBuilder {
            id: "gpt_diagnostics",
            build: gpt_diagnostics::register,
        },
    ]
}

#[cfg(test)]
pub(crate) fn registered_builder_ids() -> impl Iterator<Item = &'static str> {
    builders().iter().map(|builder| builder.id)
}
