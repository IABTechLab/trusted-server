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

Secrets use `TRUSTED_SERVER_SECRET_{STORE}_{KEY}`. The exact required variables
are:

- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET`
- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE`

Axum reads the permission policy file the settings name from the folder
`TRUSTED_SERVER_PERMISSIONS_DIR` names, as `<folder>/<name>.yaml`, and checks
it against the SHA-256 the push wrote into the blob, so the folder must hold
the bytes that were pushed. The push writes no copy of the file for Axum. The
smoke points the variable at the repository's `config/permissions` folder,
which holds the sample its settings name.

Before the positive request, the smoke omits the config, the policy folder and
each secret in separate process starts. Each request must return 500 and its
log must identify the missing environment variable or exact setting path. This
prevents a port, launcher, or origin error from satisfying a negative case.

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
the request to the ad server, and the full Prebid Server response (including
its `ext.debug` echo of the resolved request) when the `[demand.<name>]` table
of a Prebid Server demand source sets `debug = true`. Enable it only for short
local debugging sessions.

## Runtime boundaries

Axum sanitizes forwarded client-IP headers but does not resolve
`[trusted_client_ip]`; see the
[adapter matrix](./api-reference#adapter-and-startup-support). The modules'
request filters and the device module `[device]` selects run as on Fastly,
and `[device] module = "platform"` classifies from the `User-Agent`, because
the dev server supplies no TLS or HTTP/2 signals. An Edge Cookie module
creates and writes its cookie with no identity graph behind it, and
`POST /_ts/api/v1/ec/resolve` sets one the same way. `[[proxy.asset_routes]]`
are served from each route's own source with the asset body held whole, with
no limit. The dev server has no Image Optimizer, so startup warns about each
route that enables one, and each request such a route would optimize answers
`502` (see [Asset Routes](./asset-routes)).

Axum sends no [auction telemetry](./telemetry#adapter-support), and warns at
startup when `[analytics]` selects a module.

Axum always verifies HTTPS origin certificates against the public certificate
authorities built into its client. It does not honor
`proxy.certificate_check = false` and logs a warning at startup when the
setting is off, so an origin whose certificate is self-signed or issued by a
private authority cannot be reached through it.

For an edge deployment, use the [Fastly](./fastly),
[Cloudflare](./cloudflare), or [Spin](./spin) guide.
