use http::header::HeaderName;

pub const COOKIE_TS_EC: &str = "ts-ec";
/// Non-`HttpOnly` companion to [`COOKIE_TS_EC`], set when a client-cycle
/// resolve succeeds. It carries no identity (the value is `1`); it only lets
/// the page script see that an Edge Cookie exists, which the `HttpOnly` cookie
/// itself cannot, so the script does not re-post on every page view.
pub const COOKIE_TS_EC_RESOLVED: &str = "ts-ecr";
/// Cookie written by the Trusted Server JS SDK containing a standard-base64-encoded
/// JSON array of Extended User IDs (`[{ source, uids }]`) from identity providers.
pub const COOKIE_TS_EIDS: &str = "ts-eids";
pub const COOKIE_TS_TESTER: &str = "ts-tester";
pub const COOKIE_SHAREDID: &str = "sharedId";

pub const HEADER_X_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
pub const HEADER_X_TS_EC: HeaderName = HeaderName::from_static("x-ts-ec");
pub const HEADER_X_TS_EIDS: HeaderName = HeaderName::from_static("x-ts-eids");
pub const HEADER_X_TS_EC_CONSENT: HeaderName = HeaderName::from_static("x-ts-ec-consent");
pub const HEADER_X_TS_EIDS_TRUNCATED: HeaderName = HeaderName::from_static("x-ts-eids-truncated");
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

/// The fixed response headers that carry Edge Cookie identity output.
///
/// EC finalization strips these from a response the request was not permitted
/// to carry an identity on (see `clear_ec_headers_on_response` in
/// [`finalize`](crate::ec::finalize)), and they are also internal headers, so
/// [`INTERNAL_HEADERS`] is built from this list rather than repeating it. That
/// is the whole reason the list lives here alongside `INTERNAL_HEADERS` and not
/// beside its only reader, because two hand-written copies of one list drift as
/// soon as a header is added to one of them.
///
/// Uses `&str` slices for the same reason [`INTERNAL_HEADERS`] does.
pub const EC_RESPONSE_HEADERS: &[&str] = &[
    "x-ts-ec",
    "x-ts-eids",
    "x-ts-ec-consent",
    "x-ts-eids-truncated",
];

/// The internal headers that are not part of the Edge Cookie output surface.
///
/// Kept apart from [`EC_RESPONSE_HEADERS`] only so [`INTERNAL_HEADERS`] can be
/// assembled from the two without repeating either. Add a header here unless it
/// is one EC finalization has to strip, in which case it belongs in
/// [`EC_RESPONSE_HEADERS`] and reaches [`INTERNAL_HEADERS`] from there.
const NON_EC_INTERNAL_HEADERS: &[&str] = &[
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

/// How many names [`INTERNAL_HEADERS`] holds.
const INTERNAL_HEADER_COUNT: usize = EC_RESPONSE_HEADERS.len() + NON_EC_INTERNAL_HEADERS.len();

/// Joins the two source lists into the array [`INTERNAL_HEADERS`] borrows.
///
/// Written as a `const fn` because slice concatenation is not available in a
/// `const` initializer, and the join has to happen while the crate is compiled
/// so no caller pays for it.
const fn join_internal_headers() -> [&'static str; INTERNAL_HEADER_COUNT] {
    let mut joined = [""; INTERNAL_HEADER_COUNT];
    let mut i = 0;
    while i < EC_RESPONSE_HEADERS.len() {
        joined[i] = EC_RESPONSE_HEADERS[i];
        i += 1;
    }
    let mut j = 0;
    while j < NON_EC_INTERNAL_HEADERS.len() {
        joined[i + j] = NON_EC_INTERNAL_HEADERS[j];
        j += 1;
    }
    joined
}

/// TS-internal header names that must NOT be forwarded to downstream third-party services.
///
/// These headers are used internally by Trusted Server for identification, geo-enrichment,
/// debugging, and compression hints. Leaking them to external origins could expose
/// data and internal implementation details.
///
/// Built at compile time from [`EC_RESPONSE_HEADERS`] followed by
/// [`NON_EC_INTERNAL_HEADERS`], so an Edge Cookie response header cannot be
/// added to one list and missed in the other.
///
/// Uses `&str` slices because `HeaderName` has interior mutability and cannot appear
/// in `const` context.
pub const INTERNAL_HEADERS: &[&str] = &join_internal_headers();

// Consent-related cookie names
pub const COOKIE_EUCONSENT_V2: &str = "euconsent-v2";
pub const COOKIE_GPP: &str = "__gpp";
pub const COOKIE_GPP_SID: &str = "__gpp_sid";
pub const COOKIE_US_PRIVACY: &str = "us_privacy";

// Consent-related header names
pub const HEADER_SEC_GPC: HeaderName = HeaderName::from_static("sec-gpc");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_edge_cookie_response_header_is_an_internal_header() {
        // These two lists used to be written out by hand in two files, with
        // nothing keeping them in step, so a new Edge Cookie response header
        // could be stripped by EC finalization and still forwarded to a third
        // party. `INTERNAL_HEADERS` is now assembled from
        // `EC_RESPONSE_HEADERS`, and this is the assertion that fails if
        // anyone goes back to writing them out separately.
        for header in EC_RESPONSE_HEADERS {
            assert!(
                INTERNAL_HEADERS.contains(header),
                "`{header}` carries Edge Cookie output, so it must never be forwarded"
            );
        }

        assert_eq!(
            INTERNAL_HEADERS.len(),
            EC_RESPONSE_HEADERS.len() + NON_EC_INTERNAL_HEADERS.len(),
            "every internal header should come from exactly one of the two source lists"
        );

        for (index, header) in INTERNAL_HEADERS.iter().enumerate() {
            assert!(
                !INTERNAL_HEADERS[index + 1..].contains(header),
                "`{header}` is listed twice, so the two source lists overlap"
            );
        }
    }
}
