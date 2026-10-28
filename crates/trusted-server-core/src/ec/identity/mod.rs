//! Server-side identity providers.
//!
//! An [`IdentityProvider`] turns request signals into `OpenRTB` EIDs.
//! Providers only build requests and parse buffered responses. The
//! coordinator owns eligibility, consent, transport limits and KV
//! write-back, so every page load makes at most one browser-driven KV
//! write. See `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`.

pub(crate) mod catalog;
pub(crate) mod coordinator;
pub(crate) mod denials;
pub mod descriptor;
#[cfg(test)]
pub(crate) mod examples;
pub mod exchange;
pub mod flush;
pub(crate) mod generic_pull;
pub mod ids;
pub mod input;
pub mod measurement;
pub(crate) mod mirror;
pub(crate) mod mutation;
pub(crate) mod pending;
pub(crate) mod policy;
pub mod provider;
pub(crate) mod removal;
pub(crate) mod request;
pub mod resolution;
pub(crate) mod revision;
pub mod runtime;
pub(crate) mod servable;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use descriptor::{
    Access, Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor,
    ProviderDescriptorBuilder, ResolveKind, Resolves, UsOptOut,
};
pub use exchange::{ProviderRequest, ProviderResponse};
pub use ids::{IdentityProviderId, SourceDomain, UnixTime};
pub use input::{CaptureInput, HashedEmail, ProviderConsent, Region, ResolveInput, StoredId};
pub use provider::{IdentityProvider, IdentityProviderConfig};
pub use resolution::{
    IdentityProviderError, Resolution, ResolvedEid, ResolvedUid, StateChange, When,
};
pub use runtime::IdentityRuntime;
