//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod builder;
pub mod mapping;
pub mod sink;
pub mod types;

pub use builder::build_auction_events;
pub use mapping::{build_completed_auction_events, completed_outcome, provider_calls_from_result};
pub use sink::{AuctionEventSink, InMemorySink, NoopSink};
pub use types::{
    to_ndjson, AuctionEventRow, AuctionObservationContext, AuctionSource, EventKind,
    ProviderCallOutcome, ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
