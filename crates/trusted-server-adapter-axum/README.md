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

## Native celld lab variant

The AWS RC's prepared-store startup is also available with the default-off
`celld-config`, `celld-kv`, and `lab-store-check` features. This variant loads a
verified config envelope before listening and retains live
`trusted_server_state` KV through production `RuntimeServices`. It does not
implement EC conditional storage or enable auctions. Default builds retain the
previous generic-KV-unavailable behavior.

Use `edgezero.lab.toml` for matching CLI store metadata. Keep the root manifest
for non-lab builds. `lab-store-check` requires explicit `celld-config` and
`celld-kv` bindings for `trusted_server_config` and `trusted_server_state` in
`EDGEZERO__STORE_BINDINGS_FILE`. Incomplete selections fail before preparation;
there is no local config/redb fallback. Native secret references still use
`EDGEZERO__SECRETS__*`.

The diagnostic is `/_ts/admin/lab/store-check`, under the existing mandatory
admin handler. PUT accepts `{"value":"proof"}` and checks read-back, GET reads
without writing, and DELETE checks absence. All use the fixed key
`ts-lab/store-check`. Success JSON contains `config_sha256`, `scheme`, `host`,
`key`, and nullable `value`. The checksum is the adopted envelope's payload
checksum before secret resolution. Scheme/host come from the same approved
URI/Host/RequestInfo facts used by real URL rewriting and backend consumers,
not raw forwarding headers. Responses are not cacheable. Provider
failures return non-success without credentials or remote response bodies.

Build the app with the root Dockerfile and
`--build-arg NATIVE_FEATURES=celld-config,celld-kv,lab-store-check`. The default
final target runs the app as UID/GID 10001. Inject the binding file at an
absolute path readable by that UID. No `/config` or `/data` root is implicitly
configured by the image. `docker build --target cli` builds the matching
operator CLI at `/usr/local/bin/ts`; it is not a deployment CLI extension.

On Linux the pinned celld provider uses reqwest 0.13's platform verifier and
native CA loader. Select a deployment-injected public CA with
`SSL_CERT_FILE=/etc/ts-lab/ca.pem` and `SSL_CERT_DIR=/etc/ssl/certs` to retain
system public CAs for outbound HTTPS. The file and directory must be readable
by UID 10001. The native CA loader uses only the environment-selected paths
when either variable is set, so a private-only file without the system CA
directory can break public HTTPS upstreams. Keep TLS verification enabled.

For a single-host lab behind HTTPS termination, the optional
`TS_LAB_PUBLIC_ORIGIN=https://lab.example.com` supplies fixed public URL facts
without enabling forwarding-header trust. It is available only with
`lab-store-check` and absent by default. Startup requires an HTTPS origin whose
authority exactly equals the adopted `publisher.domain`. Userinfo, paths including
a trailing slash, queries and fragments fail startup before listening.
The origin is captured once; changing it or publisher configuration requires a
restart.

Application handlers reject a missing, duplicate or mismatched Host. An absolute
request target must also agree with the configured HTTPS origin. They never
repair a hostile authority or derive public facts from forwarding headers.
Approved requests retain their path/query/body and receive the same HTTPS facts
in URI, Host and RequestInfo. Native ingress client address and geography remain
unchanged. The local `/health` route does not consume public URL facts. This does
not authenticate an ingress proxy or change the framework's trusted proxy policy.
Fly HTTPS acceptance still requires live deployment proof.

Run the actual disposable-service proof with dummy tokens and a native TLS
scoped service granting config read/write to the operator and config read plus
state read/write/delete/list to the runtime:

```bash
export TS_TEST_CELLD_ENDPOINT=https://127.0.0.1:7780
export TS_TEST_CELLD_CA=/path/to/disposable-public-ca.pem
# Supply TS_TEST_CELLD_RUNTIME_TOKEN and TS_TEST_CELLD_OPERATOR_TOKEN privately.
cargo test --locked -p trusted-server-adapter-axum \
  --target x86_64-unknown-linux-gnu \
  --features celld-config,celld-kv,lab-store-check \
  --test native_celld -- --include-ignored
```

This uses service aliases `ts-config` and `ts-state`. It publishes synthetic,
auction-disabled envelopes and exercises the actual TS binary, immutable
config adoption, authenticated diagnostic, independent KV read-back, restart
retention, and failures. It is not a hosted persistence or Fly ingress proof.
