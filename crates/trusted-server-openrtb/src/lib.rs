//! `OpenRTB` 2.6 request and response data model.
//!
//! Types are generated from the IAB `OpenRTB` proto file via `prost-build`, then
//! post-processed to add serde JSON support and `ext` fields. See
//! `crates/trusted-server-openrtb/generate.sh` and the `trusted-server-openrtb-codegen` crate for the
//! generation pipeline.
#![allow(
    clippy::pub_use,
    reason = "OpenRTB exposes generated nested proto types as a flat public API"
)]

/// Serde helper that serializes `Option<bool>` as `Option<i32>` (`1`/`0`).
///
/// The `OpenRTB` JSON spec represents boolean-like fields as integers (`0`/`1`),
/// but the IAB proto file defines them as `bool`. This module bridges the gap
/// so that the Rust types use `bool` while the JSON wire format uses integers.
/// Deserialization accepts both forms for robustness.
#[allow(
    clippy::missing_errors_doc,
    reason = "serde helpers expose the signature required by serde"
)]
pub mod bool_as_int {
    use serde::{Deserialize as _, Deserializer, Serializer};

    #[inline]
    pub fn serialize<S: Serializer>(
        value: &Option<bool>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(true) => serializer.serialize_some(&1_i32),
            Some(false) => serializer.serialize_some(&0_i32),
            None => serializer.serialize_none(),
        }
    }

    #[inline]
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<bool>, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(deserializer)?;
        match value {
            Some(serde_json::Value::Bool(boolean_value)) => Ok(Some(boolean_value)),
            Some(serde_json::Value::Number(number)) => Ok(Some(number.as_i64().map_or_else(
                || {
                    number
                        .as_f64()
                        .is_some_and(|float_value| float_value != 0.0_f64)
                },
                |integer_value| integer_value != 0,
            ))),
            // Some bidders send boolean-as-int fields as strings (e.g.
            // `"secure": "1"` instead of `"secure": 1`). Accept the common
            // string representations for robustness.
            Some(serde_json::Value::String(string_value)) => match string_value.as_str() {
                "1" | "true" => Ok(Some(true)),
                "0" | "false" => Ok(Some(false)),
                other => {
                    log::warn!(
                        "bool_as_int: unrecognized string value \"{other}\", treating as None"
                    );
                    Ok(None)
                }
            },
            _ => Ok(None),
        }
    }
}

// Generated from proto/openrtb.proto. Regenerate with `./crates/trusted-server-openrtb/generate.sh`.
// Suppress clippy on generated code — doc comments and method signatures come
// from prost codegen and are not worth hand-editing.
#[allow(
    dead_code,
    clippy::absolute_paths,
    clippy::allow_attributes_without_reason,
    clippy::doc_markdown,
    clippy::indexing_slicing,
    clippy::must_use_candidate,
    clippy::pedantic,
    clippy::restriction,
    clippy::return_self_not_must_use,
    clippy::struct_excessive_bools,
    reason = "generated OpenRTB bindings preserve prost-generated signatures and paths"
)]
mod generated;

// Codegen module — included here only for testing; the same source is
// `include!`'d by `crates/trusted-server-openrtb-codegen/src/main.rs` for the actual code generation.
#[cfg(test)]
#[allow(
    clippy::arbitrary_source_item_ordering,
    clippy::arithmetic_side_effects,
    clippy::else_if_without_else,
    clippy::format_push_string,
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    clippy::string_slice,
    reason = "codegen is test-only text transformation logic for generated OpenRTB bindings"
)]
mod codegen;

// Re-export nested types at crate root for flat, ergonomic access.
// These correspond to the top-level OpenRTB 2.6 objects that are nested inside
// BidRequest / BidResponse in the proto schema.
pub use generated::{BidRequest, BidResponse};

pub use generated::bid_request::App;
pub use generated::bid_request::Audio;
pub use generated::bid_request::Banner;
pub use generated::bid_request::BrandVersion;
pub use generated::bid_request::Channel;
pub use generated::bid_request::Content;
pub use generated::bid_request::Data;
pub use generated::bid_request::Deal;
pub use generated::bid_request::Device;
pub use generated::bid_request::Dooh;
pub use generated::bid_request::DurFloors;
pub use generated::bid_request::Eid;
pub use generated::bid_request::Format;
pub use generated::bid_request::Geo;
pub use generated::bid_request::Imp;
pub use generated::bid_request::Metric;
pub use generated::bid_request::Native;
pub use generated::bid_request::Network;
pub use generated::bid_request::Pmp;
pub use generated::bid_request::Producer;
pub use generated::bid_request::Publisher;
pub use generated::bid_request::Qty;
pub use generated::bid_request::RefSettings;
pub use generated::bid_request::Refresh;
pub use generated::bid_request::Regs;
pub use generated::bid_request::Segment;
pub use generated::bid_request::Site;
pub use generated::bid_request::Source;
pub use generated::bid_request::SupplyChain;
pub use generated::bid_request::SupplyChainNode;
pub use generated::bid_request::User;
pub use generated::bid_request::UserAgent;
pub use generated::bid_request::Video;
pub use generated::bid_request::eid::Uid;
pub use generated::bid_response::{Bid, SeatBid};

/// Convenience alias for a JSON object used in `OpenRTB` `ext` fields.
pub type Object = serde_json::Map<String, serde_json::Value>;

/// Convert a serializable struct into an `Option<Object>` suitable for an
/// `OpenRTB` `ext` field. Returns `None` when serialization produces an empty
/// map (i.e. all fields were skipped), so that `ext` is omitted from the JSON
/// output rather than emitting `"ext": {}`.
///
/// Types that need `to_ext()` must explicitly implement this trait (no blanket
/// impl) to avoid polluting autocomplete on unrelated `Serialize` types.
pub trait ToExt: serde::Serialize {
    /// Serialize `self` into an `OpenRTB` extension object.
    #[inline]
    fn to_ext(&self) -> Option<Object> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) if !map.is_empty() => Some(map),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BidRequest;
    use serde_json::json;

    /// Helper struct to exercise `bool_as_int` through serde round-trips.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct BoolAsIntWrapper {
        #[serde(
            with = "crate::bool_as_int",
            skip_serializing_if = "Option::is_none",
            default
        )]
        flag: Option<bool>,
    }

    #[test]
    fn preserves_openrtb_26_privacy_dooh_and_refresh_fields() {
        let payload = json!({
            "id": "request-1",
            "imp": [
                {
                    "id": "imp-1",
                    "banner": {
                        "w": 300_i32,
                        "h": 250_i32
                    },
                    "qty": {
                        "multiplier": 14.2_f64,
                        "sourcetype": 1_i32,
                        "vendor": "measurement.example"
                    },
                    "dt": 1_735_689_600_000.0_f64,
                    "refresh": {
                        "refsettings": [
                            {
                                "reftype": 1_i32,
                                "minint": 30_i32
                            }
                        ],
                        "count": 2_i32
                    },
                    "video": {
                        "mimes": ["video/mp4"],
                        "durfloors": [
                            {
                                "mindur": 1_i32,
                                "bidfloor": 5.0_f64
                            }
                        ]
                    }
                }
            ],
            "dooh": {
                "id": "screen-group-1",
                "venuetype": ["retail"],
                "venuetypetax": 1_i32,
                "domain": "inventory.example"
            },
            "regs": {
                "gpp": "DBABMA~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA",
                "gpp_sid": [7_i32],
                "gdpr": 1_i32
            },
            "acat": ["IAB1"]
        });

        let bid_request: BidRequest =
            serde_json::from_value(payload.clone()).expect("should deserialize OpenRTB 2.6 fields");
        let serialized =
            serde_json::to_value(&bid_request).expect("should serialize OpenRTB 2.6 fields");

        assert_eq!(
            serialized["regs"]["gpp"], payload["regs"]["gpp"],
            "should preserve regs.gpp"
        );
        assert_eq!(
            serialized["regs"]["gpp_sid"], payload["regs"]["gpp_sid"],
            "should preserve regs.gpp_sid"
        );
        assert_eq!(
            serialized["regs"]["gdpr"], payload["regs"]["gdpr"],
            "should preserve regs.gdpr"
        );
        assert_eq!(
            serialized["acat"], payload["acat"],
            "should preserve bidrequest.acat"
        );
        assert_eq!(
            serialized["dooh"], payload["dooh"],
            "should preserve bidrequest.dooh"
        );
        assert_eq!(
            serialized["imp"][0]["qty"], payload["imp"][0]["qty"],
            "should preserve imp.qty"
        );
        assert_eq!(
            serialized["imp"][0]["dt"], payload["imp"][0]["dt"],
            "should preserve imp.dt"
        );
        assert_eq!(
            serialized["imp"][0]["refresh"], payload["imp"][0]["refresh"],
            "should preserve imp.refresh"
        );
        assert_eq!(
            serialized["imp"][0]["video"]["durfloors"], payload["imp"][0]["video"]["durfloors"],
            "should preserve video.durfloors"
        );
    }

    /// Deserializing an empty `{}` should produce a valid `BidRequest` with
    /// all fields at their default values.
    #[test]
    fn deserializes_empty_object_to_defaults() {
        let bid_request: BidRequest =
            serde_json::from_str("{}").expect("should deserialize empty object");
        assert!(bid_request.id.is_none(), "id should be None");
        assert!(bid_request.imp.is_empty(), "imp should be empty");
        assert!(bid_request.site.is_none(), "site should be None");
        assert!(bid_request.regs.is_none(), "regs should be None");
        assert!(bid_request.ext.is_none(), "ext should be None");
    }

    /// Unknown fields should be silently ignored (serde default behaviour with
    /// `#[serde(default)]`).
    #[test]
    fn ignores_unknown_fields_gracefully() {
        let payload = json!({
            "id": "req-1",
            "imp": [],
            "totally_unknown_field": "surprise",
            "another_unknown": 42_i32
        });
        let bid_request: BidRequest =
            serde_json::from_value(payload).expect("should ignore unknown fields");
        assert_eq!(bid_request.id.as_deref(), Some("req-1"));
    }

    #[test]
    fn bool_as_int_deserializes_null_to_none() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": null}"#).expect("should handle null");
        assert_eq!(wrapper.flag, None);
    }

    #[test]
    fn bool_as_int_deserializes_string_one_as_true() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": "1"}"#).expect("should handle string");
        assert_eq!(
            wrapper.flag,
            Some(true),
            "string '1' should be treated as true"
        );
    }

    #[test]
    fn bool_as_int_deserializes_string_zero_as_false() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": "0"}"#).expect("should handle string");
        assert_eq!(
            wrapper.flag,
            Some(false),
            "string '0' should be treated as false"
        );
    }

    #[test]
    fn bool_as_int_deserializes_string_true_as_true() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": "true"}"#).expect("should handle string");
        assert_eq!(
            wrapper.flag,
            Some(true),
            "string 'true' should be treated as true"
        );
    }

    #[test]
    fn bool_as_int_deserializes_unknown_string_to_none() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": "yes"}"#).expect("should handle string");
        assert_eq!(
            wrapper.flag, None,
            "unrecognised string should be treated as None"
        );
    }

    #[test]
    fn bool_as_int_deserializes_negative_number() {
        // -1 is non-zero, so it should be treated as true.
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": -1}"#).expect("should handle negative");
        assert_eq!(wrapper.flag, Some(true), "-1 (non-zero) should be true");
    }

    #[test]
    fn bool_as_int_round_trips_true_as_1() {
        let wrapper = BoolAsIntWrapper { flag: Some(true) };
        let json = serde_json::to_value(&wrapper).expect("should serialize");
        assert_eq!(json["flag"], 1_i32, "true should serialize as 1");
    }

    #[test]
    fn bool_as_int_round_trips_false_as_0() {
        let wrapper = BoolAsIntWrapper { flag: Some(false) };
        let json = serde_json::to_value(&wrapper).expect("should serialize");
        assert_eq!(json["flag"], 0_i32, "false should serialize as 0");
    }

    #[test]
    fn bool_as_int_omits_none() {
        let wrapper = BoolAsIntWrapper { flag: None };
        let json = serde_json::to_value(&wrapper).expect("should serialize");
        assert!(
            json.get("flag").is_none(),
            "None should be omitted via skip_serializing_if"
        );
    }

    #[test]
    fn bool_as_int_deserializes_float_zero_as_false() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": 0.0}"#).expect("should handle float zero");
        assert_eq!(
            wrapper.flag,
            Some(false),
            "0.0 should be treated as false, not true"
        );
    }

    #[test]
    fn bool_as_int_deserializes_float_one_as_true() {
        let wrapper: BoolAsIntWrapper =
            serde_json::from_str(r#"{"flag": 1.0}"#).expect("should handle float one");
        assert_eq!(
            wrapper.flag,
            Some(true),
            "1.0 (non-zero) should be treated as true"
        );
    }
}
