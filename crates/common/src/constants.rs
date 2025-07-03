use http::header::HeaderName;

pub const HEADER_SYNTHETIC_FRESH: HeaderName = HeaderName::from_static("x-synthetic-fresh");
pub const HEADER_SYNTHETIC_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
pub const HEADER_X_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
pub const HEADER_SYNTHETIC_TRUSTED_SERVER: HeaderName =
    HeaderName::from_static("x-synthetic-trusted-server");
pub const HEADER_X_CONSENT_ADVERTISING: HeaderName =
    HeaderName::from_static("x-consent-advertising");
pub const HEADER_X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
pub const HEADER_X_GEO_CITY: HeaderName = HeaderName::from_static("x-geo-city");
pub const HEADER_X_GEO_CONTINENT: HeaderName = HeaderName::from_static("x-geo-continent");
pub const HEADER_X_GEO_COORDINATES: HeaderName = HeaderName::from_static("x-geo-coordinates");
pub const HEADER_X_GEO_COUNTRY: HeaderName = HeaderName::from_static("x-geo-country");
pub const HEADER_X_GEO_INFO_AVAILABLE: HeaderName = HeaderName::from_static("x-geo-info-available");
pub const HEADER_X_GEO_METRO_CODE: HeaderName = HeaderName::from_static("x-geo-metro-code");
pub const HEADER_X_GEO_REGION: HeaderName = HeaderName::from_static("x-geo-region");
pub const HEADER_X_SUBJECT_ID: HeaderName = HeaderName::from_static("x-subject-id");
pub const HEADER_X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
pub const HEADER_X_COMPRESS_HINT: HeaderName = HeaderName::from_static("x-compress-hint");
pub const HEADER_X_DEBUG_FASTLY_POP: HeaderName = HeaderName::from_static("x-debug-fastly-pop");
