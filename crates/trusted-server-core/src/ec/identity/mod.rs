//! Server-side identity providers.
//!
//! An [`IdentityProvider`](provider::IdentityProvider) turns request signals
//! into `OpenRTB` EIDs. Providers only build requests and parse buffered
//! responses. The coordinator owns eligibility, consent, transport limits and
//! KV write-back, so every page load makes at most one browser-driven KV
//! write.

pub mod descriptor;
pub mod ids;
pub mod resolution;

pub use descriptor::{Access, Input};
pub use ids::{IdentityProviderId, SourceDomain, UnixTime};
pub use resolution::IdentityProviderError;
