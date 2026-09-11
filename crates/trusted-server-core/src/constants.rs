//! Canonical cookie, header, and environment-variable names.

use http::header::HeaderName;

/// Edge Cookie identifier cookie.
pub const COOKIE_TS_EC: &str = "ts-ec";
/// Cookie written by the Trusted Server JS SDK containing a standard-base64-encoded
/// JSON array of Extended User IDs (`[{ source, uids }]`) from identity providers.
pub const COOKIE_TS_EIDS: &str = "ts-eids";
/// Opt-in cookie for tester-only behavior.
pub const COOKIE_TS_TESTER: &str = "ts-tester";
/// Prebid shared-ID cookie.
pub const COOKIE_SHAREDID: &str = "sharedId";

/// Publisher-provided user identifier header.
pub const HEADER_X_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
/// Edge Cookie identifier header.
pub const HEADER_X_TS_EC: HeaderName = HeaderName::from_static("x-ts-ec");
/// Extended User IDs header.
pub const HEADER_X_TS_EIDS: HeaderName = HeaderName::from_static("x-ts-eids");
/// Edge Cookie consent-decision header.
pub const HEADER_X_TS_EC_CONSENT: HeaderName = HeaderName::from_static("x-ts-ec-consent");
/// Marker indicating the Extended User IDs value was truncated.
pub const HEADER_X_TS_EIDS_TRUNCATED: HeaderName = HeaderName::from_static("x-ts-eids-truncated");
/// Normalized advertising-consent header.
pub const HEADER_X_CONSENT_ADVERTISING: HeaderName =
    HeaderName::from_static("x-consent-advertising");
/// Standard client forwarding-chain header.
pub const HEADER_X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
/// Edge-derived client city header.
pub const HEADER_X_GEO_CITY: HeaderName = HeaderName::from_static("x-geo-city");
/// Edge-derived client continent header.
pub const HEADER_X_GEO_CONTINENT: HeaderName = HeaderName::from_static("x-geo-continent");
/// Edge-derived client latitude/longitude header.
pub const HEADER_X_GEO_COORDINATES: HeaderName = HeaderName::from_static("x-geo-coordinates");
/// Edge-derived client country header.
pub const HEADER_X_GEO_COUNTRY: HeaderName = HeaderName::from_static("x-geo-country");
/// Marker indicating whether edge geo data is available.
pub const HEADER_X_GEO_INFO_AVAILABLE: HeaderName = HeaderName::from_static("x-geo-info-available");
/// Edge-derived DMA or metro-code header.
pub const HEADER_X_GEO_METRO_CODE: HeaderName = HeaderName::from_static("x-geo-metro-code");
/// Edge-derived client region header.
pub const HEADER_X_GEO_REGION: HeaderName = HeaderName::from_static("x-geo-region");
/// Consent subject identifier header.
pub const HEADER_X_SUBJECT_ID: HeaderName = HeaderName::from_static("x-subject-id");
/// Request correlation identifier header.
pub const HEADER_X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
/// Origin compression-preference hint.
pub const HEADER_X_COMPRESS_HINT: HeaderName = HeaderName::from_static("x-compress-hint");
/// Fastly point-of-presence diagnostic header.
pub const HEADER_X_DEBUG_FASTLY_POP: HeaderName = HeaderName::from_static("x-debug-fastly-pop");

// Staging / version identification headers
/// Trusted Server deployment-version response header.
pub const HEADER_X_TS_VERSION: HeaderName = HeaderName::from_static("x-ts-version");
/// Trusted Server environment response header.
pub const HEADER_X_TS_ENV: HeaderName = HeaderName::from_static("x-ts-env");

// Fastly environment variables
/// Fastly service-version environment variable.
pub const ENV_FASTLY_SERVICE_VERSION: &str = "FASTLY_SERVICE_VERSION";
/// Fastly staging-mode environment variable.
pub const ENV_FASTLY_IS_STAGING: &str = "FASTLY_IS_STAGING";

// Common standard header names used across modules
/// Canonical `User-Agent` header name.
pub const HEADER_USER_AGENT: HeaderName = HeaderName::from_static("user-agent");
/// Canonical `Accept` header name.
pub const HEADER_ACCEPT: HeaderName = HeaderName::from_static("accept");
/// Canonical `Accept-Language` header name.
pub const HEADER_ACCEPT_LANGUAGE: HeaderName = HeaderName::from_static("accept-language");
/// Canonical `Accept-Encoding` header name.
pub const HEADER_ACCEPT_ENCODING: HeaderName = HeaderName::from_static("accept-encoding");
/// Canonical `Referer` header name.
pub const HEADER_REFERER: HeaderName = HeaderName::from_static("referer");

/// TS-internal header names that must NOT be forwarded to downstream third-party services.
///
/// These headers are used internally by Trusted Server for identification, geo-enrichment,
/// debugging, and compression hints. Leaking them to external origins could expose
/// data and internal implementation details.
///
/// Uses `&str` slices because `HeaderName` has interior mutability and cannot appear
/// in `const` context.
pub const INTERNAL_HEADERS: &[&str] = &[
    "x-ts-ec",
    "x-ts-eids",
    "x-ts-ec-consent",
    "x-ts-eids-truncated",
    "x-pub-user-id",
    "x-subject-id",
    "x-consent-advertising",
    "x-forwarded-for",
    "x-geo-city",
    "x-geo-continent",
    "x-geo-coordinates",
    "x-geo-country",
    "x-geo-info-available",
    "x-geo-metro-code",
    "x-geo-region",
    "x-request-id",
    "x-compress-hint",
    "x-debug-fastly-pop",
    // Trusted TLS metadata injected by the Fastly EdgeZero entry point.
    // Injected after stripping spoofable forwarded headers so they cannot be
    // client-supplied. Must not be forwarded to downstream origins.
    "x-ts-tls-protocol",
    "x-ts-tls-cipher",
];

// Consent-related cookie names
/// IAB TCF v2 consent-string cookie.
pub const COOKIE_EUCONSENT_V2: &str = "euconsent-v2";
/// IAB GPP consent-string cookie.
pub const COOKIE_GPP: &str = "__gpp";
/// IAB GPP section-identifier cookie.
pub const COOKIE_GPP_SID: &str = "__gpp_sid";
/// Legacy IAB US Privacy consent cookie.
pub const COOKIE_US_PRIVACY: &str = "us_privacy";

// Consent-related header names
/// Global Privacy Control request header.
pub const HEADER_SEC_GPC: HeaderName = HeaderName::from_static("sec-gpc");
