//! What an adapter reports about the platform a deployment runs on.

use serde::Serialize;

/// What the adapter reports about the platform the deployment runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformIdentity {
    /// The platform's name.
    pub name: String,
    /// The platform's identifier for the service running this build.
    pub service_id: String,
    /// The platform's version number of that service.
    pub service_version: String,
    /// Whether the platform's staging environment answered.
    pub staging: bool,
    /// The location that answered.
    pub pop: String,
}

/// What a fact the platform did not report reads as.
pub const UNKNOWN: &str = "unknown";

impl PlatformIdentity {
    /// A platform that reports its name and nothing else.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            service_id: UNKNOWN.to_owned(),
            service_version: UNKNOWN.to_owned(),
            staging: false,
            pop: UNKNOWN.to_owned(),
        }
    }
}

impl Default for PlatformIdentity {
    /// A platform nothing has been reported about.
    fn default() -> Self {
        Self::named(UNKNOWN)
    }
}
