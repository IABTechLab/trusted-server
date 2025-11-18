//! Integration module registry and sample implementations.

use crate::settings::Settings;

mod registry;
pub mod testlight;

pub use registry::{
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationEndpoint,
    IntegrationMetadata, IntegrationProxy, IntegrationRegistration, IntegrationRegistrationBuilder,
    IntegrationRegistry, IntegrationScriptContext, IntegrationScriptRewriter,
};

type IntegrationBuilder = fn(&Settings) -> Option<IntegrationRegistration>;

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[testlight::register]
}
