# trusted-server-adapter-axum

Native development adapter for Trusted Server. It runs the shared application
through Axum on the host, without an edge simulator, and is not a production
deployment target.

The crate owns Axum route registration, middleware, outbound HTTP through
`reqwest`, and environment-backed platform services. Its route surface follows
the shared core contract, but request-time KV operations are unavailable and a
startup error makes every route, including `/health`, return 500. Application
configuration reaches the process through the documented environment bridge;
the local EdgeZero config-store file is not read directly.

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
