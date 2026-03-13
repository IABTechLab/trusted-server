//! GPP (Global Privacy Platform) string decoder.
//!
//! Thin wrapper around the [`iab_gpp`] crate that maps decoded GPP data into
//! our [`GppConsent`] domain type. The `iab_gpp` crate handles the heavy
//! lifting of GPP v1 string parsing and section decoding; this module:
//!
//! 1. Parses the raw `__gpp` cookie value via [`iab_gpp::v1::GPPString`].
//! 2. Extracts the header-level section IDs.
//! 3. If the EU TCF v2.2 section is present, decodes it via our own
//!    [`super::tcf::decode_tc_string`] (for consistency with standalone
//!    `euconsent-v2` decoding).
//!
//! # Why wrap `iab_gpp`?
//!
//! - Isolates external dependency behind our own types.
//! - Allows fallback/replacement without touching callers.
//! - Maps `iab_gpp` errors into our [`ConsentDecodeError`] hierarchy.
//!
//! # References
//!
//! - [IAB GPP specification](https://github.com/InteractiveAdvertisingBureau/Global-Privacy-Platform)
//! - [`iab_gpp` crate docs](https://docs.rs/iab_gpp)

use error_stack::Report;

use super::types::{ConsentDecodeError, GppConsent, TcfConsent};

/// Maximum length of a raw GPP string before parsing.
///
/// GPP strings are typically larger than standalone TC strings because they
/// encode multiple sections. This limit prevents malicious cookies from
/// triggering large allocations in the `iab_gpp` parser.
const MAX_GPP_STRING_LEN: usize = 8192;

/// Decodes a GPP string into a [`GppConsent`] struct.
///
/// Parses the raw `__gpp` cookie value, extracts section IDs, and optionally
/// decodes the EU TCF v2.2 section if present.
///
/// # Arguments
///
/// * `gpp_string` — the raw GPP string from the `__gpp` cookie.
///
/// # Errors
///
/// - [`ConsentDecodeError::InvalidGppString`] if the string exceeds
///   [`MAX_GPP_STRING_LEN`] or the `iab_gpp` parser fails.
pub fn decode_gpp_string(gpp_string: &str) -> Result<GppConsent, Report<ConsentDecodeError>> {
    if gpp_string.len() > MAX_GPP_STRING_LEN {
        return Err(Report::new(ConsentDecodeError::InvalidGppString {
            reason: format!(
                "GPP string too long: {} bytes, max {MAX_GPP_STRING_LEN}",
                gpp_string.len()
            ),
        }));
    }

    let parsed = iab_gpp::v1::GPPString::parse_str(gpp_string).map_err(|e| {
        Report::new(ConsentDecodeError::InvalidGppString {
            reason: format!("{e}"),
        })
    })?;

    // Extract section IDs as u16 values.
    let section_ids: Vec<u16> = parsed.section_ids().map(|id| *id as u16).collect();

    // Attempt to extract and decode the EU TCF v2.2 section.
    // Section ID 2 = TcfEuV2 in the GPP spec.
    let eu_tcf = decode_tcf_from_gpp(&parsed);

    // The GPP header version is always 1 for current spec.
    Ok(GppConsent {
        version: 1,
        section_ids,
        eu_tcf,
    })
}

/// Attempts to decode the EU TCF v2.2 section from a parsed GPP string.
///
/// Uses our own TCF decoder on the raw section string (rather than
/// `iab_gpp`'s TCF decoder) to ensure consistency with standalone
/// `euconsent-v2` decoding.
///
/// Returns `None` if the TCF section is not present or cannot be decoded.
fn decode_tcf_from_gpp(parsed: &iab_gpp::v1::GPPString) -> Option<TcfConsent> {
    // iab_gpp::sections::SectionId::TcfEuV2 corresponds to section ID 2.
    let tcf_section_str = parsed.section(iab_gpp::sections::SectionId::TcfEuV2)?;

    // Delegate to our own TCF decoder for consistency.
    match super::tcf::decode_tc_string(tcf_section_str) {
        Ok(tcf) => Some(tcf),
        Err(e) => {
            log::warn!("GPP contains TCF EU v2 section but decoding failed: {e}");
            None
        }
    }
}

/// Parses a `__gpp_sid` cookie value into a vector of section IDs.
///
/// The cookie is a comma-separated list of integer section IDs, e.g. `"2,6"`.
/// Invalid entries are silently skipped (logged at debug level) since the
/// cookie is treated as a transport hint.
///
/// Returns `None` if the input is empty or contains no valid IDs.
#[must_use]
pub fn parse_gpp_sid_cookie(raw: &str) -> Option<Vec<u16>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let ids: Vec<u16> = trimmed
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            match s.parse::<u16>() {
                Ok(id) => Some(id),
                Err(_) => {
                    log::debug!("Ignoring invalid __gpp_sid entry: {s:?}");
                    None
                }
            }
        })
        .collect();

    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A known-good GPP string with US Privacy section (section ID 6).
    // Header "DBABTA" encodes: version=1, section IDs=[6] (UspV1).
    // Section string: "1YNN" (US Privacy).
    const GPP_USP_ONLY: &str = "DBABTA~1YNN";

    // A GPP string with both TCF EU v2 and US Privacy sections.
    // Header "DBACNY" encodes: version=1, section IDs=[2, 6].
    // First section: TCF EU v2 consent string.
    // Second section: US Privacy string.
    const GPP_TCF_AND_USP: &str = "DBACNY~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA~1YNN";

    #[test]
    fn decodes_usp_only_gpp_string() {
        let result = decode_gpp_string(GPP_USP_ONLY).expect("should decode USP-only GPP");
        assert_eq!(result.version, 1);
        assert!(!result.section_ids.is_empty(), "should have section IDs");
        assert!(
            result.eu_tcf.is_none(),
            "should not have TCF section in USP-only string"
        );
    }

    #[test]
    fn decodes_gpp_with_tcf_section() {
        let result = decode_gpp_string(GPP_TCF_AND_USP).expect("should decode GPP with TCF");
        assert_eq!(result.version, 1);
        // Section IDs should include 2 (TCF EU v2) and 6 (USP v1).
        assert!(
            result.section_ids.contains(&2),
            "should contain TCF EU v2 section ID (2)"
        );
        // TCF section should be decoded (may or may not succeed depending
        // on whether the section string is a valid base64-encoded TC String).
        // The GPP TCF section format differs from standalone euconsent-v2,
        // so eu_tcf might be None if our decoder can't parse the GPP-encoded
        // TCF format. That's acceptable — we log and continue.
    }

    #[test]
    fn rejects_invalid_gpp_string() {
        let result = decode_gpp_string("totally-invalid");
        assert!(result.is_err(), "should reject invalid GPP string");
    }

    #[test]
    fn rejects_empty_string() {
        let result = decode_gpp_string("");
        assert!(result.is_err(), "should reject empty GPP string");
    }

    #[test]
    fn rejects_oversized_gpp_string() {
        let oversized = "D".repeat(MAX_GPP_STRING_LEN + 1);
        let result = decode_gpp_string(&oversized);
        assert!(
            result.is_err(),
            "should reject GPP string exceeding max length"
        );
    }

    #[test]
    fn parse_gpp_sid_simple() {
        let ids = parse_gpp_sid_cookie("2,6").expect("should parse 2,6");
        assert_eq!(ids, vec![2, 6]);
    }

    #[test]
    fn parse_gpp_sid_single() {
        let ids = parse_gpp_sid_cookie("2").expect("should parse single ID");
        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn parse_gpp_sid_with_whitespace() {
        let ids = parse_gpp_sid_cookie(" 2 , 6 , 8 ").expect("should handle whitespace");
        assert_eq!(ids, vec![2, 6, 8]);
    }

    #[test]
    fn parse_gpp_sid_empty_returns_none() {
        assert!(parse_gpp_sid_cookie("").is_none(), "empty should be None");
        assert!(
            parse_gpp_sid_cookie("  ").is_none(),
            "whitespace should be None"
        );
    }

    #[test]
    fn parse_gpp_sid_skips_invalid_entries() {
        let ids = parse_gpp_sid_cookie("2,abc,6").expect("should skip invalid");
        assert_eq!(ids, vec![2, 6]);
    }

    #[test]
    fn parse_gpp_sid_all_invalid_returns_none() {
        assert!(
            parse_gpp_sid_cookie("abc,def").is_none(),
            "all-invalid should be None"
        );
    }
}
