//! Identifiers shared by the identity provider API.

use core::ops::Add;
use core::str::FromStr;
use std::time::Duration;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use super::resolution::IdentityProviderError;
use crate::ec::partner::normalize_partner_source_domain;

/// Longest identity provider ID, in bytes.
const MAX_PROVIDER_ID_LENGTH: usize = 63;

/// An identity provider's ID, such as `id5` or `pull-ssp-example-com`.
///
/// Matches `^[a-z][a-z0-9_-]{0,62}$`, so it can be a bare TOML key and can
/// equal an integration ID that uses `_`. Separate from
/// [`crate::auction::ProviderId`].
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct IdentityProviderId(String);

impl IdentityProviderId {
    /// The ID as written in config.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for IdentityProviderId {
    type Err = Report<IdentityProviderError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut bytes = value.bytes();
        let valid_first = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let valid_rest = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        });
        if valid_first && valid_rest && value.len() <= MAX_PROVIDER_ID_LENGTH {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityProviderError::config(format!(
                "identity provider id `{value}` must match ^[a-z][a-z0-9_-]{{0,62}}$"
            )))
        }
    }
}

impl TryFrom<String> for IdentityProviderId {
    type Error = Report<IdentityProviderError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<IdentityProviderId> for String {
    fn from(id: IdentityProviderId) -> Self {
        id.0
    }
}

/// An EID `source` domain, normalized. Also the key of the KV `ids` map.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct SourceDomain(String);

impl SourceDomain {
    /// The normalized domain.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a vendor's spelling of a source names this one, after
    /// normalization.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        normalize_partner_source_domain(name).is_ok_and(|normalized| normalized == self.0)
    }
}

impl FromStr for SourceDomain {
    type Err = Report<IdentityProviderError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize_partner_source_domain(value)
            .map(Self)
            .map_err(IdentityProviderError::config)
    }
}

impl TryFrom<String> for SourceDomain {
    type Error = Report<IdentityProviderError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SourceDomain> for String {
    fn from(source: SourceDomain) -> Self {
        source.0
    }
}

/// Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixTime(u64);

impl UnixTime {
    /// From Unix seconds.
    #[must_use]
    pub fn from_secs(secs: u64) -> Self {
        Self(secs)
    }

    /// From Unix milliseconds, for vendors such as Lockr and UID2. Floors to
    /// whole seconds.
    #[must_use]
    pub fn from_millis(millis: u64) -> Self {
        Self(millis / 1000)
    }

    /// As Unix seconds.
    #[must_use]
    pub fn as_secs(self) -> u64 {
        self.0
    }
}

impl Add<Duration> for UnixTime {
    type Output = UnixTime;

    fn add(self, rhs: Duration) -> UnixTime {
        UnixTime(self.0.saturating_add(rhs.as_secs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_accepts_the_documented_grammar() {
        for id in ["id5", "pull-ssp-example-com", "lockr_aim", "a"] {
            assert!(
                id.parse::<IdentityProviderId>().is_ok(),
                "should accept `{id}`"
            );
        }
        let longest = format!("a{}", "b".repeat(62));
        longest
            .parse::<IdentityProviderId>()
            .expect("should accept 63 characters");
    }

    #[test]
    fn provider_id_rejects_everything_else() {
        let too_long = format!("a{}", "b".repeat(63));
        for id in ["", "1id", "Id5", "id 5", "id.5", too_long.as_str()] {
            let error = id
                .parse::<IdentityProviderId>()
                .expect_err("should reject the id");
            assert!(
                matches!(
                    error.current_context(),
                    IdentityProviderError::Config { .. }
                ),
                "should be a Config error for `{id}`"
            );
        }
    }

    #[test]
    fn source_domain_normalizes_and_matches() {
        let source: SourceDomain = "ID5-Sync.com.".parse().expect("should parse");
        assert_eq!(
            source.as_str(),
            "id5-sync.com",
            "should lower-case and drop the trailing dot"
        );
        assert!(
            source.matches("id5-sync.COM"),
            "should match another spelling"
        );
        assert!(
            !source.matches("example.com"),
            "should not match another domain"
        );
        assert!(
            !source.matches("https://id5-sync.com"),
            "should not match a URL"
        );
    }

    #[test]
    fn source_domain_round_trips_through_serde() {
        let source: SourceDomain = "example.com".parse().expect("should parse");
        let json = serde_json::to_string(&source).expect("should serialize");
        assert_eq!(json, "\"example.com\"", "should serialize as a bare string");
        let back: SourceDomain = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(back, source, "should round-trip");
        assert!(
            serde_json::from_str::<SourceDomain>("\"not/a/domain\"").is_err(),
            "should reject an invalid domain on deserialize"
        );
    }

    #[test]
    fn unix_time_converts_millis_and_adds_durations() {
        assert_eq!(
            UnixTime::from_millis(1_760_000_000_999).as_secs(),
            1_760_000_000,
            "should floor millis"
        );
        let later = UnixTime::from_secs(100) + Duration::from_secs(50);
        assert_eq!(later, UnixTime::from_secs(150), "should add seconds");
        let saturated = UnixTime::from_secs(u64::MAX) + Duration::from_secs(1);
        assert_eq!(saturated, UnixTime::from_secs(u64::MAX), "should saturate");
    }
}
