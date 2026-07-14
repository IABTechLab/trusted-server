//! Canonical auction input validation.
//!
//! This module owns bounded validation for the canonical slot contract shared
//! by browser and server-side auction entry points.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use error_stack::{ensure, Report};
use serde_json::Value as JsonValue;

use crate::auction::types::{AdFormat, MediaType};
use crate::auction::ContextValue;
use crate::error::TrustedServerError;

/// Limits applied while validating canonical auction input.
#[derive(Debug, Clone)]
pub struct AuctionInputLimits {
    pub max_ad_units: usize,
    pub max_slot_id_bytes: usize,
    pub max_formats_per_slot: usize,
    pub max_bidders_per_slot: usize,
    pub max_bidder_name_bytes: usize,
    pub max_bidder_params_bytes: usize,
    pub max_targeting_entries: usize,
    pub max_targeting_key_bytes: usize,
    pub max_targeting_value_bytes: usize,
    pub max_context_entries: usize,
    pub max_context_key_bytes: usize,
    pub max_context_text_bytes: usize,
    pub max_context_string_list_items: usize,
    pub max_context_string_list_item_bytes: usize,
}

impl Default for AuctionInputLimits {
    fn default() -> Self {
        Self {
            max_ad_units: 100,
            max_slot_id_bytes: 256,
            max_formats_per_slot: 20,
            max_bidders_per_slot: 50,
            max_bidder_name_bytes: 128,
            max_bidder_params_bytes: 16 * 1024,
            max_targeting_entries: 64,
            max_targeting_key_bytes: 64,
            max_targeting_value_bytes: 4 * 1024,
            max_context_entries: 32,
            max_context_key_bytes: 64,
            max_context_text_bytes: 1024,
            max_context_string_list_items: 100,
            max_context_string_list_item_bytes: 256,
        }
    }
}

/// Raw bidder input before duplicate-name and size checks.
#[derive(Debug, Clone)]
pub struct RawBidder {
    pub name: String,
    pub params: JsonValue,
}

/// Raw slot input before canonical validation.
#[derive(Debug, Clone)]
pub struct RawAdSlot {
    pub id: String,
    pub formats: Vec<AdFormat>,
    pub floor_usd: Option<f64>,
    pub targeting: BTreeMap<String, JsonValue>,
    pub bidders: Vec<RawBidder>,
}

/// Validated slot identifier.
#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct SlotId(String);

/// Finite, non-negative USD floor value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FiniteNonNegativeF64(f64);

/// Bounded slot-level targeting value.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetingValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Array(Vec<TargetingValue>),
}

/// Slot after canonical validation.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedAdSlot {
    pub id: SlotId,
    pub formats: Vec<AdFormat>,
    pub floor_usd: Option<FiniteNonNegativeF64>,
    pub targeting: BTreeMap<String, TargetingValue>,
    pub bidders: BTreeMap<String, JsonValue>,
}

impl SlotId {
    /// Return the validated slot ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FiniteNonNegativeF64 {
    /// Return the validated floating-point value.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

/// Validate raw slots against the canonical auction contract.
///
/// # Errors
///
/// Returns [`TrustedServerError::BadRequest`] when any slot violates canonical
/// limits or invariants.
pub fn validate_slots(
    raw: Vec<RawAdSlot>,
    limits: &AuctionInputLimits,
) -> Result<Vec<ValidatedAdSlot>, Report<TrustedServerError>> {
    ensure!(
        raw.len() <= limits.max_ad_units,
        bad_request("Auction request exceeds maximum ad unit count")
    );

    let mut seen_slot_ids = BTreeSet::new();
    let mut validated = Vec::with_capacity(raw.len());

    for slot in raw {
        let id = validate_slot_id(&slot.id, limits)?;
        ensure!(
            seen_slot_ids.insert(id.clone()),
            bad_request("Auction request contains duplicate slot id")
        );

        let formats = validate_formats(slot.formats, limits)?;
        let floor_usd = validate_floor(slot.floor_usd)?;
        let targeting = validate_targeting(slot.targeting, limits)?;
        let bidders = validate_bidders(slot.bidders, limits)?;

        validated.push(ValidatedAdSlot {
            id,
            formats,
            floor_usd,
            targeting,
            bidders,
        });
    }

    Ok(validated)
}

/// Validate auction-level context against the configured allowlist.
///
/// # Errors
///
/// Returns [`TrustedServerError::BadRequest`] when the context object contains
/// unsupported, disallowed, or over-limit entries.
pub fn validate_context(
    config: Option<&JsonValue>,
    allowed_keys: &HashSet<String>,
    limits: &AuctionInputLimits,
) -> Result<HashMap<String, ContextValue>, Report<TrustedServerError>> {
    let Some(config) = config else {
        return Ok(HashMap::new());
    };
    let Some(object) = config.as_object() else {
        return Err(Report::new(bad_request(
            "Auction context must be a JSON object",
        )));
    };

    ensure!(
        object.len() <= limits.max_context_entries,
        bad_request("Auction context exceeds maximum entry count")
    );

    let mut context = HashMap::new();
    for (key, value) in object {
        let trimmed = key.trim();
        ensure!(
            !trimmed.is_empty() && trimmed.len() <= limits.max_context_key_bytes,
            bad_request("Auction context key must be non-empty and within the byte limit")
        );
        ensure!(
            allowed_keys.contains(trimmed),
            bad_request("Auction context contains disallowed context key")
        );

        context.insert(trimmed.to_string(), validate_context_value(value, limits)?);
    }

    Ok(context)
}

fn validate_slot_id(
    raw: &str,
    limits: &AuctionInputLimits,
) -> Result<SlotId, Report<TrustedServerError>> {
    let trimmed = raw.trim();
    ensure!(
        !trimmed.is_empty() && trimmed.len() <= limits.max_slot_id_bytes,
        bad_request("Auction slot id must be non-empty and within the byte limit")
    );
    Ok(SlotId(trimmed.to_string()))
}

fn validate_formats(
    formats: Vec<AdFormat>,
    limits: &AuctionInputLimits,
) -> Result<Vec<AdFormat>, Report<TrustedServerError>> {
    ensure!(
        !formats.is_empty(),
        bad_request("Auction slot must declare at least one format")
    );
    ensure!(
        formats.len() <= limits.max_formats_per_slot,
        bad_request("Auction slot exceeds maximum format count")
    );

    let mut seen = BTreeSet::new();
    for format in &formats {
        ensure!(
            format.width > 0 && format.height > 0,
            bad_request("Auction slot format dimensions must be positive")
        );
        ensure!(
            i32::try_from(format.width).is_ok() && i32::try_from(format.height).is_ok(),
            bad_request("Auction slot format dimensions must fit within i32")
        );

        let key = (
            media_type_key(&format.media_type),
            format.width,
            format.height,
        );
        ensure!(
            seen.insert(key),
            bad_request("Auction slot contains duplicate format")
        );
    }

    Ok(formats)
}

fn media_type_key(media_type: &MediaType) -> &'static str {
    match media_type {
        MediaType::Banner => "banner",
        MediaType::Video => "video",
        MediaType::Native => "native",
    }
}

fn validate_floor(
    floor: Option<f64>,
) -> Result<Option<FiniteNonNegativeF64>, Report<TrustedServerError>> {
    floor.map_or(Ok(None), |value| {
        ensure!(
            value.is_finite() && value >= 0.0,
            bad_request("Auction slot floor must be finite and non-negative")
        );
        Ok(Some(FiniteNonNegativeF64(value)))
    })
}

fn validate_bidders(
    raw_bidders: Vec<RawBidder>,
    limits: &AuctionInputLimits,
) -> Result<BTreeMap<String, JsonValue>, Report<TrustedServerError>> {
    ensure!(
        raw_bidders.len() <= limits.max_bidders_per_slot,
        bad_request("Auction slot exceeds maximum bidder count")
    );

    let mut bidders = BTreeMap::new();
    for bidder in raw_bidders {
        let name = bidder.name.trim();
        ensure!(
            !name.is_empty() && name.len() <= limits.max_bidder_name_bytes,
            bad_request("Auction bidder name must be non-empty and within the byte limit")
        );

        let params_size = serialized_json_len(&bidder.params)?;
        ensure!(
            params_size <= limits.max_bidder_params_bytes,
            bad_request("Auction bidder params exceed maximum serialized size")
        );

        ensure!(
            bidders.insert(name.to_string(), bidder.params).is_none(),
            bad_request("Auction slot contains duplicate bidder")
        );
    }

    Ok(bidders)
}

fn validate_targeting(
    targeting: BTreeMap<String, JsonValue>,
    limits: &AuctionInputLimits,
) -> Result<BTreeMap<String, TargetingValue>, Report<TrustedServerError>> {
    ensure!(
        targeting.len() <= limits.max_targeting_entries,
        bad_request("Auction slot exceeds maximum targeting entry count")
    );

    let mut validated = BTreeMap::new();
    for (key, value) in targeting {
        let trimmed = key.trim();
        ensure!(
            !trimmed.is_empty() && trimmed.len() <= limits.max_targeting_key_bytes,
            bad_request("Auction targeting key must be non-empty and within the byte limit")
        );

        let value_size = serialized_json_len(&value)?;
        ensure!(
            value_size <= limits.max_targeting_value_bytes,
            bad_request("Auction targeting value exceeds maximum serialized size")
        );

        validated.insert(trimmed.to_string(), validate_targeting_value(&value)?);
    }

    Ok(validated)
}

fn validate_targeting_value(
    value: &JsonValue,
) -> Result<TargetingValue, Report<TrustedServerError>> {
    match value {
        JsonValue::String(value) => Ok(TargetingValue::String(value.clone())),
        JsonValue::Bool(value) => Ok(TargetingValue::Boolean(*value)),
        JsonValue::Number(value) => value.as_f64().map_or_else(
            || {
                Err(Report::new(bad_request(
                    "Auction targeting value must be finite",
                )))
            },
            |number| Ok(TargetingValue::Number(number)),
        ),
        JsonValue::Array(values) => {
            let mut validated = Vec::with_capacity(values.len());
            for value in values {
                match value {
                    JsonValue::String(value) => {
                        validated.push(TargetingValue::String(value.clone()));
                    }
                    JsonValue::Bool(value) => {
                        validated.push(TargetingValue::Boolean(*value));
                    }
                    JsonValue::Number(value) => {
                        let Some(number) = value.as_f64() else {
                            return Err(Report::new(bad_request(
                                "Auction targeting value must be finite",
                            )));
                        };
                        validated.push(TargetingValue::Number(number));
                    }
                    JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => {
                        return Err(Report::new(bad_request(
                            "Auction targeting value must be a scalar or scalar array",
                        )));
                    }
                }
            }
            Ok(TargetingValue::Array(validated))
        }
        JsonValue::Null | JsonValue::Object(_) => Err(Report::new(bad_request(
            "Auction targeting value must be a scalar or scalar array",
        ))),
    }
}

fn validate_context_value(
    value: &JsonValue,
    limits: &AuctionInputLimits,
) -> Result<ContextValue, Report<TrustedServerError>> {
    match value {
        JsonValue::String(value) => {
            ensure!(
                value.len() <= limits.max_context_text_bytes,
                bad_request("Auction context text exceeds maximum byte length")
            );
            Ok(ContextValue::Text(value.clone()))
        }
        JsonValue::Number(value) => value.as_f64().map_or_else(
            || {
                Err(Report::new(bad_request(
                    "Auction context number must be finite",
                )))
            },
            |number| Ok(ContextValue::Number(number)),
        ),
        JsonValue::Array(values) => {
            ensure!(
                values.len() <= limits.max_context_string_list_items,
                bad_request("Auction context string list exceeds maximum item count")
            );

            let mut strings = Vec::with_capacity(values.len());
            for value in values {
                let JsonValue::String(value) = value else {
                    return Err(Report::new(bad_request(
                        "Auction context arrays must contain only strings",
                    )));
                };
                ensure!(
                    value.len() <= limits.max_context_string_list_item_bytes,
                    bad_request("Auction context string list item exceeds maximum byte length")
                );
                strings.push(value.clone());
            }
            Ok(ContextValue::StringList(strings))
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Object(_) => Err(Report::new(
            bad_request("Auction context value has unsupported type"),
        )),
    }
}

fn serialized_json_len(value: &JsonValue) -> Result<usize, Report<TrustedServerError>> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|err| {
            Report::new(bad_request("Auction JSON value could not be serialized"))
                .attach(err.to_string())
        })
}

fn bad_request(message: &str) -> TrustedServerError {
    TrustedServerError::BadRequest {
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::auction::types::MediaType;

    fn format(width: u32, height: u32, media_type: MediaType) -> AdFormat {
        AdFormat {
            width,
            height,
            media_type,
        }
    }

    fn bidder(name: &str) -> RawBidder {
        RawBidder {
            name: name.to_string(),
            params: json!({ "placementId": 123 }),
        }
    }

    fn raw_slot(id: &str) -> RawAdSlot {
        RawAdSlot {
            id: id.to_string(),
            formats: vec![format(300, 250, MediaType::Banner)],
            floor_usd: Some(0.25),
            targeting: BTreeMap::from([
                ("pos".to_string(), json!("atf")),
                ("test".to_string(), json!(true)),
            ]),
            bidders: vec![bidder("appnexus")],
        }
    }

    fn validate_one(slot: RawAdSlot) -> Result<Vec<ValidatedAdSlot>, Report<TrustedServerError>> {
        validate_slots(vec![slot], &AuctionInputLimits::default())
    }

    fn assert_invalid(slot: RawAdSlot, expected: &str) {
        let err = validate_one(slot).expect_err("should reject invalid slot");
        assert!(
            format!("{err:?}").contains(expected),
            "should include expected error `{expected}` in {err:?}"
        );
    }

    #[test]
    fn validated_ad_slot_rejects_blank_oversized_and_duplicate_ids() {
        assert_invalid(raw_slot("  "), "slot id");

        let oversized = "x".repeat(257);
        assert_invalid(raw_slot(&oversized), "slot id");

        let result = validate_slots(
            vec![raw_slot("div-gpt-top"), raw_slot("div-gpt-top")],
            &AuctionInputLimits::default(),
        );
        let err = result.expect_err("should reject duplicate slot IDs");
        assert!(
            format!("{err:?}").contains("duplicate slot id"),
            "should explain duplicate slot ID"
        );
    }

    #[test]
    fn validated_ad_slot_rejects_missing_zero_overflow_and_duplicate_formats() {
        let mut missing = raw_slot("missing");
        missing.formats.clear();
        assert_invalid(missing, "format");

        let mut zero_width = raw_slot("zero-width");
        zero_width.formats = vec![format(0, 250, MediaType::Banner)];
        assert_invalid(zero_width, "positive");

        let mut overflow = raw_slot("overflow");
        overflow.formats = vec![format(i32::MAX as u32 + 1, 250, MediaType::Banner)];
        assert_invalid(overflow, "i32");

        let mut duplicate = raw_slot("duplicate");
        duplicate.formats = vec![
            format(300, 250, MediaType::Banner),
            format(300, 250, MediaType::Banner),
        ];
        assert_invalid(duplicate, "duplicate format");
    }

    #[test]
    fn validated_ad_slot_rejects_invalid_floors() {
        for floor in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut slot = raw_slot("floor");
            slot.floor_usd = Some(floor);
            assert_invalid(slot, "floor");
        }
    }

    #[test]
    fn validated_ad_slot_rejects_blank_duplicate_and_oversized_bidders() {
        let mut blank = raw_slot("blank-bidder");
        blank.bidders = vec![bidder("  ")];
        assert_invalid(blank, "bidder");

        let mut duplicate = raw_slot("duplicate-bidder");
        duplicate.bidders = vec![bidder("appnexus"), bidder("appnexus")];
        assert_invalid(duplicate, "duplicate bidder");

        let mut oversized_name = raw_slot("oversized-bidder");
        oversized_name.bidders = vec![bidder(&"x".repeat(129))];
        assert_invalid(oversized_name, "bidder");

        let mut oversized_params = raw_slot("oversized-params");
        oversized_params.bidders = vec![RawBidder {
            name: "appnexus".to_string(),
            params: json!({ "data": "x".repeat(16 * 1024) }),
        }];
        assert_invalid(oversized_params, "bidder params");
    }

    #[test]
    fn validated_ad_slot_rejects_invalid_targeting() {
        let mut too_many = raw_slot("too-many-targeting");
        too_many.targeting = (0..65)
            .map(|idx| (format!("key-{idx}"), json!("value")))
            .collect();
        assert_invalid(too_many, "targeting");

        let mut bad_key = raw_slot("bad-key");
        bad_key.targeting = BTreeMap::from([("x".repeat(65), json!("value"))]);
        assert_invalid(bad_key, "targeting key");

        let mut nested = raw_slot("nested");
        nested.targeting = BTreeMap::from([("pos".to_string(), json!({ "nested": true }))]);
        assert_invalid(nested, "targeting value");

        let mut null_value = raw_slot("null-value");
        null_value.targeting = BTreeMap::from([("score".to_string(), JsonValue::Null)]);
        assert_invalid(null_value, "targeting value");
    }

    #[test]
    fn validated_ad_slot_locks_limit_boundaries() {
        let limits = AuctionInputLimits::default();

        let slots = (0..limits.max_ad_units)
            .map(|idx| raw_slot(&format!("slot-{idx}")))
            .collect();
        validate_slots(slots, &limits).expect("should accept max ad units");

        let slots = (0..=limits.max_ad_units)
            .map(|idx| raw_slot(&format!("slot-{idx}")))
            .collect();
        let _err = validate_slots(slots, &limits).expect_err("should reject one over max ad units");

        let mut max_formats = raw_slot("max-formats");
        max_formats.formats = (1..=limits.max_formats_per_slot)
            .map(|idx| format(idx as u32, 250, MediaType::Banner))
            .collect();
        validate_one(max_formats).expect("should accept max formats");

        let mut over_formats = raw_slot("over-formats");
        over_formats.formats = (1..=limits.max_formats_per_slot + 1)
            .map(|idx| format(idx as u32, 250, MediaType::Banner))
            .collect();
        assert_invalid(over_formats, "format");

        let mut max_bidders = raw_slot("max-bidders");
        max_bidders.bidders = (0..limits.max_bidders_per_slot)
            .map(|idx| bidder(&format!("bidder-{idx}")))
            .collect();
        validate_one(max_bidders).expect("should accept max bidders");

        let mut over_bidders = raw_slot("over-bidders");
        over_bidders.bidders = (0..=limits.max_bidders_per_slot)
            .map(|idx| bidder(&format!("bidder-{idx}")))
            .collect();
        assert_invalid(over_bidders, "bidder");
    }

    #[test]
    fn validated_ad_slot_preserves_valid_economics_targeting_and_media() {
        let mut slot = raw_slot("  div-gpt-top  ");
        slot.formats = vec![
            format(300, 250, MediaType::Banner),
            format(640, 480, MediaType::Video),
            format(1, 1, MediaType::Native),
        ];
        slot.targeting = BTreeMap::from([
            ("pos".to_string(), json!("atf")),
            ("enabled".to_string(), json!(true)),
            ("score".to_string(), json!(1.25)),
            ("segments".to_string(), json!(["a", 2, false])),
        ]);

        let slots = validate_one(slot).expect("should accept valid slot");
        let slot = slots.first().expect("should return one slot");

        assert_eq!(slot.id, SlotId("div-gpt-top".to_string()));
        assert_eq!(
            slot.formats,
            vec![
                format(300, 250, MediaType::Banner),
                format(640, 480, MediaType::Video),
                format(1, 1, MediaType::Native),
            ],
            "should preserve media formats"
        );
        assert_eq!(slot.floor_usd, Some(FiniteNonNegativeF64(0.25)));
        assert_eq!(
            slot.targeting.get("segments"),
            Some(&TargetingValue::Array(vec![
                TargetingValue::String("a".to_string()),
                TargetingValue::Number(2.0),
                TargetingValue::Boolean(false),
            ])),
            "should preserve scalar targeting arrays"
        );
    }

    #[test]
    fn validated_ad_slot_context_rejects_disallowed_unsupported_and_over_limit_entries() {
        let limits = AuctionInputLimits::default();
        let allowed = HashSet::from(["segments".to_string(), "lockr_id".to_string()]);

        let too_many = JsonValue::Object(
            (0..=limits.max_context_entries)
                .map(|idx| (format!("key-{idx}"), json!("value")))
                .collect(),
        );
        let allowed_many = (0..=limits.max_context_entries)
            .map(|idx| format!("key-{idx}"))
            .collect();
        let _err = validate_context(Some(&too_many), &allowed_many, &limits)
            .expect_err("should reject context over entry limit");

        let disallowed = json!({ "blocked": "value" });
        let _err = validate_context(Some(&disallowed), &allowed, &limits)
            .expect_err("should reject disallowed context key");

        let unsupported = json!({ "segments": { "nested": true } });
        let _err = validate_context(Some(&unsupported), &allowed, &limits)
            .expect_err("should reject unsupported context value");

        let oversized_text = json!({ "lockr_id": "x".repeat(1025) });
        let _err = validate_context(Some(&oversized_text), &allowed, &limits)
            .expect_err("should reject oversized context text");

        let oversized_list = json!({ "segments": vec!["x"; 101] });
        let _err = validate_context(Some(&oversized_list), &allowed, &limits)
            .expect_err("should reject oversized context string list");
    }

    #[test]
    fn validated_ad_slot_context_preserves_allowed_bounded_values() {
        let config = json!({
            "segments": ["seg-a", "seg-b"],
            "lockr_id": "lockr-123",
            "count": 2
        });
        let allowed = HashSet::from([
            "segments".to_string(),
            "lockr_id".to_string(),
            "count".to_string(),
        ]);

        let context = validate_context(Some(&config), &allowed, &AuctionInputLimits::default())
            .expect("should accept allowed bounded context");

        assert_eq!(
            context.get("segments"),
            Some(&ContextValue::StringList(vec![
                "seg-a".to_string(),
                "seg-b".to_string()
            ])),
            "should preserve string-list context"
        );
        assert_eq!(
            context.get("lockr_id"),
            Some(&ContextValue::Text("lockr-123".to_string())),
            "should preserve text context"
        );
        assert_eq!(
            context.get("count"),
            Some(&ContextValue::Number(2.0)),
            "should preserve numeric context"
        );
    }
}
