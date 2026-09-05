//! Shared governance types for checked records.

use error_stack::Report;

/// Validation failure for a checked record field.
#[derive(Debug, derive_more::Display)]
pub enum ModelError {
    /// A required text field is empty or only whitespace.
    #[display("{field} must not be blank")]
    Blank {
        /// Name of the invalid field.
        field: &'static str,
    },
    /// An expiry is not a canonical UTC timestamp.
    #[display("expiry must use YYYY-MM-DDTHH:MM:SSZ")]
    InvalidExpiry,
}

impl core::error::Error for ModelError {}

/// Non-empty owner responsible for a governed record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Owner(String);

impl Owner {
    /// Create an owner from non-empty text.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains only whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, Report<ModelError>> {
        required_text(value.into(), "owner").map(Self)
    }

    /// Return the owner text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Non-empty rationale for a governed record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rationale(String);

impl Rationale {
    /// Create a rationale from non-empty text.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains only whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, Report<ModelError>> {
        required_text(value.into(), "rationale").map(Self)
    }

    /// Return the rationale text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Canonical UTC expiry timestamp for a governed record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expiry(String);

impl Expiry {
    /// Parse a `YYYY-MM-DDTHH:MM:SSZ` expiry timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error when the timestamp is not canonical UTC or contains
    /// an invalid calendar or clock component.
    pub fn parse(value: impl Into<String>) -> Result<Self, Report<ModelError>> {
        let value = value.into();
        if valid_expiry(&value) {
            Ok(Self(value))
        } else {
            Err(Report::new(ModelError::InvalidExpiry))
        }
    }

    /// Return the canonical timestamp text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Mandatory governance attached to an expiring exception or prose override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Governance {
    owner: Owner,
    rationale: Rationale,
    expiry: Expiry,
}

impl Governance {
    /// Create governance with all structurally required fields.
    #[must_use]
    pub const fn new(owner: Owner, rationale: Rationale, expiry: Expiry) -> Self {
        Self {
            owner,
            rationale,
            expiry,
        }
    }

    /// Return the responsible owner.
    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    /// Return the bounded rationale.
    #[must_use]
    pub const fn rationale(&self) -> &Rationale {
        &self.rationale
    }

    /// Return the expiry timestamp.
    #[must_use]
    pub const fn expiry(&self) -> &Expiry {
        &self.expiry
    }
}

fn required_text(value: String, field: &'static str) -> Result<String, Report<ModelError>> {
    if value.trim().is_empty() {
        Err(Report::new(ModelError::Blank { field }))
    } else {
        Ok(value)
    }
}

fn valid_expiry(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return false;
    }

    let Some(year) = decimal(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = decimal(bytes, 5, 2) else {
        return false;
    };
    let Some(day) = decimal(bytes, 8, 2) else {
        return false;
    };
    let Some(hour) = decimal(bytes, 11, 2) else {
        return false;
    };
    let Some(minute) = decimal(bytes, 14, 2) else {
        return false;
    };
    let Some(second) = decimal(bytes, 17, 2) else {
        return false;
    };

    year > 0
        && (1..=12).contains(&month)
        && day > 0
        && day <= days_in_month(year, month)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

fn decimal(bytes: &[u8], start: usize, length: usize) -> Option<u32> {
    bytes
        .get(start..start + length)?
        .iter()
        .try_fold(0, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(*byte - b'0'))
        })
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}
