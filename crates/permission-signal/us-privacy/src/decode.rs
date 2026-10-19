//! The US Privacy string decoder, the four character format.
//!
//! The string is `VNOL`:
//! - **V** (version): always `1`
//! - **N** (notice): `Y` given, `N` not given, `-` not applicable
//! - **O** (opt-out of sale): `Y` opted out, `N` not, `-` not applicable
//! - **L** (LSPA covered): `Y`, `N`, `-`
//!
//! # References
//!
//! - [IAB US Privacy String specification](https://github.com/InteractiveAdvertisingBureau/USPrivacy/blob/master/CCPA/US%20Privacy%20String.md)

use core::fmt;

use error_stack::{Report, ResultExt as _};

/// A decoded US Privacy string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsPrivacy {
    /// Specification version, always 1.
    pub version: u8,
    /// Whether explicit notice has been given to the consumer.
    pub notice_given: PrivacyFlag,
    /// Whether the consumer has opted out of the sale of personal information.
    pub opt_out_sale: PrivacyFlag,
    /// Whether the transaction is covered by the Limited Service Provider
    /// Agreement.
    pub lspa_covered: PrivacyFlag,
}

/// One flag of the string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyFlag {
    /// `Y`.
    Yes,
    /// `N`.
    No,
    /// `-`, not applicable or unknown.
    NotApplicable,
}

impl fmt::Display for PrivacyFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yes => write!(f, "Y"),
            Self::No => write!(f, "N"),
            Self::NotApplicable => write!(f, "-"),
        }
    }
}

impl From<bool> for PrivacyFlag {
    fn from(value: bool) -> Self {
        if value { Self::Yes } else { Self::No }
    }
}

impl fmt::Display for UsPrivacy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}{}",
            self.version, self.notice_given, self.opt_out_sale, self.lspa_covered,
        )
    }
}

/// Why a US Privacy string could not be decoded.
#[derive(Debug, derive_more::Display)]
pub enum UsPrivacyDecodeError {
    /// The string is not four characters, is not version 1, or carries a
    /// flag that is not `Y`, `N` or `-`.
    #[display("invalid US Privacy string: {reason}")]
    InvalidUsPrivacy {
        /// What was wrong with it.
        reason: String,
    },
}

impl core::error::Error for UsPrivacyDecodeError {}

/// Decodes a US Privacy string.
///
/// # Errors
///
/// [`UsPrivacyDecodeError::InvalidUsPrivacy`] when the string is not exactly
/// four characters, is not version 1, or carries an invalid flag.
pub fn decode_us_privacy(s: &str) -> Result<UsPrivacy, Report<UsPrivacyDecodeError>> {
    let chars: Vec<char> = s.chars().collect();

    if chars.len() != 4 {
        return Err(Report::new(UsPrivacyDecodeError::InvalidUsPrivacy {
            reason: format!("expected 4 characters, got {}", chars.len()),
        }));
    }

    let version = match chars[0] {
        '1' => 1_u8,
        other => {
            return Err(Report::new(UsPrivacyDecodeError::InvalidUsPrivacy {
                reason: format!("unsupported version '{other}', expected '1'"),
            }));
        }
    };

    let notice_given =
        parse_flag(chars[1]).change_context(UsPrivacyDecodeError::InvalidUsPrivacy {
            reason: format!("invalid notice flag '{}'", chars[1]),
        })?;

    let opt_out_sale =
        parse_flag(chars[2]).change_context(UsPrivacyDecodeError::InvalidUsPrivacy {
            reason: format!("invalid opt-out flag '{}'", chars[2]),
        })?;

    let lspa_covered =
        parse_flag(chars[3]).change_context(UsPrivacyDecodeError::InvalidUsPrivacy {
            reason: format!("invalid LSPA flag '{}'", chars[3]),
        })?;

    Ok(UsPrivacy {
        version,
        notice_given,
        opt_out_sale,
        lspa_covered,
    })
}

fn parse_flag(c: char) -> Result<PrivacyFlag, Report<UsPrivacyDecodeError>> {
    match c {
        'Y' | 'y' => Ok(PrivacyFlag::Yes),
        'N' | 'n' => Ok(PrivacyFlag::No),
        '-' => Ok(PrivacyFlag::NotApplicable),
        other => Err(Report::new(UsPrivacyDecodeError::InvalidUsPrivacy {
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
        assert_eq!(result.to_string(), "1YNN");
        assert_eq!(PrivacyFlag::NotApplicable.to_string(), "-");
        assert_eq!(PrivacyFlag::from(true), PrivacyFlag::Yes);
        assert_eq!(PrivacyFlag::from(false), PrivacyFlag::No);
    }

    #[test]
    fn rejects_wrong_lengths() {
        assert!(
            decode_us_privacy("1YN").is_err(),
            "should reject 3 characters"
        );
        assert!(
            decode_us_privacy("1YNNN").is_err(),
            "should reject 5 characters"
        );
        assert!(decode_us_privacy("").is_err(), "should reject empty");
    }

    #[test]
    fn rejects_bad_version() {
        assert!(
            decode_us_privacy("2YNN").is_err(),
            "should reject version 2"
        );
    }

    #[test]
    fn rejects_invalid_flag() {
        assert!(decode_us_privacy("1XNN").is_err(), "should reject flag 'X'");
    }

    #[test]
    fn accepts_lowercase_flags() {
        let result = decode_us_privacy("1ynn").expect("should accept lowercase");
        assert_eq!(result.notice_given, PrivacyFlag::Yes);
        assert_eq!(result.opt_out_sale, PrivacyFlag::No);
        assert_eq!(result.lspa_covered, PrivacyFlag::No);
    }
}
