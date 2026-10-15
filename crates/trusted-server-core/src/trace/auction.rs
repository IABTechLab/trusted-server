//! Closed public auction evidence and exact opaque correlation tokens.

use error_stack::Report;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, MapAccess, SeqAccess, Visitor},
};
use std::{fmt, marker::PhantomData};
use uuid::Uuid;

const PROVIDER_LIMIT: usize = 16;
const SLOT_LIMIT: usize = 64;
const REQUESTED_SIZE_LIMIT: usize = 16;
const MAXIMUM_DIMENSION: u32 = 100_000;

/// A bounded failure to validate an exact opaque trace token.
#[derive(Debug, derive_more::Display)]
#[display("invalid trace token")]
pub struct TraceTokenError;

impl core::error::Error for TraceTokenError {}

/// An exact lowercase UUID-v4 auction token, independent of internal IDs.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, derive_more::Display)]
#[serde(try_from = "String")]
pub struct DiagnosticAuctionId(String);

impl DiagnosticAuctionId {
    /// Parse an exact auction token without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`TraceTokenError`] when the supplied bytes are not the public
    /// lowercase, unhyphenated UUID-v4 auction-token shape.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::trace::DiagnosticAuctionId;
    /// let token = DiagnosticAuctionId::parse("ts-auc-00000000000040008000000000000000")?;
    /// assert_eq!(token.as_str(), "ts-auc-00000000000040008000000000000000");
    /// # Ok::<(), error_stack::Report<trusted_server_core::trace::TraceTokenError>>(())
    /// ```
    ///
    /// # Performance
    ///
    /// Validation checks a fixed byte shape and allocates only for accepted input.
    pub fn parse(value: &str) -> Result<Self, Report<TraceTokenError>> {
        if !valid_token(value, "ts-auc-", false) {
            return Err(Report::new(TraceTokenError));
        }
        Ok(Self(value.to_owned()))
    }

    /// Generate a fresh public auction token using a UUID v4.
    ///
    /// # Panics
    ///
    /// Panics if the UUID random source cannot provide randomness.
    ///
    /// # Examples
    ///
    /// ```
    /// let token = trusted_server_core::trace::DiagnosticAuctionId::generate();
    /// assert!(token.as_str().starts_with("ts-auc-"));
    /// ```
    #[must_use]
    pub fn generate() -> Self {
        Self(format!("ts-auc-{}", Uuid::new_v4().simple()))
    }

    /// Borrow the exact stored correlation bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// let token = trusted_server_core::trace::DiagnosticAuctionId::generate();
    /// assert_eq!(token.as_str().len(), 39);
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DiagnosticAuctionId {
    type Error = Report<TraceTokenError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !valid_token(&value, "ts-auc-", false) {
            return Err(Report::new(TraceTokenError));
        }
        Ok(Self(value))
    }
}

impl From<DiagnosticAuctionId> for String {
    fn from(value: DiagnosticAuctionId) -> Self {
        value.0
    }
}

/// An exact lowercase hyphenated UUID-v4 reference for one accepted slot.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, derive_more::Display)]
#[serde(try_from = "String")]
pub struct TraceSlotRef(String);

impl TraceSlotRef {
    /// Parse an exact slot reference without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`TraceTokenError`] for any other token shape, UUID version or
    /// variant, including a normalized alternative.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::trace::TraceSlotRef;
    /// let token = TraceSlotRef::parse("ts-slot-00000000-0000-4000-8000-000000000000")?;
    /// assert_eq!(token.as_str().len(), 44);
    /// # Ok::<(), error_stack::Report<trusted_server_core::trace::TraceTokenError>>(())
    /// ```
    ///
    /// # Performance
    ///
    /// Validation checks a fixed byte shape and allocates only for accepted input.
    pub fn parse(value: &str) -> Result<Self, Report<TraceTokenError>> {
        if !valid_token(value, "ts-slot-", true) {
            return Err(Report::new(TraceTokenError));
        }
        Ok(Self(value.to_owned()))
    }

    /// Generate a fresh canonical slot reference using a UUID v4.
    ///
    /// # Panics
    ///
    /// Panics if the UUID random source cannot provide randomness.
    ///
    /// # Examples
    ///
    /// ```
    /// let token = trusted_server_core::trace::TraceSlotRef::generate();
    /// assert!(token.as_str().starts_with("ts-slot-"));
    /// ```
    #[must_use]
    pub fn generate() -> Self {
        Self(format!("ts-slot-{}", Uuid::new_v4().hyphenated()))
    }

    /// Borrow the exact stored reference bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// let token = trusted_server_core::trace::TraceSlotRef::generate();
    /// assert_eq!(token.as_str().len(), 44);
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TraceSlotRef {
    type Error = Report<TraceTokenError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !valid_token(&value, "ts-slot-", true) {
            return Err(Report::new(TraceTokenError));
        }
        Ok(Self(value))
    }
}

impl From<TraceSlotRef> for String {
    fn from(value: TraceSlotRef) -> Self {
        value.0
    }
}

fn valid_token(value: &str, prefix: &str, hyphenated: bool) -> bool {
    let Some(uuid) = value.strip_prefix(prefix).map(str::as_bytes) else {
        return false;
    };
    let (length, version, variant) = if hyphenated {
        (36, 14, 19)
    } else {
        (32, 12, 16)
    };
    uuid.len() == length
        && uuid[version] == b'4'
        && matches!(uuid[variant], b'8' | b'9' | b'a' | b'b')
        && uuid.iter().enumerate().all(|(index, byte)| {
            if hyphenated && matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                matches!(byte, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

/// The server call site that produced the auction observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceAuctionSource {
    /// The initial publisher document's server-side auction.
    InitialNavigationSsat,
    /// The server-side auction for a SPA page-bids request.
    SpaPageBids,
    /// The programmatic auction API.
    AuctionApi,
}

/// The directly observed auction terminal outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceAuctionTerminalStatus {
    /// Execution completed, including a zero-bid result.
    Completed,
    /// Auction execution or collection failed.
    ExecutionFailed,
    /// The split dispatch path could not start the auction.
    DispatchFailed,
    /// Dispatched work could not be collected or delivered.
    Abandoned,
    /// Consent, policy, or the definitive empty slot list skipped execution.
    Skipped,
}

/// A bounded terminal category without provider text or errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceAuctionTerminalReason {
    /// An explicit policy or consent decision skipped execution.
    PolicySkipped,
    /// The definitive accepted slot list is empty.
    NoEligibleSlots,
    /// The unsuccessful split dispatch path launched no provider.
    NoProviderLaunched,
    /// A provider execution failure is directly known.
    ProviderExecutionFailed,
    /// Collecting dispatched work failed.
    CollectionFailed,
    /// No more specific allowlisted reason is observed.
    Unknown,
}

/// The role of one auction-wide provider call without its identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceProviderRole {
    /// A bidder call.
    Bidder,
    /// A mediation call.
    Mediator,
    /// The role is not known.
    Unknown,
}

/// The directly observed status of one auction-wide provider call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceProviderStatus {
    /// The call returned a successful response with bids.
    Success,
    /// The call returned no bids.
    NoBid,
    /// The call failed.
    Error,
    /// The call remains in flight at the observed point.
    Pending,
    /// The in-flight call was abandoned.
    Abandoned,
    /// The status cannot be determined.
    Unknown,
}

/// The observed selection and final delivery disposition for an accepted slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceSlotCandidate {
    /// The selected candidate entered the final response.
    Selected,
    /// No candidate was selected.
    NoCandidate,
    /// A selected winner could not safely enter the final response.
    SelectedUnrenderable,
    /// Existing observations cannot distinguish the accepted slot instance.
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceProviderCall {
    #[serde(deserialize_with = "deserialize_ordinal")]
    provider_number: u16,
    #[serde(deserialize_with = "deserialize_string_enum")]
    role: TraceProviderRole,
    #[serde(deserialize_with = "deserialize_string_enum")]
    status: TraceProviderStatus,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_duration",
        skip_serializing_if = "Option::is_none"
    )]
    response_time_ms: Option<u32>,
    #[serde(deserialize_with = "deserialize_count")]
    returned_bid_count: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct TraceSize([u32; 2]);

impl<'de> Deserialize<'de> for TraceSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let dimensions = <[TraceDimension; 2]>::deserialize(deserializer)?;
        Ok(Self(dimensions.map(|dimension| dimension.0)))
    }
}

struct TraceDimension(u32);

impl<'de> Deserialize<'de> for TraceDimension {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let dimension = deserializer.deserialize_any(UnsignedVisitor::<MAXIMUM_DIMENSION>)?;
        if dimension == 0 {
            return Err(D::Error::custom("invalid trace dimension"));
        }
        u32::try_from(dimension)
            .map(Self)
            .map_err(|_| D::Error::custom("invalid trace dimension"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceAuctionSlot {
    #[serde(deserialize_with = "deserialize_ordinal")]
    slot_number: u16,
    slot_ref: TraceSlotRef,
    #[serde(deserialize_with = "deserialize_sizes")]
    requested_sizes: Vec<TraceSize>,
    #[serde(deserialize_with = "deserialize_count")]
    returned_bid_count: u16,
    #[serde(deserialize_with = "deserialize_string_enum")]
    candidate: TraceSlotCandidate,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    selected_creative_size: Option<TraceSize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceAuctionTruncation {
    #[serde(deserialize_with = "deserialize_count")]
    omitted_provider_calls: u16,
    #[serde(deserialize_with = "deserialize_count")]
    omitted_slots: u16,
    #[serde(deserialize_with = "deserialize_count")]
    omitted_nested_values: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProviderToSlotNoBid {
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceAuctionCoverage {
    #[serde(deserialize_with = "deserialize_string_enum")]
    provider_to_slot_no_bid: ProviderToSlotNoBid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceAuctionEvidenceFields {
    #[serde(deserialize_with = "deserialize_version")]
    schema_version: u8,
    diagnostic_auction_id: DiagnosticAuctionId,
    #[serde(deserialize_with = "deserialize_string_enum")]
    source: TraceAuctionSource,
    #[serde(deserialize_with = "deserialize_string_enum")]
    terminal_status: TraceAuctionTerminalStatus,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_enum",
        skip_serializing_if = "Option::is_none"
    )]
    terminal_reason: Option<TraceAuctionTerminalReason>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_duration",
        skip_serializing_if = "Option::is_none"
    )]
    total_time_ms: Option<u32>,
    #[serde(deserialize_with = "deserialize_providers")]
    provider_calls: Vec<TraceProviderCall>,
    #[serde(deserialize_with = "deserialize_slots")]
    slots: Vec<TraceAuctionSlot>,
    #[serde(deserialize_with = "deserialize_object")]
    truncation: TraceAuctionTruncation,
    #[serde(deserialize_with = "deserialize_object")]
    coverage: TraceAuctionCoverage,
}

/// Closed version-one server facts, separate from bid payloads and telemetry.
///
/// Fields remain private so object-only deserialization and checked core
/// projection are the only constructors. Serialization includes only opaque
/// tokens, bounded counts, dimensions and durations, and allowlisted categories.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceAuctionEvidenceV1(
    #[serde(deserialize_with = "deserialize_object")] TraceAuctionEvidenceFields,
);

impl TraceAuctionEvidenceV1 {
    /// Borrow this observation's exact public auction token.
    ///
    /// # Examples
    ///
    /// ```
    /// # use trusted_server_core::trace::TraceAuctionEvidenceV1;
    /// fn token(evidence: &TraceAuctionEvidenceV1) -> &str {
    ///     evidence.diagnostic_auction_id().as_str()
    /// }
    /// ```
    #[must_use]
    pub fn diagnostic_auction_id(&self) -> &DiagnosticAuctionId {
        &self.0.diagnostic_auction_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TraceUnavailableReason {
    EvidenceProjectionFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum TraceTransportOutcome {
    Evidence {
        #[serde(deserialize_with = "deserialize_version")]
        schema_version: u8,
        evidence: TraceAuctionEvidenceV1,
    },
    Unavailable {
        #[serde(deserialize_with = "deserialize_version")]
        schema_version: u8,
        #[serde(deserialize_with = "deserialize_string_enum")]
        unavailable_reason: TraceUnavailableReason,
    },
}

/// An exclusive evidence or fixed projection-failure transport envelope.
///
/// The unavailable branch never includes raw input, errors, or partial evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceAuctionTransportV1(
    #[serde(deserialize_with = "deserialize_object")] TraceTransportOutcome,
);

impl TraceAuctionTransportV1 {
    /// Construct the fixed bounded projection-failure marker.
    ///
    /// # Examples
    ///
    /// ```
    /// let transport = trusted_server_core::trace::TraceAuctionTransportV1::unavailable();
    /// assert!(transport.evidence().is_none());
    /// ```
    #[must_use]
    pub fn unavailable() -> Self {
        Self(TraceTransportOutcome::Unavailable {
            schema_version: 1,
            unavailable_reason: TraceUnavailableReason::EvidenceProjectionFailed,
        })
    }

    /// Borrow complete evidence, absent when projection was unavailable.
    ///
    /// # Examples
    ///
    /// ```
    /// let transport = trusted_server_core::trace::TraceAuctionTransportV1::unavailable();
    /// assert!(transport.evidence().is_none());
    /// ```
    #[must_use]
    pub fn evidence(&self) -> Option<&TraceAuctionEvidenceV1> {
        match &self.0 {
            TraceTransportOutcome::Evidence { evidence, .. } => Some(evidence),
            TraceTransportOutcome::Unavailable { .. } => None,
        }
    }
}

impl From<TraceAuctionEvidenceV1> for TraceAuctionTransportV1 {
    fn from(evidence: TraceAuctionEvidenceV1) -> Self {
        Self(TraceTransportOutcome::Evidence {
            schema_version: 1,
            evidence,
        })
    }
}

struct UnsignedVisitor<const MAXIMUM: u32>;

impl<'de, const MAXIMUM: u32> Visitor<'de> for UnsignedVisitor<MAXIMUM> {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded nonnegative integer")
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        if value <= u64::from(MAXIMUM) {
            Ok(value)
        } else {
            Err(E::custom("invalid trace integer"))
        }
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        let value = u64::try_from(value).map_err(|_| E::custom("invalid trace integer"))?;
        self.visit_u64(value)
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        if !value.is_finite() || value < 0.0 || value > f64::from(MAXIMUM) || value.fract() != 0.0 {
            return Err(E::custom("invalid trace integer"));
        }
        // The accepted value is an exact integer within the u32 range.
        Ok(value as u64)
    }
}

fn deserialize_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let value = deserializer.deserialize_any(UnsignedVisitor::<{ u16::MAX as u32 }>)?;
    u16::try_from(value).map_err(|_| D::Error::custom("invalid trace count"))
}

fn deserialize_ordinal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let ordinal = deserialize_count(deserializer)?;
    if ordinal == 0 {
        return Err(D::Error::custom("invalid trace ordinal"));
    }
    Ok(ordinal)
}

fn deserialize_optional_duration<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    let value = deserializer.deserialize_any(UnsignedVisitor::<{ u32::MAX }>)?;
    u32::try_from(value)
        .map(Some)
        .map_err(|_| D::Error::custom("invalid trace duration"))
}

fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let version = deserializer.deserialize_any(UnsignedVisitor::<1>)?;
    if version != 1 {
        return Err(D::Error::custom("unsupported trace schema"));
    }
    Ok(1)
}

fn deserialize_present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

fn deserialize_string_enum<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    let value = String::deserialize(deserializer)?;
    T::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(value))
}

fn deserialize_optional_enum<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    deserialize_string_enum(deserializer).map(Some)
}

struct ObjectValue<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectValue<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_object(deserializer).map(Self)
    }
}

struct ObjectVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a trace object")
    }

    fn visit_map<A: MapAccess<'de>>(self, object: A) -> Result<T, A::Error> {
        T::deserialize(serde::de::value::MapAccessDeserializer::new(object))
    }
}

fn deserialize_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

struct BoundedSequence<T, const LIMIT: usize> {
    object_members: bool,
    marker: PhantomData<T>,
}

impl<'de, T: Deserialize<'de>, const LIMIT: usize> Visitor<'de> for BoundedSequence<T, LIMIT> {
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded trace array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(LIMIT));
        while let Some(value) = if self.object_members {
            sequence
                .next_element::<ObjectValue<T>>()?
                .map(|value| value.0)
        } else {
            sequence.next_element::<T>()?
        } {
            if values.len() == LIMIT {
                return Err(A::Error::custom("oversized trace array"));
            }
            values.push(value);
        }
        Ok(values)
    }
}

fn deserialize_providers<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TraceProviderCall>, D::Error> {
    deserializer.deserialize_seq(BoundedSequence::<TraceProviderCall, PROVIDER_LIMIT> {
        object_members: true,
        marker: PhantomData,
    })
}

fn deserialize_slots<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TraceAuctionSlot>, D::Error> {
    deserializer.deserialize_seq(BoundedSequence::<TraceAuctionSlot, SLOT_LIMIT> {
        object_members: true,
        marker: PhantomData,
    })
}

fn deserialize_sizes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TraceSize>, D::Error> {
    deserializer.deserialize_seq(BoundedSequence::<TraceSize, REQUESTED_SIZE_LIMIT> {
        object_members: false,
        marker: PhantomData,
    })
}

pub(crate) mod projection {
    use std::time::Duration;

    use error_stack::Report;

    use super::{
        DiagnosticAuctionId, MAXIMUM_DIMENSION, PROVIDER_LIMIT, ProviderToSlotNoBid,
        REQUESTED_SIZE_LIMIT, SLOT_LIMIT, TraceAuctionCoverage, TraceAuctionEvidenceFields,
        TraceAuctionEvidenceV1, TraceAuctionSlot, TraceAuctionSource, TraceAuctionTerminalReason,
        TraceAuctionTerminalStatus, TraceAuctionTransportV1, TraceAuctionTruncation,
        TraceProviderCall, TraceProviderRole, TraceProviderStatus, TraceSize, TraceSlotCandidate,
        TraceSlotRef,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct ObservedTraceProviderCall {
        pub(crate) provider_number: usize,
        pub(crate) role: TraceProviderRole,
        pub(crate) status: TraceProviderStatus,
        pub(crate) response_time: Option<Duration>,
        pub(crate) returned_bid_count: usize,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct ObservedTraceSlot {
        pub(crate) slot_number: usize,
        pub(crate) slot_ref: TraceSlotRef,
        pub(crate) requested_sizes: Vec<[u32; 2]>,
        pub(crate) returned_bid_count: usize,
        pub(crate) candidate: TraceSlotCandidate,
        pub(crate) selected_creative_size: Option<[u32; 2]>,
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(crate) struct ObservedTraceTruncation {
        pub(crate) omitted_provider_calls: usize,
        pub(crate) omitted_slots: usize,
        pub(crate) omitted_nested_values: usize,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct ObservedTraceAuction {
        pub(crate) diagnostic_auction_id: DiagnosticAuctionId,
        pub(crate) source: TraceAuctionSource,
        pub(crate) terminal_status: TraceAuctionTerminalStatus,
        pub(crate) terminal_reason: Option<TraceAuctionTerminalReason>,
        pub(crate) total_time: Option<Duration>,
        pub(crate) provider_calls: Vec<ObservedTraceProviderCall>,
        pub(crate) slots: Vec<ObservedTraceSlot>,
        pub(crate) truncation: ObservedTraceTruncation,
    }

    #[derive(Debug, derive_more::Display)]
    #[display("trace evidence projection failed")]
    struct TraceEvidenceProjectionError;

    impl core::error::Error for TraceEvidenceProjectionError {}

    pub(crate) fn project_auction_transport(
        observed: &ObservedTraceAuction,
    ) -> TraceAuctionTransportV1 {
        match project_auction_evidence(observed) {
            Ok(evidence) => TraceAuctionTransportV1::from(evidence),
            Err(_) => TraceAuctionTransportV1::unavailable(),
        }
    }

    fn project_auction_evidence(
        observed: &ObservedTraceAuction,
    ) -> Result<TraceAuctionEvidenceV1, Report<TraceEvidenceProjectionError>> {
        // Required invalid source facts reject evidence even beyond its limits.
        for provider in &observed.provider_calls {
            checked_ordinal(provider.provider_number)?;
            checked_count(provider.returned_bid_count)?;
        }
        for slot in &observed.slots {
            checked_ordinal(slot.slot_number)?;
            checked_count(slot.returned_bid_count)?;
        }
        let mut truncation = TraceAuctionTruncation {
            omitted_provider_calls: checked_count(observed.truncation.omitted_provider_calls)?,
            omitted_slots: checked_count(observed.truncation.omitted_slots)?,
            omitted_nested_values: checked_count(observed.truncation.omitted_nested_values)?,
        };
        let retained_providers = observed.provider_calls.len().min(PROVIDER_LIMIT);
        add_omissions(
            &mut truncation.omitted_provider_calls,
            observed.provider_calls.len() - retained_providers,
        )?;
        let retained_slots = observed.slots.len().min(SLOT_LIMIT);
        add_omissions(
            &mut truncation.omitted_slots,
            observed.slots.len() - retained_slots,
        )?;
        let total_time_ms =
            project_duration(observed.total_time, &mut truncation.omitted_nested_values)?;

        let mut provider_calls = Vec::with_capacity(retained_providers);
        for provider in observed.provider_calls.iter().take(PROVIDER_LIMIT) {
            provider_calls.push(TraceProviderCall {
                provider_number: checked_ordinal(provider.provider_number)?,
                role: provider.role,
                status: provider.status,
                response_time_ms: project_duration(
                    provider.response_time,
                    &mut truncation.omitted_nested_values,
                )?,
                returned_bid_count: checked_count(provider.returned_bid_count)?,
            });
        }

        let mut slots = Vec::with_capacity(retained_slots);
        for slot in observed.slots.iter().take(SLOT_LIMIT) {
            let retained_sizes = slot.requested_sizes.len().min(REQUESTED_SIZE_LIMIT);
            add_omissions(
                &mut truncation.omitted_nested_values,
                slot.requested_sizes.len() - retained_sizes,
            )?;
            let mut requested_sizes = Vec::with_capacity(retained_sizes);
            for size in slot.requested_sizes.iter().take(REQUESTED_SIZE_LIMIT) {
                if let Some(size) =
                    project_size(Some(*size), &mut truncation.omitted_nested_values)?
                {
                    requested_sizes.push(size);
                }
            }
            slots.push(TraceAuctionSlot {
                slot_number: checked_ordinal(slot.slot_number)?,
                slot_ref: slot.slot_ref.clone(),
                requested_sizes,
                returned_bid_count: checked_count(slot.returned_bid_count)?,
                candidate: slot.candidate,
                selected_creative_size: project_size(
                    slot.selected_creative_size,
                    &mut truncation.omitted_nested_values,
                )?,
            });
        }

        Ok(TraceAuctionEvidenceV1(TraceAuctionEvidenceFields {
            schema_version: 1,
            diagnostic_auction_id: observed.diagnostic_auction_id.clone(),
            source: observed.source,
            terminal_status: observed.terminal_status,
            terminal_reason: observed.terminal_reason,
            total_time_ms,
            provider_calls,
            slots,
            truncation,
            coverage: TraceAuctionCoverage {
                provider_to_slot_no_bid: ProviderToSlotNoBid::Unavailable,
            },
        }))
    }

    fn checked_count(count: usize) -> Result<u16, Report<TraceEvidenceProjectionError>> {
        u16::try_from(count).map_err(|_| Report::new(TraceEvidenceProjectionError))
    }

    fn checked_ordinal(ordinal: usize) -> Result<u16, Report<TraceEvidenceProjectionError>> {
        let ordinal = checked_count(ordinal)?;
        if ordinal == 0 {
            return Err(Report::new(TraceEvidenceProjectionError));
        }
        Ok(ordinal)
    }

    fn add_omissions(
        counter: &mut u16,
        additional: usize,
    ) -> Result<(), Report<TraceEvidenceProjectionError>> {
        let additional = checked_count(additional)?;
        *counter = counter
            .checked_add(additional)
            .ok_or_else(|| Report::new(TraceEvidenceProjectionError))?;
        Ok(())
    }

    fn project_duration(
        duration: Option<Duration>,
        omissions: &mut u16,
    ) -> Result<Option<u32>, Report<TraceEvidenceProjectionError>> {
        let Some(duration) = duration else {
            return Ok(None);
        };
        match u32::try_from(duration.as_millis()) {
            Ok(milliseconds) => Ok(Some(milliseconds)),
            Err(_) => {
                add_omissions(omissions, 1)?;
                Ok(None)
            }
        }
    }

    fn project_size(
        size: Option<[u32; 2]>,
        omissions: &mut u16,
    ) -> Result<Option<TraceSize>, Report<TraceEvidenceProjectionError>> {
        let Some(size) = size else {
            return Ok(None);
        };
        if size
            .iter()
            .all(|dimension| (1..=MAXIMUM_DIMENSION).contains(dimension))
        {
            Ok(Some(TraceSize(size)))
        } else {
            add_omissions(omissions, 1)?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::time::Duration;

    use super::projection::{
        ObservedTraceAuction, ObservedTraceProviderCall, ObservedTraceSlot,
        ObservedTraceTruncation, project_auction_transport,
    };

    const AUCTION_TOKEN: &str = "ts-auc-00000000000040008000000000000000";
    const SLOT_TOKEN: &str = "ts-slot-00000000-0000-4000-8000-000000000000";

    #[test]
    fn trace_auction_tokens_accept_exact_v4_rfc_variants() {
        for variant in ['8', '9', 'a', 'b'] {
            let auction = format!("ts-auc-0123456789ab4def{variant}123456789abcdef");
            let slot = format!("ts-slot-01234567-89ab-4def-{variant}123-456789abcdef");
            assert_eq!(
                DiagnosticAuctionId::parse(&auction)
                    .expect("should accept exact auction token")
                    .as_str(),
                auction,
                "should retain exact auction bytes"
            );
            assert_eq!(
                TraceSlotRef::parse(&slot)
                    .expect("should accept exact slot token")
                    .as_str(),
                slot,
                "should retain exact slot bytes"
            );
        }
    }

    #[test]
    fn trace_auction_tokens_reject_normalized_and_internal_alternatives() {
        for invalid in [
            "",
            "00000000-0000-4000-8000-000000000000",
            "ts-auc-00000000-0000-4000-8000-000000000000",
            "ts-auc-00000000000010008000000000000000",
            "ts-auc-00000000000040007000000000000000",
            "ts-auc-0000000000004000c000000000000000",
            "ts-auc-0000000000004000A000000000000000",
            "ts-auc-0000000000004000800000000000000g",
            " ts-auc-00000000000040008000000000000000",
            "ts-auc-00000000000040008000000000000000\n",
            "TS-AUC-00000000000040008000000000000000",
            "ts-auc-00000000000040008000000000000000\u{202e}",
        ] {
            assert!(
                DiagnosticAuctionId::parse(invalid).is_err(),
                "should reject invalid auction token"
            );
        }
        for invalid in [
            "",
            AUCTION_TOKEN,
            "ts-slot-00000000000040008000000000000000",
            "ts-slot-00000000-0000-1000-8000-000000000000",
            "ts-slot-00000000-0000-4000-7000-000000000000",
            "ts-slot-00000000-0000-4000-c000-000000000000",
            "ts-slot-00000000-0000-4000-A000-000000000000",
            "ts-slot-00000000-0000-4000-8000-00000000000g",
            " ts-slot-00000000-0000-4000-8000-000000000000",
            "ts-slot-00000000-0000-4000-8000-000000000000\n",
            "TS-SLOT-00000000-0000-4000-8000-000000000000",
            "ts-slot-00000000-0000-4000-8000-000000000000\u{202e}",
        ] {
            assert!(
                TraceSlotRef::parse(invalid).is_err(),
                "should reject invalid slot reference"
            );
        }
    }

    #[test]
    fn trace_auction_tokens_generate_distinct_exact_owned_values() {
        let auctions = [
            DiagnosticAuctionId::generate(),
            DiagnosticAuctionId::generate(),
        ];
        let slots = [TraceSlotRef::generate(), TraceSlotRef::generate()];
        assert_ne!(
            auctions[0], auctions[1],
            "should generate fresh auction IDs"
        );
        assert_ne!(slots[0], slots[1], "should generate fresh slot references");
        for auction in auctions {
            assert_eq!(
                auction.as_str().len(),
                39,
                "should retain simple UUID shape"
            );
            assert_eq!(
                DiagnosticAuctionId::parse(auction.as_str())
                    .expect("should validate generated auction token"),
                auction,
                "should round-trip exact auction bytes"
            );
        }
        for slot in slots {
            assert_eq!(
                slot.as_str().len(),
                44,
                "should retain hyphenated UUID shape"
            );
            assert_eq!(
                TraceSlotRef::parse(slot.as_str()).expect("should validate generated slot ref"),
                slot,
                "should round-trip exact slot bytes"
            );
        }
    }

    #[test]
    fn trace_auction_tokens_use_strict_string_serde() {
        let auction =
            DiagnosticAuctionId::parse(AUCTION_TOKEN).expect("should accept exact auction token");
        let slot = TraceSlotRef::parse(SLOT_TOKEN).expect("should accept exact slot token");
        assert_eq!(
            json!(auction),
            json!(AUCTION_TOKEN),
            "should serialize only opaque bytes"
        );
        assert_eq!(
            json!(slot),
            json!(SLOT_TOKEN),
            "should serialize only opaque bytes"
        );
        for value in [
            json!(null),
            json!(1),
            json!({}),
            json!([]),
            json!("internal-example-id"),
        ] {
            assert!(
                serde_json::from_value::<DiagnosticAuctionId>(value.clone()).is_err(),
                "should reject invalid auction JSON"
            );
            assert!(
                serde_json::from_value::<TraceSlotRef>(value).is_err(),
                "should reject invalid slot JSON"
            );
        }
    }

    fn evidence_json() -> serde_json::Value {
        json!({
            "schema_version": 1,
            "diagnostic_auction_id": AUCTION_TOKEN,
            "source": "initial_navigation_ssat",
            "terminal_status": "completed",
            "terminal_reason": "unknown",
            "total_time_ms": 10,
            "provider_calls": [{
                "provider_number": 1,
                "role": "bidder",
                "status": "success",
                "response_time_ms": 5,
                "returned_bid_count": 2
            }],
            "slots": [{
                "slot_number": 1,
                "slot_ref": SLOT_TOKEN,
                "requested_sizes": [[300, 250]],
                "returned_bid_count": 2,
                "candidate": "selected",
                "selected_creative_size": [300, 250]
            }],
            "truncation": {
                "omitted_provider_calls": 0,
                "omitted_slots": 0,
                "omitted_nested_values": 0
            },
            "coverage": {"provider_to_slot_no_bid": "unavailable"}
        })
    }

    fn assert_invalid_evidence(value: serde_json::Value) {
        assert!(
            serde_json::from_value::<TraceAuctionEvidenceV1>(value).is_err(),
            "should reject invalid public auction evidence"
        );
    }

    #[test]
    fn trace_auction_evidence_round_trips_the_exact_owned_schema() {
        let input = evidence_json();
        let original = input.clone();
        let evidence = serde_json::from_value::<TraceAuctionEvidenceV1>(input)
            .expect("should accept complete evidence");
        assert_eq!(
            json!(evidence),
            original,
            "should preserve only exact schema fields"
        );
        assert_eq!(
            evidence.diagnostic_auction_id().as_str(),
            AUCTION_TOKEN,
            "should expose the checked correlation token"
        );
        let transport = TraceAuctionTransportV1::from(evidence);
        assert_eq!(
            json!(transport),
            json!({"schema_version":1,"evidence":original}),
            "should use exclusive evidence envelope"
        );
        assert!(
            transport.evidence().is_some(),
            "should expose available evidence"
        );
    }

    #[test]
    fn trace_auction_evidence_rejects_unknown_keys_at_every_boundary() {
        for pointer in [
            "",
            "/provider_calls/0",
            "/slots/0",
            "/truncation",
            "/coverage",
        ] {
            let mut input = evidence_json();
            input
                .pointer_mut(pointer)
                .expect("should find the schema object")
                .as_object_mut()
                .expect("should expose the schema object")
                .insert("future_example_field".to_owned(), json!("example"));
            assert_invalid_evidence(input);
        }
    }

    #[test]
    fn trace_auction_evidence_accepts_only_documented_enum_members() {
        for (pointer, members) in [
            (
                "/source",
                &["initial_navigation_ssat", "spa_page_bids", "auction_api"][..],
            ),
            (
                "/terminal_status",
                &[
                    "completed",
                    "execution_failed",
                    "dispatch_failed",
                    "abandoned",
                    "skipped",
                ][..],
            ),
            (
                "/terminal_reason",
                &[
                    "policy_skipped",
                    "no_eligible_slots",
                    "no_provider_launched",
                    "provider_execution_failed",
                    "collection_failed",
                    "unknown",
                ][..],
            ),
            (
                "/provider_calls/0/role",
                &["bidder", "mediator", "unknown"][..],
            ),
            (
                "/provider_calls/0/status",
                &[
                    "success",
                    "no_bid",
                    "error",
                    "pending",
                    "abandoned",
                    "unknown",
                ][..],
            ),
            (
                "/slots/0/candidate",
                &[
                    "selected",
                    "no_candidate",
                    "selected_unrenderable",
                    "unknown",
                ][..],
            ),
        ] {
            for member in members {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find enum member") = json!(member);
                let accepted = serde_json::from_value::<TraceAuctionEvidenceV1>(input.clone())
                    .expect("should accept documented enum member");
                assert_eq!(json!(accepted), input, "should preserve exact enum bytes");
            }
            for invalid in [
                json!("future_example_member"),
                json!("UNKNOWN"),
                json!("unknown "),
                json!("\u{202e}unknown"),
                json!(null),
                json!(0),
                json!({}),
            ] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find enum member") = invalid;
                assert_invalid_evidence(input);
            }
        }
    }

    #[test]
    fn trace_auction_evidence_requires_primitive_strings_for_enum_fields() {
        for (pointer, member) in [
            ("/source", "initial_navigation_ssat"),
            ("/terminal_status", "completed"),
            ("/terminal_reason", "unknown"),
            ("/provider_calls/0/role", "bidder"),
            ("/provider_calls/0/status", "success"),
            ("/slots/0/candidate", "selected"),
            ("/coverage/provider_to_slot_no_bid", "unavailable"),
        ] {
            let mut input = evidence_json();
            *input.pointer_mut(pointer).expect("should find enum field") = json!({(member): null});
            let encoded =
                serde_json::to_string(&input).expect("should serialize object-form enum fixture");
            assert!(
                serde_json::from_str::<TraceAuctionEvidenceV1>(&encoded).is_err(),
                "should reject object-form enum from JSON bytes"
            );
            assert_invalid_evidence(input);
        }
    }

    #[test]
    fn trace_auction_evidence_requires_objects_at_every_object_boundary() {
        let original = evidence_json();
        for (pointer, alternative) in [
            (
                "",
                json!([
                    1,
                    AUCTION_TOKEN,
                    "initial_navigation_ssat",
                    "completed",
                    "unknown",
                    10,
                    original["provider_calls"],
                    original["slots"],
                    original["truncation"],
                    original["coverage"]
                ]),
            ),
            ("/provider_calls/0", json!([1, "bidder", "success", 5, 2])),
            (
                "/slots/0",
                json!([1, SLOT_TOKEN, [[300, 250]], 2, "selected", [300, 250]]),
            ),
            ("/truncation", json!([0, 0, 0])),
            ("/coverage", json!(["unavailable"])),
        ] {
            let mut input = original.clone();
            *input
                .pointer_mut(pointer)
                .expect("should find object boundary") = alternative;
            let encoded =
                serde_json::to_string(&input).expect("should encode positional-array fixture");
            assert!(
                serde_json::from_str::<TraceAuctionEvidenceV1>(&encoded).is_err(),
                "should reject positional arrays from JSON bytes"
            );
            assert_invalid_evidence(input);
        }
    }

    #[test]
    fn trace_auction_evidence_rejects_null_optional_and_missing_required_fields() {
        for pointer in [
            "/terminal_reason",
            "/total_time_ms",
            "/provider_calls/0/response_time_ms",
            "/slots/0/selected_creative_size",
        ] {
            let mut input = evidence_json();
            *input
                .pointer_mut(pointer)
                .expect("should find optional field") = json!(null);
            assert_invalid_evidence(input);
        }
        for pointer in [
            "",
            "/provider_calls/0",
            "/slots/0",
            "/truncation",
            "/coverage",
        ] {
            let source = evidence_json();
            let names: Vec<_> = source
                .pointer(pointer)
                .expect("should find required object")
                .as_object()
                .expect("should expose required object")
                .keys()
                .filter(|name| {
                    !matches!(
                        name.as_str(),
                        "terminal_reason"
                            | "total_time_ms"
                            | "response_time_ms"
                            | "selected_creative_size"
                    )
                })
                .cloned()
                .collect();
            for name in names {
                let mut input = source.clone();
                input
                    .pointer_mut(pointer)
                    .expect("should find required object")
                    .as_object_mut()
                    .expect("should expose required object")
                    .remove(&name);
                assert_invalid_evidence(input);
            }
        }
        let mut input = evidence_json();
        input
            .as_object_mut()
            .expect("should expose evidence")
            .remove("terminal_reason");
        input
            .as_object_mut()
            .expect("should expose evidence")
            .remove("total_time_ms");
        input["provider_calls"][0]
            .as_object_mut()
            .expect("should expose provider")
            .remove("response_time_ms");
        input["slots"][0]
            .as_object_mut()
            .expect("should expose slot")
            .remove("selected_creative_size");
        let accepted = serde_json::from_value::<TraceAuctionEvidenceV1>(input.clone())
            .expect("should accept absent optional facts");
        assert_eq!(
            json!(accepted),
            input,
            "should leave unsupported optional facts absent"
        );
    }

    #[test]
    fn trace_auction_evidence_enforces_exact_cardinality_limits() {
        for (pointer, limit) in [
            ("/provider_calls", 16),
            ("/slots", 64),
            ("/slots/0/requested_sizes", 16),
        ] {
            let mut input = evidence_json();
            let member = input.pointer(pointer).expect("should find bounded array")[0].clone();
            *input
                .pointer_mut(pointer)
                .expect("should find bounded array") = json!(vec![member.clone(); limit]);
            assert!(
                serde_json::from_value::<TraceAuctionEvidenceV1>(input.clone()).is_ok(),
                "should accept exact array limit"
            );
            input
                .pointer_mut(pointer)
                .expect("should find bounded array")
                .as_array_mut()
                .expect("should expose array")
                .push(member);
            assert_invalid_evidence(input);
        }
        let mut empty = evidence_json();
        empty["provider_calls"] = json!([]);
        empty["slots"] = json!([]);
        assert!(
            serde_json::from_value::<TraceAuctionEvidenceV1>(empty).is_ok(),
            "should accept completed zero-bid evidence"
        );
    }

    #[test]
    fn trace_auction_evidence_requires_positive_u16_ordinals_and_u16_counts() {
        for pointer in ["/provider_calls/0/provider_number", "/slots/0/slot_number"] {
            for valid in [1_u32, u32::from(u16::MAX)] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find ordinal") = json!(valid);
                assert!(
                    serde_json::from_value::<TraceAuctionEvidenceV1>(input).is_ok(),
                    "should accept positive u16 ordinal"
                );
            }
            for invalid in [
                json!(0),
                json!(-1),
                json!(65536),
                json!(1.5),
                json!("1"),
                json!(null),
            ] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find ordinal") = invalid;
                assert_invalid_evidence(input);
            }
        }
        for pointer in [
            "/provider_calls/0/returned_bid_count",
            "/slots/0/returned_bid_count",
            "/truncation/omitted_provider_calls",
            "/truncation/omitted_slots",
            "/truncation/omitted_nested_values",
        ] {
            for valid in [0_u32, u32::from(u16::MAX)] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find count") = json!(valid);
                assert!(
                    serde_json::from_value::<TraceAuctionEvidenceV1>(input).is_ok(),
                    "should accept u16 count"
                );
            }
            for invalid in [
                json!(-1),
                json!(65536),
                json!(1.5),
                json!("0"),
                json!(null),
                json!(9007199254740992_u64),
            ] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find count") = invalid;
                assert_invalid_evidence(input);
            }
        }
    }

    #[test]
    fn trace_auction_evidence_requires_bounded_durations_dimensions_version_and_coverage() {
        for pointer in ["/total_time_ms", "/provider_calls/0/response_time_ms"] {
            for valid in [0_u64, u64::from(u32::MAX)] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find duration") = json!(valid);
                assert!(
                    serde_json::from_value::<TraceAuctionEvidenceV1>(input).is_ok(),
                    "should accept u32 duration"
                );
            }
            for invalid in [
                json!(-1),
                json!(4294967296_u64),
                json!(0.5),
                json!("0"),
                json!(null),
            ] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find duration") = invalid;
                assert_invalid_evidence(input);
            }
        }
        for pointer in [
            "/slots/0/requested_sizes/0",
            "/slots/0/selected_creative_size",
        ] {
            for valid in [[1, 1], [100000, 100000]] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find dimensions") = json!(valid);
                assert!(
                    serde_json::from_value::<TraceAuctionEvidenceV1>(input).is_ok(),
                    "should accept dimension bounds"
                );
            }
            for invalid in [
                json!([0, 1]),
                json!([1, 0]),
                json!([100001, 1]),
                json!([1, 100001]),
                json!([1]),
                json!([1, 1, 1]),
                json!([-1, 1]),
                json!([1.5, 1]),
                json!(["1", 1]),
                json!(null),
            ] {
                let mut input = evidence_json();
                *input.pointer_mut(pointer).expect("should find dimensions") = invalid;
                assert_invalid_evidence(input);
            }
        }
        for invalid in [json!(0), json!(2), json!("1"), json!(null), json!(1.5)] {
            let mut input = evidence_json();
            input["schema_version"] = invalid;
            assert_invalid_evidence(input);
        }
        for invalid in [
            json!("available"),
            json!("Unavailable"),
            json!(null),
            json!(0),
        ] {
            let mut input = evidence_json();
            input["coverage"]["provider_to_slot_no_bid"] = invalid;
            assert_invalid_evidence(input);
        }
    }

    #[test]
    fn trace_auction_evidence_accepts_integral_json_numbers_without_syntax_coercion() {
        let mut input = evidence_json();
        for pointer in [
            "/schema_version",
            "/total_time_ms",
            "/provider_calls/0/provider_number",
            "/provider_calls/0/returned_bid_count",
            "/provider_calls/0/response_time_ms",
            "/slots/0/slot_number",
            "/slots/0/returned_bid_count",
            "/truncation/omitted_slots",
        ] {
            *input
                .pointer_mut(pointer)
                .expect("should find bounded integer") = json!(1.0);
        }
        input["slots"][0]["requested_sizes"] = json!([[1.0, 100000.0]]);
        input["slots"][0]["selected_creative_size"] = json!([1.0, 100000.0]);
        assert!(
            serde_json::from_value::<TraceAuctionEvidenceV1>(input.clone()).is_ok(),
            "should accept finite integral JSON values like the browser"
        );
        let scientific = serde_json::to_string(&input)
            .expect("should serialize numeric fixture")
            .replace("1.0", "1e0");
        assert!(
            serde_json::from_str::<TraceAuctionEvidenceV1>(&scientific).is_ok(),
            "should accept integral scientific notation"
        );
    }

    #[test]
    fn trace_auction_transport_is_exclusive_and_rejects_extra_keys() {
        let marker = TraceAuctionTransportV1::unavailable();
        assert_eq!(
            json!(marker),
            json!({"schema_version":1,"unavailable_reason":"evidence_projection_failed"}),
            "should expose only fixed bounded projection failure"
        );
        assert!(
            marker.evidence().is_none(),
            "should distinguish failed projection"
        );
        for input in [
            json!({"schema_version":1}),
            json!({"schema_version":1,"evidence":evidence_json(),"unavailable_reason":"evidence_projection_failed"}),
            json!({"schema_version":1,"evidence":null}),
            json!({"schema_version":2,"evidence":evidence_json()}),
            json!({"schema_version":1,"unavailable_reason":"example raw provider failure"}),
            json!({"schema_version":1,"unavailable_reason":"evidence_projection_failed","future_example_field":0}),
            json!({"schema_version":1,"evidence":evidence_json(),"future_example_field":0}),
        ] {
            assert!(
                serde_json::from_value::<TraceAuctionTransportV1>(input).is_err(),
                "should reject malformed whole envelope"
            );
        }
        for input in [
            json!(marker),
            json!({"schema_version":1,"evidence":evidence_json()}),
        ] {
            let accepted = serde_json::from_value::<TraceAuctionTransportV1>(input.clone())
                .expect("should accept exactly one envelope branch");
            assert_eq!(json!(accepted), input, "should retain exact branch schema");
        }
    }

    #[test]
    fn trace_auction_transport_marker_requires_primitive_string_reason() {
        let input = json!({
            "schema_version":1,
            "unavailable_reason":{"evidence_projection_failed":null}
        });
        let encoded =
            serde_json::to_string(&input).expect("should serialize object-form marker fixture");
        assert!(
            serde_json::from_str::<TraceAuctionTransportV1>(&encoded).is_err(),
            "should reject object-form marker from JSON bytes"
        );
        assert!(
            serde_json::from_value::<TraceAuctionTransportV1>(input).is_err(),
            "should reject object-form marker from a value"
        );
    }

    #[test]
    fn trace_auction_transport_requires_an_object_envelope() {
        for input in [
            json!([1, evidence_json()]),
            json!([1, "evidence_projection_failed"]),
        ] {
            let encoded =
                serde_json::to_string(&input).expect("should encode positional transport fixture");
            assert!(
                serde_json::from_str::<TraceAuctionTransportV1>(&encoded).is_err(),
                "should reject positional transport from JSON bytes"
            );
            assert!(
                serde_json::from_value::<TraceAuctionTransportV1>(input).is_err(),
                "should reject positional transport from a value"
            );
        }
    }

    #[test]
    fn trace_auction_evidence_rejects_duplicate_members_and_invalid_unicode() {
        let evidence = serde_json::to_string(&evidence_json()).expect("should serialize fixture");
        let duplicate = evidence.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        );
        assert!(
            serde_json::from_str::<TraceAuctionEvidenceV1>(&duplicate).is_err(),
            "should reject duplicate schema members"
        );
        let duplicate = evidence.replacen(
            "\"provider_number\":1",
            "\"provider_number\":1,\"provider_number\":1",
            1,
        );
        assert!(
            serde_json::from_str::<TraceAuctionEvidenceV1>(&duplicate).is_err(),
            "should reject duplicate provider members"
        );
        let invalid_unicode = evidence.replace(AUCTION_TOKEN, "\\ud800");
        assert!(
            serde_json::from_str::<TraceAuctionEvidenceV1>(&invalid_unicode).is_err(),
            "should reject invalid Unicode strings"
        );
        for invalid in [
            "example".repeat(30),
            format!("{AUCTION_TOKEN}\u{0085}"),
            format!("{SLOT_TOKEN}\u{2066}"),
        ] {
            let mut input = evidence_json();
            input["diagnostic_auction_id"] = json!(invalid);
            assert_invalid_evidence(input);
        }
    }

    fn observed_auction() -> ObservedTraceAuction {
        ObservedTraceAuction {
            diagnostic_auction_id: DiagnosticAuctionId::parse(AUCTION_TOKEN)
                .expect("should parse observation token"),
            source: TraceAuctionSource::InitialNavigationSsat,
            terminal_status: TraceAuctionTerminalStatus::Completed,
            terminal_reason: Some(TraceAuctionTerminalReason::Unknown),
            total_time: Some(Duration::from_millis(10)),
            provider_calls: vec![ObservedTraceProviderCall {
                provider_number: 1,
                role: TraceProviderRole::Bidder,
                status: TraceProviderStatus::Success,
                response_time: Some(Duration::from_millis(5)),
                returned_bid_count: 2,
            }],
            slots: vec![ObservedTraceSlot {
                slot_number: 1,
                slot_ref: TraceSlotRef::parse(SLOT_TOKEN)
                    .expect("should parse observation slot ref"),
                requested_sizes: vec![[300, 250]],
                returned_bid_count: 2,
                candidate: TraceSlotCandidate::Selected,
                selected_creative_size: Some([300, 250]),
            }],
            truncation: ObservedTraceTruncation::default(),
        }
    }

    #[test]
    fn trace_auction_projection_copies_exact_public_facts_without_mutation() {
        let observed = observed_auction();
        let original = observed.clone();
        let projected = project_auction_transport(&observed);
        assert_eq!(
            json!(projected),
            json!({"schema_version":1,"evidence":evidence_json()}),
            "should project the closed public model"
        );
        assert_eq!(
            observed, original,
            "should leave caller-owned facts unchanged"
        );
        let validated = serde_json::from_value::<TraceAuctionTransportV1>(json!(projected))
            .expect("should validate core-produced transport");
        assert_eq!(
            validated, projected,
            "should produce the same strict contract accepted at ingress"
        );
    }

    #[test]
    fn trace_auction_projection_retains_order_and_counts_all_bounded_discards() {
        let mut observed = observed_auction();
        let provider = observed.provider_calls[0].clone();
        observed.provider_calls = (1..=18)
            .map(|provider_number| ObservedTraceProviderCall {
                provider_number,
                ..provider.clone()
            })
            .collect();
        let mut slot = observed.slots[0].clone();
        slot.requested_sizes = (1..=20).map(|dimension| [dimension, 250]).collect();
        observed.slots = (1..=66)
            .map(|slot_number| ObservedTraceSlot {
                slot_number,
                slot_ref: TraceSlotRef::generate(),
                ..slot.clone()
            })
            .collect();
        observed.truncation = ObservedTraceTruncation {
            omitted_provider_calls: 3,
            omitted_slots: 4,
            omitted_nested_values: 5,
        };
        let original = observed.clone();
        let projected = project_auction_transport(&observed);
        let evidence = json!(projected.evidence().expect("should project bounded facts"));
        assert_eq!(
            evidence["provider_calls"]
                .as_array()
                .expect("should expose providers")
                .len(),
            16,
            "should retain first provider calls"
        );
        assert_eq!(
            evidence["slots"]
                .as_array()
                .expect("should expose slots")
                .len(),
            64,
            "should retain first definitive slots"
        );
        for (index, provider) in evidence["provider_calls"]
            .as_array()
            .expect("should expose providers")
            .iter()
            .enumerate()
        {
            assert_eq!(
                provider["provider_number"],
                json!(index + 1),
                "should preserve launch order"
            );
        }
        for (index, slot) in evidence["slots"]
            .as_array()
            .expect("should expose slots")
            .iter()
            .enumerate()
        {
            assert_eq!(
                slot["slot_number"],
                json!(index + 1),
                "should preserve definitive slot order"
            );
            assert_eq!(
                slot["slot_ref"],
                json!(observed.slots[index].slot_ref),
                "should preserve the exact accepted token association"
            );
            assert_eq!(
                slot["requested_sizes"],
                json!(
                    (1..=16)
                        .map(|dimension| [dimension, 250])
                        .collect::<Vec<_>>()
                ),
                "should retain first requested sizes"
            );
        }
        assert_eq!(
            evidence["truncation"],
            json!({"omitted_provider_calls":5,"omitted_slots":6,"omitted_nested_values":261}),
            "should add every omitted retained-slot size with checked counters"
        );
        assert_eq!(
            observed, original,
            "should preserve all ordinary caller work and facts"
        );
    }

    #[test]
    fn trace_auction_projection_omits_optional_overflow_and_invalid_sizes_with_counts() {
        let mut observed = observed_auction();
        observed.total_time = Some(Duration::MAX);
        observed.provider_calls[0].response_time =
            Some(Duration::from_millis(u64::from(u32::MAX) + 1));
        observed.slots[0].requested_sizes = vec![
            [0, 250],
            [300, 0],
            [100001, 250],
            [300, 100001],
            [1, 1],
            [100000, 100000],
        ];
        observed.slots[0].selected_creative_size = Some([0, 250]);
        let projected = project_auction_transport(&observed);
        let evidence = json!(
            projected
                .evidence()
                .expect("should preserve required facts")
        );
        assert!(
            evidence.get("total_time_ms").is_none(),
            "should omit unrepresentable monotonic duration"
        );
        assert!(
            evidence["provider_calls"][0]
                .get("response_time_ms")
                .is_none(),
            "should omit provider duration overflow"
        );
        assert!(
            evidence["slots"][0].get("selected_creative_size").is_none(),
            "should omit invalid optional creative dimensions"
        );
        assert_eq!(
            evidence["slots"][0]["requested_sizes"],
            json!([[1, 1], [100000, 100000]]),
            "should omit invalid optional requested sizes without normalizing"
        );
        assert_eq!(
            evidence["truncation"]["omitted_nested_values"],
            json!(7),
            "should count every unsupported optional fact"
        );
        assert_eq!(
            evidence["slots"][0]["candidate"],
            json!("selected"),
            "should preserve observed candidate disposition"
        );
    }

    #[test]
    fn trace_auction_projection_keeps_exact_numeric_boundaries_and_zero_durations() {
        let mut observed = observed_auction();
        observed.total_time = Some(Duration::ZERO);
        observed.provider_calls[0].response_time = Some(Duration::from_millis(u64::from(u32::MAX)));
        observed.provider_calls[0].provider_number = usize::from(u16::MAX);
        observed.provider_calls[0].returned_bid_count = usize::from(u16::MAX);
        observed.slots[0].slot_number = usize::from(u16::MAX);
        observed.slots[0].returned_bid_count = usize::from(u16::MAX);
        observed.slots[0].requested_sizes = vec![[1, 1], [100000, 100000]];
        observed.slots[0].selected_creative_size = Some([100000, 100000]);
        let projected = project_auction_transport(&observed);
        let evidence = json!(projected.evidence().expect("should preserve exact bounds"));
        assert_eq!(
            evidence["total_time_ms"],
            json!(0),
            "should preserve zero monotonic duration"
        );
        assert_eq!(
            evidence["provider_calls"][0]["response_time_ms"],
            json!(u32::MAX),
            "should preserve maximum duration without clamping"
        );
        assert_eq!(
            evidence["provider_calls"][0]["provider_number"],
            json!(u16::MAX),
            "should preserve maximum ordinal"
        );
        assert_eq!(
            evidence["slots"][0]["returned_bid_count"],
            json!(u16::MAX),
            "should preserve maximum bid count"
        );
        assert_eq!(
            evidence["truncation"],
            json!({"omitted_provider_calls":0,"omitted_slots":0,"omitted_nested_values":0}),
            "should not count representable values as omissions"
        );
    }

    #[test]
    fn trace_auction_projection_required_conversion_failure_emits_only_marker() {
        for field in [
            "provider_number",
            "provider_count",
            "slot_number",
            "slot_count",
        ] {
            for invalid in [usize::from(u16::MAX) + 1, usize::MAX] {
                let mut observed = observed_auction();
                match field {
                    "provider_number" => observed.provider_calls[0].provider_number = invalid,
                    "provider_count" => observed.provider_calls[0].returned_bid_count = invalid,
                    "slot_number" => observed.slots[0].slot_number = invalid,
                    "slot_count" => observed.slots[0].returned_bid_count = invalid,
                    _ => panic!("should use the documented test field"),
                }
                assert_eq!(
                    json!(project_auction_transport(&observed)),
                    json!(TraceAuctionTransportV1::unavailable()),
                    "should reject required conversion without exposing partial facts"
                );
            }
        }
        for provider_ordinal in [true, false] {
            let mut observed = observed_auction();
            if provider_ordinal {
                observed.provider_calls[0].provider_number = 0;
            } else {
                observed.slots[0].slot_number = 0;
            }
            assert!(
                project_auction_transport(&observed).evidence().is_none(),
                "should reject zero ordinal"
            );
        }
    }

    #[test]
    fn trace_auction_projection_rejects_required_invalid_tail_facts() {
        for field in [
            "provider_number",
            "provider_count",
            "slot_number",
            "slot_count",
        ] {
            let mut observed = observed_auction();
            observed.provider_calls = vec![observed.provider_calls[0].clone(); 17];
            observed.slots = vec![observed.slots[0].clone(); 65];
            match field {
                "provider_number" => observed.provider_calls[16].provider_number = 0,
                "provider_count" => {
                    observed.provider_calls[16].returned_bid_count = usize::from(u16::MAX) + 1
                }
                "slot_number" => observed.slots[64].slot_number = 0,
                "slot_count" => observed.slots[64].returned_bid_count = usize::from(u16::MAX) + 1,
                _ => panic!("should use the documented tail field"),
            }
            assert_eq!(
                json!(project_auction_transport(&observed)),
                json!(TraceAuctionTransportV1::unavailable()),
                "should reject required invalid facts before tail truncation"
            );
        }
        let mut observed = observed_auction();
        observed.slots = vec![observed.slots[0].clone(); 65];
        observed.slots[64].requested_sizes = vec![[0, 0]];
        observed.slots[64].selected_creative_size = Some([0, 0]);
        let projected = project_auction_transport(&observed);
        let evidence = json!(
            projected
                .evidence()
                .expect("should truncate optional tail facts")
        );
        assert_eq!(
            evidence["truncation"],
            json!({"omitted_provider_calls":0,"omitted_slots":1,"omitted_nested_values":0}),
            "should keep optional nested accounting within retained slots"
        );
    }

    #[test]
    fn trace_auction_projection_omission_overflow_never_wraps_or_saturates() {
        for field in ["providers", "slots", "nested"] {
            let mut observed = observed_auction();
            match field {
                "providers" => observed.truncation.omitted_provider_calls = usize::from(u16::MAX),
                "slots" => observed.truncation.omitted_slots = usize::from(u16::MAX),
                "nested" => observed.truncation.omitted_nested_values = usize::from(u16::MAX),
                _ => panic!("should use the documented omission field"),
            }
            assert!(
                project_auction_transport(&observed).evidence().is_some(),
                "should accept exact omission counter maximum"
            );
            match field {
                "providers" => {
                    observed.provider_calls = vec![observed.provider_calls[0].clone(); 17]
                }
                "slots" => observed.slots = vec![observed.slots[0].clone(); 65],
                "nested" => observed.total_time = Some(Duration::MAX),
                _ => panic!("should use the documented omission field"),
            }
            assert_eq!(
                json!(project_auction_transport(&observed)),
                json!(TraceAuctionTransportV1::unavailable()),
                "should fail checked omission addition instead of saturation"
            );
        }
        for truncation in [
            ObservedTraceTruncation {
                omitted_provider_calls: usize::from(u16::MAX) + 1,
                ..ObservedTraceTruncation::default()
            },
            ObservedTraceTruncation {
                omitted_slots: usize::MAX,
                ..ObservedTraceTruncation::default()
            },
            ObservedTraceTruncation {
                omitted_nested_values: usize::MAX,
                ..ObservedTraceTruncation::default()
            },
        ] {
            let mut observed = observed_auction();
            observed.truncation = truncation;
            assert!(
                project_auction_transport(&observed).evidence().is_none(),
                "should reject unrepresentable carried omissions"
            );
        }
    }

    #[test]
    fn trace_auction_projection_counts_sizes_in_the_first_prefix_without_backfilling() {
        let mut observed = observed_auction();
        observed.slots[0].requested_sizes = vec![[0, 0]; 16];
        observed.slots[0].requested_sizes.push([300, 250]);
        let projected = project_auction_transport(&observed);
        let evidence = json!(
            projected
                .evidence()
                .expect("should omit malformed optional sizes")
        );
        assert_eq!(
            evidence["slots"][0]["requested_sizes"],
            json!([]),
            "should never backfill from beyond the requested-size prefix"
        );
        assert_eq!(
            evidence["truncation"]["omitted_nested_values"],
            json!(17),
            "should count invalid prefix and discarded tail separately"
        );
        observed.slots[0].requested_sizes = vec![[300, 250]; usize::from(u16::MAX) + 17];
        assert!(
            project_auction_transport(&observed).evidence().is_none(),
            "should reject more than u16 omitted sizes"
        );
    }
}
