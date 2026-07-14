use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const DURATION_BUCKET_UPPER_BOUNDS_MICROS: [u64; 9] = [
    100, 500, 1_000, 5_000, 10_000, 50_000, 100_000, 500_000, 1_000_000,
];
const DURATION_BUCKET_COUNT: usize = DURATION_BUCKET_UPPER_BOUNDS_MICROS.len() + 1;

/// Low-overhead process-local metrics for `ts dev proxy`.
#[derive(Default)]
pub struct ProxyMetrics {
    browser_connections: AtomicU64,
    initial_heads_parsed: AtomicU64,
    initial_heads_rejected: AtomicU64,
    tcp_attempts: AtomicU64,
    tcp_established: AtomicU64,
    pool_hits: AtomicU64,
    pool_misses: AtomicU64,
    pool_stale: AtomicU64,
    pool_retries: AtomicU64,
    requests_completed: AtomicU64,
    requests_failed: AtomicU64,
    initial_head_parse_latency: DurationHistogram,
    connect_latency: DurationHistogram,
    pool_acquisition_latency: DurationHistogram,
    queue_wait_latency: DurationHistogram,
    request_to_headers_latency: DurationHistogram,
    dns_lookup_latency: DurationHistogram,
    dns_cache_hits: AtomicU64,
    dns_cache_misses: AtomicU64,
    tls_handshake_latency: DurationHistogram,
    http_handshake_latency: DurationHistogram,
    negotiated_http1: AtomicU64,
    ca_hits: AtomicU64,
    ca_misses: AtomicU64,
    unexpected_ca_mints: AtomicU64,
    ca_mint_latency: DurationHistogram,
}

impl ProxyMetrics {
    /// Records an accepted browser-side connection.
    pub fn record_browser_connection(&self) {
        self.browser_connections.fetch_add(1, Ordering::Relaxed);
    }
    /// Records a successfully parsed initial request head and its latency.
    pub fn record_initial_head_parsed(&self, duration: Duration) {
        self.initial_heads_parsed.fetch_add(1, Ordering::Relaxed);
        self.initial_head_parse_latency.record(duration);
    }

    /// Records a rejected initial request head and its parse latency.
    pub fn record_initial_head_rejected(&self, duration: Duration) {
        self.initial_heads_rejected.fetch_add(1, Ordering::Relaxed);
        self.initial_head_parse_latency.record(duration);
    }

    /// Records one TCP connection attempt.
    pub fn record_tcp_attempt(&self) {
        self.tcp_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a successful TCP connection and its latency.
    pub fn record_tcp_established(&self, duration: Duration) {
        self.tcp_established.fetch_add(1, Ordering::Relaxed);
        self.connect_latency.record(duration);
    }

    /// Records total manager acquisition latency, including failed acquisitions.
    pub fn record_pool_acquisition(&self, duration: Duration) {
        self.pool_acquisition_latency.record(duration);
    }

    /// Records time spent queued behind upstream connection limits.
    pub fn record_queue_wait(&self, duration: Duration) {
        self.queue_wait_latency.record(duration);
    }

    /// Records reuse of an idle upstream connection.
    pub fn record_pool_hit(&self) {
        self.pool_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a new upstream connection reservation.
    pub fn record_pool_miss(&self) {
        self.pool_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Records detection of a stale pooled sender.
    pub fn record_pool_stale(&self) {
        self.pool_stale.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one safe stale-connection retry.
    pub fn record_pool_retry(&self) {
        self.pool_retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a response that reached terminal end-of-stream successfully.
    pub fn record_request_completed(&self) {
        self.requests_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a request that terminated without a complete response.
    pub fn record_request_failed(&self) {
        self.requests_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records latency from request dispatch to upstream response headers.
    pub fn record_request_to_headers(&self, duration: Duration) {
        self.request_to_headers_latency.record(duration);
    }

    /// Records one underlying DNS lookup latency.
    pub fn record_dns_lookup(&self, duration: Duration) {
        self.dns_lookup_latency.record(duration);
    }

    /// Records a ready or in-flight DNS cache hit.
    pub fn record_dns_cache_hit(&self) {
        self.dns_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a DNS cache miss that starts resolver work.
    pub fn record_dns_cache_miss(&self) {
        self.dns_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one completed TLS handshake latency.
    pub fn record_tls_handshake(&self, duration: Duration) {
        self.tls_handshake_latency.record(duration);
    }

    /// Records one completed upstream HTTP handshake latency.
    pub fn record_http_handshake(&self, duration: Duration) {
        self.http_handshake_latency.record(duration);
    }

    /// Records negotiation of an HTTP/1 upstream connection.
    pub fn record_negotiated_http1(&self) {
        self.negotiated_http1.fetch_add(1, Ordering::Relaxed);
    }

    /// Records reuse of a cached development leaf certificate.
    pub fn record_ca_hit(&self) {
        self.ca_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one development certificate mint and whether it was unexpected.
    pub fn record_ca_miss(&self, duration: Duration, unexpected: bool) {
        self.ca_misses.fetch_add(1, Ordering::Relaxed);
        self.ca_mint_latency.record(duration);
        if unexpected {
            self.unexpected_ca_mints.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    /// Formats a redacted aggregate summary suitable for shutdown diagnostics.
    pub fn debug_summary(&self) -> String {
        let snapshot = self.snapshot();
        format!(
            "dev proxy metrics: browser_connections={} initial_heads_parsed={} initial_heads_rejected={} \
             initial_head_parse_samples={} tcp_attempts={} tcp_established={} \
             connect_samples={} pool_hits={} pool_misses={} pool_stale={} pool_retries={} requests_completed={} \
             requests_failed={} pool_acquisition_samples={} queue_wait_samples={} \
             request_to_headers_samples={} dns_lookup_samples={} dns_cache_hits={} dns_cache_misses={} tls_handshake_samples={} \
             http_handshake_samples={} negotiated_http1={} ca_hits={} ca_misses={} unexpected_ca_mints={} \
             ca_mint_samples={} duration_bounds_us={:?} \
             initial_head_parse_us_total={} initial_head_parse_us_buckets={:?} \
             connect_us_total={} connect_us_buckets={:?} \
             pool_acquisition_us_total={} pool_acquisition_us_buckets={:?} \
             queue_wait_us_total={} queue_wait_us_buckets={:?} \
             request_to_headers_us_total={} request_to_headers_us_buckets={:?} \
             dns_lookup_us_total={} dns_lookup_us_buckets={:?} \
             tls_handshake_us_total={} tls_handshake_us_buckets={:?} \
             http_handshake_us_total={} http_handshake_us_buckets={:?} \
             ca_mint_us_total={} ca_mint_us_buckets={:?}",
            snapshot.browser_connections,
            snapshot.initial_heads_parsed,
            snapshot.initial_heads_rejected,
            snapshot.initial_head_parse_latency.total(),
            snapshot.tcp_attempts,
            snapshot.tcp_established,
            snapshot.connect_latency.total(),
            snapshot.pool_hits,
            snapshot.pool_misses,
            snapshot.pool_stale,
            snapshot.pool_retries,
            snapshot.requests_completed,
            snapshot.requests_failed,
            snapshot.pool_acquisition_latency.total(),
            snapshot.queue_wait_latency.total(),
            snapshot.request_to_headers_latency.total(),
            snapshot.dns_lookup_latency.total(),
            snapshot.dns_cache_hits,
            snapshot.dns_cache_misses,
            snapshot.tls_handshake_latency.total(),
            snapshot.http_handshake_latency.total(),
            snapshot.negotiated_http1,
            snapshot.ca_hits,
            snapshot.ca_misses,
            snapshot.unexpected_ca_mints,
            snapshot.ca_mint_latency.total(),
            DURATION_BUCKET_UPPER_BOUNDS_MICROS,
            snapshot.initial_head_parse_latency.total_micros(),
            snapshot.initial_head_parse_latency.buckets(),
            snapshot.connect_latency.total_micros(),
            snapshot.connect_latency.buckets(),
            snapshot.pool_acquisition_latency.total_micros(),
            snapshot.pool_acquisition_latency.buckets(),
            snapshot.queue_wait_latency.total_micros(),
            snapshot.queue_wait_latency.buckets(),
            snapshot.request_to_headers_latency.total_micros(),
            snapshot.request_to_headers_latency.buckets(),
            snapshot.dns_lookup_latency.total_micros(),
            snapshot.dns_lookup_latency.buckets(),
            snapshot.tls_handshake_latency.total_micros(),
            snapshot.tls_handshake_latency.buckets(),
            snapshot.http_handshake_latency.total_micros(),
            snapshot.http_handshake_latency.buckets(),
            snapshot.ca_mint_latency.total_micros(),
            snapshot.ca_mint_latency.buckets(),
        )
    }

    #[must_use]
    /// Captures a point-in-time copy of every counter and duration histogram.
    pub fn snapshot(&self) -> ProxyMetricsSnapshot {
        ProxyMetricsSnapshot {
            browser_connections: self.browser_connections.load(Ordering::Relaxed),
            initial_heads_parsed: self.initial_heads_parsed.load(Ordering::Relaxed),
            initial_heads_rejected: self.initial_heads_rejected.load(Ordering::Relaxed),
            tcp_attempts: self.tcp_attempts.load(Ordering::Relaxed),
            tcp_established: self.tcp_established.load(Ordering::Relaxed),
            pool_hits: self.pool_hits.load(Ordering::Relaxed),
            pool_misses: self.pool_misses.load(Ordering::Relaxed),
            pool_stale: self.pool_stale.load(Ordering::Relaxed),
            pool_retries: self.pool_retries.load(Ordering::Relaxed),
            requests_completed: self.requests_completed.load(Ordering::Relaxed),
            requests_failed: self.requests_failed.load(Ordering::Relaxed),
            initial_head_parse_latency: self.initial_head_parse_latency.snapshot(),
            connect_latency: self.connect_latency.snapshot(),
            pool_acquisition_latency: self.pool_acquisition_latency.snapshot(),
            queue_wait_latency: self.queue_wait_latency.snapshot(),
            request_to_headers_latency: self.request_to_headers_latency.snapshot(),
            dns_lookup_latency: self.dns_lookup_latency.snapshot(),
            dns_cache_hits: self.dns_cache_hits.load(Ordering::Relaxed),
            dns_cache_misses: self.dns_cache_misses.load(Ordering::Relaxed),
            tls_handshake_latency: self.tls_handshake_latency.snapshot(),
            http_handshake_latency: self.http_handshake_latency.snapshot(),
            negotiated_http1: self.negotiated_http1.load(Ordering::Relaxed),
            ca_hits: self.ca_hits.load(Ordering::Relaxed),
            ca_misses: self.ca_misses.load(Ordering::Relaxed),
            unexpected_ca_mints: self.unexpected_ca_mints.load(Ordering::Relaxed),
            ca_mint_latency: self.ca_mint_latency.snapshot(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
/// Point-in-time aggregate metrics for tests and diagnostics.
pub struct ProxyMetricsSnapshot {
    /// Browser-side connections accepted.
    pub browser_connections: u64,
    /// Initial request heads parsed successfully.
    pub initial_heads_parsed: u64,
    /// Initial request heads rejected.
    pub initial_heads_rejected: u64,
    /// TCP connection attempts.
    pub tcp_attempts: u64,
    /// TCP connections established.
    pub tcp_established: u64,
    /// Idle pool hits.
    pub pool_hits: u64,
    /// New connection reservations.
    pub pool_misses: u64,
    /// Stale pooled senders detected.
    pub pool_stale: u64,
    /// Safe stale retries attempted.
    pub pool_retries: u64,
    /// Responses completed successfully.
    pub requests_completed: u64,
    /// Requests terminated unsuccessfully.
    pub requests_failed: u64,
    /// Initial-head parsing latency.
    pub initial_head_parse_latency: DurationHistogramSnapshot,
    /// Successful TCP connection latency.
    pub connect_latency: DurationHistogramSnapshot,
    /// Manager acquisition latency.
    pub pool_acquisition_latency: DurationHistogramSnapshot,
    /// Manager queue-wait latency.
    pub queue_wait_latency: DurationHistogramSnapshot,
    /// Request-to-response-headers latency.
    pub request_to_headers_latency: DurationHistogramSnapshot,
    /// Underlying DNS lookup latency.
    pub dns_lookup_latency: DurationHistogramSnapshot,
    /// Ready or in-flight DNS cache hits.
    pub dns_cache_hits: u64,
    /// DNS resolver work started after a cache miss.
    pub dns_cache_misses: u64,
    /// TLS handshake latency.
    pub tls_handshake_latency: DurationHistogramSnapshot,
    /// HTTP handshake latency.
    pub http_handshake_latency: DurationHistogramSnapshot,
    /// HTTP/1 connections negotiated.
    pub negotiated_http1: u64,
    /// Development certificate cache hits.
    pub ca_hits: u64,
    /// Development certificate cache misses.
    pub ca_misses: u64,
    /// Certificate mints after the expected startup set.
    pub unexpected_ca_mints: u64,
    /// Development certificate mint latency.
    pub ca_mint_latency: DurationHistogramSnapshot,
}

#[derive(Debug, Eq, PartialEq)]
/// Immutable fixed-bucket duration histogram snapshot.
pub struct DurationHistogramSnapshot {
    buckets: [u64; DURATION_BUCKET_COUNT],
    total_micros: u64,
}

impl DurationHistogramSnapshot {
    #[must_use]
    /// Returns the fixed bucket counts in ascending-bound order plus overflow.
    pub fn buckets(&self) -> &[u64; DURATION_BUCKET_COUNT] {
        &self.buckets
    }

    #[must_use]
    /// Returns the number of recorded duration samples.
    pub fn total(&self) -> u64 {
        self.buckets.iter().sum()
    }

    #[must_use]
    /// Returns the saturating sum of recorded durations in microseconds.
    pub fn total_micros(&self) -> u64 {
        self.total_micros
    }
}

struct DurationHistogram {
    buckets: [AtomicU64; DURATION_BUCKET_COUNT],
    total_micros: AtomicU64,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            total_micros: AtomicU64::new(0),
        }
    }
}

impl DurationHistogram {
    fn record(&self, duration: Duration) {
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        let bucket = DURATION_BUCKET_UPPER_BOUNDS_MICROS
            .iter()
            .position(|upper_bound| micros <= *upper_bound)
            .unwrap_or(DURATION_BUCKET_COUNT - 1);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.total_micros.fetch_add(micros, Ordering::Relaxed);
    }

    #[must_use]
    fn snapshot(&self) -> DurationHistogramSnapshot {
        DurationHistogramSnapshot {
            buckets: std::array::from_fn(|index| self.buckets[index].load(Ordering::Relaxed)),
            total_micros: self.total_micros.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_separates_attempts_from_established() {
        let metrics = ProxyMetrics::default();

        metrics.record_tcp_attempt();
        metrics.record_tcp_attempt();
        metrics.record_tcp_established(Duration::from_millis(7));
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.tcp_attempts, 2, "should count every attempt");
        assert_eq!(
            snapshot.tcp_established, 1,
            "should count only established connections"
        );
        assert_eq!(
            snapshot.connect_latency.total(),
            1,
            "should record one successful connect duration"
        );
    }

    #[test]
    fn histogram_uses_fixed_bounded_buckets() {
        let metrics = ProxyMetrics::default();

        for duration in [
            Duration::ZERO,
            Duration::from_micros(100),
            Duration::from_micros(101),
            Duration::from_secs(1),
            Duration::from_micros(1_000_001),
        ] {
            metrics.record_tcp_established(duration);
        }
        let snapshot = metrics.snapshot();

        assert_eq!(
            snapshot.connect_latency.buckets(),
            &[2, 1, 0, 0, 0, 0, 0, 0, 1, 1],
            "should place timings into fixed buckets with a final overflow bucket"
        );
    }

    #[test]
    fn snapshot_records_initial_head_and_pool_phase_timings() {
        let metrics = ProxyMetrics::default();

        metrics.record_initial_head_parsed(Duration::from_micros(250));
        metrics.record_initial_head_rejected(Duration::from_micros(500));
        metrics.record_pool_acquisition(Duration::from_millis(2));
        metrics.record_queue_wait(Duration::from_millis(3));
        let snapshot = metrics.snapshot();

        assert_eq!(
            snapshot.initial_heads_parsed, 1,
            "should count parsed heads"
        );
        assert_eq!(
            snapshot.initial_heads_rejected, 1,
            "should count rejected heads"
        );
        assert_eq!(
            snapshot.initial_head_parse_latency.total(),
            2,
            "should time both accepted and rejected parsing"
        );
        assert_eq!(
            snapshot.pool_acquisition_latency.total(),
            1,
            "should time pool acquisition"
        );
        assert_eq!(
            snapshot.queue_wait_latency.total(),
            1,
            "should time queue waits"
        );
    }

    #[test]
    fn debug_summary_contains_only_aggregate_metrics() {
        let metrics = ProxyMetrics::default();
        metrics.record_tcp_attempt();
        metrics.record_tcp_established(Duration::from_millis(7));
        metrics.record_initial_head_parsed(Duration::from_micros(250));
        metrics.record_request_to_headers(Duration::from_millis(5));
        metrics.record_dns_lookup(Duration::from_millis(7));
        metrics.record_tls_handshake(Duration::from_millis(11));
        metrics.record_http_handshake(Duration::from_millis(13));
        metrics.record_ca_miss(Duration::from_millis(17), false);

        let summary = metrics.debug_summary();

        assert!(
            summary.contains("tcp_attempts=1"),
            "should report aggregate attempt count"
        );
        assert!(
            summary.contains("connect_samples=1"),
            "should report aggregate timing count"
        );
        assert!(
            summary.contains("initial_heads_parsed=1"),
            "should report aggregate parse count"
        );
        assert!(
            summary.contains("duration_bounds_us=[100, 500")
                && summary.contains("connect_us_buckets=[0, 0, 0, 0, 1"),
            "should report interpretable aggregate timing buckets"
        );
        for expected in [
            "request_to_headers_us_total=5000",
            "request_to_headers_us_buckets=",
            "dns_lookup_us_total=7000",
            "dns_lookup_us_buckets=",
            "tls_handshake_us_total=11000",
            "tls_handshake_us_buckets=",
            "http_handshake_us_total=13000",
            "http_handshake_us_buckets=",
            "ca_mint_us_total=17000",
            "ca_mint_us_buckets=",
        ] {
            assert!(
                summary.contains(expected),
                "summary should include {expected}: {summary}"
            );
        }
        assert!(
            !summary.contains("http://")
                && !summary.contains("authorization")
                && !summary.contains("certificate"),
            "should not contain request or credential fields"
        );
    }
}
