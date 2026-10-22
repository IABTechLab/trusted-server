//! Server-side identity providers.
//!
//! An [`IdentityProvider`] turns request signals
//! into `OpenRTB` EIDs. Providers only build requests and parse buffered
//! responses. The coordinator owns eligibility, consent, transport limits and
//! KV write-back, so every page load makes at most one browser-driven KV
//! write.

// Several crate-private helpers are only called once the coordinator is wired
// into requests later in this series. Remove this allow when that lands.
#![allow(
    dead_code,
    reason = "crate-private identity helpers are wired into requests by later commits"
)]

pub(crate) mod catalog;
pub mod descriptor;
pub mod exchange;
pub mod ids;
pub mod input;
pub(crate) mod policy;
pub mod provider;
pub mod resolution;
pub(crate) mod revision;
pub(crate) mod servable;
#[cfg(test)]
pub(crate) mod test_support;

pub use descriptor::{
    Access, Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor,
    ProviderDescriptorBuilder, ResolveKind, Resolves, UsOptOut,
};
pub use exchange::{ProviderRequest, ProviderResponse};
pub use ids::{IdentityProviderId, SourceDomain, UnixTime};
pub use input::{CaptureInput, HashedEmail, ProviderConsent, Region, ResolveInput, StoredId};
pub use provider::{IdentityProvider, IdentityProviderConfig};
pub use resolution::{IdentityProviderError, Resolution, ResolvedId, StateChange, When};
