use std::net::IpAddr;

use error_stack::Report;

use super::{GeoInfo, PlatformError, PlatformGeo};

/// A geo provider that resolves nothing.
///
/// Installed when the `[geo] provider` selector is unset, and when it is set to
/// `"none"`, which spells the same choice explicitly. A client IP is then never
/// sent to any host geo service, so a default deployment is not tied to any host
/// geo capability. Every geo consumer already treats [`GeoInfo`] as optional, so
/// a `None` result degrades gracefully: the permission baseline falls back to
/// the configured `[geo] default_country`, the auction omits geo, and so on.
///
/// Adapter crates should use this type rather than defining their own stub so
/// the behavior is the same on every platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledGeo;

impl PlatformGeo for DisabledGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}
