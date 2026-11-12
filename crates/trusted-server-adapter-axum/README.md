# trusted-server-adapter-axum

Native development adapter for Trusted Server. It runs the shared application
through Axum on the host, without an edge simulator, and is not a production
deployment target.

The crate owns Axum route registration, middleware, and environment-backed
platform services. Outbound HTTP goes through EdgeZero's Axum client. Its
route surface follows the shared core contract, but request-time KV operations
are unavailable. A startup error, such as a missing config or secret, stops the
process with a non-zero exit before it listens. Application configuration
reaches the process through the documented environment bridge; the local
EdgeZero config-store file is not read directly.

The listener's ingress limits come from `EDGEZERO__ADAPTER__INGRESS__*`
environment variables, and an invalid value also stops startup. The
[Axum development guide](../../docs/guide/axum-dev.md#ingress-limits) lists
them.

Build and test from the repository root:

```bash
cargo build-axum
cargo test-axum
```

Run the isolated first-success check with `./scripts/smoke-axum.sh`. See the
[Axum development guide](../../docs/guide/axum-dev.md) for its config and
secret handoff, negative cases, success oracle, and cleanup behavior. Shared
runtime behavior belongs in
[`trusted-server-core`](../trusted-server-core/README.md), not this adapter.
