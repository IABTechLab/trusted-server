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
    pub fn record_browser_connection(&self) {
        self.browser_connections.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_initial_head_parsed(&self, duration: Duration) {
        self.initial_heads_parsed.fetch_add(1, Ordering::Relaxed);
        self.initial_head_parse_latency.record(duration);
    }

    pub fn record_initial_head_rejected(&self, duration: Duration) {
        self.initial_heads_rejected.fetch_add(1, Ordering::Relaxed);
        self.initial_head_parse_latency.record(duration);
    }

    pub fn record_tcp_attempt(&self) {
        self.tcp_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tcp_established(&self, duration: Duration) {
        self.tcp_established.fetch_add(1, Ordering::Relaxed);
        self.connect_latency.record(duration);
    }

    pub fn record_pool_acquisition(&self, duration: Duration) {
        self.pool_acquisition_latency.record(duration);
    }

    pub fn record_queue_wait(&self, duration: Duration) {
        self.queue_wait_latency.record(duration);
    }

    pub fn record_pool_hit(&self) {
        self.pool_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_pool_miss(&self) {
        self.pool_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_pool_stale(&self) {
        self.pool_stale.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_pool_retry(&self) {
        self.pool_retries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_request_completed(&self) {
        self.requests_completed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_request_failed(&self) {
        self.requests_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_request_to_headers(&self, duration: Duration) {
        self.request_to_headers_latency.record(duration);
    }

    pub fn record_dns_lookup(&self, duration: Duration) {
        self.dns_lookup_latency.record(duration);
    }

    pub fn record_dns_cache_hit(&self) {
        self.dns_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dns_cache_miss(&self) {
        self.dns_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tls_handshake(&self, duration: Duration) {
        self.tls_handshake_latency.record(duration);
    }

    pub fn record_http_handshake(&self, duration: Duration) {
        self.http_handshake_latency.record(duration);
    }

    pub fn record_negotiated_http1(&self) {
        self.negotiated_http1.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ca_hit(&self) {
        self.ca_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ca_miss(&self, duration: Duration, unexpected: bool) {
        self.ca_misses.fetch_add(1, Ordering::Relaxed);
        self.ca_mint_latency.record(duration);
        if unexpected {
            self.unexpected_ca_mints.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn debug_summary(&self) -> String {
        let snapshot = self.snapshot();
        format!(
            "dev proxy metrics: browser_connections={} initial_heads_parsed={} initial_heads_rejected={} \
             initial_head_parse_samples={} tcp_attempts={} tcp_established={} \
             connect_samples={} pool_hits={} pool_misses={} pool_stale={} pool_retries={} requests_completed={} \
             requests_failed={} pool_acquisition_samples={} queue_wait_samples={} \
             request_to_headers_samples={} dns_lookup_samples={} dns_cache_hits={} dns_cache_misses={} tls_handshake_samples={} \
             http_handshake_samples={} negotiated_http1={} ca_hits={} ca_misses={} unexpected_ca_mints={} \
             duration_bounds_us={:?} initial_head_parse_us_buckets={:?} \
             connect_us_buckets={:?} pool_acquisition_us_buckets={:?} \
             queue_wait_us_buckets={:?}",
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
            DURATION_BUCKET_UPPER_BOUNDS_MICROS,
            snapshot.initial_head_parse_latency.buckets(),
            snapshot.connect_latency.buckets(),
            snapshot.pool_acquisition_latency.buckets(),
            snapshot.queue_wait_latency.buckets(),
        )
    }

    #[must_use]
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
pub struct ProxyMetricsSnapshot {
    pub browser_connections: u64,
    pub initial_heads_parsed: u64,
    pub initial_heads_rejected: u64,
    pub tcp_attempts: u64,
    pub tcp_established: u64,
    pub pool_hits: u64,
    pub pool_misses: u64,
    pub pool_stale: u64,
    pub pool_retries: u64,
    pub requests_completed: u64,
    pub requests_failed: u64,
    pub initial_head_parse_latency: DurationHistogramSnapshot,
    pub connect_latency: DurationHistogramSnapshot,
    pub pool_acquisition_latency: DurationHistogramSnapshot,
    pub queue_wait_latency: DurationHistogramSnapshot,
    pub request_to_headers_latency: DurationHistogramSnapshot,
    pub dns_lookup_latency: DurationHistogramSnapshot,
    pub dns_cache_hits: u64,
    pub dns_cache_misses: u64,
    pub tls_handshake_latency: DurationHistogramSnapshot,
    pub http_handshake_latency: DurationHistogramSnapshot,
    pub negotiated_http1: u64,
    pub ca_hits: u64,
    pub ca_misses: u64,
    pub unexpected_ca_mints: u64,
    pub ca_mint_latency: DurationHistogramSnapshot,
}

#[derive(Debug, Eq, PartialEq)]
pub struct DurationHistogramSnapshot {
    buckets: [u64; DURATION_BUCKET_COUNT],
    total_micros: u64,
}

impl DurationHistogramSnapshot {
    #[must_use]
    pub fn buckets(&self) -> &[u64; DURATION_BUCKET_COUNT] {
        &self.buckets
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.buckets.iter().sum()
    }

    #[must_use]
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
        assert!(
            !summary.contains("http://")
                && !summary.contains("authorization")
                && !summary.contains("certificate"),
            "should not contain request or credential fields"
        );
    }
}
