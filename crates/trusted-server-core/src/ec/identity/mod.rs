//! Server-side identity providers.
//!
//! An `IdentityProvider` turns request signals into `OpenRTB` EIDs.
//! Providers only build requests and parse buffered responses. The
//! coordinator owns eligibility, consent, transport limits and KV
//! write-back, so every page load makes at most one browser-driven KV
//! write. See `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`.

pub mod descriptor;
pub mod ids;
pub mod resolution;

pub use descriptor::{
    Access, Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor,
    ProviderDescriptorBuilder, ResolveKind, Resolves, UsOptOut,
};
pub use ids::{IdentityProviderId, SourceDomain, UnixTime};
pub use resolution::IdentityProviderError;
