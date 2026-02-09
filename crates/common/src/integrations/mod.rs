//! Integration module registry and sample implementations.

use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod didomi;
pub mod google_tag_manager;
pub mod lockr;
pub mod nextjs;
pub mod permutive;
pub mod prebid;
mod registry;
pub mod testlight;

pub use registry::{
    AttributeRewriteAction, AttributeRewriteOutcome, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationDocumentState, IntegrationEndpoint,
    IntegrationHtmlContext, IntegrationHtmlPostProcessor, IntegrationMetadata, IntegrationProxy,
    IntegrationRegistration, IntegrationRegistrationBuilder, IntegrationRegistry,
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

type IntegrationBuilder = fn(&Settings) -> Option<IntegrationRegistration>;

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[
        prebid::register,
        testlight::register,
        nextjs::register,
        permutive::register,
        lockr::register,
        didomi::register,
        google_tag_manager::register,
    ]
}
