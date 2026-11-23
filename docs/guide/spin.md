# Fermyon Spin deployment

The Spin adapter packages Trusted Server as a WASI component. Startup reads
the config blob from Spin's `default` key-value store and resolves credentials
through Spin variables.

## Support status

| Adapter | Release status | Liveness | Startup failure | Readiness (ready/failed) | Provider fan-out | Trusted-client-IP handling | Request normalization            |
| ------- | -------------- | -------- | --------------- | ------------------------ | ---------------- | -------------------------- | -------------------------------- |
| `spin`  | experimental   | `200`    | `503`           | `200` / `503`            | multiple         | outermost sanitize         | innermost Spin-header derivation |

This row is the canonical status summary. Spin supports multiple auction
providers. GET/HEAD `/health` and `/ready` answer after component and SDK
application dispatch begins. Caught initialization failures preserve liveness
and make readiness and ordinary routes unavailable, and the full report goes
only to the log. See the
[operational response contract](./api-reference#operational-probe-contract).

EdgeZero's Spin `run_app` installs the log backend, and Trusted Server
installs none of its own. Records go to the component's stderr, which `spin up`
writes to `<component>_stderr.txt` under its `--log-dir`. The level comes from
`EDGEZERO__LOGGING__LEVEL` in the component's environment and defaults to
`info`, so the startup report and every warning, such as the ignored
`publisher.origin_host_header_override`, are logged. EdgeZero's own startup
warnings, under the `edgezero::boot` target, stay at `warn` whatever the level.

## Prerequisites

Spin 4.1.0 or newer, pinned as `spin` in `.tool-versions`. `spin-sdk` 8 imports
WASI HTTP 0.3, which Spin 4.0.x does not provide, so an older runtime cannot
start the component. Install or upgrade from the
[Spin install guide](https://spinframework.dev/install).

asdf has no first-party Spin plugin, so `asdf install` does not provide this
one; install it directly, or add a community plugin if you want asdf to manage
it. The pin records the required version and is what the smoke checks against.

Nothing in the build catches this. The component compiles, `cargo
test-spin` passes, and only `spin up` reports:

```
Error: component imports instance `wasi:http/types@0.3.0`, but a
matching implementation was not found in the linker
```

`./scripts/smoke-spin.sh` checks the runtime version before it starts the app,
so it fails with a clear message rather than that linker error.

## Verify the complete local handoff

With Spin 4.1.0 or newer installed, run:

```bash
./scripts/smoke-spin.sh
```

The script creates an isolated manifest and strictly validates a fresh app
config. It maps the logical EdgeZero store with
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default`, then runs `ts
config push --adapter spin --local`. Without that mapping, the push can land in
a store the runtime never reads. Local push writes the temporary
`.spin/sqlite_key_value.db`; no repository-local Spin state is used.

Spin encodes `v_<store>_v_<key>` names. The smoke supplies these exact
variables:

- `v_trusted_x5fserver_x5fsecrets_v_handler_x5fpassword`
- `v_trusted_x5fserver_x5fsecrets_v_publisher_x5fproxy_x5fsecret`
- `v_trusted_x5fserver_x5fsecrets_v_ec_x5fpassphrase`

It first launches `spin up` before config is pushed, then relaunches it while
omitting each variable independently. Every publisher request must return 503,
and the controlled config or variable delta must identify the intended missing
input. A generic degraded response cannot satisfy the check. The smoke reads
that diagnostic from the component's stderr.

The final non-health publisher request must return 200, retain the stub-origin
sentinel, rewrite its origin URL to the Spin listener, and omit the original
URL. The exit trap stops Spin and the stub origin and removes the isolated
manifest, SQLite store, response captures, and logs.

The smoke also checks malformed envelopes and invalid settings in a valid
config envelope, fixed GET/HEAD probe responses, anonymous probes under a
catch-all auth rule, and an ordinary protected-route challenge. Positive starts
wait for `/ready`; negative starts wait for liveness or transport. Probe calls
must leave origin counters unchanged; the final publisher control must reach
the origin. See the [operational response contract](./api-reference#operational-probe-contract)
for the shared assertions.

## Runtime boundaries

Use the same store mapping and encoded variables in the deployment provider;
empty variable defaults fail closed.

Request-time config comes from the `default` label that startup reads. Spin
ignores the requested store name, so every request-time config read uses that
label. That covers request-signing metadata (`current-kid`, `active-kids`, and
one public JWK per `kid`) and DataDome IP CIDR bypass lists. Keep those keys
distinct from each other and from `trusted_server_config`. Request-signing
private keys are encoded component variables, such as
`v_signing_x5fkeys_v_<encoded kid>` for store `signing_keys`.

The generic runtime KV store is the `trusted_server_kv` label, which the
component opens on first use. `spin.toml` does not grant it, so KV operations
answer `KvError::OpenFailed` and every route is still served. To enable it,
add the label to the component's `key_value_stores` and give it a backend in
the runtime config. `ts provision --adapter spin` adds every KV and config
label `edgezero.toml` declares, including `trusted_server_kv`, after which
`spin up` needs runtime config for each of them. Never point
`EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME` at `default`: generic KV
writes would then share the label that holds the app config and signing
metadata.

EC lookup and key rotation remain unavailable. The read-only
`/_ts/admin/eids` diagnostic remains available. Forwarded client-IP headers
are sanitized, and Spin derives its runtime authority, scheme, and
client-address headers in the innermost normalization layer. See the
[store inventory](./edgezero#store-inventory) for every store each adapter
reads.

The integration CI installs the pinned Spin and runs the `spin_runtime_`
tests against the production component, including discovery with signing
metadata in `default`. The health/startup CI matrix runs the production smoke
with the pinned Spin runtime. Manual verification should also record the
tested commit and exact tool versions.

Compare the [Fastly](./fastly), [Cloudflare](./cloudflare), and
[Axum](./axum-dev) adapter journeys.
