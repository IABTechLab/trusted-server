//! US Privacy string decoder.
//!
//! Parses the legacy 4-character IAB US Privacy string (CCPA format).
//!
//! # Format
//!
//! The string is exactly 4 characters: `VNOL` where:
//! - **V** (version): always `1`
//! - **N** (notice): `Y` = given, `N` = not given, `-` = N/A
//! - **O** (opt-out of sale): `Y` = opted out, `N` = not opted out, `-` = N/A
//! - **L** (LSPA covered): `Y` = yes, `N` = no, `-` = N/A
//!
//! # References
//!
//! - [IAB US Privacy String specification](https://github.com/InteractiveAdvertisingBureau/USPrivacy/blob/master/CCPA/US%20Privacy%20String.md)

use error_stack::{Report, ResultExt};

use super::types::{ConsentDecodeError, PrivacyFlag, UsPrivacy};

/// Decodes a US Privacy string into a [`UsPrivacy`] struct.
///
/// # Errors
///
/// - [`ConsentDecodeError::InvalidUsPrivacy`] if the string is not exactly
///   4 characters, has an unsupported version, or contains invalid flag values.
pub fn decode_us_privacy(s: &str) -> Result<UsPrivacy, Report<ConsentDecodeError>> {
    let chars: Vec<char> = s.chars().collect();

    if chars.len() != 4 {
        return Err(Report::new(ConsentDecodeError::InvalidUsPrivacy {
            reason: format!("expected 4 characters, got {}", chars.len()),
        }));
    }

    let version = match chars[0] {
        '1' => 1u8,
        other => {
            return Err(Report::new(ConsentDecodeError::InvalidUsPrivacy {
                reason: format!("unsupported version '{}', expected '1'", other),
            }));
        }
    };

    let notice_given =
        parse_flag(chars[1]).change_context(ConsentDecodeError::InvalidUsPrivacy {
            reason: format!("invalid notice flag '{}'", chars[1]),
        })?;

    let opt_out_sale =
        parse_flag(chars[2]).change_context(ConsentDecodeError::InvalidUsPrivacy {
            reason: format!("invalid opt-out flag '{}'", chars[2]),
        })?;

    let lspa_covered =
        parse_flag(chars[3]).change_context(ConsentDecodeError::InvalidUsPrivacy {
            reason: format!("invalid LSPA flag '{}'", chars[3]),
        })?;

    Ok(UsPrivacy {
        version,
        notice_given,
        opt_out_sale,
        lspa_covered,
    })
}

/// Parses a single US Privacy flag character.
fn parse_flag(c: char) -> Result<PrivacyFlag, Report<ConsentDecodeError>> {
    match c {
        'Y' | 'y' => Ok(PrivacyFlag::Yes),
        'N' | 'n' => Ok(PrivacyFlag::No),
        '-' => Ok(PrivacyFlag::NotApplicable),
        other => Err(Report::new(ConsentDecodeError::InvalidUsPrivacy {
            reason: format!("invalid flag character '{other}'"),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_string() {
        let result = decode_us_privacy("1YNN").expect("should decode 1YNN");
        assert_eq!(result.version, 1);
        assert_eq!(result.notice_given, PrivacyFlag::Yes);
        assert_eq!(result.opt_out_sale, PrivacyFlag::No);
        assert_eq!(result.lspa_covered, PrivacyFlag::No);
    }

    #[test]
    fn decodes_all_yes() {
        let result = decode_us_privacy("1YYY").expect("should decode 1YYY");
        assert_eq!(result.notice_given, PrivacyFlag::Yes);
        assert_eq!(result.opt_out_sale, PrivacyFlag::Yes);
        assert_eq!(result.lspa_covered, PrivacyFlag::Yes);
    }

    #[test]
    fn decodes_all_not_applicable() {
        let result = decode_us_privacy("1---").expect("should decode 1---");
        assert_eq!(result.notice_given, PrivacyFlag::NotApplicable);
        assert_eq!(result.opt_out_sale, PrivacyFlag::NotApplicable);
        assert_eq!(result.lspa_covered, PrivacyFlag::NotApplicable);
    }

    #[test]
    fn decodes_mixed_flags() {
        let result = decode_us_privacy("1NYN").expect("should decode 1NYN");
        assert_eq!(result.notice_given, PrivacyFlag::No);
        assert_eq!(result.opt_out_sale, PrivacyFlag::Yes);
        assert_eq!(result.lspa_covered, PrivacyFlag::No);
    }

    #[test]
    fn roundtrips_through_display() {
        let result = decode_us_privacy("1YNN").expect("should decode");
        assert_eq!(
            result.to_string(),
            "1YNN",
            "should roundtrip through Display"
        );
    }

    #[test]
    fn rejects_too_short() {
        let result = decode_us_privacy("1YN");
        assert!(result.is_err(), "should reject 3-char string");
    }

    #[test]
    fn rejects_too_long() {
        let result = decode_us_privacy("1YNNN");
        assert!(result.is_err(), "should reject 5-char string");
    }

    #[test]
    fn rejects_empty() {
        let result = decode_us_privacy("");
        assert!(result.is_err(), "should reject empty string");
    }

    #[test]
    fn rejects_bad_version() {
        let result = decode_us_privacy("2YNN");
        assert!(result.is_err(), "should reject version 2");
    }

    #[test]
    fn rejects_invalid_flag() {
        let result = decode_us_privacy("1XNN");
        assert!(result.is_err(), "should reject invalid flag 'X'");
    }

    #[test]
    fn accepts_lowercase_flags() {
        let result = decode_us_privacy("1ynn").expect("should accept lowercase");
        assert_eq!(result.notice_given, PrivacyFlag::Yes);
        assert_eq!(result.opt_out_sale, PrivacyFlag::No);
        assert_eq!(result.lspa_covered, PrivacyFlag::No);
    }
}
