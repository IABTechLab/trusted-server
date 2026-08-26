//! Per-request phase timing collection and Server-Timing rendering.
//!
//! Collection is always-on and infallible: saturating math, lock failure
//! drops the sample, no panics. See the design spec
//! `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{HeaderName, HeaderValue, Response};
// `std::time::Instant::now()` panics on `wasm32-unknown-unknown` (the
// Cloudflare adapter's target); `web_time` re-exports std's `Instant` on
// every other target.
use web_time::Instant;

use crate::cache_policy::cache_control_headers_are_private_or_no_store;

/// `Server-Timing` header name. Not present in the `http` crate's `header`
/// module (unlike `CACHE_CONTROL` etc.), so declared locally following the
/// same `HeaderName::from_static` pattern used in
/// `trusted_server_core::constants`.
const HEADER_SERVER_TIMING: HeaderName = HeaderName::from_static("server-timing");

/// Number of [`Phase`] variants; sizes the fixed-slot duration array in
/// [`Inner`].
const PHASE_COUNT: usize = 8;

/// A distinct stage of request handling that duration can be attributed to.
///
/// Variants map to fixed slots in [`RequestTimings`], in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Time spent constructing the app/handler before request processing
    /// begins.
    AppBuild,
    /// Time spent in request/response filtering (e.g. HTML rewriting).
    Filter,
    /// Time spent resolving geographic signals for the request.
    Geo,
    /// Time spent reading or writing the Edge Cookie key-value store.
    EcKv,
    /// Time spent waiting on the origin fetch.
    Origin,
    /// Time spent looking up a cached template.
    TemplateCacheLookup,
    /// Time spent waiting on the auction. Row-only: never rendered as a
    /// `Server-Timing` header entry.
    AuctionWait,
    /// Time spent streaming the response body. Row-only: never rendered as
    /// a `Server-Timing` header entry.
    Stream,
}

impl Phase {
    /// Maps this variant to its fixed slot in the [`Inner::phases`] array.
    fn index(self) -> usize {
        match self {
            Self::AppBuild => 0,
            Self::Filter => 1,
            Self::Geo => 2,
            Self::EcKv => 3,
            Self::Origin => 4,
            Self::TemplateCacheLookup => 5,
            Self::AuctionWait => 6,
            Self::Stream => 7,
        }
    }

    /// `Server-Timing` header entry name; row-only phases return `None`.
    fn header_name(self) -> Option<&'static str> {
        match self {
            Self::AppBuild => Some("ts-appbuild"),
            Self::Filter => Some("ts-filter"),
            Self::Geo => Some("ts-geo"),
            Self::EcKv => Some("ts-kv"),
            Self::Origin => Some("ts-origin"),
            Self::TemplateCacheLookup => Some("ts-template-cache"),
            Self::AuctionWait | Self::Stream => None,
        }
    }
}

/// Where in the response the auction wait occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionWaitPlacement {
    /// The auction was awaited before response headers were sent.
    PreHeader,
    /// The auction was awaited while streaming the response body.
    InStream,
}

/// Mutable state behind [`RequestTimings`], guarded by a [`Mutex`].
struct Inner {
    /// Instant the request started; the reference point for elapsed marks.
    t0: Instant,
    /// Accumulated duration per [`Phase`], indexed by [`Phase::index`].
    phases: [Option<Duration>; PHASE_COUNT],
    /// Elapsed time at the first [`RequestTimings::mark_headers_ready`] call.
    headers_ready_total: Option<Duration>,
    /// Elapsed time at the first [`RequestTimings::mark_request_elapsed`]
    /// call.
    request_elapsed: Option<Duration>,
    /// Placement recorded by the most recent
    /// [`RequestTimings::record_auction_wait`] call.
    auction_wait_placement: Option<AuctionWaitPlacement>,
    /// Response body size in bytes, set via
    /// [`RequestTimings::set_resp_bytes`].
    resp_bytes: Option<u64>,
}

/// Per-request phase timing collector.
///
/// Cheap to clone (an [`Arc`] handle) and safe to share across threads and
/// async tasks handling the same request. Every method is infallible: lock
/// contention or poisoning silently drops the sample rather than blocking or
/// panicking.
#[derive(Clone)]
pub struct RequestTimings(Arc<Mutex<Inner>>);

impl RequestTimings {
    /// Starts a new collector with its clock reference (`t0`) set to now.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Inner {
            t0: Instant::now(),
            phases: [None; PHASE_COUNT],
            headers_ready_total: None,
            request_elapsed: None,
            auction_wait_placement: None,
            resp_bytes: None,
        })))
    }

    /// Accumulates `dur` into `phase`'s running total.
    ///
    /// Repeated calls for the same phase saturate-add rather than overwrite.
    /// Drops the sample silently on lock contention or poisoning.
    pub fn record(&self, phase: Phase, dur: Duration) {
        let Ok(mut inner) = self.0.try_lock() else {
            return;
        };
        let index = phase.index();
        let accumulated = inner.phases[index]
            .unwrap_or(Duration::ZERO)
            .saturating_add(dur);
        inner.phases[index] = Some(accumulated);
    }

    /// Records an auction wait duration under [`Phase::AuctionWait`] and
    /// stores its placement.
    ///
    /// Drops the sample silently on lock contention or poisoning.
    pub fn record_auction_wait(&self, placement: AuctionWaitPlacement, dur: Duration) {
        let Ok(mut inner) = self.0.try_lock() else {
            return;
        };
        let index = Phase::AuctionWait.index();
        let accumulated = inner.phases[index]
            .unwrap_or(Duration::ZERO)
            .saturating_add(dur);
        inner.phases[index] = Some(accumulated);
        inner.auction_wait_placement = Some(placement);
    }

    /// Starts a guard that records elapsed time into `phase` when dropped.
    #[must_use]
    pub fn span(&self, phase: Phase) -> PhaseSpan {
        PhaseSpan {
            timings: self.clone(),
            phase,
            started: Instant::now(),
        }
    }

    /// Stamps the elapsed time since `t0` as `headers_ready_total`, the
    /// first time this is called.
    ///
    /// Subsequent calls are no-ops (first call wins). Drops the sample
    /// silently on lock contention or poisoning.
    pub fn mark_headers_ready(&self) {
        let Ok(mut inner) = self.0.try_lock() else {
            return;
        };
        if inner.headers_ready_total.is_none() {
            inner.headers_ready_total = Some(inner.t0.elapsed());
        }
    }

    /// Stamps the elapsed time since `t0` as `request_elapsed`, the first
    /// time this is called.
    ///
    /// Subsequent calls are no-ops (first call wins). Drops the sample
    /// silently on lock contention or poisoning.
    pub fn mark_request_elapsed(&self) {
        let Ok(mut inner) = self.0.try_lock() else {
            return;
        };
        if inner.request_elapsed.is_none() {
            inner.request_elapsed = Some(inner.t0.elapsed());
        }
    }

    /// Records the response body size in bytes.
    ///
    /// Drops the sample silently on lock contention or poisoning.
    pub fn set_resp_bytes(&self, bytes: u64) {
        let Ok(mut inner) = self.0.try_lock() else {
            return;
        };
        inner.resp_bytes = Some(bytes);
    }

    /// Renders a `Server-Timing` header value, or `None` before
    /// [`RequestTimings::mark_headers_ready`] has been called.
    ///
    /// `ts-total` is rendered first from `headers_ready_total`, followed by
    /// the recorded header-bearing phases (`ts-appbuild`, `ts-filter`,
    /// `ts-geo`, `ts-kv`, `ts-origin`, `ts-template-cache`) in enum
    /// declaration order. Unrecorded phases are omitted. Durations are
    /// rendered as milliseconds with one decimal place. Drops the sample
    /// silently (returning `None`) on lock contention or poisoning.
    #[must_use]
    pub fn server_timing_value(&self) -> Option<String> {
        let inner = self.0.try_lock().ok()?;
        let total = inner.headers_ready_total?;
        let mut entries = vec![format_entry("ts-total", total)];
        for phase in HEADER_PHASES {
            let Some(name) = phase.header_name() else {
                continue;
            };
            if let Some(dur) = inner.phases[phase.index()] {
                entries.push(format_entry(name, dur));
            }
        }
        Some(entries.join(", "))
    }

    /// Captures the current state as a [`TimingSnapshot`].
    ///
    /// Returns an all-`None` snapshot on lock contention or poisoning,
    /// consistent with the infallibility of every other method.
    #[must_use]
    pub fn snapshot(&self) -> TimingSnapshot {
        let Ok(inner) = self.0.try_lock() else {
            return TimingSnapshot::default();
        };
        TimingSnapshot {
            time_elapsed_ms: duration_ms(inner.headers_ready_total),
            request_elapsed_ms: duration_ms(inner.request_elapsed),
            appbuild_ms: duration_ms(inner.phases[Phase::AppBuild.index()]),
            filter_ms: duration_ms(inner.phases[Phase::Filter.index()]),
            geo_ms: duration_ms(inner.phases[Phase::Geo.index()]),
            kv_ms: duration_ms(inner.phases[Phase::EcKv.index()]),
            origin_ms: duration_ms(inner.phases[Phase::Origin.index()]),
            template_cache_ms: duration_ms(inner.phases[Phase::TemplateCacheLookup.index()]),
            auction_wait_ms: duration_ms(inner.phases[Phase::AuctionWait.index()]),
            stream_ms: duration_ms(inner.phases[Phase::Stream.index()]),
            auction_wait_placement: inner.auction_wait_placement,
            resp_bytes: inner.resp_bytes,
        }
    }
}

impl Default for RequestTimings {
    fn default() -> Self {
        Self::new()
    }
}

/// Stamps [`RequestTimings::mark_headers_ready`] and, when `enabled` and the
/// response is conclusively private, appends the rendered `Server-Timing`
/// header.
///
/// Always stamps `mark_headers_ready` regardless of whether the header is
/// rendered, so the collector's `ts-total` reflects the moment headers
/// commit. Appends rather than overwrites so a pre-existing `Server-Timing`
/// value set upstream survives alongside the TS-owned set. A response is
/// never promoted to shared-cacheable just because the header would
/// otherwise be omitted: this only gates emission, it does not touch
/// `Cache-Control`.
///
/// Generic over the response body type so every adapter's terminal layer can
/// call the same emission logic regardless of which body type its HTTP stack
/// uses.
pub fn append_server_timing_if_private<B>(
    response: &mut Response<B>,
    timings: &RequestTimings,
    enabled: bool,
) {
    timings.mark_headers_ready();

    let conclusively_private = cache_control_headers_are_private_or_no_store(response.headers());
    if !enabled || !conclusively_private {
        return;
    }

    let Some(value) = timings.server_timing_value() else {
        return;
    };
    match HeaderValue::from_str(&value) {
        Ok(header_value) => {
            response
                .headers_mut()
                .append(HEADER_SERVER_TIMING, header_value);
        }
        Err(error) => log::warn!("skipping server-timing header: {error}"),
    }
}

/// The header-bearing phases (see [`Phase::header_name`]), in the enum
/// declaration order [`RequestTimings::server_timing_value`] renders them in.
const HEADER_PHASES: [Phase; 6] = [
    Phase::AppBuild,
    Phase::Filter,
    Phase::Geo,
    Phase::EcKv,
    Phase::Origin,
    Phase::TemplateCacheLookup,
];

/// Formats one `Server-Timing` entry as `name;dur=<ms with one decimal>`.
fn format_entry(name: &str, dur: Duration) -> String {
    format!("{name};dur={:.1}", dur.as_secs_f64() * 1000.0)
}

/// Converts a recorded [`Duration`] to whole milliseconds, saturating to
/// [`u32::MAX`] instead of overflowing.
fn duration_ms(dur: Option<Duration>) -> Option<u32> {
    dur.map(|dur| u32::try_from(dur.as_millis()).unwrap_or(u32::MAX))
}

/// RAII guard returned by [`RequestTimings::span`] that records its own
/// elapsed lifetime into the originating phase when dropped.
pub struct PhaseSpan {
    /// The collector this span reports into on drop.
    timings: RequestTimings,
    /// The phase this span's elapsed time is recorded under.
    phase: Phase,
    /// The instant the span was created.
    started: Instant,
}

impl Drop for PhaseSpan {
    fn drop(&mut self) {
        self.timings.record(self.phase, self.started.elapsed());
    }
}

/// A point-in-time, plain-data view of a [`RequestTimings`] collector.
///
/// All durations are whole milliseconds; unrecorded phases are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimingSnapshot {
    /// Elapsed time from request start to
    /// [`RequestTimings::mark_headers_ready`], in milliseconds.
    pub time_elapsed_ms: Option<u32>,
    /// Elapsed time from request start to
    /// [`RequestTimings::mark_request_elapsed`], in milliseconds.
    pub request_elapsed_ms: Option<u32>,
    /// Accumulated [`Phase::AppBuild`] duration, in milliseconds.
    pub appbuild_ms: Option<u32>,
    /// Accumulated [`Phase::Filter`] duration, in milliseconds.
    pub filter_ms: Option<u32>,
    /// Accumulated [`Phase::Geo`] duration, in milliseconds.
    pub geo_ms: Option<u32>,
    /// Accumulated [`Phase::EcKv`] duration, in milliseconds.
    pub kv_ms: Option<u32>,
    /// Accumulated [`Phase::Origin`] duration, in milliseconds.
    pub origin_ms: Option<u32>,
    /// Accumulated [`Phase::TemplateCacheLookup`] duration, in milliseconds.
    pub template_cache_ms: Option<u32>,
    /// Accumulated [`Phase::AuctionWait`] duration, in milliseconds.
    pub auction_wait_ms: Option<u32>,
    /// Accumulated [`Phase::Stream`] duration, in milliseconds.
    pub stream_ms: Option<u32>,
    /// Placement recorded by the most recent
    /// [`RequestTimings::record_auction_wait`] call.
    pub auction_wait_placement: Option<AuctionWaitPlacement>,
    /// Response body size in bytes, set via
    /// [`RequestTimings::set_resp_bytes`].
    pub resp_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_omits_unrecorded_phases_and_orders_total_first() {
        let timings = RequestTimings::new();
        timings.record(Phase::Filter, Duration::from_micros(9_100));
        timings.mark_headers_ready();
        let value = timings
            .server_timing_value()
            .expect("should render after mark_headers_ready");
        assert!(
            value.starts_with("ts-total;dur="),
            "should lead with ts-total: {value}"
        );
        assert!(
            value.contains("ts-filter;dur=9.1"),
            "should render one decimal: {value}"
        );
        assert!(
            !value.contains("ts-geo"),
            "should omit unrecorded phases: {value}"
        );
    }

    #[test]
    fn render_returns_none_before_headers_ready() {
        let timings = RequestTimings::new();
        timings.record(Phase::Geo, Duration::from_millis(1));
        assert!(
            timings.server_timing_value().is_none(),
            "should require the snapshot"
        );
    }

    #[test]
    fn repeated_phases_accumulate_saturating() {
        let timings = RequestTimings::new();
        timings.record(Phase::Geo, Duration::from_millis(2));
        timings.record(Phase::Geo, Duration::from_millis(3));
        timings.mark_headers_ready();
        let snapshot = timings.snapshot();
        assert_eq!(snapshot.geo_ms, Some(5), "should accumulate repeats");
    }

    #[test]
    fn mark_headers_ready_is_first_call_wins() {
        let timings = RequestTimings::new();
        timings.mark_headers_ready();
        let first = timings.snapshot().time_elapsed_ms;
        std::thread::sleep(Duration::from_millis(5));
        timings.mark_headers_ready();
        assert_eq!(
            timings.snapshot().time_elapsed_ms,
            first,
            "should not restamp"
        );
    }

    #[test]
    fn span_guard_records_on_drop() {
        let timings = RequestTimings::new();
        {
            let _span = timings.span(Phase::Origin);
            std::thread::sleep(Duration::from_millis(2));
        }
        timings.mark_headers_ready();
        assert!(
            timings.snapshot().origin_ms.expect("should record on drop") >= 1,
            "should measure elapsed span time"
        );
    }

    #[test]
    fn auction_wait_records_placement() {
        let timings = RequestTimings::new();
        timings.record_auction_wait(AuctionWaitPlacement::PreHeader, Duration::from_millis(40));
        let snapshot = timings.snapshot();
        assert_eq!(snapshot.auction_wait_ms, Some(40), "should record wait");
        assert_eq!(
            snapshot.auction_wait_placement,
            Some(AuctionWaitPlacement::PreHeader),
            "should record placement"
        );
    }

    #[test]
    fn rendered_names_never_include_vendor_terms() {
        let timings = RequestTimings::new();
        for phase in [
            Phase::AppBuild,
            Phase::Filter,
            Phase::Geo,
            Phase::EcKv,
            Phase::Origin,
            Phase::TemplateCacheLookup,
        ] {
            timings.record(phase, Duration::from_millis(1));
        }
        timings.mark_headers_ready();
        let value = timings.server_timing_value().expect("should render");
        assert!(
            !value.to_ascii_lowercase().contains("datadome"),
            "should mask vendors"
        );
    }

    #[test]
    fn append_server_timing_emits_on_private_response_when_enabled() {
        let mut response = Response::builder()
            .header("cache-control", "private, no-store")
            .body(())
            .expect("should build a private response fixture");
        let timings = RequestTimings::new();

        append_server_timing_if_private(&mut response, &timings, true);

        let header = response
            .headers()
            .get("server-timing")
            .and_then(|value| value.to_str().ok())
            .expect("should emit a Server-Timing header");
        assert!(
            header.starts_with("ts-total;dur="),
            "should lead with the stored total: {header}"
        );
    }

    #[test]
    fn append_server_timing_marks_headers_ready_even_when_not_emitted() {
        let mut response = Response::builder()
            .header("cache-control", "max-age=60")
            .body(())
            .expect("should build a shared-cacheable response fixture");
        let timings = RequestTimings::new();

        append_server_timing_if_private(&mut response, &timings, true);

        assert!(
            response.headers().get("server-timing").is_none(),
            "should not emit on a shared-cacheable response"
        );
        assert!(
            timings.server_timing_value().is_some(),
            "should still stamp mark_headers_ready so ts-total reflects the freeze point"
        );
    }
}
