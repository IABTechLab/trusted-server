# Axum server

The Axum adapter runs Trusted Server as a native process. Locally it is the
shortest path for inspecting requests without an edge runtime. The same binary
is the production entry point: it loads config and secrets from prepared stores
(local files, or AWS AppConfig and Secrets Manager), refuses to listen when a
selected store cannot be prepared, answers `/ready` and every route with `503`
when a required config value or secret is missing or invalid, and drains
in-flight requests on SIGTERM. See
[Axum production startup](./native-rc-demo). Edge Cookie records and partner
APIs run through the shared identity app over EdgeZero's prepared record
service; see [native EC persistence](./native-ec-persistence).

## Support status

| Adapter | Release status     | Liveness | Startup failure | Readiness (ready/failed) | Provider fan-out | Trusted-client-IP handling | Request normalization |
| ------- | ------------------ | -------- | --------------- | ------------------------ | ---------------- | -------------------------- | --------------------- |
| `axum`  | production startup | `200`    | `503`           | `200` / `503`            | multiple         | outermost sanitize         | none                  |

This row is the canonical status summary. Axum supports multiple auction
providers. GET/HEAD `/health` remains live after caught initialization failure;
`/ready` reports initialization readiness. Application or failure-router
construction completes before the listener binds. See the
[operational response contract](./api-reference#operational-probe-contract).

## Verify the complete local handoff

From the repository root, run:

```bash
./scripts/smoke-axum.sh
```

The script creates and strictly validates an isolated config, then runs `ts
config push --adapter axum --local`. It places the exported envelope in the
`trusted_server_config` entry of an isolated
`local-config-trusted_server_config.json` map. The process reads that prepared
local provider from `EDGEZERO__ADAPTER__CONFIG_DIR`, validates its bounded
envelope and active secret references, and installs immutable snapshots and
the actual router before listening.

The smoke also supplies a writable `EDGEZERO__ADAPTER__DATA_DIR`. The record
store is always declared and EdgeZero prepares its local provider before TS
loads application config, even when `ec.storage` is absent. Every local-provider
start needs that directory; remote-only bindings do not. See
[Getting started](./getting-started), Option B, for the complete shell setup.

Local secrets use these variables:

- `EDGEZERO__SECRETS__HANDLER_PASSWORD`
- `EDGEZERO__SECRETS__PUBLISHER_PROXY_SECRET`
- `EDGEZERO__SECRETS__EC_PASSPHRASE`

Only referenced secrets are captured during startup. Changes to provider files
or values require restarting the process. Legacy `TRUSTED_SERVER_CONFIG_*`
and `TRUSTED_SERVER_SECRET_*` variables are not fallback inputs. The
[Axum production startup](./native-rc-demo) covers provider selection and lifecycle
checks.

Before the positive request, the smoke omits the config and each secret in
separate process starts. Each start must keep Axum listening with the failure
router: each publisher request must return 503 and its log must identify the
missing prepared config or required app-config secret. This prevents a port,
launcher, or origin error from satisfying a negative case.

The final publisher request must return 200, include the stub-origin sentinel,
rewrite its origin URL to the Axum listener, and remove the original URL. The
exit trap stops the Axum process and stub origin, then removes the temporary
config, local-push output, bodies, headers, and logs.

The smoke also checks malformed envelopes and invalid settings in a valid
config envelope, fixed GET/HEAD probe responses, anonymous probes under a
catch-all auth rule, and an ordinary protected-route challenge. Positive starts
wait for `/ready`; negative starts wait for liveness or transport. Probe calls
must leave origin counters unchanged; the final publisher control must reach
the origin. See the [operational response contract](./api-reference#operational-probe-contract)
for the shared assertions.

## Listener address

EdgeZero's Axum production startup binds the listener at
`EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT`, `127.0.0.1:8787` when
unset. A host that is not an IP address or a port that is not a non-zero
number stops startup before the listener binds. The adapter does not read
`PORT`.

## Ingress limits

The listener reads its HTTP/1 ingress limits from the environment at startup.
An unset variable keeps its default. A set value must be a positive integer in
range. An invalid value stops startup with exit status 1 and logs
`invalid Axum ingress setting` with the setting's name, never its value.

| Variable                                           | Default | Range            |
| -------------------------------------------------- | ------- | ---------------- |
| `EDGEZERO__ADAPTER__INGRESS__MAX_CONNECTIONS`      | 256     | 1 to 4096        |
| `EDGEZERO__ADAPTER__INGRESS__MAX_RAW_HEAD_BYTES`   | 65536   | 8192 to 1048576  |
| `EDGEZERO__ADAPTER__INGRESS__MAX_HEADER_COUNT`     | 100     | 1 to 1024        |
| `EDGEZERO__ADAPTER__INGRESS__HEAD_READ_TIMEOUT_MS` | 10000   | 1 to 120000 (ms) |

The raw-head limit covers the request line and every header field together,
and response headers are held to the same byte limit. A request head over the
byte limit or the field limit gets 431. A connection beyond `MAX_CONNECTIONS`
is closed without a response, and so is a connection whose request head does
not arrive within the head read timeout, which also bounds an idle keep-alive
connection.

## Logging

EdgeZero's Axum production startup installs the logger, and Trusted Server
installs none of its own. It logs to standard output at `info` by default. Set
`EDGEZERO__LOGGING__LEVEL` to a single level (`trace`, `debug`, `info`,
`warn`, `error`, or `off`, in any case) to change it, for example
`EDGEZERO__LOGGING__LEVEL=debug cargo run -p trusted-server-adapter-axum`.
An unrecognized value, including a per-module filter such as
`trusted_server=debug`, stops startup. `RUST_LOG` is not read. EdgeZero's
startup warnings, logged under the `edgezero::boot` target, stay at `warn`
whatever the level.

The `trace` level can log auction payloads and TCF consent strings, for example
the `adserver_mock` mediation request, and the full Prebid Server response
(including its `ext.debug` echo of the resolved request) when a provider sets
`profile_config.debug = true`. Enable it only for short local debugging
sessions.

## Runtime boundaries

Axum implements the read-only `/_ts/admin/eids` diagnostic and EC record lookup
through the configured shared record service. Record lookup returns 501 without
identity storage; key rotation and other administrative mutations remain
unsupported. It sanitizes forwarded client-IP headers but does not resolve
`[trusted_client_ip]`; see the
[adapter matrix](./api-reference#adapter-and-startup-support).

The generic runtime KV store is unsupported: reads and writes return
`KvError::Unavailable`, a warning is logged once per process, and publisher
pages continue to be served. `trusted_server_kv` remains declared in the
application manifest because identity storage needs it; identity records reach
it through EdgeZero's prepared record service, not generic KV, and there is no
adapter-local file store. Config and required secrets are loaded once at
startup from prepared stores, independently of KV. See the
[store inventory](./edgezero#store-inventory).

For an edge deployment, use the [Fastly](./fastly),
[Cloudflare](./cloudflare), or [Spin](./spin) guide.
