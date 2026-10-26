# Axum development server

The Axum adapter runs Trusted Server as a native local process. It is the
shortest path for inspecting requests without an edge runtime; it is not a
production deployment target.

## Support status

| Adapter | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization |
| ------- | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | --------------------- |
| `axum`  | development    | real   | `500`          | no             | multiple         | outermost sanitize         | none                  |

This row is the canonical status summary. Axum supports multiple
auction providers and exposes a real `/health` handler. A startup failure
replaces every route with a 500 response, including `/health`.

## Verify the complete local handoff

From the repository root, run:

```bash
./scripts/smoke-axum.sh
```

The script creates and strictly validates an isolated config, then runs `ts
config push --adapter axum --local`. That push writes the config envelope to
the temporary `.edgezero/` directory; the Axum process does not read that file
directly. The script reads the envelope and exports it as
`TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG`.

App credentials use generated names from
`ts secrets plan --adapter axum --app-config trusted-server.toml --no-env`.
The renderer uses the runtime's actual environment mapping. Do not reuse
`HANDLER_PASSWORD`, `PUBLISHER_PROXY_SECRET`, or `EC_PASSPHRASE` labels.
Follow the [native input walkthrough](./secrets#axum-native-environment-input):
inject each raw value into the shell or supervisor that actually launches Axum.
A child process cannot export into its parent.

`ts secrets check --adapter axum` checks only its own environment for presence,
not remote process injection, credential validity, or delivery. Direct `set`
is unsupported. Native reads and TLS/no-follow rehearsal remain release gates.

Before the positive request, the smoke omits the config and each secret in
separate process starts. Each request must return 500 and its log must identify
the missing environment variable or exact setting path. This prevents a port,
launcher, or origin error from satisfying a negative case.

The final publisher request must return 200, include the stub-origin sentinel,
rewrite its origin URL to the Axum listener, and remove the original URL. The
exit trap stops the Axum process and stub origin, then removes the temporary
config, local-push output, bodies, headers, and logs.

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
