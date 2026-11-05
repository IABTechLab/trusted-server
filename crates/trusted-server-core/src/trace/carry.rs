//! Private, request-local observations at the live auction boundary.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use web_time::Instant;

use crate::auction::types::{AdSlot, AuctionResponse, Bid, BidStatus};

use super::auction::projection::{
    ObservedTraceAuction, ObservedTraceProviderCall, ObservedTraceSlot, ObservedTraceTruncation,
    project_auction_transport,
};

use super::auction::{
    DiagnosticAuctionId, TraceAuctionSource, TraceAuctionTerminalReason,
    TraceAuctionTerminalStatus, TraceAuctionTransportV1, TraceProviderRole, TraceProviderStatus,
    TraceSlotCandidate, TraceSlotRef,
};

const PROVIDER_LIMIT: usize = 16;
const SLOT_LIMIT: usize = 64;
const SIZE_LIMIT: usize = 16;

/// Ephemeral facts; provider identities, bid bodies, and creatives are never retained.
#[derive(Clone)]
pub(crate) struct TraceAuctionCarry {
    token: DiagnosticAuctionId,
    state: Arc<Mutex<TraceAuctionState>>,
}

struct TraceAuctionState {
    observed: ObservedTraceAuction,
    started_at: Instant,
    provider_starts: Vec<Instant>,
    provider_count: usize,
    slot_keys: Vec<String>,
    slot_refs: Vec<TraceSlotRef>,
    buckets: HashMap<String, SlotBucket>,
    terminal: bool,
    invalid: bool,
    collection_failed: bool,
    provider_failed: bool,
    transport: Option<TraceAuctionTransportV1>,
}

#[derive(Default)]
struct SlotBucket {
    accepted_instances: usize,
    returned_bid_count: u16,
}

impl TraceAuctionState {
    fn refresh_projection(&mut self) {
        if self.terminal {
            self.transport = Some(if self.invalid {
                TraceAuctionTransportV1::unavailable()
            } else {
                project_auction_transport(&self.observed)
            });
        }
    }
}

/// One observation is consumed at its actual response or failure boundary.
pub(crate) struct TraceProviderObservation {
    number: Option<usize>,
    started_at: Instant,
}

/// Cancellation terminalizes synchronously without constructing telemetry.
pub(crate) struct TraceAuctionCancellationGuard {
    carry: TraceAuctionCarry,
    armed: bool,
}

impl TraceAuctionCancellationGuard {
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TraceAuctionCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.carry.finish(
                TraceAuctionTerminalStatus::Abandoned,
                Some(TraceAuctionTerminalReason::Unknown),
            );
        }
    }
}

impl TraceAuctionCarry {
    pub(crate) fn capture_if_enabled(
        active: bool,
        source: TraceAuctionSource,
        slots: &[AdSlot],
    ) -> Option<Self> {
        if !active {
            return None;
        }
        let token = DiagnosticAuctionId::generate();
        let mut state = TraceAuctionState {
            observed: ObservedTraceAuction {
                diagnostic_auction_id: token.clone(),
                source,
                terminal_status: TraceAuctionTerminalStatus::Completed,
                terminal_reason: None,
                total_time: None,
                provider_calls: Vec::new(),
                slots: Vec::new(),
                truncation: ObservedTraceTruncation::default(),
            },
            started_at: Instant::now(),
            provider_starts: Vec::new(),
            provider_count: 0,
            slot_keys: Vec::new(),
            slot_refs: Vec::new(),
            buckets: HashMap::new(),
            terminal: false,
            invalid: slots.len() > usize::from(u16::MAX),
            collection_failed: false,
            provider_failed: false,
            transport: None,
        };
        if !state.invalid {
            for (index, slot) in slots.iter().enumerate() {
                state
                    .buckets
                    .entry(slot.id.clone())
                    .or_default()
                    .accepted_instances += 1;
                let slot_ref = TraceSlotRef::generate();
                state.slot_refs.push(slot_ref.clone());
                if index >= SLOT_LIMIT {
                    state.observed.truncation.omitted_slots += 1;
                    continue;
                }
                let retained_sizes = slot.formats.len().min(SIZE_LIMIT);
                let omitted_sizes = slot.formats.len() - retained_sizes;
                if let Some(omitted) = state
                    .observed
                    .truncation
                    .omitted_nested_values
                    .checked_add(omitted_sizes)
                {
                    state.observed.truncation.omitted_nested_values = omitted;
                    state.invalid |= omitted > usize::from(u16::MAX);
                } else {
                    state.invalid = true;
                }
                state.slot_keys.push(slot.id.clone());
                state.observed.slots.push(ObservedTraceSlot {
                    slot_number: index + 1,
                    slot_ref,
                    requested_sizes: slot
                        .formats
                        .iter()
                        .take(SIZE_LIMIT)
                        .map(|format| [format.width, format.height])
                        .collect(),
                    returned_bid_count: 0,
                    candidate: TraceSlotCandidate::NoCandidate,
                    selected_creative_size: None,
                });
            }
        }
        let carry = Self {
            token,
            state: Arc::new(Mutex::new(state)),
        };
        if slots.is_empty() {
            carry.finish(
                TraceAuctionTerminalStatus::Skipped,
                Some(TraceAuctionTerminalReason::NoEligibleSlots),
            );
        }
        Some(carry)
    }

    pub(crate) fn token(&self) -> DiagnosticAuctionId {
        self.token.clone()
    }

    pub(crate) fn slot_ref(&self, index: usize) -> Option<TraceSlotRef> {
        self.state.lock().ok()?.slot_refs.get(index).cloned()
    }

    pub(crate) fn cancellation_guard(&self) -> TraceAuctionCancellationGuard {
        TraceAuctionCancellationGuard {
            carry: self.clone(),
            armed: true,
        }
    }

    pub(crate) fn bind_client_refs(&self, refs: &[Option<TraceSlotRef>]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if refs.len() != state.slot_refs.len() {
            return;
        }
        let mut seen = HashSet::new();
        let mut duplicates = HashSet::new();
        for token in refs.iter().flatten() {
            if !seen.insert(token.clone()) {
                duplicates.insert(token.clone());
            }
        }
        // Reserve even duplicated client refs so a fallback can never masquerade
        // as a client association that was deliberately declined.
        let mut used = seen;
        for (index, client) in refs.iter().enumerate() {
            let token =
                if let Some(token) = client.as_ref().filter(|token| !duplicates.contains(*token)) {
                    token.clone()
                } else {
                    let mut token = state.slot_refs[index].clone();
                    while !used.insert(token.clone()) {
                        token = TraceSlotRef::generate();
                    }
                    token
                };
            state.slot_refs[index] = token.clone();
            if let Some(slot) = state.observed.slots.get_mut(index) {
                slot.slot_ref = token;
            }
        }
        state.refresh_projection();
    }

    pub(crate) fn observe_delivery(
        &self,
        winners: &HashMap<String, Bid>,
        delivered: &HashSet<String>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for index in 0..state.observed.slots.len() {
            let key = &state.slot_keys[index];
            let ambiguous = state
                .buckets
                .get(key)
                .is_none_or(|bucket| bucket.accepted_instances > 1);
            let winner = (!ambiguous).then(|| winners.get(key)).flatten();
            let candidate = if ambiguous {
                TraceSlotCandidate::Unknown
            } else if winner.is_some() {
                if delivered.contains(key) {
                    TraceSlotCandidate::Selected
                } else {
                    TraceSlotCandidate::SelectedUnrenderable
                }
            } else {
                TraceSlotCandidate::NoCandidate
            };
            let selected_size = winner.map(|bid| [bid.width, bid.height]);
            let slot = &mut state.observed.slots[index];
            slot.candidate = candidate;
            slot.selected_creative_size = selected_size;
        }
        // Delivery is a later observation. It cannot remint identity or rewrite
        // terminal status/time; pure projection also avoids accumulating omissions.
        state.refresh_projection();
    }

    pub(crate) fn launch_provider(&self, role: TraceProviderRole) -> TraceProviderObservation {
        let started_at = Instant::now();
        let mut provider = TraceProviderObservation {
            number: None,
            started_at,
        };
        let Ok(mut state) = self.state.lock() else {
            return provider;
        };
        if state.terminal {
            return provider;
        }
        let Some(number) = state.provider_count.checked_add(1) else {
            state.invalid = true;
            return provider;
        };
        state.provider_count = number;
        state.invalid |= number > usize::from(u16::MAX);
        provider.number = Some(number);
        if number <= PROVIDER_LIMIT {
            state.provider_starts.push(started_at);
            state
                .observed
                .provider_calls
                .push(ObservedTraceProviderCall {
                    provider_number: number,
                    role,
                    status: TraceProviderStatus::Pending,
                    response_time: None,
                    returned_bid_count: 0,
                });
        } else {
            state.observed.truncation.omitted_provider_calls = number - PROVIDER_LIMIT;
        }
        provider
    }

    pub(crate) fn observe_response(
        &self,
        provider: TraceProviderObservation,
        response: &AuctionResponse,
    ) {
        let status = match response.status {
            BidStatus::Success if response.bids.is_empty() => TraceProviderStatus::NoBid,
            BidStatus::Success => TraceProviderStatus::Success,
            BidStatus::NoBid => TraceProviderStatus::NoBid,
            BidStatus::Error => TraceProviderStatus::Error,
            BidStatus::Pending => TraceProviderStatus::Pending,
        };
        self.record_provider_outcome(
            provider,
            status,
            response.bids.len(),
            response.bids.iter().map(|bid| bid.slot_id.as_str()),
        );
    }

    pub(crate) fn observe_failure(&self, provider: TraceProviderObservation) {
        self.record_provider_outcome(provider, TraceProviderStatus::Error, 0, std::iter::empty());
    }

    pub(crate) fn observe_collection_failure(&self) {
        if let Ok(mut state) = self.state.lock()
            && !state.terminal
        {
            state.collection_failed = true;
        }
    }

    // Consume the private observation so a completion cannot reuse its launch.
    #[allow(clippy::needless_pass_by_value)]
    fn record_provider_outcome<'a>(
        &self,
        provider: TraceProviderObservation,
        status: TraceProviderStatus,
        returned_bid_count: usize,
        slot_ids: impl Iterator<Item = &'a str>,
    ) {
        let Some(number) = provider.number else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.terminal {
            return;
        }
        state.invalid |= returned_bid_count > usize::from(u16::MAX);
        state.provider_failed |= status == TraceProviderStatus::Error;
        if let Some(observed) = state.observed.provider_calls.get_mut(number - 1) {
            observed.status = status;
            observed.response_time = Some(provider.started_at.elapsed());
            observed.returned_bid_count = returned_bid_count;
        }
        // Only accepted routing keys enter this bounded private table. Tail
        // counts remain required source facts even when details are omitted.
        for key in slot_ids {
            if let Some(bucket) = state.buckets.get_mut(key) {
                if let Some(count) = bucket.returned_bid_count.checked_add(1) {
                    bucket.returned_bid_count = count;
                } else {
                    state.invalid = true;
                }
            }
        }
    }

    pub(crate) fn finish(
        &self,
        status: TraceAuctionTerminalStatus,
        reason: Option<TraceAuctionTerminalReason>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.observed.diagnostic_auction_id = self.token.clone();
        state.observed.terminal_status =
            if status == TraceAuctionTerminalStatus::Completed && state.collection_failed {
                TraceAuctionTerminalStatus::ExecutionFailed
            } else {
                status
            };
        state.observed.terminal_reason =
            if status == TraceAuctionTerminalStatus::Completed && state.collection_failed {
                Some(TraceAuctionTerminalReason::CollectionFailed)
            } else {
                reason
            };
        state.observed.total_time = Some(state.started_at.elapsed());
        for index in 0..state.observed.slots.len() {
            let Some(bucket) = state.buckets.get(&state.slot_keys[index]) else {
                continue;
            };
            let count = usize::from(bucket.returned_bid_count);
            let ambiguous = bucket.accepted_instances > 1;
            let slot = &mut state.observed.slots[index];
            slot.returned_bid_count = count;
            slot.candidate = if ambiguous || count > 0 {
                TraceSlotCandidate::Unknown
            } else {
                TraceSlotCandidate::NoCandidate
            };
        }
        if status == TraceAuctionTerminalStatus::Abandoned {
            for index in 0..state.observed.provider_calls.len() {
                let duration = state.provider_starts[index].elapsed();
                let provider = &mut state.observed.provider_calls[index];
                if provider.status == TraceProviderStatus::Pending {
                    provider.status = TraceProviderStatus::Abandoned;
                    provider.response_time = Some(duration);
                }
            }
        }
        state.refresh_projection();
    }

    pub(crate) fn finish_dispatch_failed(&self, no_provider_launched: bool) {
        let reason = if no_provider_launched {
            TraceAuctionTerminalReason::NoProviderLaunched
        } else if self.state.lock().is_ok_and(|state| state.provider_failed) {
            TraceAuctionTerminalReason::ProviderExecutionFailed
        } else {
            TraceAuctionTerminalReason::Unknown
        };
        self.finish(TraceAuctionTerminalStatus::DispatchFailed, Some(reason));
    }

    pub(crate) fn transport(&self) -> Option<TraceAuctionTransportV1> {
        match self.state.lock() {
            Ok(state) => state.transport.clone(),
            Err(_) => Some(TraceAuctionTransportV1::unavailable()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use crate::auction::types::{AdFormat, AdSlot, MediaType};

    use super::*;

    fn slot(id: &str) -> AdSlot {
        AdSlot {
            id: id.to_string(),
            formats: vec![AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 250,
            }],
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        }
    }

    fn capture(slots: &[AdSlot]) -> TraceAuctionCarry {
        TraceAuctionCarry::capture_if_enabled(true, TraceAuctionSource::AuctionApi, slots)
            .expect("should capture a gated auction")
    }

    fn transport(carry: &TraceAuctionCarry) -> Value {
        serde_json::to_value(
            carry
                .transport()
                .expect("should project a terminal auction"),
        )
        .expect("should serialize the bounded transport")
    }

    #[test]
    fn trace_auction_identity_is_stable_across_terminal_outcomes() {
        for (status, reason) in [
            (TraceAuctionTerminalStatus::Completed, None),
            (
                TraceAuctionTerminalStatus::Skipped,
                Some(TraceAuctionTerminalReason::PolicySkipped),
            ),
            (
                TraceAuctionTerminalStatus::Skipped,
                Some(TraceAuctionTerminalReason::NoEligibleSlots),
            ),
            (
                TraceAuctionTerminalStatus::DispatchFailed,
                Some(TraceAuctionTerminalReason::NoProviderLaunched),
            ),
            (
                TraceAuctionTerminalStatus::ExecutionFailed,
                Some(TraceAuctionTerminalReason::ProviderExecutionFailed),
            ),
            (
                TraceAuctionTerminalStatus::Abandoned,
                Some(TraceAuctionTerminalReason::Unknown),
            ),
        ] {
            let carry = capture(&[slot("example-slot")]);
            assert!(
                carry.slot_ref(0).is_some(),
                "should retain the definitive accepted occurrence reference"
            );
            let token = carry.token();
            let outer = carry.clone();
            assert!(
                carry.transport().is_none(),
                "should not invent a pending terminal result"
            );

            carry.finish(status, reason);
            outer.finish(
                TraceAuctionTerminalStatus::Abandoned,
                Some(TraceAuctionTerminalReason::Unknown),
            );

            assert_eq!(outer.token(), token, "should retain the pre-dispatch token");
            let value = transport(&outer);
            assert_eq!(
                value["evidence"]["diagnostic_auction_id"],
                token.as_str(),
                "should use the same public token"
            );
            assert_eq!(
                value["evidence"]["terminal_status"],
                json!(status),
                "should terminalize idempotently"
            );
            assert_eq!(
                value["evidence"]["terminal_reason"],
                json!(reason),
                "should preserve the directly observed reason"
            );
        }
    }

    #[test]
    fn trace_auction_disabled_allocates_no_carry() {
        assert!(
            TraceAuctionCarry::capture_if_enabled(
                false,
                TraceAuctionSource::AuctionApi,
                &[slot("example-slot")]
            )
            .is_none(),
            "should not allocate trace state when the frozen gate is false"
        );
    }

    #[test]
    fn trace_auction_empty_definitive_slots_terminalize_without_dispatch() {
        let carry = capture(&[]);
        assert_eq!(
            transport(&carry)["evidence"]["terminal_reason"],
            "no_eligible_slots",
            "should retain no-slot facts without fabricating telemetry or dispatch"
        );
    }

    #[test]
    fn trace_auction_provider_numbers_follow_launch_order() {
        let carry = capture(&[slot("example-slot")]);
        let first = carry.launch_provider(TraceProviderRole::Bidder);
        let second = carry.launch_provider(TraceProviderRole::Bidder);
        let third = carry.launch_provider(TraceProviderRole::Mediator);

        carry.record_provider_outcome(second, TraceProviderStatus::NoBid, 0, std::iter::empty());
        carry.record_provider_outcome(
            third,
            TraceProviderStatus::Success,
            2,
            std::iter::repeat_n("example-slot", 2),
        );
        carry.record_provider_outcome(first, TraceProviderStatus::Error, 0, std::iter::empty());
        carry.finish(TraceAuctionTerminalStatus::Completed, None);

        let value = transport(&carry);
        let providers = value["evidence"]["provider_calls"]
            .as_array()
            .expect("should retain provider facts");
        assert_eq!(
            providers
                .iter()
                .map(|provider| provider["provider_number"].clone())
                .collect::<Vec<_>>(),
            vec![json!(1), json!(2), json!(3)],
            "should number actual launches rather than completions"
        );
        assert_eq!(
            providers[0]["status"], "error",
            "should retain the first call's disposition"
        );
        assert_eq!(
            providers[1]["status"], "no_bid",
            "should retain a zero-bid response"
        );
        assert_eq!(
            providers[2]["role"], "mediator",
            "should distinguish mediation without an identity"
        );
        assert_eq!(
            value["evidence"]["slots"][0]["returned_bid_count"], 2,
            "should count actual returned records"
        );
    }

    #[test]
    fn trace_auction_checks_tail_bucket_counts_before_prefix_projection() {
        let slots = (0..65)
            .map(|index| slot(&format!("example-slot-{index}")))
            .collect::<Vec<_>>();
        let carry = capture(&slots);
        for _ in 0..4 {
            let provider = carry.launch_provider(TraceProviderRole::Bidder);
            carry.record_provider_outcome(
                provider,
                TraceProviderStatus::Success,
                20_000,
                std::iter::repeat_n("example-slot-64", 20_000),
            );
        }
        carry.finish(TraceAuctionTerminalStatus::Completed, None);
        assert_eq!(
            transport(&carry),
            json!({"schema_version":1,"unavailable_reason":"evidence_projection_failed"}),
            "should reject a required count overflow outside the retained prefix"
        );
    }

    #[test]
    fn trace_auction_retains_valid_disjoint_tail_counts_without_global_saturation() {
        let slots = (0..66)
            .map(|index| slot(&format!("example-slot-{index}")))
            .collect::<Vec<_>>();
        let carry = capture(&slots);
        for id in ["example-slot-64", "example-slot-65"] {
            let provider = carry.launch_provider(TraceProviderRole::Bidder);
            carry.record_provider_outcome(
                provider,
                TraceProviderStatus::Success,
                usize::from(u16::MAX),
                std::iter::repeat_n(id, usize::from(u16::MAX)),
            );
        }
        carry.finish(TraceAuctionTerminalStatus::Completed, None);
        let value = transport(&carry);
        assert_eq!(
            value["evidence"]["slots"]
                .as_array()
                .expect("should retain the prefix")
                .len(),
            64,
            "should preserve the public slot bound"
        );
        assert_eq!(
            value["evidence"]["truncation"]["omitted_slots"], 2,
            "should count omitted definitive slots exactly"
        );
        assert_eq!(
            value["evidence"]["provider_calls"][0]["returned_bid_count"],
            u16::MAX,
            "should retain valid per-provider counts without global clamping"
        );
    }

    #[test]
    fn trace_auction_duplicate_keys_share_non_disjoint_counts_and_unknown_candidates() {
        let carry = capture(&[slot("example-duplicate"), slot("example-duplicate")]);
        let provider = carry.launch_provider(TraceProviderRole::Bidder);
        carry.record_provider_outcome(
            provider,
            TraceProviderStatus::Success,
            1,
            std::iter::once("example-duplicate"),
        );
        carry.finish(TraceAuctionTerminalStatus::Completed, None);
        let value = transport(&carry);
        let slots = value["evidence"]["slots"]
            .as_array()
            .expect("should retain both definitive slots");
        assert_ne!(
            slots[0]["slot_ref"], slots[1]["slot_ref"],
            "should retain distinct auction-local opaque refs"
        );
        for slot in slots {
            assert_eq!(
                slot["returned_bid_count"], 1,
                "should expose the shared bucket without splitting it"
            );
            assert_eq!(
                slot["candidate"], "unknown",
                "should not infer a duplicate-key winner"
            );
        }
    }

    #[test]
    fn trace_auction_abandonment_retains_launch_order_and_is_terminal() {
        let carry = capture(&[slot("example-slot")]);
        let _first = carry.launch_provider(TraceProviderRole::Bidder);
        let _second = carry.launch_provider(TraceProviderRole::Mediator);
        carry.finish(
            TraceAuctionTerminalStatus::Abandoned,
            Some(TraceAuctionTerminalReason::Unknown),
        );
        carry.finish(TraceAuctionTerminalStatus::Completed, None);
        let value = transport(&carry);
        assert_eq!(
            value["evidence"]["terminal_status"], "abandoned",
            "should not overwrite abandoned facts"
        );
        assert_eq!(
            value["evidence"]["provider_calls"][0]["status"], "abandoned",
            "should synchronously terminalize outstanding calls"
        );
        assert_eq!(
            value["evidence"]["provider_calls"][1]["provider_number"], 2,
            "should retain mediator launch order"
        );
    }
    fn selected_bid(id: &str) -> Bid {
        serde_json::from_value(json!({"slot_id":id,"price":1.0,"currency":"USD","creative":"<div>Example creative</div>","bidder":"fictional-bidder","width":300,"height":250,"metadata":{}})).expect("should build a fictional selected bid")
    }

    #[test]
    fn trace_slot_conversion_client_refs_echo_only_unique_accepted_tokens() {
        let first = TraceSlotRef::parse("ts-slot-00000000-0000-4000-8000-000000000001")
            .expect("should parse the first client ref");
        let repeated = TraceSlotRef::parse("ts-slot-00000000-0000-4000-8000-000000000002")
            .expect("should parse the repeated client ref");
        let carry = capture(&[
            slot("first"),
            slot("duplicate"),
            slot("duplicate"),
            slot("missing"),
        ]);

        carry.bind_client_refs(&[
            Some(first.clone()),
            Some(repeated.clone()),
            Some(repeated.clone()),
            None,
        ]);
        carry.finish(TraceAuctionTerminalStatus::Completed, None);

        let value = transport(&carry);
        let slots = value["evidence"]["slots"]
            .as_array()
            .expect("should retain accepted slots");
        assert_eq!(
            slots[0]["slot_ref"],
            first.as_str(),
            "should echo the exact unique accepted client ref"
        );
        for slot in &slots[1..] {
            assert_ne!(
                slot["slot_ref"],
                repeated.as_str(),
                "should replace every repeated accepted token with a fresh server ref"
            );
        }
        assert_eq!(
            slots
                .iter()
                .map(|slot| slot["slot_ref"]
                    .as_str()
                    .expect("should retain opaque string refs"))
                .collect::<HashSet<_>>()
                .len(),
            4,
            "should keep all accepted occurrence refs distinct"
        );
    }

    #[test]
    fn trace_slot_conversion_delivery_reprojects_without_rewriting_terminal_facts() {
        let carry = capture(&[
            slot("selected"),
            slot("dropped"),
            slot("lost"),
            slot("duplicate"),
            slot("duplicate"),
            slot("invalid-size"),
        ]);
        let mut invalid_size = selected_bid("invalid-size");
        invalid_size.width = 0;
        let bids = vec![
            selected_bid("selected"),
            selected_bid("dropped"),
            selected_bid("lost"),
            selected_bid("duplicate"),
            invalid_size,
        ];
        let provider = carry.launch_provider(TraceProviderRole::Bidder);
        carry.observe_response(
            provider,
            &AuctionResponse::success("fictional-provider", bids.clone(), 0),
        );
        carry.finish(TraceAuctionTerminalStatus::Completed, None);
        let before = transport(&carry);
        let winners = bids
            .into_iter()
            .filter(|bid| bid.slot_id != "lost")
            .map(|bid| (bid.slot_id.clone(), bid))
            .collect();

        carry.observe_delivery(
            &winners,
            &HashSet::from([
                "selected".to_string(),
                "duplicate".to_string(),
                "invalid-size".to_string(),
            ]),
        );

        let after = transport(&carry);
        assert_eq!(
            after["evidence"]["total_time_ms"], before["evidence"]["total_time_ms"],
            "should preserve immutable terminal timing"
        );
        assert_eq!(
            after["evidence"]["terminal_status"], "completed",
            "should preserve the observed terminal status"
        );
        let slots = after["evidence"]["slots"]
            .as_array()
            .expect("should retain definitive slots");
        assert_eq!(
            slots[0]["candidate"], "selected",
            "should require actual delivery inclusion for selected"
        );
        assert_eq!(
            slots[0]["selected_creative_size"],
            json!([300, 250]),
            "should copy only the selected dimensions"
        );
        assert_eq!(
            slots[1]["candidate"], "selected_unrenderable",
            "should preserve a winner omitted by response conversion"
        );
        assert_eq!(
            slots[2]["candidate"], "no_candidate",
            "should preserve actual no-winner selection despite a returned bid"
        );
        for slot in &slots[3..5] {
            assert_eq!(
                slot["candidate"], "unknown",
                "should not infer an instance disposition from a duplicate routing key"
            );
            assert!(
                slot.get("selected_creative_size").is_none(),
                "should omit ambiguous instance dimensions"
            );
        }
        assert!(
            slots[5].get("selected_creative_size").is_none(),
            "should omit malformed optional dimensions"
        );
        assert_eq!(
            after["evidence"]["truncation"]["omitted_nested_values"], 1,
            "should count the malformed optional size exactly once"
        );
        carry.observe_delivery(
            &winners,
            &HashSet::from([
                "selected".to_string(),
                "duplicate".to_string(),
                "invalid-size".to_string(),
            ]),
        );
        assert_eq!(
            transport(&carry),
            after,
            "should reproject without accumulating omission counts or rewriting terminal timing"
        );
    }
}
