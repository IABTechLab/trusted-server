# Dev Proxy Performance Implementation Notes

## Environment

- Date: 2026-07-11
- Machine architecture: Apple arm64
- Operating system: macOS 26.5.1 (25F80)
- Rust: 1.95.0 (`aarch64-apple-darwin`, LLVM 22.1.2)
- Branch: `perf/dev-proxy-optimization`

## Task 1: Baseline

Command:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin --test proxy_perf -- --ignored --nocapture --test-threads=1
```

Raw output:

```text
PERF_RUN workload=matched_concurrency_6 variant=baseline run=1 duration_us=608415 tcp_attempts=100 tcp_established=100 tls_handshakes=100 failures=0
PERF_RUN workload=saturation_concurrency_20 variant=baseline run=1 duration_us=206839 tcp_attempts=100 tcp_established=100 tls_handshakes=100 failures=0
PERF_RUN workload=sequential_tls variant=baseline run=1 duration_us=70804 tcp_attempts=100 tcp_established=100 tls_handshakes=100 failures=0
```

The baseline confirms the structural problem: every request establishes a new
upstream TCP connection and performs a new TLS handshake. These are single-run
foundation measurements, not retention evidence. Task 10 performs two warmups
and ten alternating baseline/pooled runs before drawing timing conclusions.

## Experiment Decisions

| Stage                       | Decision | Evidence                                               |
| --------------------------- | -------- | ------------------------------------------------------ |
| HTTP/1 pooling              | RETAIN   | 100→1 sequential connections; 59.7% median improvement |
| Buffered initial-head parse | RETAIN   | Entry 28.5%; retained parser contribution 8.6%         |
| DNS cache                   | RETAIN   | 13.6% median churn-workload improvement                |
| Upstream HTTP/2             | REJECT   | Entry passed; bounded-capacity feasibility failed      |
| `TCP_NODELAY`               | REJECT   | Both one-sided median gains were below 3%              |

## HTTP/1 Pooling Results

Commands used the ignored `proxy_perf` workloads with two discarded warmup
rounds, then ten alternating runs per variant. `TS_PERF_VARIANT=baseline` is a
harness-only pool-disabled mode; it is not a CLI option.

| Workload / variant       | Median (µs) |  p95 (µs) | MAD (µs) | Connections / handshakes | Failures |
| ------------------------ | ----------: | --------: | -------: | -----------------------: | -------: |
| sequential / baseline    |    83,962.5 |   108,705 |    883.5 |                100 / 100 |        0 |
| sequential / pooled      |    33,828.5 |    41,289 |    335.5 |                    1 / 1 |        0 |
| concurrency 6 / baseline |   601,727.5 | 1,296,127 |    4,225 |                100 / 100 |        0 |
| concurrency 6 / pooled   |   566,188.5 |   574,799 |    3,627 |            53–68 / 53–68 |        0 |

Sequential localhost duration improved 59.7%, with the structural result of
100-to-1 TCP connections and TLS handshakes. Matched concurrency improved 5.9%.
At exactly six synchronized clients, the two-idle-per-origin bound deliberately
retires burst-surplus connections between waves; under concurrency-20
saturation, queued work retains exactly six live connections.

## Buffered Initial Parsing

The 1,000-connection PAC workload measured the original byte reader at
77,933 µs total and 22,239 µs aggregate parse time (28.5%), clearing the 5%
entry gate. The retained 1 KiB chunk reader plus exact-once prefixed I/O measured
48,667 µs total and 4,205 µs parse time (8.6%). The 8 KiB limit applies only
through the header terminator; over-read bytes are replayed on MITM, blind
tunnel, and plain-forward paths.

## DNS Cache

The post-pooling DNS workload measured 1,936 µs of lookup time against 8,590 µs
of upstream-header time (22.5%), clearing entry. The retained cache uses a
30-second TTL, 64-entry LRU bound, concurrent-miss coalescing, no failure
caching, and strict `--resolve` bypass.

| Variant        | Median (µs) | p95 (µs) | MAD (µs) | Lookups per 100 opens |
| -------------- | ----------: | -------: | -------: | --------------------: |
| cache disabled |   111,461.5 |  120,372 |    1,514 |                   100 |
| cache enabled  |    96,273.5 |   97,998 |    1,135 |      1 miss + 99 hits |

The cache improved median duration by 13.6% with no added failures, so it was
retained.

## HTTP/2 Feasibility

The cap-6 versus harness-only cap-20 entry comparison measured medians of
521,830.5 µs and 200,660.5 µs respectively (61.5% contribution), so feasibility
was evaluated. It was rejected because Hyper's maintained public client API
does not expose the peer's current and dynamically lowered
`SETTINGS_MAX_CONCURRENT_STREAMS` in a way that can prove the combined manager
and Hyper waiter bounds. `SendRequest::ready()` is not sufficient. No `http2`
feature, proof scaffolding, or dormant production code remains.

## TCP_NODELAY

Ten retained samples after two warmups produced:

| Variant       | Median (µs) | p95 (µs) | MAD (µs) | Gain vs off |
| ------------- | ----------: | -------: | -------: | ----------: |
| off/off       |    40,794.5 |   42,649 |    1,368 |           — |
| browser only  |    40,329.5 |   42,199 |    1,844 |        1.1% |
| upstream only |    40,166.5 |   42,561 |  2,032.5 |        1.5% |
| both          |    40,568.5 |   41,928 |    827.5 |        0.6% |

Neither one-sided variant reached the 3% retention threshold. All experimental
socket-option code was removed.

## Retained Constants

- HTTP/1 live connections: 6 per origin, 64 global
- Idle HTTP/1 connections: 2 per origin, 32 global, 60-second expiry
- Waiters: 32 per origin, 128 global, 30-second acquisition timeout
- DNS: 30-second TTL, 64 entries
- Shutdown manager drain: 2 seconds after Safari/system proxy restoration
