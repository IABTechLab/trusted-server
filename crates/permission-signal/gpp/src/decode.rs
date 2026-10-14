//! The GPP (Global Privacy Platform) string decoder.
//!
//! Wraps the [`iab_gpp`] crate, which parses the GPP header and decodes each
//! US section, and reads out the two things this module acts on: the section
//! identifiers the string carries, and whether any US section opts the
//! visitor out of sale. The EU section the string may also carry is another
//! scheme's, which decodes it from the raw section this module hands over
//! through [`section`].
//!
//! # References
//!
//! - [IAB GPP specification](https://github.com/InteractiveAdvertisingBureau/Global-Privacy-Platform)
//! - [`iab_gpp` crate docs](https://docs.rs/iab_gpp)

use error_stack::Report;

/// Longest raw GPP string decoded, so a hostile cookie cannot make the
/// parser allocate without bound.
pub const MAX_GPP_STRING_LEN: usize = 8192;

/// GPP section identifiers of the US state and national sections, 7 to 23
/// in the GPP v1 specification.
const US_SECTION_ID_RANGE: core::ops::RangeInclusive<u16> = 7..=23;

/// What this module reads from a decoded GPP string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GppConsent {
    /// The section identifiers the string carries.
    pub section_ids: Vec<u16>,
    /// Whether a US section opts the visitor out of sale.
    ///
    /// - `Some(true)` when a decodable US section says so
    /// - `Some(false)` when at least one decodable US section says not and
    ///   none says so
    /// - `None` when no US section is present or none decodes to an answer
    pub us_sale_opt_out: Option<bool>,
}

/// Why a GPP string could not be decoded.
#[derive(Debug, derive_more::Display)]
pub enum GppDecodeError {
    /// The string is too long or the parser refused it.
    #[display("invalid GPP string: {reason}")]
    InvalidGppString {
        /// What the parser said.
        reason: String,
    },
}

impl core::error::Error for GppDecodeError {}

/// Decodes a GPP string into what this module reads from it.
///
/// # Errors
///
/// [`GppDecodeError::InvalidGppString`] when the string exceeds
/// [`MAX_GPP_STRING_LEN`] or the parser refuses it.
pub fn decode_gpp_string(gpp_string: &str) -> Result<GppConsent, Report<GppDecodeError>> {
    let parsed = parse(gpp_string)?;
    let section_ids: Vec<u16> = parsed.section_ids().map(|id| *id as u16).collect();
    let us_sale_opt_out = decode_us_sale_opt_out(&parsed);
    Ok(GppConsent {
        section_ids,
        us_sale_opt_out,
    })
}

/// The raw string of section `id` in `gpp_string`, when the string parses
/// and carries that section, for the scheme that decodes it.
#[must_use]
pub fn section(gpp_string: &str, id: u16) -> Option<String> {
    let parsed = parse(gpp_string).ok()?;
    let section_id = parsed.section_ids().find(|held| **held as u16 == id)?;
    parsed.section(*section_id).map(str::to_owned)
}

/// The section identifiers of a request's GPP signal: the decoded string's
/// when it decodes, otherwise the `__gpp_sid` cookie's, which carries the
/// same list as a transport hint.
#[must_use]
pub fn section_ids_of(gpp_string: Option<&str>, gpp_sid: Option<&str>) -> Option<Vec<u16>> {
    gpp_string
        .and_then(|raw| parse(raw).ok())
        .map(|parsed| parsed.section_ids().map(|id| *id as u16).collect())
        .or_else(|| gpp_sid.and_then(parse_gpp_sid_cookie))
}

fn parse(gpp_string: &str) -> Result<iab_gpp::v1::GPPString, Report<GppDecodeError>> {
    if gpp_string.len() > MAX_GPP_STRING_LEN {
        return Err(Report::new(GppDecodeError::InvalidGppString {
            reason: format!(
                "GPP string too long: {} bytes, max {MAX_GPP_STRING_LEN}",
                gpp_string.len()
            ),
        }));
    }
    iab_gpp::v1::GPPString::parse_str(gpp_string).map_err(|error| {
        Report::new(GppDecodeError::InvalidGppString {
            reason: format!("{error}"),
        })
    })
}

/// The sale opt-out across every US section, read conservatively: any
/// decodable section opting out wins, otherwise any decodable section not
/// opting out answers `Some(false)`, otherwise there is no answer.
fn decode_us_sale_opt_out(parsed: &iab_gpp::v1::GPPString) -> Option<bool> {
    let mut result = None;
    for us_section_id in parsed
        .section_ids()
        .filter(|id| US_SECTION_ID_RANGE.contains(&(**id as u16)))
    {
        match parsed.decode_section(*us_section_id) {
            Ok(section) => match us_sale_opt_out_from_section(&section) {
                Some(true) => return Some(true),
                Some(false) => result = Some(false),
                None => {}
            },
            Err(error) => {
                log::warn!("Failed to decode US GPP section {us_section_id}: {error}");
            }
        }
    }
    result
}

fn us_sale_opt_out_from_section(section: &iab_gpp::sections::Section) -> Option<bool> {
    use iab_gpp::sections::Section;
    use iab_gpp::sections::us_common::OptOut;

    // Kept in step with the US state variants `iab_gpp` adds.
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

/// Parses a `__gpp_sid` cookie value, a comma-separated list of section
/// identifiers such as `2,6`, skipping an entry that is not a number.
///
/// `None` when the value is empty or carries no number.
#[must_use]
pub fn parse_gpp_sid_cookie(raw: &str) -> Option<Vec<u16>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let ids: Vec<u16> = trimmed
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            match entry.parse::<u16>() {
                Ok(id) => Some(id),
                Err(_) => {
                    log::debug!("Ignoring invalid __gpp_sid entry: {entry:?}");
                    None
                }
            }
        })
        .collect();

    if ids.is_empty() { None } else { Some(ids) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Header "DBABTA" encodes version 1 with section 6 (US Privacy), and the
    // section string is a US Privacy string.
    const GPP_USP_ONLY: &str = "DBABTA~1YNN";

    // Header "DBACNY" encodes version 1 with sections 2 (EU TCF v2) and 6.
    const GPP_TCF_AND_USP: &str = "DBACNY~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA~1YNN";

    #[test]
    fn decodes_a_string_with_one_section() {
        let result = decode_gpp_string(GPP_USP_ONLY).expect("should decode");
        assert_eq!(result.section_ids, vec![6]);
        assert_eq!(
            result.us_sale_opt_out, None,
            "section 6 is not a US section"
        );
    }

    #[test]
    fn reads_the_section_identifiers_of_a_string_with_two() {
        let result = decode_gpp_string(GPP_TCF_AND_USP).expect("should decode");
        assert_eq!(result.section_ids, vec![2, 6]);
    }

    #[test]
    fn hands_over_the_raw_section_another_scheme_decodes() {
        assert_eq!(
            section(GPP_TCF_AND_USP, 2).as_deref(),
            Some("CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA"),
            "section 2 is handed over as written"
        );
        assert_eq!(section(GPP_TCF_AND_USP, 7), None, "a section not carried");
        assert_eq!(
            section("not a GPP string", 2),
            None,
            "a string that does not parse"
        );
    }

    #[test]
    fn section_identifiers_come_from_the_string_or_the_hint() {
        assert_eq!(
            section_ids_of(Some(GPP_TCF_AND_USP), Some("7")),
            Some(vec![2, 6]),
            "a string that decodes is authoritative over the hint"
        );
        assert_eq!(
            section_ids_of(Some("not a GPP string"), Some("2,6")),
            Some(vec![2, 6]),
            "the hint stands in when the string does not decode"
        );
        assert_eq!(section_ids_of(None, Some("7,8")), Some(vec![7, 8]));
        assert_eq!(section_ids_of(None, None), None);
    }

    #[test]
    fn rejects_an_invalid_string() {
        assert!(decode_gpp_string("totally-invalid").is_err());
        assert!(decode_gpp_string("").is_err());
    }

    #[test]
    fn rejects_an_oversized_string() {
        let oversized = "D".repeat(MAX_GPP_STRING_LEN + 1);
        assert!(decode_gpp_string(&oversized).is_err());
    }

    #[test]
    fn parses_the_section_hint() {
        assert_eq!(parse_gpp_sid_cookie("2,6"), Some(vec![2, 6]));
        assert_eq!(parse_gpp_sid_cookie("2"), Some(vec![2]));
        assert_eq!(parse_gpp_sid_cookie(" 2 , 6 , 8 "), Some(vec![2, 6, 8]));
        assert_eq!(parse_gpp_sid_cookie("2,abc,6"), Some(vec![2, 6]));
        assert_eq!(parse_gpp_sid_cookie(""), None);
        assert_eq!(parse_gpp_sid_cookie("  "), None);
        assert_eq!(parse_gpp_sid_cookie("abc,def"), None);
    }

    #[test]
    fn decodes_a_us_section_that_does_not_opt_out() {
        let result = decode_gpp_string("DBABLA~BVQqAAAAAgA.QA").expect("should decode");
        assert_eq!(result.us_sale_opt_out, Some(false));
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
    fn no_us_section_gives_no_opt_out_answer() {
        let result = decode_gpp_string(GPP_TCF_AND_USP).expect("should decode");
        assert_eq!(result.us_sale_opt_out, None);
    }

    #[test]
    fn a_later_us_section_opting_out_wins_over_an_earlier_one_that_does_not() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "BVVVVVVVVWA.AA")]);
        let result = decode_gpp_string(&gpp).expect("should decode");
        assert_eq!(result.us_sale_opt_out, Some(true));
    }

    #[test]
    fn several_us_sections_none_opting_out_answer_false() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "BVgVVVVVVWA.AA")]);
        let result = decode_gpp_string(&gpp).expect("should decode");
        assert_eq!(result.us_sale_opt_out, Some(false));
    }

    #[test]
    fn a_decodable_answer_stands_beside_an_undecodable_section() {
        let gpp = gpp_with_sections(&[(7, "BVQqAAAAAgA.QA"), (9, "not-a-valid-usva-section")]);
        let result = decode_gpp_string(&gpp).expect("should decode the header");
        assert_eq!(result.us_sale_opt_out, Some(false));

        let gpp = gpp_with_sections(&[(7, "not-a-valid-usnat-section"), (9, "BVVVVVVVVWA.AA")]);
        let result = decode_gpp_string(&gpp).expect("should decode the header");
        assert_eq!(result.us_sale_opt_out, Some(true));
    }

    #[test]
    fn only_undecodable_us_sections_give_no_answer() {
        let gpp = gpp_with_sections(&[(7, "not-a-valid-usnat-section"), (9, "also-invalid")]);
        let result = decode_gpp_string(&gpp).expect("should decode the header");
        assert_eq!(result.us_sale_opt_out, None);
    }
}
