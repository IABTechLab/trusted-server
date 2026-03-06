//! `OpenRTB` 2.6 request and response data model.
//!
//! Types are generated from the IAB `OpenRTB` proto file via `prost-build`, then
//! post-processed to add serde JSON support and `ext` fields. See `build.rs`
//! for the generation pipeline.

use serde::Serialize;
use serde_json::{Map, Value};

/// Convenience alias for a JSON object used in `OpenRTB` `ext` fields.
pub type Object = Map<String, Value>;

/// Serde helper that serializes `Option<bool>` as `Option<i32>` (`1`/`0`).
///
/// The `OpenRTB` JSON spec represents boolean-like fields as integers (`0`/`1`),
/// but the IAB proto file defines them as `bool`. This module bridges the gap
/// so that the Rust types use `bool` while the JSON wire format uses integers.
/// Deserialization accepts both forms for robustness.
#[allow(clippy::missing_errors_doc)]
pub mod bool_as_int {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<bool>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(true) => serializer.serialize_some(&1i32),
            Some(false) => serializer.serialize_some(&0i32),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<bool>, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(deserializer)?;
        match value {
            Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
            Some(serde_json::Value::Number(n)) => Ok(Some(n.as_i64() != Some(0))),
            _ => Ok(None),
        }
    }
}

/// Convert a serializable struct into an `Option<Object>` suitable for an
/// `OpenRTB` `ext` field. Returns `None` when serialization produces an empty
/// map (i.e. all fields were skipped), so that `ext` is omitted from the JSON
/// output rather than emitting `"ext": {}`.
pub trait ToExt {
    /// Serialize `self` into an `OpenRTB` extension object.
    fn to_ext(&self) -> Option<Object>;
}

impl<T: Serialize> ToExt for T {
    fn to_ext(&self) -> Option<Object> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) if !map.is_empty() => Some(map),
            _ => None,
        }
    }
}

// Include the prost-generated (and post-processed) types.
// Suppress clippy on generated code — doc comments and method signatures come
// from prost codegen and are not worth hand-editing.
#[allow(
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::struct_excessive_bools
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/com.iabtechlab.openrtb.v2.rs"));
}
pub use generated::*;

// Re-export nested types at crate root for flat, ergonomic access.
// These correspond to the top-level OpenRTB 2.6 objects that are nested inside
// BidRequest / BidResponse in the proto schema.

pub use bid_request::eid::Uid;
pub use bid_request::App;
pub use bid_request::Audio;
pub use bid_request::Banner;
pub use bid_request::BrandVersion;
pub use bid_request::Channel;
pub use bid_request::Content;
pub use bid_request::Data;
pub use bid_request::Deal;
pub use bid_request::Device;
pub use bid_request::Dooh;
pub use bid_request::DurFloors;
pub use bid_request::Eid;
pub use bid_request::Format;
pub use bid_request::Geo;
pub use bid_request::Imp;
pub use bid_request::Metric;
pub use bid_request::Native;
pub use bid_request::Network;
pub use bid_request::Pmp;
pub use bid_request::Producer;
pub use bid_request::Publisher;
pub use bid_request::Qty;
pub use bid_request::RefSettings;
pub use bid_request::Refresh;
pub use bid_request::Regs;
pub use bid_request::Segment;
pub use bid_request::Site;
pub use bid_request::Source;
pub use bid_request::SupplyChain;
pub use bid_request::SupplyChainNode;
pub use bid_request::User;
pub use bid_request::UserAgent;
pub use bid_request::Video;
pub use bid_response::{Bid, SeatBid};

#[cfg(test)]
mod tests {
    use super::BidRequest;
    use serde_json::json;

    #[test]
    fn preserves_openrtb_26_privacy_dooh_and_refresh_fields() {
        let payload = json!({
            "id": "request-1",
            "imp": [
                {
                    "id": "imp-1",
                    "banner": {
                        "w": 300,
                        "h": 250
                    },
                    "qty": {
                        "multiplier": 14.2,
                        "sourcetype": 1,
                        "vendor": "measurement.example"
                    },
                    "dt": 1_735_689_600_000.0_f64,
                    "refresh": {
                        "refsettings": [
                            {
                                "reftype": 1,
                                "minint": 30
                            }
                        ],
                        "count": 2
                    },
                    "video": {
                        "mimes": ["video/mp4"],
                        "durfloors": [
                            {
                                "mindur": 1,
                                "bidfloor": 5.0
                            }
                        ]
                    }
                }
            ],
            "dooh": {
                "id": "screen-group-1",
                "venuetype": ["retail"],
                "venuetypetax": 1,
                "domain": "inventory.example"
            },
            "regs": {
                "gpp": "DBABMA~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA",
                "gpp_sid": [7],
                "gdpr": 1
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
}
