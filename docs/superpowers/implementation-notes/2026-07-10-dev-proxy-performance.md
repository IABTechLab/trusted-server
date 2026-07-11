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

| Stage                       | Decision | Evidence |
| --------------------------- | -------- | -------- |
| HTTP/1 pooling              | Pending  | Task 10  |
| Buffered initial-head parse | Pending  | Task 10  |
| DNS cache                   | Pending  | Task 10  |
| Upstream HTTP/2             | Pending  | Task 10  |
| `TCP_NODELAY`               | Pending  | Task 14  |
