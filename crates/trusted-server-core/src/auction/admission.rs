//! Admission of an upstream bid against the slots the request asked for.
//!
//! A bid is admitted when it names a slot the request carried and its
//! dimensions match exactly one format that slot asked for. These rules are
//! core's, so a source's response reader in any crate applies them, whatever
//! protocol it reads.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::routing::{ProviderAuctionInput, ProviderSlotInput};
use super::types::{AdFormat, AdSlot, AuctionResponse};

/// Fixed reasons why an upstream bid failed admission.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum BidRejectionReason {
    /// The bid is malformed.
    InvalidBid,
    /// The bid names a slot the request did not carry.
    UnrequestedImpression,
    /// The bid's dimensions match no format the slot asked for.
    DimensionMismatch,
    /// The bid's dimensions match more than one format the slot asked for.
    AmbiguousDimensions,
}

impl BidRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidBid => "invalid_bid",
            Self::UnrequestedImpression => "unrequested_impression",
            Self::DimensionMismatch => "dimension_mismatch",
            Self::AmbiguousDimensions => "ambiguous_dimensions",
        }
    }
}

/// Bounded aggregate diagnostics for rejected upstream bids.
#[derive(Debug, Default)]
pub struct ResponseAdmissionDiagnostics {
    rejected_bid_count: u32,
    reason_counts: BTreeMap<BidRejectionReason, u32>,
}

impl ResponseAdmissionDiagnostics {
    /// Record one rejected bid without retaining upstream payload data.
    pub fn record(&mut self, reason: BidRejectionReason) {
        self.rejected_bid_count = self.rejected_bid_count.saturating_add(1);
        let count = self.reason_counts.entry(reason).or_default();
        *count = count.saturating_add(1);
    }

    /// Attach fixed-cardinality rejection counts to a provider response.
    pub fn attach_to(self, response: &mut AuctionResponse) {
        if self.rejected_bid_count == 0 {
            return;
        }
        let reasons = self
            .reason_counts
            .into_iter()
            .map(|(reason, count)| (reason.as_str().to_string(), json!(count)))
            .collect::<Map<_, _>>();
        response.metadata.insert(
            "response_admission".to_string(),
            json!({
                "rejected_bid_count": self.rejected_bid_count,
                "rejection_reasons": reasons,
            }),
        );
    }
}

#[derive(Clone, Copy)]
enum DimensionMatch {
    Unique((u32, u32)),
    Ambiguous,
}

impl DimensionMatch {
    fn include(&mut self, dimensions: (u32, u32)) {
        if matches!(self, Self::Unique(existing) if *existing != dimensions) {
            *self = Self::Ambiguous;
        }
    }

    fn resolve(self) -> Result<(u32, u32), BidRejectionReason> {
        match self {
            Self::Unique(dimensions) => Ok(dimensions),
            Self::Ambiguous => Err(BidRejectionReason::AmbiguousDimensions),
        }
    }
}

/// Precomputed dimension resolutions for one requested slot.
pub struct SlotBidDimensions {
    exact: BTreeSet<(u32, u32)>,
    by_width: BTreeMap<u32, DimensionMatch>,
    by_height: BTreeMap<u32, DimensionMatch>,
    unqualified: Option<DimensionMatch>,
}

impl SlotBidDimensions {
    fn from_formats(formats: &[AdFormat]) -> Self {
        let mut exact = BTreeSet::new();
        let mut by_width = BTreeMap::new();
        let mut by_height = BTreeMap::new();
        let mut unqualified: Option<DimensionMatch> = None;

        for format in formats {
            let dimensions = (format.width, format.height);
            if !exact.insert(dimensions) {
                continue;
            }
            by_width
                .entry(format.width)
                .and_modify(|candidate: &mut DimensionMatch| candidate.include(dimensions))
                .or_insert(DimensionMatch::Unique(dimensions));
            by_height
                .entry(format.height)
                .and_modify(|candidate: &mut DimensionMatch| candidate.include(dimensions))
                .or_insert(DimensionMatch::Unique(dimensions));
            match &mut unqualified {
                Some(candidate) => candidate.include(dimensions),
                None => unqualified = Some(DimensionMatch::Unique(dimensions)),
            }
        }

        Self {
            exact,
            by_width,
            by_height,
            unqualified,
        }
    }
}

/// Precomputed requested banner dimensions keyed by slot id.
pub type BidDimensionIndex = BTreeMap<String, SlotBidDimensions>;

/// Build the requested-dimension index once for one provider response.
#[must_use]
pub fn build_bid_dimension_index(input: &ProviderAuctionInput) -> BidDimensionIndex {
    build_bid_dimension_index_from_slots(input.slots().iter().map(ProviderSlotInput::slot))
}

/// Build the requested-dimension index from plain [`AdSlot`]s.
///
/// The first slot wins when several share an id.
#[must_use]
pub fn build_bid_dimension_index_from_slots<'a>(
    slots: impl IntoIterator<Item = &'a AdSlot>,
) -> BidDimensionIndex {
    let mut index = BidDimensionIndex::new();
    for slot in slots {
        index
            .entry(slot.id.clone())
            .or_insert_with(|| SlotBidDimensions::from_formats(slot.formats.as_slice()));
    }
    index
}

/// Parse an optional positive bid dimension from a JSON field.
///
/// # Errors
///
/// Returns [`BidRejectionReason::InvalidBid`] when the value is present and is
/// not a positive whole number that fits.
pub fn parse_optional_bid_dimension(
    value: &Value,
    key: &str,
) -> Result<Option<u32>, BidRejectionReason> {
    let Some(raw) = value.get(key) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }

    let dimension = raw
        .as_u64()
        .and_then(|dimension| u32::try_from(dimension).ok())
        .or_else(|| {
            let dimension = raw.as_f64()?;
            (dimension.is_finite()
                && dimension > 0.0
                && dimension.fract() == 0.0
                && dimension <= f64::from(u32::MAX))
            .then_some(dimension as u32)
        })
        .filter(|dimension| *dimension > 0)
        .ok_or(BidRejectionReason::InvalidBid)?;
    Ok(Some(dimension))
}

/// Validate explicit dimensions or infer them from one matching banner format.
///
/// # Errors
///
/// Returns the reason the bid is refused: the slot was not requested, or the
/// dimensions match none or more than one of its formats.
pub fn resolve_bid_dimensions(
    dimensions_by_slot: &BidDimensionIndex,
    slot_id: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(u32, u32), BidRejectionReason> {
    let dimensions = dimensions_by_slot
        .get(slot_id)
        .ok_or(BidRejectionReason::UnrequestedImpression)?;

    match (width, height) {
        (Some(width), Some(height)) => dimensions
            .exact
            .contains(&(width, height))
            .then_some((width, height))
            .ok_or(BidRejectionReason::DimensionMismatch),
        (Some(width), None) => dimensions
            .by_width
            .get(&width)
            .copied()
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
        (None, Some(height)) => dimensions
            .by_height
            .get(&height)
            .copied()
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
        (None, None) => dimensions
            .unqualified
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::auction::types::MediaType;

    fn slot(id: &str, formats: &[(u32, u32)]) -> AdSlot {
        AdSlot {
            id: id.to_string(),
            formats: formats
                .iter()
                .map(|&(width, height)| AdFormat {
                    media_type: MediaType::Banner,
                    width,
                    height,
                })
                .collect(),
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        }
    }

    #[test]
    fn dimensions_are_admitted_exactly_inferred_uniquely_and_refused_otherwise() {
        let index = build_bid_dimension_index_from_slots(&[slot(
            "one",
            &[(300, 250), (300, 600), (320, 50)],
        )]);

        assert_eq!(
            resolve_bid_dimensions(&index, "one", Some(300), Some(250)),
            Ok((300, 250)),
            "an exact match is admitted"
        );
        assert_eq!(
            resolve_bid_dimensions(&index, "one", None, Some(50)),
            Ok((320, 50)),
            "a unique height infers the width"
        );
        assert_eq!(
            resolve_bid_dimensions(&index, "one", Some(300), None),
            Err(BidRejectionReason::AmbiguousDimensions),
            "a width two formats share is ambiguous"
        );
        assert_eq!(
            resolve_bid_dimensions(&index, "one", None, None),
            Err(BidRejectionReason::AmbiguousDimensions),
            "no dimensions against several formats is ambiguous"
        );
        assert_eq!(
            resolve_bid_dimensions(&index, "one", Some(999), Some(1)),
            Err(BidRejectionReason::DimensionMismatch),
            "dimensions no format asked for are refused"
        );
        assert_eq!(
            resolve_bid_dimensions(&index, "other", Some(300), Some(250)),
            Err(BidRejectionReason::UnrequestedImpression),
            "a slot the request did not carry is refused"
        );
    }

    #[test]
    fn the_first_slot_wins_when_several_share_an_id() {
        let index = build_bid_dimension_index_from_slots(&[
            slot("one", &[(300, 250)]),
            slot("one", &[(728, 90)]),
        ]);
        assert_eq!(
            resolve_bid_dimensions(&index, "one", None, None),
            Ok((300, 250))
        );
    }

    #[test]
    fn a_dimension_is_a_positive_whole_number_or_absent() {
        for (value, expected) in [
            (json!({}), Ok(None)),
            (json!({"w": null}), Ok(None)),
            (json!({"w": 300}), Ok(Some(300))),
            (json!({"w": 300.0}), Ok(Some(300))),
            (json!({"w": 300.5}), Err(BidRejectionReason::InvalidBid)),
            (json!({"w": 0}), Err(BidRejectionReason::InvalidBid)),
            (json!({"w": -1}), Err(BidRejectionReason::InvalidBid)),
            (
                json!({"w": 4_294_967_296.0}),
                Err(BidRejectionReason::InvalidBid),
            ),
            (json!({"w": "300"}), Err(BidRejectionReason::InvalidBid)),
        ] {
            assert_eq!(
                parse_optional_bid_dimension(&value, "w"),
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn rejections_are_counted_by_reason_and_attached_only_when_there_are_any() {
        let mut none = AuctionResponse::no_bid("source", 0);
        ResponseAdmissionDiagnostics::default().attach_to(&mut none);
        assert!(
            !none.metadata.contains_key("response_admission"),
            "nothing rejected writes no diagnostics"
        );

        let mut diagnostics = ResponseAdmissionDiagnostics::default();
        diagnostics.record(BidRejectionReason::InvalidBid);
        diagnostics.record(BidRejectionReason::InvalidBid);
        diagnostics.record(BidRejectionReason::DimensionMismatch);
        let mut some = AuctionResponse::no_bid("source", 0);
        diagnostics.attach_to(&mut some);
        assert_eq!(
            some.metadata["response_admission"],
            json!({
                "rejected_bid_count": 3,
                "rejection_reasons": {"dimension_mismatch": 1, "invalid_bid": 2},
            })
        );
    }
}
