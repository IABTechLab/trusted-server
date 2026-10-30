//! Per-provider outcomes of one navigation's lookups.

use crate::ec::eid_types::{EidValidationError, ProviderId, SourceDomain};
use crate::ec::kv_types::KvEid;

/// Validation drops for one response, by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DropCounts {
    /// Blank or oversized UIDs, or negative agent types.
    pub invalid_uid: u16,
    /// Invalid `ext`, `inserter`, `matcher` or `mm`.
    pub invalid_field: u16,
    /// Entries for sources the provider does not claim.
    pub unclaimed_source: u16,
    /// UIDs whose expiry is not after the request time.
    pub expired: u16,
    /// Entries or UIDs beyond the provider's shape limits.
    pub over_limit: u16,
    /// Duplicate UIDs within one source.
    pub duplicate: u16,
}

impl DropCounts {
    /// Counts one validation failure.
    pub fn record(&mut self, reason: EidValidationError) {
        let counter = match reason {
            EidValidationError::EmptyUid
            | EidValidationError::UidTooLong
            | EidValidationError::NegativeAgentType => &mut self.invalid_uid,
            EidValidationError::UnknownMatchMethod
            | EidValidationError::InvalidExt
            | EidValidationError::FieldTooLong => &mut self.invalid_field,
            EidValidationError::UnclaimedSource => &mut self.unclaimed_source,
            EidValidationError::Expired => &mut self.expired,
        };
        *counter = counter.saturating_add(1);
    }

    /// Total drops.
    #[must_use]
    pub fn total(&self) -> u16 {
        self.invalid_uid
            .saturating_add(self.invalid_field)
            .saturating_add(self.unclaimed_source)
            .saturating_add(self.expired)
            .saturating_add(self.over_limit)
            .saturating_add(self.duplicate)
    }
}

/// Why a provider was not called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The provider is disabled.
    Disabled,
    /// Consent does not permit this provider.
    Consent,
    /// None of its sources is due.
    NotDue,
    /// Every due source needs input V1 never has (hashed email).
    Input,
    /// The built URL is not `https` or not allowlisted.
    Destination,
    /// The provider's hourly rate limit is reached.
    RateLimited,
}

/// One outcome per provider per navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupStatus {
    /// Valid entries were returned.
    Found {
        /// UIDs kept after validation.
        kept: u16,
        /// UIDs or entries dropped by validation.
        dropped: DropCounts,
    },
    /// The provider knows no ID for this visitor.
    NoMatch,
    /// No response before the shared deadline.
    Timeout,
    /// A non-success HTTP status.
    HttpError(http::StatusCode),
    /// The request could not be sent or the body exceeded the limit.
    TransportFailed,
    /// The body could not be parsed at all.
    InvalidResponse,
    /// The provider was not called.
    Skipped(SkipReason),
}

impl LookupStatus {
    /// Metric name for logs (spec §12.2).
    #[must_use]
    pub fn metric_name(&self) -> &'static str {
        match self {
            Self::Found { .. } => "found",
            Self::NoMatch => "no_match",
            Self::Timeout => "timeout",
            Self::HttpError(_) => "http_error",
            Self::TransportFailed => "transport_error",
            Self::InvalidResponse => "invalid_response",
            Self::Skipped(SkipReason::Disabled) => "skipped_disabled",
            Self::Skipped(SkipReason::Consent) => "skipped_consent",
            Self::Skipped(SkipReason::NotDue) => "skipped_not_due",
            Self::Skipped(SkipReason::Input) => "skipped_input",
            Self::Skipped(SkipReason::RateLimited) => "skipped_rate_limited",
            Self::Skipped(SkipReason::Destination) => "destination_rejected",
        }
    }
}

/// One provider's outcome and validated entries.
#[derive(Debug, Clone)]
pub struct ProviderLookup {
    /// The provider.
    pub provider: ProviderId,
    /// What happened.
    pub status: LookupStatus,
    /// Validated entries per returned source; empty unless `Found`.
    pub entries: Vec<(SourceDomain, Vec<KvEid>)>,
}

/// Validated results of one navigation's lookups.
#[derive(Debug, Clone, Default)]
pub struct LookupResults {
    lookups: Vec<ProviderLookup>,
}

impl LookupResults {
    /// Wraps per-provider results in provider-config order.
    #[must_use]
    pub fn new(lookups: Vec<ProviderLookup>) -> Self {
        Self { lookups }
    }

    /// Iterates results in provider-config order.
    pub fn iter(&self) -> impl Iterator<Item = &ProviderLookup> {
        self.lookups.iter()
    }

    /// Whether no provider produced a result.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lookups.is_empty()
    }
}
