//! Geographic location utilities for the trusted server.
//!
//! This module provides Fastly-specific helpers for extracting geographic
//! information from incoming requests and writing geo headers to responses.
//!
//! The [`GeoInfo`] data type is defined in [`crate::platform`] as platform-
//! neutral data; this module re-exports it and adds Fastly-coupled `impl`
//! blocks for construction and response header injection.

use fastly::geo::{geo_lookup, Geo};
use fastly::{Request, Response};

pub use crate::platform::GeoInfo;

use crate::constants::{
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE, HEADER_X_GEO_REGION,
};

/// Convert a Fastly [`Geo`] value into a platform-neutral [`GeoInfo`].
///
/// Shared by [`GeoInfo::from_request`] and `FastlyPlatformGeo::lookup` in
/// `trusted-server-adapter-fastly` so that field mapping is never duplicated.
pub fn geo_from_fastly(geo: &Geo) -> GeoInfo {
    GeoInfo {
        city: geo.city().to_string(),
        country: geo.country_code().to_string(),
        continent: format!("{:?}", geo.continent()),
        latitude: geo.latitude(),
        longitude: geo.longitude(),
        metro_code: geo.metro_code(),
        region: geo.region().map(str::to_string),
    }
}

impl GeoInfo {
    /// Creates a new `GeoInfo` from a request by performing a geo lookup.
    ///
    /// # Legacy
    ///
    /// This is a Fastly-coupled convenience method that predates the
    /// `platform` abstraction. New code should use
    /// `RuntimeServices::geo.lookup(client_info.client_ip)` instead, which
    /// goes through [`crate::platform::PlatformGeo`] and does not require
    /// direct access to the Fastly request.
    ///
    /// # Returns
    ///
    /// `Some(GeoInfo)` if geo data is available, `None` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(geo_info) = GeoInfo::from_request(&req) {
    ///     println!("User is in {} ({})", geo_info.city, geo_info.country);
    /// }
    /// ```
    pub fn from_request(req: &Request) -> Option<Self> {
        req.get_client_ip_addr()
            .and_then(geo_lookup)
            .map(|geo| geo_from_fastly(&geo))
    }

    /// Sets geo information headers on the response.
    ///
    /// Adds `x-geo-city`, `x-geo-country`, `x-geo-continent`, `x-geo-coordinates`,
    /// `x-geo-metro-code`, `x-geo-region` (when available), and
    /// `x-geo-info-available: true` to the given response.
    pub fn set_response_headers(&self, response: &mut Response) {
        response.set_header(HEADER_X_GEO_CITY, &self.city);
        response.set_header(HEADER_X_GEO_COUNTRY, &self.country);
        response.set_header(HEADER_X_GEO_CONTINENT, &self.continent);
        response.set_header(HEADER_X_GEO_COORDINATES, self.coordinates_string());
        if self.has_metro_code() {
            response.set_header(HEADER_X_GEO_METRO_CODE, self.metro_code.to_string());
        }
        if let Some(ref region) = self.region {
            response.set_header(HEADER_X_GEO_REGION, region);
        }
        response.set_header(HEADER_X_GEO_INFO_AVAILABLE, "true");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::Response;

    fn sample_geo_info() -> GeoInfo {
        GeoInfo {
            city: "San Francisco".to_string(),
            country: "US".to_string(),
            continent: "NorthAmerica".to_string(),
            latitude: 37.7749,
            longitude: -122.4194,
            metro_code: 807,
            region: Some("CA".to_string()),
        }
    }

    #[test]
    fn set_response_headers_sets_all_geo_headers() {
        let geo = sample_geo_info();
        let mut response = Response::new();

        geo.set_response_headers(&mut response);

        assert_eq!(
            response
                .get_header(HEADER_X_GEO_CITY)
                .expect("should have city header")
                .to_str()
                .expect("should be valid str"),
            "San Francisco",
            "should set city header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_COUNTRY)
                .expect("should have country header")
                .to_str()
                .expect("should be valid str"),
            "US",
            "should set country header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_CONTINENT)
                .expect("should have continent header")
                .to_str()
                .expect("should be valid str"),
            "NorthAmerica",
            "should set continent header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_COORDINATES)
                .expect("should have coordinates header")
                .to_str()
                .expect("should be valid str"),
            "37.7749,-122.4194",
            "should set coordinates header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_METRO_CODE)
                .expect("should have metro code header")
                .to_str()
                .expect("should be valid str"),
            "807",
            "should set metro code header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_REGION)
                .expect("should have region header")
                .to_str()
                .expect("should be valid str"),
            "CA",
            "should set region header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_INFO_AVAILABLE)
                .expect("should have info available header")
                .to_str()
                .expect("should be valid str"),
            "true",
            "should set geo info available to true"
        );
    }

    #[test]
    fn set_response_headers_omits_metro_code_when_zero() {
        let geo = GeoInfo {
            metro_code: 0,
            ..sample_geo_info()
        };
        let mut response = Response::new();

        geo.set_response_headers(&mut response);

        assert!(
            response.get_header(HEADER_X_GEO_METRO_CODE).is_none(),
            "should not set metro code header when metro_code is 0"
        );
        assert!(
            response.get_header(HEADER_X_GEO_CITY).is_some(),
            "should still set city header"
        );
    }

    #[test]
    fn set_response_headers_omits_region_when_none() {
        let geo = GeoInfo {
            region: None,
            ..sample_geo_info()
        };
        let mut response = Response::new();

        geo.set_response_headers(&mut response);

        assert!(
            response.get_header(HEADER_X_GEO_REGION).is_none(),
            "should not set region header when region is None"
        );
        assert!(
            response.get_header(HEADER_X_GEO_CITY).is_some(),
            "should still set city header"
        );
        assert_eq!(
            response
                .get_header(HEADER_X_GEO_INFO_AVAILABLE)
                .expect("should have info available header")
                .to_str()
                .expect("should be valid str"),
            "true",
            "should still set geo info available to true"
        );
    }
}
