//! Optional client tokens follow the exact accepted conversion occurrence.

use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use super::TraceSlotRef;

/// Raw input occurrences and definitive accepted occurrences are separate.
/// Only the normal converter decides which source occurrence becomes a slot.
#[derive(Clone, Default)]
pub(crate) struct TraceClientSlotRefs {
    source: Arc<[Option<TraceSlotRef>]>,
    accepted: Arc<Mutex<Vec<Option<TraceSlotRef>>>>,
}

impl TraceClientSlotRefs {
    pub(crate) fn from_raw(body: &[u8]) -> Self {
        let source = serde_json::from_slice::<RawTraceRequest>(body)
            .map(|request| request.ad_units.0)
            .unwrap_or_default();
        Self {
            source: Arc::from(source),
            accepted: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn begin_conversion(&self) {
        if let Ok(mut accepted) = self.accepted.lock() {
            accepted.clear();
        }
    }

    pub(crate) fn record_accepted(&self, input_index: usize) {
        if let Ok(mut accepted) = self.accepted.lock() {
            accepted.push(self.source.get(input_index).cloned().flatten());
        }
    }

    pub(crate) fn accepted_refs(&self) -> Option<Vec<Option<TraceSlotRef>>> {
        self.accepted.lock().ok().map(|accepted| accepted.clone())
    }
}

#[derive(Deserialize)]
struct RawTraceRequest {
    #[serde(rename = "adUnits")]
    ad_units: RawTraceUnits,
}

struct RawTraceUnits(Vec<Option<TraceSlotRef>>);

impl<'de> Deserialize<'de> for RawTraceUnits {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(TraceUnitsVisitor)
    }
}

struct TraceUnitsVisitor;

impl<'de> Visitor<'de> for TraceUnitsVisitor {
    type Value = RawTraceUnits;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the already-accepted adUnits array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut units = Vec::new();
        while let Some(unit) = sequence.next_element::<&RawValue>()? {
            // Borrow only during extraction. Optional numeric range failures
            // cannot discard another unit's valid association.
            let mut parser = serde_json::Deserializer::from_str(unit.get());
            units.push(
                TraceMemberStage::Unit
                    .deserialize(&mut parser)
                    .unwrap_or(None),
            );
        }
        Ok(RawTraceUnits(units))
    }
}

#[derive(Clone, Copy)]
enum TraceMemberStage {
    Unit,
    Extension,
    Namespace,
    Token,
}

impl TraceMemberStage {
    fn member(self) -> Option<(&'static str, Self)> {
        match self {
            Self::Unit => Some(("ext", Self::Extension)),
            Self::Extension => Some(("trusted_server", Self::Namespace)),
            Self::Namespace => Some(("trace_slot_ref", Self::Token)),
            Self::Token => None,
        }
    }
}

impl<'de> DeserializeSeed<'de> for TraceMemberStage {
    type Value = Option<TraceSlotRef>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(TraceMemberVisitor(self))
    }
}

struct TraceMemberVisitor(TraceMemberStage);

impl<'de> Visitor<'de> for TraceMemberVisitor {
    type Value = Option<TraceSlotRef>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an optional trace namespace member")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen = false;
        let mut duplicate = false;
        let mut token = None;
        while let Some(key) = map.next_key::<String>()? {
            if let Some((member, stage)) = self.0.member()
                && key == member
            {
                duplicate |= seen;
                seen = true;
                token = map.next_value_seed(stage)?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(if duplicate { None } else { token })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(if matches!(self.0, TraceMemberStage::Token) {
            TraceSlotRef::parse(value).ok()
        } else {
            None
        })
    }

    fn visit_bool<E: serde::de::Error>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_i64<E: serde::de::Error>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_u64<E: serde::de::Error>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_f64<E: serde::de::Error>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
}
