//! The reader's location, the platform-neutral [`GeoInfo`] that a geo module
//! resolves for a request, re-exported from [`crate::platform`].
//!
//! Core writes no part of it into a response. A serve middleware that a
//! deployment places does that, from the location the request resolved.

pub use crate::platform::GeoInfo;
