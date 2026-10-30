//! Application-visible mobile trace routing and response policy.

mod actions;
mod auction;
mod carry;
mod slot_refs;
pub(crate) use carry::{TraceAuctionCarry, TraceProviderObservation};
pub(crate) use slot_refs::TraceClientSlotRefs;
mod context;
mod cookies;
mod dispatch;
mod routes;
mod shell;
mod types;

#[cfg(test)]
pub(crate) use actions::action_response;
pub use auction::{
    DiagnosticAuctionId, TraceAuctionEvidenceV1, TraceAuctionSource, TraceAuctionTerminalReason,
    TraceAuctionTerminalStatus, TraceAuctionTransportV1, TraceProviderRole, TraceProviderStatus,
    TraceSlotCandidate, TraceSlotRef, TraceTokenError,
};
pub use context::{ContextProjectionError, project_request_context};
pub use cookies::inspect_cookies;
pub use dispatch::{TraceCaptureGate, TraceMetadata, TracePreDispatchHook};
#[cfg(test)]
pub(crate) use routes::state_response;
pub use routes::{TraceDispatch, TracePreflight, is_trace_path, preflight};
pub use types::{
    CookieHealth, CookieHealthDetail, CookieHealthState, TraceCookies, TraceNetwork,
    TraceRequestContextV1,
};

/// Marks a local trace response that bypasses ordinary adapter finalization.
///
/// Adapters must preserve the trace response contract through conversion.
#[derive(Clone, Copy, Debug)]
pub struct TraceTerminalResponse;
