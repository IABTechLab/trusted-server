//! Server-side identity providers.
//!
//! An [`IdentityProvider`] turns request signals into `OpenRTB` EIDs.
//! Providers only build requests and parse buffered responses. The
//! coordinator owns eligibility, consent, transport limits and KV
//! write-back, so every page load makes at most one browser-driven KV
//! write. See `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`.

pub mod descriptor;
pub mod exchange;
pub mod ids;
pub mod input;
pub(crate) mod policy;
pub mod provider;
pub mod resolution;

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
