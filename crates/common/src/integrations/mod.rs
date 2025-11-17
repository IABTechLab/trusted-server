//! Integration module registry and sample implementations.

mod registry;
pub mod starlight;

pub use registry::{
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationEndpoint,
    IntegrationProxy, IntegrationRegistry,
};
