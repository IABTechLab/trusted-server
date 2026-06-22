//! Pure auction telemetry: row types, builder, and sink abstraction.
//!
//! Wiring into the orchestrator, SSAT dispatch/collect, and the Fastly sink
//! lives in separate modules; this module performs no I/O.

pub mod types;

pub use types::{
    AuctionSource, EventKind, ProviderCallStatus, ProviderRole, TerminalStatus,
};
