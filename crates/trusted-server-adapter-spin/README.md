# trusted-server-adapter-spin

Experimental Fermyon Spin adapter for Trusted Server. This migration selects
exact `spin-sdk = 7.0.0`, Rust target `wasm32-wasip2`, and the P3 `wasi:http`
0.3.0 interface. It requires **Spin 4.1.0 or newer**. A successful wasip2 build
or native test does not establish compatibility with a P2-only host or Spin 4.0.

Build and test from the repository root:

```bash
cargo build --package trusted-server-adapter-spin --target wasm32-wasip2 --features spin --release
cargo test-spin
spin up --from crates/trusted-server-adapter-spin
```

Inspect the emitted component's imports/exports with `wasm-tools component wit`
and exercise it with the selected Spin host before deployment. `spin.toml`
loads `target/wasm32-wasip2/release/trusted_server_adapter_spin.wasm`.
Repository-wide aliases, toolchain targets and CI must select the same target.
Check `scripts/smoke-spin.sh` and the deployment guide against this bootstrap
contract before using them as migration proof. Legacy expectations of routed
503 responses or healthy failed-startup probes are no longer valid.

## Invocation-local bootstrap

Every invocation awaits the existing BlobEnvelope from the `default` KV store,
verifies it, removes inactive secret references, loads the required encoded
secret variables and validates resolved settings. Only then does it build the
app with Spin platform metadata and dispatch through EdgeZero admission/egress.
Routes and middleware read immutable typed state. No synchronous provider
executor or global live SDK-resource cache is used.

Bootstrap failures, including a pending provider exceeding the budget, are
explicit **pre-admission exceptions** returned at the FFI boundary. They do not
install a startup-error router, answer `/health` successfully, or create routed
body/egress completion records. Once initialized, `/health` remains a routed
liveness endpoint. The concrete `SpinResponse` leaves transmission ownership
with EdgeZero. `anyhow::Result` is confined to that SDK-required FFI boundary.

## Finite read-only snapshots

The old config adapter mapped every logical config-store name to the same
`default` KV store and read keys verbatim. Preserve that mapping by snapshotting
that entire bounded store. This includes configured DataDome CIDR sources and
all stored public JWKs, so verification is not restricted to `active-kids`.
Use a dedicated config store, not one shared with arbitrary binary/application
KV data. Every enumerated config value must be UTF-8.

Limits per invocation:

- 5 seconds for bootstrap, shared across provider reads and key enumeration.
- 128 config keys, each containing 1 through 256 UTF-8 bytes.
- 1 MiB per config value, including the serialized envelope.
- JSON depth 64 and 100,000 nodes for verified envelope data.
- 64 distinct store/key secret addresses, including the current signing key.
- 64 KiB per nonempty UTF-8 secret and 4 MiB aggregate config/key/secret bytes,
  including secret store/key names.

SDK key enumeration is streamed and stops on the first excess key. The SDK
materializes individual keys and values before the adapter can reject excess bytes;
these are retained-value limits, not a bound on host allocation or total Wasm
heap use. A deadline drops guest provider futures, not a claim that the host
has cancelled its work. Multi-key/provider reads are not an atomic release.
Publish stable inputs before serving traffic.

Secret names keep the existing collision-resistant store/key encoding, for
example `signing_keys` plus `kid-current` becomes
`v_signing_x5fkeys_v_kid_x2dcurrent`. The RC's flat lowercase secret-store mapping
is not used. Enabled signing loads `current-kid`, `active-kids`, public JWKs and
the current private key, then checks the current public/private pair with the
existing core parsers. Configured signing is not disabled for this adapter.
Missing snapshot values and management writes return typed errors; key rotation
and deactivation remain unsupported. Snapshots reload on the next invocation,
not during a request. Existing core consumer caches, including DataDome's parsed
CIDR TTL cache, retain their current policy. Explicit-settings/services builders
remain native test seams. The adapter still permits only one enabled auction
provider.
