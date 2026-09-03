//! The Fastly host geo provider.
//!
//! [`FastlyPlatformGeo`] implements [`PlatformGeo`] using Fastly's `geo_lookup`,
//! for deployments on Fastly Compute. It is the host platform's geo provider,
//! injected by the Fastly adapter via `build_geo_provider`. With no selector,
//! or `provider = "platform"`, this host lookup resolves the location, and
//! `provider = "none"` disables geo instead.
//!
//! Like the Fastly device provider, this crate calls the Fastly SDK directly,
//! so it depends on the `fastly` crate and builds only for the `wasm32-wasip1`
//! target. The platform-neutral `PlatformGeo` trait and the `DisabledGeo`
//! default both live in `trusted-server-core`.

use std::net::IpAddr;

use error_stack::Report;
use fastly::geo::{Geo, geo_lookup};
use trusted_server_core::platform::{GeoInfo, PlatformError, PlatformGeo};

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
        asn: None,
    }
}

/// Fastly geo-lookup implementation of [`PlatformGeo`].
///
/// The host platform geo provider for Fastly Compute. The adapter injects it via
/// `build_geo_provider`. With no selector, or `provider = "platform"`, it
/// resolves the location, and `provider = "none"` disables geo instead.
pub struct FastlyPlatformGeo;

#[async_trait::async_trait(?Send)]
impl PlatformGeo for FastlyPlatformGeo {
    async fn lookup(
        &self,
        client_ip: Option<IpAddr>,
        _services: &trusted_server_core::platform::RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(client_ip
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo)))
    }
}
