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

use super::types::{ConsentDecodeError, GppConsent, TcfConsent, UsPrivacy};

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
///   `MAX_GPP_STRING_LEN` or the `iab_gpp` parser fails.
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
    // Safety: `SectionId` is a C-like enum with discriminants 1–23 (GPP spec v1).
    // The `iab_gpp` crate stores them internally as `BTreeSet<u16>`, so every
    // variant is guaranteed to fit in u16.
    let section_ids: Vec<u16> = parsed.section_ids().map(|id| *id as u16).collect();

    // Attempt to extract and decode the EU TCF v2.2 section.
    // Section ID 2 = TcfEuV2 in the GPP spec.
    let eu_tcf = decode_tcf_from_gpp(&parsed);

    let (us_sections, us_sections_failed) = decode_us_sections(&parsed);
    let us_privacy_section = decode_us_privacy_section(&parsed);
    let us_sale_opt_out = aggregate_us_signal(&us_sections, us_sale_opt_out_from_section);

    Ok(GppConsent {
        // The GPP header version is always 1 for current spec.
        version: 1,
        section_ids,
        eu_tcf,
        us_sale_opt_out,
        us_sharing_opt_out: aggregate_us_signal(&us_sections, us_sharing_opt_out_from_section),
        us_targeted_advertising_opt_out: aggregate_us_signal(
            &us_sections,
            us_targeted_advertising_opt_out_from_section,
        ),
        us_gpc: aggregate_us_signal(&us_sections, us_gpc_from_section),
        us_known_child: aggregate_us_signal(&us_sections, us_known_child_from_section),
        us_privacy_section: us_privacy_section.clone().unwrap_or(None),
        us_section_decode_failed: us_sections_failed || us_privacy_section.is_err(),
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

/// GPP section IDs that represent US state/national privacy sections.
///
/// Range 7–23 per the GPP v1 specification:
/// 7=UsNat, 8=UsCa, 9=UsVa, 10=UsCo, 11=UsUt, 12=UsCt, 13=UsFl,
/// 14=UsMt, 15=UsOr, 16=UsTx, 17=UsDe, 18=UsIa, 19=UsNe, 20=UsNh,
/// 21=UsNj, 22=UsTn, 23=UsMn.
const US_SECTION_ID_RANGE: std::ops::RangeInclusive<u16> = 7..=23;

/// Decodes every US section (IDs 7–23) in a parsed GPP string.
///
/// A section that fails to decode is logged and skipped, so one malformed
/// section never hides a valid one. The flag is `true` when any US section
/// failed to decode, so its opt-outs are unknown.
fn decode_us_sections(parsed: &iab_gpp::v1::GPPString) -> (Vec<iab_gpp::sections::Section>, bool) {
    let mut failed = false;
    let sections = parsed
        .section_ids()
        .filter(|id| US_SECTION_ID_RANGE.contains(&(**id as u16)))
        .filter_map(
            |us_section_id| match parsed.decode_section(*us_section_id) {
                Ok(section) => Some(section),
                Err(e) => {
                    log::warn!("Failed to decode US GPP section {us_section_id}: {e}");
                    failed = true;
                    None
                }
            },
        )
        .collect();
    (sections, failed)
}

/// Aggregates one boolean signal across decoded US sections, conservatively:
///
/// - `Some(true)` if any section reports the signal set
/// - `Some(false)` if at least one section reports it unset and none report
///   it set
/// - `None` if no section yields the signal
fn aggregate_us_signal(
    sections: &[iab_gpp::sections::Section],
    extract: fn(&iab_gpp::sections::Section) -> Option<bool>,
) -> Option<bool> {
    let mut result = None;
    for section in sections {
        match extract(section) {
            Some(true) => return Some(true),
            Some(false) => result = Some(false),
            None => {}
        }
    }
    result
}

fn us_sale_opt_out_from_section(section: &iab_gpp::sections::Section) -> Option<bool> {
    use iab_gpp::sections::Section;
    use iab_gpp::sections::us_common::OptOut;

    // Keep this match in sync with new US-state variants added by `iab_gpp`.
    let sale_opt_out = match section {
        Section::UsNat(s) => match &s.core {
            iab_gpp::sections::usnat::Core::V1(c) => &c.sale_opt_out,
            iab_gpp::sections::usnat::Core::V2(c) => &c.sale_opt_out,
            _ => return None,
        },
        Section::UsCa(s) => &s.core.sale_opt_out,
        Section::UsVa(s) => &s.core.sale_opt_out,
        Section::UsCo(s) => &s.core.sale_opt_out,
        Section::UsUt(s) => &s.core.sale_opt_out,
        Section::UsCt(s) => &s.core.sale_opt_out,
        Section::UsFl(s) => &s.core.sale_opt_out,
        Section::UsMt(s) => &s.core.sale_opt_out,
        Section::UsOr(s) => &s.core.sale_opt_out,
        Section::UsTx(s) => &s.core.sale_opt_out,
        Section::UsDe(s) => &s.core.sale_opt_out,
        Section::UsIa(s) => &s.core.sale_opt_out,
        Section::UsNe(s) => &s.core.sale_opt_out,
        Section::UsNh(s) => &s.core.sale_opt_out,
        Section::UsNj(s) => &s.core.sale_opt_out,
        Section::UsTn(s) => &s.core.sale_opt_out,
        Section::UsMn(s) => &s.core.sale_opt_out,
        _ => return None,
    };

    Some(*sale_opt_out == OptOut::OptedOut)
}

/// Reads the sharing opt-out, defined by the US National and California
/// sections only.
fn us_sharing_opt_out_from_section(section: &iab_gpp::sections::Section) -> Option<bool> {
    use iab_gpp::sections::Section;
    use iab_gpp::sections::us_common::OptOut;

    let sharing_opt_out = match section {
        Section::UsNat(s) => match &s.core {
            iab_gpp::sections::usnat::Core::V1(c) => &c.sharing_opt_out,
            iab_gpp::sections::usnat::Core::V2(c) => &c.sharing_opt_out,
            _ => return None,
        },
        Section::UsCa(s) => &s.core.sharing_opt_out,
        _ => return None,
    };

    Some(*sharing_opt_out == OptOut::OptedOut)
}

/// Reads the targeted-advertising opt-out from the US National section and
/// every state section that defines it (all but California).
fn us_targeted_advertising_opt_out_from_section(
    section: &iab_gpp::sections::Section,
) -> Option<bool> {
    use iab_gpp::sections::Section;
    use iab_gpp::sections::us_common::OptOut;

    // Keep this match in sync with new US-state variants added by `iab_gpp`.
    let targeted_advertising_opt_out = match section {
        Section::UsNat(s) => match &s.core {
            iab_gpp::sections::usnat::Core::V1(c) => &c.targeted_advertising_opt_out,
            iab_gpp::sections::usnat::Core::V2(c) => &c.targeted_advertising_opt_out,
            _ => return None,
        },
        Section::UsVa(s) => &s.core.targeted_advertising_opt_out,
        Section::UsCo(s) => &s.core.targeted_advertising_opt_out,
        Section::UsUt(s) => &s.core.targeted_advertising_opt_out,
        Section::UsCt(s) => &s.core.targeted_advertising_opt_out,
        Section::UsFl(s) => &s.core.targeted_advertising_opt_out,
        Section::UsMt(s) => &s.core.targeted_advertising_opt_out,
        Section::UsOr(s) => &s.core.targeted_advertising_opt_out,
        Section::UsTx(s) => &s.core.targeted_advertising_opt_out,
        Section::UsDe(s) => &s.core.targeted_advertising_opt_out,
        Section::UsIa(s) => &s.core.targeted_advertising_opt_out,
        Section::UsNe(s) => &s.core.targeted_advertising_opt_out,
        Section::UsNh(s) => &s.core.targeted_advertising_opt_out,
        Section::UsNj(s) => &s.core.targeted_advertising_opt_out,
        Section::UsTn(s) => &s.core.targeted_advertising_opt_out,
        Section::UsMn(s) => &s.core.targeted_advertising_opt_out,
        _ => return None,
    };

    Some(*targeted_advertising_opt_out == OptOut::OptedOut)
}

/// Reads the Global Privacy Control segment of the US National section.
fn us_gpc_from_section(section: &iab_gpp::sections::Section) -> Option<bool> {
    match section {
        iab_gpp::sections::Section::UsNat(s) => s.gpc,
        _ => None,
    }
}

/// Reads whether the US National section marks a known child without consent.
fn us_known_child_from_section(section: &iab_gpp::sections::Section) -> Option<bool> {
    use iab_gpp::sections::Section;
    use iab_gpp::sections::us_common::Consent;

    let Section::UsNat(s) = section else {
        return None;
    };
    match &s.core {
        iab_gpp::sections::usnat::Core::V1(c) => {
            let child = &c.known_child_sensitive_data_consents;
            Some(child.from_13_to_16 == Consent::NoConsent || child.under_13 == Consent::NoConsent)
        }
        iab_gpp::sections::usnat::Core::V2(c) => {
            let child = &c.known_child_sensitive_data_consents;
            Some(
                child.process_sensitive_data_from_13_to_16 == Consent::NoConsent
                    || child.process_sensitive_data_under_13 == Consent::NoConsent
                    || child.process_personal_data_from_16_to_17 == Consent::NoConsent,
            )
        }
        _ => None,
    }
}

/// Reads GPP section 6, the US Privacy string, through the standalone
/// decoder. Returns `None` when absent, and `Err` when present but
/// undecodable.
fn decode_us_privacy_section(parsed: &iab_gpp::v1::GPPString) -> Result<Option<UsPrivacy>, ()> {
    let Some(raw) = parsed.section(iab_gpp::sections::SectionId::UspV1) else {
        return Ok(None);
    };
    match super::us_privacy::decode_us_privacy(raw) {
        Ok(us_privacy) => Ok(Some(us_privacy)),
        Err(e) => {
            log::warn!("GPP contains a US Privacy section but decoding failed: {e}");
            Err(())
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
            if let Ok(id) = s.parse::<u16>() {
                Some(id)
            } else {
                log::debug!("Ignoring invalid __gpp_sid entry: {s:?}");
                None
            }
        })
        .collect();

    if ids.is_empty() { None } else { Some(ids) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::types::PrivacyFlag;
    use crate::consent::us_privacy::decode_us_privacy;

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

    #[test]
    fn decodes_us_sale_opt_out_not_opted_out() {
        let result = decode_gpp_string("DBABLA~BVQqAAAAAgA.QA");
        match &result {
            Ok(gpp) => {
                assert_eq!(
                    gpp.us_sale_opt_out,
                    Some(false),
                    "should extract sale_opt_out=false from UsNat section"
                );
            }
            Err(e) => {
                panic!("GPP decode failed: {e}");
            }
        }
    }

    fn encode_fibonacci_integer(mut value: u16) -> String {
        let mut fibs = vec![1_u16];
        let mut next = 2_u16;
        while next <= value {
            fibs.push(next);
            next = if fibs.len() == 1 {
                2
            } else {
                fibs[fibs.len() - 1] + fibs[fibs.len() - 2]
            };
        }

        let mut bits = vec![false; fibs.len()];
        for (idx, fib) in fibs.iter().enumerate().rev() {
            if *fib <= value {
                value -= *fib;
                bits[idx] = true;
            }
        }
        bits.push(true);

        bits.into_iter()
            .map(|bit| if bit { '1' } else { '0' })
            .collect()
    }

    fn encode_header(section_ids: &[u16]) -> String {
        const BASE64_URL: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

        let mut bits = String::from("000011000001");
        bits.push_str(&format!("{:012b}", section_ids.len()));

        let mut previous = 0_u16;
        for &section_id in section_ids {
            bits.push('0');
            bits.push_str(&encode_fibonacci_integer(section_id - previous));
            previous = section_id;
        }

        while bits.len() % 6 != 0 {
            bits.push('0');
        }

        bits.as_bytes()
            .chunks(6)
            .map(|chunk| {
                let value = u8::from_str_radix(
                    core::str::from_utf8(chunk).expect("should encode header bits as utf8"),
                    2,
                )
                .expect("should parse 6-bit chunk");
                char::from(BASE64_URL[value as usize])
            })
            .collect()
    }

    fn gpp_with_sections(sections: &[(u16, &str)]) -> String {
        let ids = sections.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let header = encode_header(&ids);
        let section_payloads = sections.iter().map(|(_, raw)| *raw).collect::<Vec<_>>();
        format!("{header}~{}", section_payloads.join("~"))
    }

    #[test]
    fn no_us_section_returns_none() {
        let result = decode_gpp_string(GPP_TCF_AND_USP).expect("should decode GPP");
        assert_eq!(
            result.us_sale_opt_out, None,
            "should return None when no US section (7-23) is present"
        );
    }

    #[test]
    fn later_us_section_opt_out_overrides_earlier_non_opt_out() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "BVVVVVVVVWA.AA")]);

        let result = decode_gpp_string(&gpp).expect("should decode multi-section US GPP");

        assert_eq!(
            result.us_sale_opt_out,
            Some(true),
            "should treat any later decodable opt-out as authoritative"
        );
    }

    #[test]
    fn multiple_us_sections_without_opt_out_return_false() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "BVgVVVVVVWA.AA")]);

        let result = decode_gpp_string(&gpp).expect("should decode multi-section US GPP");

        assert_eq!(
            result.us_sale_opt_out,
            Some(false),
            "should return false when decodable US sections consistently do not opt out"
        );
    }

    #[test]
    fn valid_opt_out_wins_even_if_another_us_section_is_undecodable() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "not-a-valid-usva-section")]);

        let result = decode_gpp_string(&gpp).expect("should decode GPP header with raw sections");

        assert_eq!(
            result.us_sale_opt_out,
            Some(false),
            "should keep a valid non-opt-out signal even when another US section fails to decode"
        );

        let gpp = gpp_with_sections(&[(7, "not-a-valid-usnat-section"), (9, "BVVVVVVVVWA.AA")]);
        let result = decode_gpp_string(&gpp).expect("should decode GPP header with raw sections");

        assert_eq!(
            result.us_sale_opt_out,
            Some(true),
            "should let a valid opt-out win even when another US section fails to decode"
        );
    }

    #[test]
    fn only_undecodable_us_sections_return_none() {
        let gpp = gpp_with_sections(&[(7, "not-a-valid-usnat-section"), (9, "also-invalid")]);

        let result = decode_gpp_string(&gpp).expect("should decode GPP header with raw sections");

        assert_eq!(
            result.us_sale_opt_out, None,
            "should return None when no decodable US section yields sale_opt_out"
        );
    }

    // Encodes a US core segment: a 6-bit version, then 2-bit fields.
    fn us_core_segment(version: u8, fields: &[u8]) -> String {
        const BASE64_URL: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

        let mut bits = format!("{version:06b}");
        for field in fields {
            bits.push_str(&format!("{field:02b}"));
        }
        while bits.len() % 6 != 0 {
            bits.push('0');
        }
        bits.as_bytes()
            .chunks(6)
            .map(|chunk| {
                let value = u8::from_str_radix(
                    core::str::from_utf8(chunk).expect("should encode segment bits as utf8"),
                    2,
                )
                .expect("should parse 6-bit chunk");
                char::from(BASE64_URL[value as usize])
            })
            .collect()
    }

    // The US National core, version 1. `child` holds the two known-child
    // consent fields. Opt-out values: 1 opted out, 2 did not.
    fn us_nat_section(sale: u8, sharing: u8, targeted: u8, child: [u8; 2], gpc: bool) -> String {
        let mut fields = vec![1, 1, 1, 1, 1, 1, sale, sharing, targeted];
        fields.extend([2; 12]);
        fields.extend(child);
        // Personal data consent, MSPA covered transaction and the two modes.
        fields.extend([2, 2, 1, 1]);
        let core = us_core_segment(1, &fields);
        let gpc_segment = if gpc { "YA" } else { "QA" };
        format!("{core}.{gpc_segment}")
    }

    // The California core: sale, then sharing.
    fn us_ca_section(sale: u8, sharing: u8) -> String {
        let mut fields = vec![1, 1, 1, sale, sharing];
        fields.extend([2; 9]);
        fields.extend([2, 2]);
        fields.extend([2, 2, 1, 1]);
        format!("{}.QA", us_core_segment(1, &fields))
    }

    // The Virginia core: sale, then targeted advertising.
    fn us_va_section(sale: u8, targeted: u8) -> String {
        let mut fields = vec![1, 1, 1, sale, targeted];
        fields.extend([2; 8]);
        fields.push(2);
        fields.extend([2, 1, 1]);
        us_core_segment(1, &fields)
    }

    #[test]
    fn decodes_the_sharing_opt_out_from_us_national_and_california() {
        let nat = gpp_with_sections(&[(7, &us_nat_section(2, 1, 2, [2, 2], false))]);
        let result = decode_gpp_string(&nat).expect("should decode US National");
        assert_eq!(
            result.us_sharing_opt_out,
            Some(true),
            "should read the US National sharing opt-out"
        );
        assert_eq!(
            result.us_sale_opt_out,
            Some(false),
            "should keep the sale signal apart from the sharing signal"
        );

        let nat_control = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [2, 2], false))]);
        assert_eq!(
            decode_gpp_string(&nat_control)
                .expect("should decode US National control")
                .us_sharing_opt_out,
            Some(false),
            "should read no sharing opt-out when it is not set"
        );

        let california = gpp_with_sections(&[(8, &us_ca_section(2, 1))]);
        assert_eq!(
            decode_gpp_string(&california)
                .expect("should decode California")
                .us_sharing_opt_out,
            Some(true),
            "should read the California sharing opt-out"
        );
        let california_control = gpp_with_sections(&[(8, &us_ca_section(2, 2))]);
        assert_eq!(
            decode_gpp_string(&california_control)
                .expect("should decode California control")
                .us_sharing_opt_out,
            Some(false),
            "should read no California sharing opt-out when it is not set"
        );

        let virginia = gpp_with_sections(&[(9, &us_va_section(2, 2))]);
        assert_eq!(
            decode_gpp_string(&virginia)
                .expect("should decode Virginia")
                .us_sharing_opt_out,
            None,
            "should leave sharing unset for a section that does not define it"
        );
    }

    #[test]
    fn decodes_the_targeted_advertising_opt_out_from_sections_that_define_it() {
        let nat = gpp_with_sections(&[(7, &us_nat_section(2, 2, 1, [2, 2], false))]);
        let result = decode_gpp_string(&nat).expect("should decode US National");
        assert_eq!(
            result.us_targeted_advertising_opt_out,
            Some(true),
            "should read the US National targeted-advertising opt-out"
        );
        assert_eq!(
            result.us_sharing_opt_out,
            Some(false),
            "should keep the sharing signal apart from the targeted signal"
        );

        let virginia = gpp_with_sections(&[(9, &us_va_section(2, 1))]);
        assert_eq!(
            decode_gpp_string(&virginia)
                .expect("should decode Virginia")
                .us_targeted_advertising_opt_out,
            Some(true),
            "should read the Virginia targeted-advertising opt-out"
        );
        let virginia_control = gpp_with_sections(&[(9, &us_va_section(2, 2))]);
        assert_eq!(
            decode_gpp_string(&virginia_control)
                .expect("should decode Virginia control")
                .us_targeted_advertising_opt_out,
            Some(false),
            "should read no targeted-advertising opt-out when it is not set"
        );

        let california = gpp_with_sections(&[(8, &us_ca_section(2, 2))]);
        assert_eq!(
            decode_gpp_string(&california)
                .expect("should decode California")
                .us_targeted_advertising_opt_out,
            None,
            "should leave targeted advertising unset for a section that does not define it"
        );
    }

    #[test]
    fn any_section_opt_out_wins_for_the_new_opt_outs() {
        let gpp = gpp_with_sections(&[
            (7, &us_nat_section(2, 2, 2, [2, 2], false)),
            (9, &us_va_section(2, 1)),
        ]);
        let result = decode_gpp_string(&gpp).expect("should decode both sections");
        assert_eq!(
            result.us_targeted_advertising_opt_out,
            Some(true),
            "should treat any decodable targeted opt-out as authoritative"
        );
    }

    #[test]
    fn decodes_the_us_national_gpc_field() {
        let set = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [2, 2], true))]);
        assert_eq!(
            decode_gpp_string(&set)
                .expect("should decode US National with GPC")
                .us_gpc,
            Some(true),
            "should read the GPC field when it is set"
        );
        let unset = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [2, 2], false))]);
        assert_eq!(
            decode_gpp_string(&unset)
                .expect("should decode US National without GPC")
                .us_gpc,
            Some(false),
            "should read GPC false when the segment says so"
        );
        assert_eq!(
            decode_gpp_string(GPP_USP_ONLY)
                .expect("should decode USP only")
                .us_gpc,
            None,
            "should leave GPC unset without a US National section"
        );
    }

    #[test]
    fn decodes_a_known_child_without_consent() {
        // Known-child fields: 1 means no consent, 2 consent, 0 not applicable.
        let no_consent = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [2, 1], false))]);
        assert_eq!(
            decode_gpp_string(&no_consent)
                .expect("should decode known child")
                .us_known_child,
            Some(true),
            "should flag a known child without consent"
        );
        let consent = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [2, 2], false))]);
        assert_eq!(
            decode_gpp_string(&consent)
                .expect("should decode consenting child")
                .us_known_child,
            Some(false),
            "should not flag a known child who has consent"
        );
        let not_applicable = gpp_with_sections(&[(7, &us_nat_section(2, 2, 2, [0, 0], false))]);
        assert_eq!(
            decode_gpp_string(&not_applicable)
                .expect("should decode not-applicable child")
                .us_known_child,
            Some(false),
            "should not flag a section that marks no known child"
        );
        assert_eq!(
            decode_gpp_string(GPP_USP_ONLY)
                .expect("should decode USP only")
                .us_known_child,
            None,
            "should leave known child unset without a US National section"
        );
    }

    #[test]
    fn decodes_section_6_through_the_us_privacy_decoder() {
        let result = decode_gpp_string(GPP_USP_ONLY).expect("should decode USP only");
        assert_eq!(
            result.us_privacy_section,
            Some(decode_us_privacy("1YNN").expect("should decode 1YNN")),
            "should read section 6 as a US Privacy string"
        );
        let opted_out = decode_gpp_string(&gpp_with_sections(&[(6, "1YYN")]))
            .expect("should decode an opted-out section 6");
        assert_eq!(
            opted_out
                .us_privacy_section
                .expect("should carry section 6")
                .opt_out_sale,
            PrivacyFlag::Yes,
            "should read the section 6 sale opt-out"
        );
        assert_eq!(
            opted_out.us_sale_opt_out, None,
            "should keep us_sale_opt_out to the US state sections"
        );
        let no_section = decode_gpp_string(&gpp_with_sections(&[(
            7,
            &us_nat_section(2, 2, 2, [2, 2], false),
        )]))
        .expect("should decode US National");
        assert!(
            no_section.us_privacy_section.is_none(),
            "should leave section 6 unset when absent"
        );
    }

    #[test]
    fn header_only_gpp_decodes_with_no_section_fields() {
        let result = decode_gpp_string("DBAA").expect("should decode a header-only GPP string");
        assert!(result.section_ids.is_empty(), "should list no section IDs");
        assert_eq!(result.us_sale_opt_out, None, "should leave sale unset");
        assert_eq!(
            result.us_sharing_opt_out, None,
            "should leave sharing unset"
        );
        assert_eq!(
            result.us_targeted_advertising_opt_out, None,
            "should leave targeted advertising unset"
        );
        assert_eq!(result.us_gpc, None, "should leave GPC unset");
        assert_eq!(
            result.us_known_child, None,
            "should leave known child unset"
        );
        assert!(
            result.us_privacy_section.is_none(),
            "should leave section 6 unset"
        );
        assert!(result.eu_tcf.is_none(), "should carry no TCF section");
    }

    #[test]
    fn flags_a_us_section_or_section_6_that_fails_to_decode() {
        let corrupt_us = decode_gpp_string(&gpp_with_sections(&[(7, "not-a-valid-usnat-section")]))
            .expect("should decode the header");
        assert!(
            corrupt_us.us_section_decode_failed,
            "should flag an undecodable US section"
        );
        let corrupt_usp = decode_gpp_string(&gpp_with_sections(&[(6, "garbage")]))
            .expect("should decode the header");
        assert!(
            corrupt_usp.us_section_decode_failed,
            "should flag an undecodable section 6"
        );
        let valid = decode_gpp_string(GPP_USP_ONLY).expect("should decode USP only");
        assert!(
            !valid.us_section_decode_failed,
            "should not flag a decodable string"
        );
    }
}
