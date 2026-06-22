//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod types;

pub use types::{
    AuctionObservationContext, AuctionSource, EventKind, ProviderCallOutcome,
    ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
};
