# trusted-server-adapter-cloudflare

Cloudflare Workers adapter for Trusted Server. Production code targets
`wasm32-unknown-unknown` with the `cloudflare` feature; native builds exist for
adapter tests and deliberately exclude the Workers entry point.

The crate translates Worker requests and responses, registers the shared
router, and implements Cloudflare-specific HTTP and secret access. For each
request the Worker reads its settings from the config store, the Workers KV
namespace bound as `trusted_server_config` that `ts config push --adapter
cloudflare` writes, and serves the router built from them, or the startup
error router when they cannot be read or do not load. It has no `/health`
route or request-time KV registry and permits one enabled auction provider.

Build and test from the repository root:

```bash
cargo build-cloudflare
cargo test-cloudflare
```

Run `./scripts/smoke-cloudflare.sh` for the isolated Wrangler handoff check.
The [Cloudflare deployment guide](../../docs/guide/cloudflare.md) documents the
config store, secret bindings, failure cases, and cleanup. Keep portable
request behavior in [`trusted-server-core`](../trusted-server-core/README.md).
