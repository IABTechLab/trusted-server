//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod builder;
pub mod context;
pub mod emit;
pub mod mapping;
pub mod sink;
pub mod types;

pub use builder::build_auction_events;
pub use context::build_observation_context;
pub use emit::emit_completed_auction_telemetry;
pub use mapping::{build_completed_auction_events, completed_outcome, provider_calls_from_result};
pub use sink::{AuctionEventSink, InMemorySink, NoopSink};
pub use types::{
    to_json_line_with_event_ts, to_ndjson, AuctionEventRow, AuctionObservationContext,
    AuctionSource, EventKind, ProviderCallOutcome, ProviderCallStatus, ProviderRole,
    TerminalOutcome, TerminalStatus,
};
