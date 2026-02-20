use http::header::HeaderName;

pub const COOKIE_SYNTHETIC_ID: &str = "synthetic_id";

pub const HEADER_X_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
pub const HEADER_X_SYNTHETIC_ID: HeaderName = HeaderName::from_static("x-synthetic-id");
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

// Staging / version identification headers
pub const HEADER_X_TS_VERSION: HeaderName = HeaderName::from_static("x-ts-version");
pub const HEADER_X_TS_ENV: HeaderName = HeaderName::from_static("x-ts-env");

// Fastly environment variables
pub const ENV_FASTLY_SERVICE_VERSION: &str = "FASTLY_SERVICE_VERSION";
pub const ENV_FASTLY_IS_STAGING: &str = "FASTLY_IS_STAGING";

// Common standard header names used across modules
pub const HEADER_USER_AGENT: HeaderName = HeaderName::from_static("user-agent");
pub const HEADER_ACCEPT: HeaderName = HeaderName::from_static("accept");
pub const HEADER_ACCEPT_LANGUAGE: HeaderName = HeaderName::from_static("accept-language");
pub const HEADER_ACCEPT_ENCODING: HeaderName = HeaderName::from_static("accept-encoding");
pub const HEADER_REFERER: HeaderName = HeaderName::from_static("referer");
