//! Consent signal extraction from cookies and headers.
//!
//! Reads raw consent strings from the [`CookieJar`] and HTTP headers without
//! performing any decoding or validation. This is the first step in the consent
//! pipeline described in the [Consent Forwarding Architecture Design].

use cookie::CookieJar;
use fastly::Request;

use crate::constants::{
    COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_US_PRIVACY, HEADER_SEC_GPC,
};

use super::types::RawConsentSignals;

/// Extracts raw consent signals from a [`CookieJar`] and a [`Request`].
///
/// Reads the following consent cookies (if present):
/// - `euconsent-v2` — IAB TCF v2 consent string
/// - `__gpp` — IAB Global Privacy Platform string
/// - `__gpp_sid` — GPP section IDs (comma-separated)
/// - `us_privacy` — IAB US Privacy / CCPA string
///
/// Also reads the `Sec-GPC` header for Global Privacy Control.
///
/// No decoding or validation is performed — values are captured as-is.
pub fn extract_consent_signals(jar: Option<&CookieJar>, req: &Request) -> RawConsentSignals {
    let raw_tc_string = jar
        .and_then(|j| j.get(COOKIE_EUCONSENT_V2))
        .map(|c| c.value().to_owned());

    let raw_gpp_string = jar
        .and_then(|j| j.get(COOKIE_GPP))
        .map(|c| c.value().to_owned());

    let raw_gpp_sid = jar
        .and_then(|j| j.get(COOKIE_GPP_SID))
        .map(|c| c.value().to_owned());

    let raw_us_privacy = jar
        .and_then(|j| j.get(COOKIE_US_PRIVACY))
        .map(|c| c.value().to_owned());

    let gpc = req
        .get_header(HEADER_SEC_GPC)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "1")
        .unwrap_or(false);

    RawConsentSignals {
        raw_tc_string,
        raw_gpp_string,
        raw_gpp_sid,
        raw_us_privacy,
        gpc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cookies::parse_cookies_to_jar;

    #[test]
    fn no_cookies_no_headers() {
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(None, &req);
        assert!(signals.is_empty(), "should produce empty signals");
    }

    #[test]
    fn extracts_euconsent_v2() {
        let jar = parse_cookies_to_jar("euconsent-v2=CPXxGfAPXxGfAAHABBENBCCsAP_AAH_AAAAAHftf");
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert_eq!(
            signals.raw_tc_string.as_deref(),
            Some("CPXxGfAPXxGfAAHABBENBCCsAP_AAH_AAAAAHftf"),
            "should extract euconsent-v2 cookie value"
        );
    }

    #[test]
    fn extracts_gpp_cookies() {
        let jar = parse_cookies_to_jar("__gpp=DBACNYA~CPXxGfA; __gpp_sid=2,6");
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert_eq!(
            signals.raw_gpp_string.as_deref(),
            Some("DBACNYA~CPXxGfA"),
            "should extract __gpp cookie value"
        );
        assert_eq!(
            signals.raw_gpp_sid.as_deref(),
            Some("2,6"),
            "should extract __gpp_sid cookie value"
        );
    }

    #[test]
    fn extracts_us_privacy() {
        let jar = parse_cookies_to_jar("us_privacy=1YNN");
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert_eq!(
            signals.raw_us_privacy.as_deref(),
            Some("1YNN"),
            "should extract us_privacy cookie value"
        );
    }

    #[test]
    fn extracts_sec_gpc_header() {
        let req = Request::get("https://example.com").with_header("sec-gpc", "1");
        let signals = extract_consent_signals(None, &req);

        assert!(signals.gpc, "should detect Sec-GPC: 1 header");
    }

    #[test]
    fn sec_gpc_absent_when_not_set() {
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(None, &req);

        assert!(!signals.gpc, "should default gpc to false");
    }

    #[test]
    fn sec_gpc_absent_when_not_one() {
        let req = Request::get("https://example.com").with_header("sec-gpc", "0");
        let signals = extract_consent_signals(None, &req);

        assert!(!signals.gpc, "should not treat Sec-GPC: 0 as opt-out");
    }

    #[test]
    fn extracts_all_signals() {
        let jar =
            parse_cookies_to_jar("euconsent-v2=CPXxGf; __gpp=DBAC; __gpp_sid=2,6; us_privacy=1YNN");
        let req = Request::get("https://example.com").with_header("sec-gpc", "1");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert!(signals.raw_tc_string.is_some(), "should have tc_string");
        assert!(signals.raw_gpp_string.is_some(), "should have gpp_string");
        assert!(signals.raw_gpp_sid.is_some(), "should have gpp_sid");
        assert!(signals.raw_us_privacy.is_some(), "should have us_privacy");
        assert!(signals.gpc, "should have gpc");
    }

    #[test]
    fn empty_jar_produces_no_cookie_signals() {
        let jar = parse_cookies_to_jar("");
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert!(
            signals.raw_tc_string.is_none(),
            "should have no tc_string from empty jar"
        );
        assert!(
            signals.raw_gpp_string.is_none(),
            "should have no gpp_string from empty jar"
        );
    }

    #[test]
    fn unrelated_cookies_ignored() {
        let jar = parse_cookies_to_jar("session_id=abc123; theme=dark");
        let req = Request::get("https://example.com");
        let signals = extract_consent_signals(Some(&jar), &req);

        assert!(
            signals.is_empty(),
            "should produce empty signals for unrelated cookies"
        );
    }
}
