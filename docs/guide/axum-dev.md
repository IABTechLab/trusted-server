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

- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD`
- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET`
- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE`

Before the positive request, the smoke omits the config and each secret in
separate process starts. Each request must return 500 and its log must identify
the missing environment variable or exact setting path. This prevents a port,
launcher, or origin error from satisfying a negative case.

The final publisher request must return 200, include the stub-origin sentinel,
rewrite its origin URL to the Axum listener, and remove the original URL. The
exit trap stops the Axum process and stub origin, then removes the temporary
config, local-push output, bodies, headers, and logs.

## Listen address

Axum listens on `127.0.0.1:8787` by default. It reads EdgeZero's
`EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT` first, so a container
can listen on `0.0.0.0`. `PORT` is the port fallback local tooling uses. An
invalid EdgeZero value falls back with a startup warning, as EdgeZero does; an
invalid `PORT` stops the process. The container image sets
`EDGEZERO__ADAPTER__PORT=8080`, so a `PORT` injected by a container platform is
ignored there; set `EDGEZERO__ADAPTER__PORT` instead.

## Run the container image

`crates/trusted-server-adapter-axum/Dockerfile` builds the server image from the
repository root. Releases with package publication enabled push it to GHCR as
`trusted-server-axum`; see
[Download from GitHub Packages](./release-verification#download-from-github-packages)
to verify it. The image is Debian slim with CA certificates and the server binary
only. It runs as UID `10001`, writes nothing, listens on `0.0.0.0:8080`, and
reads the same config and secret variables as the smoke above. CI builds it and
runs that smoke against it with a read-only root filesystem:
`AXUM_SMOKE_IMAGE=<image> ./scripts/smoke-axum.sh` (Linux host networking). It
stops on `SIGINT`, the signal the server handles, so `docker stop` shuts it
down without waiting for the kill timeout. SIGTERM draining and readiness are
not implemented yet (#1236).

```bash
docker build --file crates/trusted-server-adapter-axum/Dockerfile \
  --build-arg TRUSTED_SERVER_VERSION="$(git rev-parse HEAD)" \
  --tag trusted-server-axum .
docker run --rm --read-only --cap-drop ALL --publish 8080:8080 \
  --env TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG \
  --env TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD \
  --env TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET \
  --env TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE \
  trusted-server-axum
```

`--env NAME` without a value passes that variable from your shell, so secrets
stay out of the command line. The support status above still applies inside the
container.

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
