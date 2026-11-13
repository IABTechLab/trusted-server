# Axum development server

The Axum adapter runs Trusted Server as a native local process. It is the
shortest path for inspecting requests without an edge runtime; it is not a
production deployment target.

## Support status

| Adapter | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization |
| ------- | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | --------------------- |
| `axum`  | development    | real   | exit 1         | no             | multiple         | outermost sanitize         | none                  |

This row is the canonical status summary. Axum supports multiple
auction providers and exposes a real `/health` handler. A startup failure stops
the process with exit status 1 before it listens, so no route answers.

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

Local secrets use these variables:

- `EDGEZERO__SECRETS__HANDLER_PASSWORD`
- `EDGEZERO__SECRETS__PUBLISHER_PROXY_SECRET`
- `EDGEZERO__SECRETS__EC_PASSPHRASE`

Only referenced secrets are captured during startup. Changes to provider files
or values require restarting the process. Legacy `TRUSTED_SERVER_CONFIG_*`
and `TRUSTED_SERVER_SECRET_*` variables are not fallback inputs. The
[native RC guide](./native-rc-demo) covers provider selection and lifecycle
checks.

Before the positive request, the smoke omits the config and each secret in
separate process starts. Each start must fail before Axum listens: the process
must exit non-zero and its log must identify the missing prepared config or
required app-config secret. This prevents a port, launcher, or origin error from
satisfying a negative case.

The final publisher request must return 200, include the stub-origin sentinel,
rewrite its origin URL to the Axum listener, and remove the original URL. The
exit trap stops the Axum process and stub origin, then removes the temporary
config, local-push output, bodies, headers, and logs.

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

Axum logs to standard output at `info` by default. Set `RUST_LOG` to a single
level (`trace`, `debug`, `info`, `warn`, `error`, or `off`) to change it, for
example `RUST_LOG=debug cargo run -p trusted-server-adapter-axum`.
Unrecognized values, including per-module filters such as
`trusted_server=debug`, fall back to `info` and print a warning to standard
error at startup.

The `trace` level can log auction payloads and TCF consent strings, for example
the `adserver_mock` mediation request, and the full Prebid Server response
(including its `ext.debug` echo of the resolved request) when a provider sets
`profile_config.debug = true`. Enable it only for short local debugging
sessions.

## Runtime boundaries

Axum implements the read-only `/_ts/admin/eids` diagnostic. EC record lookup
and key rotation are registered but return not-supported responses because the
adapter has no request-time KV implementation. It sanitizes forwarded client-IP
headers but does not resolve `[trusted_client_ip]`; see the
[adapter matrix](./api-reference#adapter-and-startup-support).

For an edge deployment, use the [Fastly](./fastly),
[Cloudflare](./cloudflare), or [Spin](./spin) guide.
