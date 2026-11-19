//! Value types for full-fidelity EIDs stored in the EC identity graph.
//!
//! Provider output is converted into these types at the trust boundary
//! (coordinator validation); past that point an invalid UID, agent type or
//! match method cannot be represented.

use core::fmt;
use std::sync::Arc;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;

use super::kv_types::MAX_STORED_UID_LENGTH;
use super::partner::normalize_partner_source_domain;

/// Maximum serialized size of an `ext` object, in bytes.
pub const MAX_EXT_BYTES: usize = 1024;

/// Maximum nesting depth of an `ext` object.
///
/// Keeps every whole-row or whole-request parse that embeds the `ext` well
/// under `serde_json`'s 128-level recursion limit.
pub const MAX_EXT_DEPTH: usize = 32;

/// Returns the deepest object or array nesting in JSON `text`.
///
/// Iterative, so it cannot overflow the stack on hostile input.
fn json_depth(text: &str) -> usize {
    let (mut depth, mut deepest) = (0_usize, 0_usize);
    let (mut in_string, mut escaped) = (false, false);
    for byte in text.bytes() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// Maximum size of `inserter` and `matcher`, in bytes.
pub const MAX_EID_FIELD_BYTES: usize = 255;

/// Why a provider value was rejected; counted per reason, never logged with
/// the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum EidValidationError {
    /// The UID is blank.
    #[display("UID is empty")]
    EmptyUid,
    /// The UID exceeds [`MAX_STORED_UID_LENGTH`].
    #[display("UID is too long")]
    UidTooLong,
    /// The agent type is negative.
    #[display("agent type is negative")]
    NegativeAgentType,
    /// The match method is not an `OpenRTB` 2.6 value.
    #[display("match method is unknown")]
    UnknownMatchMethod,
    /// An `ext` is not a JSON object within [`MAX_EXT_BYTES`].
    #[display("ext is not a JSON object within the size limit")]
    InvalidExt,
    /// An `inserter` or `matcher` exceeds [`MAX_EID_FIELD_BYTES`].
    #[display("EID field is too long")]
    FieldTooLong,
    /// The source is not a valid domain or not claimed by the provider.
    #[display("source is not claimed by this provider")]
    UnclaimedSource,
    /// The expiry is not after the request time.
    #[display("expiry is in the past")]
    Expired,
}

impl core::error::Error for EidValidationError {}

/// Unix time in whole seconds.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    derive_more::Display,
)]
#[serde(transparent)]
pub struct UnixSeconds(u64);

impl UnixSeconds {
    /// Wraps a Unix timestamp in seconds.
    #[must_use]
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Returns the timestamp in seconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds `seconds`, saturating at `u64::MAX`.
    #[must_use]
    pub const fn saturating_add_secs(self, seconds: u64) -> Self {
        Self(self.0.saturating_add(seconds))
    }
}

struct ArcStrVisitor;

impl<'de> Visitor<'de> for ArcStrVisitor {
    type Value = Arc<str>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Arc::from(value))
    }
}

/// Operator-chosen identifier of a configured provider instance.
///
/// Clones share one allocation. Serialized as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
pub struct ProviderId(Arc<str>);

impl ProviderId {
    /// Creates a provider ID from configuration.
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(Arc::from(id))
    }

    /// Returns the ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ProviderId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(ArcStrVisitor).map(Self)
    }
}

/// Normalized EID source domain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
pub struct SourceDomain(Arc<str>);

impl SourceDomain {
    /// Returns the normalized domain.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SourceDomain {
    type Error = EidValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        normalize_partner_source_domain(value)
            .map(|normalized| Self(Arc::from(normalized)))
            .map_err(|_| EidValidationError::UnclaimedSource)
    }
}

impl Serialize for SourceDomain {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceDomain {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = deserializer.deserialize_str(ArcStrVisitor)?;
        Self::try_from(raw.as_ref()).map_err(de::Error::custom)
    }
}

/// A non-blank UID of at most [`MAX_STORED_UID_LENGTH`] bytes, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, derive_more::Display)]
#[serde(try_from = "String")]
pub struct UidValue(Box<str>);

impl UidValue {
    /// Returns the UID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn check(value: &str) -> Result<(), EidValidationError> {
        if value.trim().is_empty() {
            return Err(EidValidationError::EmptyUid);
        }
        if value.len() > MAX_STORED_UID_LENGTH {
            return Err(EidValidationError::UidTooLong);
        }
        Ok(())
    }
}

impl TryFrom<&str> for UidValue {
    type Error = EidValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::check(value)?;
        Ok(Self(Box::from(value)))
    }
}

impl TryFrom<String> for UidValue {
    type Error = EidValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::check(&value)?;
        Ok(Self(value.into_boxed_str()))
    }
}

/// Non-negative `OpenRTB` agent type, including vendor-specific values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub struct AgentType(i32);

impl AgentType {
    /// Returns the raw agent type.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl TryFrom<i32> for AgentType {
    type Error = EidValidationError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(EidValidationError::NegativeAgentType);
        }
        Ok(Self(value))
    }
}

impl From<AgentType> for i32 {
    fn from(value: AgentType) -> Self {
        value.0
    }
}

/// `OpenRTB` 2.6 EID match method (`mm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub enum MatchMethod {
    /// `0`: unknown.
    Unknown,
    /// `1`: no matching.
    NoMatching,
    /// `2`: cookie sync.
    CookieSync,
    /// `3`: authenticated.
    Authenticated,
    /// `4`: observed.
    Observed,
    /// `5`: inference.
    Inference,
}

impl TryFrom<i32> for MatchMethod {
    type Error = EidValidationError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::NoMatching),
            2 => Ok(Self::CookieSync),
            3 => Ok(Self::Authenticated),
            4 => Ok(Self::Observed),
            5 => Ok(Self::Inference),
            _ => Err(EidValidationError::UnknownMatchMethod),
        }
    }
}

impl From<MatchMethod> for i32 {
    fn from(value: MatchMethod) -> Self {
        match value {
            MatchMethod::Unknown => 0,
            MatchMethod::NoMatching => 1,
            MatchMethod::CookieSync => 2,
            MatchMethod::Authenticated => 3,
            MatchMethod::Observed => 4,
            MatchMethod::Inference => 5,
        }
    }
}

/// When a stored UID stops being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiry {
    /// Served until replaced or removed.
    #[default]
    Never,
    /// Never served at or after this time.
    At(UnixSeconds),
}

impl Expiry {
    /// Returns whether a UID with this expiry may be served at `now`.
    #[must_use]
    pub fn is_live(self, now: UnixSeconds) -> bool {
        match self {
            Self::Never => true,
            Self::At(expires_at) => now < expires_at,
        }
    }

    /// Returns whether this is [`Expiry::Never`]; used to omit `exp`.
    #[must_use]
    pub fn is_never(&self) -> bool {
        matches!(self, Self::Never)
    }
}

/// Serde adapter storing [`Expiry`] as an optional `exp` number.
pub mod optional_exp {
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

    use super::{Expiry, UnixSeconds};

    /// Serializes `At` as its seconds; `Never` is skipped by the caller.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer>(expiry: &Expiry, serializer: S) -> Result<S::Ok, S::Error> {
        match expiry {
            Expiry::At(expires_at) => expires_at.serialize(serializer),
            Expiry::Never => serializer.serialize_none(),
        }
    }

    /// Reads an optional number; absent or `null` means [`Expiry::Never`].
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error for a non-numeric value.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Expiry, D::Error> {
        Ok(Option::<UnixSeconds>::deserialize(deserializer)?.map_or(Expiry::Never, Expiry::At))
    }
}

/// An opaque JSON object of at most [`MAX_EXT_BYTES`], compared by bytes.
///
/// Deserializing applies the same checks as [`RawJson::try_from`], so a
/// stored row cannot carry an oversized or non-object `ext`.
#[derive(Debug, Clone)]
pub struct RawJson(Box<RawValue>);

impl RawJson {
    /// Returns the raw JSON text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.get()
    }

    /// Parses the raw JSON into a [`serde_json::Value`] for `OpenRTB` output.
    ///
    /// # Panics
    ///
    /// Never in practice: the bytes were validated as JSON when stored.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::from_str(self.0.get()).expect("should parse stored raw JSON")
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl RawJson {
    fn check(value: &RawValue) -> Result<(), EidValidationError> {
        let text = value.get();
        if text.len() > MAX_EXT_BYTES
            || !text.trim_start().starts_with('{')
            || json_depth(text) > MAX_EXT_DEPTH
        {
            return Err(EidValidationError::InvalidExt);
        }
        // `RawValue` skips nested values without serde_json's recursion limit,
        // so parse once here: an `ext` that cannot become a `Value` would make
        // `to_value` panic on every serve of the row.
        serde_json::from_str::<serde_json::Value>(text)
            .map(|_| ())
            .map_err(|_| EidValidationError::InvalidExt)
    }
}

impl TryFrom<&RawValue> for RawJson {
    type Error = EidValidationError;

    fn try_from(value: &RawValue) -> Result<Self, Self::Error> {
        Self::check(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<Box<RawValue>> for RawJson {
    type Error = EidValidationError;

    fn try_from(value: Box<RawValue>) -> Result<Self, Self::Error> {
        Self::check(&value)?;
        Ok(Self(value))
    }
}

impl Serialize for RawJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::try_from(raw).map_err(de::Error::custom)
    }
}

/// EID-level fields shared by all UIDs of one stored entry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EidFields {
    /// `OpenRTB` `inserter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserter: Option<Box<str>>,
    /// `OpenRTB` `matcher`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<Box<str>>,
    /// `OpenRTB` `mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mm: Option<MatchMethod>,
    /// `OpenRTB` EID `ext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<RawJson>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uid_value_rejects_blank_and_oversized() {
        assert!(UidValue::try_from("  ").is_err(), "should reject blank UID");
        let long = "a".repeat(MAX_STORED_UID_LENGTH + 1);
        assert!(
            UidValue::try_from(long.as_str()).is_err(),
            "should reject oversized UID"
        );
        let uid = UidValue::try_from("ID-123").expect("should accept valid UID");
        assert_eq!(uid.as_str(), "ID-123", "should keep UID verbatim");
    }

    #[test]
    fn agent_type_rejects_negative() {
        assert!(
            AgentType::try_from(-1).is_err(),
            "should reject negative atype"
        );
        assert_eq!(
            AgentType::try_from(571_187)
                .expect("should accept vendor atype")
                .get(),
            571_187,
            "should keep vendor atype"
        );
    }

    #[test]
    fn match_method_round_trips_openrtb_values() {
        for value in 0..=5 {
            let method = MatchMethod::try_from(value).expect("should accept OpenRTB mm");
            assert_eq!(i32::from(method), value, "should round-trip mm {value}");
        }
        assert!(
            MatchMethod::try_from(6).is_err(),
            "should reject unknown mm"
        );
    }

    #[test]
    fn source_domain_normalizes() {
        let source = SourceDomain::try_from("IDS.Example.COM").expect("should accept domain");
        assert_eq!(
            source.as_str(),
            "ids.example.com",
            "should lowercase source domain"
        );
    }

    #[test]
    fn provider_id_round_trips_as_string() {
        let id = ProviderId::new("example-provider");
        let encoded = serde_json::to_string(&id).expect("should serialize provider id");
        assert_eq!(
            encoded, "\"example-provider\"",
            "should serialize as plain string"
        );
        let decoded: ProviderId =
            serde_json::from_str(&encoded).expect("should deserialize provider id");
        assert_eq!(decoded, id, "should round-trip provider id");
    }

    #[test]
    fn expiry_liveness() {
        let now = UnixSeconds::new(100);
        assert!(
            Expiry::Never.is_live(now),
            "should serve UIDs without expiry"
        );
        assert!(
            Expiry::At(UnixSeconds::new(101)).is_live(now),
            "should serve before expiry"
        );
        assert!(
            !Expiry::At(UnixSeconds::new(100)).is_live(now),
            "should not serve at expiry"
        );
    }

    #[test]
    fn raw_json_accepts_small_objects_only() {
        let object =
            serde_json::value::to_raw_value(&json!({ "k": 1 })).expect("should encode object");
        let raw = RawJson::try_from(object.as_ref()).expect("should accept object");
        assert_eq!(raw.as_str(), r#"{"k":1}"#, "should keep raw bytes");
        let array = serde_json::value::to_raw_value(&json!([1])).expect("should encode array");
        assert!(
            RawJson::try_from(array.as_ref()).is_err(),
            "should reject non-object"
        );
        let big = serde_json::value::to_raw_value(&json!({ "k": "x".repeat(MAX_EXT_BYTES) }))
            .expect("should encode big object");
        assert!(
            RawJson::try_from(big.as_ref()).is_err(),
            "should reject oversized ext"
        );
    }

    #[test]
    fn raw_json_rejects_ext_nested_past_the_parse_limit() {
        let depth = 150;
        let text = format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
        let raw: Box<RawValue> =
            serde_json::from_str(&text).expect("should read deep JSON as a raw value");
        assert!(
            text.len() <= MAX_EXT_BYTES,
            "should fit the size limit so only the depth is under test"
        );

        assert!(
            RawJson::try_from(raw).is_err(),
            "should reject an ext that cannot be parsed into a value"
        );
    }

    #[test]
    fn raw_json_bounds_ext_nesting_depth() {
        let nested = |depth: usize| -> Box<RawValue> {
            let text = format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth));
            serde_json::from_str(&text).expect("should read JSON as a raw value")
        };

        assert!(
            RawJson::try_from(nested(MAX_EXT_DEPTH)).is_ok(),
            "should accept an ext at the depth limit"
        );
        assert!(
            RawJson::try_from(nested(MAX_EXT_DEPTH + 1)).is_err(),
            "should reject an ext past the depth limit"
        );
        let braces_in_string: Box<RawValue> =
            serde_json::from_str(&format!(r#"{{"a":"{}\""}}"#, "{[".repeat(50)))
                .expect("should read JSON as a raw value");
        assert!(
            RawJson::try_from(braces_in_string).is_ok(),
            "should not count brackets inside strings"
        );
    }

    #[test]
    fn raw_json_compares_by_bytes() {
        let a = serde_json::value::to_raw_value(&json!({ "k": 1 })).expect("should encode");
        let b = serde_json::value::to_raw_value(&json!({ "k": 1 })).expect("should encode");
        assert_eq!(
            RawJson::try_from(a.as_ref()).expect("should accept"),
            RawJson::try_from(b.as_ref()).expect("should accept"),
            "should compare equal bytes as equal"
        );
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct WithExpiry {
        #[serde(
            default,
            rename = "exp",
            with = "optional_exp",
            skip_serializing_if = "Expiry::is_never"
        )]
        expiry: Expiry,
    }

    #[test]
    fn optional_exp_omits_never_and_reads_missing() {
        let never = serde_json::to_value(WithExpiry {
            expiry: Expiry::Never,
        })
        .expect("should serialize");
        assert_eq!(never, json!({}), "should omit exp for Never");
        let at: WithExpiry =
            serde_json::from_value(json!({ "exp": 42 })).expect("should deserialize exp");
        assert_eq!(
            at.expiry,
            Expiry::At(UnixSeconds::new(42)),
            "should read exp"
        );
        let missing: WithExpiry =
            serde_json::from_value(json!({})).expect("should deserialize empty");
        assert_eq!(missing.expiry, Expiry::Never, "should default to Never");
    }

    #[test]
    fn raw_json_deserialize_applies_same_checks() {
        let ok: RawJson = serde_json::from_str(r#"{"k":1}"#).expect("should accept small object");
        assert_eq!(ok.as_str(), r#"{"k":1}"#, "should keep raw bytes");
        assert!(
            serde_json::from_str::<RawJson>("[1]").is_err(),
            "should reject non-object on deserialize"
        );
        let big = serde_json::to_string(&json!({ "k": "x".repeat(MAX_EXT_BYTES) }))
            .expect("should encode big object");
        assert!(
            serde_json::from_str::<RawJson>(&big).is_err(),
            "should reject oversized ext on deserialize"
        );
    }
}
