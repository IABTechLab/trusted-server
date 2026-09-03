use std::net::IpAddr;

use error_stack::Report;

use super::{GeoInfo, PlatformError, PlatformGeo, RuntimeServices};

/// A geo provider that resolves nothing.
///
/// Installed when the `[geo] provider` selector is unset, and when it is set to
/// `"none"`, which spells the same choice explicitly. A client IP is then never
/// sent to any host geo service, so a default deployment is not tied to any host
/// geo capability. Every geo consumer already treats [`GeoInfo`] as optional, so
/// a `None` result degrades gracefully: the permission baseline falls back to
/// the top of the `permissions.yaml` rules tree, the auction omits geo, and so on.
///
/// Adapter crates should use this type rather than defining their own stub so
/// the behavior is the same on every platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledGeo;

#[async_trait::async_trait(?Send)]
impl PlatformGeo for DisabledGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}
