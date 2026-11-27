# EdgeZero Lifecycle and Stores

EdgeZero is the deployment and configuration layer used by the `ts` CLI, and
the runtime layer under every adapter: each adapter serves requests through
EdgeZero's request lifecycle and sends outbound HTTP through EdgeZero's client.
The repository's `edgezero.toml` declares one application, three logical
stores, and the Fastly, Axum, Cloudflare, and Spin adapter commands.

## Pinned version

The workspace `Cargo.toml` pins all six EdgeZero crates to one git revision of
`IABTechLab/edgezero`: `1619078a665188c1c282375ec9c11cd1b4f64ee7`, a commit of
EdgeZero's `rc/identity` integration branch. That branch carries pull request
275 and the identity work built on it. The pin moves with the branch until
those pull requests merge, and then to an EdgeZero release tag.

## Lifecycle

Use `ts config init` to create a file, edit the generated
`trusted-server.toml`, and run `ts config validate` before any platform
write. `ts config diff --adapter ADAPTER` compares the validated local
configuration with the selected platform value. `ts config push --adapter
ADAPTER` writes it. Build, serve, deploy, health-check, rollback, and active
version operations use the adapter commands declared in `edgezero.toml`.

Configuration is startup state, not a live control plane. A successful push
does not change a running instance until the target's restart or deployment
path loads that value. Use `--staging` only with the corresponding staging
deployment flow. On Fastly the entry key is always the logical store ID, so
staging isolation comes from selecting a different physical store with
`EDGEZERO__STORES__CONFIG__<ID>__NAME`; see
[the CLI guide](/guide/cli#lifecycle-commands).

## Logical and physical stores

The manifest declares portable logical IDs:

| Kind            | Logical ID                  |
| --------------- | --------------------------- |
| KV              | `trusted_server_runtime_kv` |
| KV (EC records) | `trusted_server_kv`         |
| Configuration   | `trusted_server_config`     |
| Secrets         | `trusted_server_secrets`    |

An adapter maps those IDs to platform resources. The runtime-facing
`StoreName` used for reads and the management-facing `StoreId` used for
writes are deliberately distinct types. Environment mappings such as
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` may change the
physical store name without changing application configuration.

Fastly binds its stores in `fastly.toml` and through the managed deploy.
Axum loads config and required secrets once at startup from EdgeZero-prepared
stores and shares immutable snapshots with requests. Its generic KV is unsupported. Cloudflare reads Worker variables, secrets
and one KV namespace binding. Spin reads its `default` key-value label,
component variables and an optional `trusted_server_runtime_kv` label.

## Store inventory

Each row is a store that a manifest declares or that the runtime opens by
name. "Startup" is the app-config load; "request" is a read or write while
serving a request. Unless a row says otherwise, an unsupported read fails and
the feature that depends on it reports its own error.

| Store                                                                                  | Declared in                                                                                                                                      | Purpose                                                       | Fastly                                                                                            | Cloudflare                                                                                                          | Spin                                                                                                | Axum                                                                                                             |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `trusted_server_config` (config)                                                       | `edgezero.toml`, Fastly and Axum `Hooks::stores()`, `fastly.toml`, `cloudflare.toml`                                                             | App-config blob that `ts config push` writes                  | Startup reads it from the linked Config Store                                                     | Not read; startup reads the `TRUSTED_SERVER_CONFIG` variable instead                                                | Startup reads the `trusted_server_config` key from the `default` label                              | Startup loads the prepared config envelope; requests read its immutable snapshot                                 |
| `trusted_server_secrets` (secrets)                                                     | `edgezero.toml`, Fastly and Axum `Hooks::stores()`, `fastly.toml`, `cloudflare.toml`, `spin.toml` `[variables]`                                  | Secret values that the app config names                       | Startup and request read the linked Secret Store                                                  | Startup and request read Worker secrets (`wrangler secret put`) by key name; `wrangler.ci.toml` defines test values | Startup and request read encoded component variables (`v_<store>_v_<key>`)                          | Startup resolves active references from prepared secrets; requests read only those captured keys                 |
| `trusted_server_runtime_kv` (KV)                                                       | `edgezero.toml`, Fastly `Hooks::stores()`, Spin `Hooks::stores()`, `fastly.toml` (local), `wrangler.toml`, `wrangler.ci.toml`, `cloudflare.toml` | Generic runtime KV that `RuntimeServices::kv_store()` returns | Request: the store linked under this ID. Unlinked: `KvError::OpenFailed`                          | Request: the `TRUSTED_SERVER_RUNTIME_KV` namespace. Unbound: `KvError::Unavailable`                                 | Request: the `trusted_server_runtime_kv` label. Not granted: `KvError::OpenFailed`                  | Unsupported: `KvError::Unavailable`; pending #1587 → #1574                                                       |
| `jwks_store` (config)                                                                  | `fastly.toml` (local)                                                                                                                            | Request-signing metadata: `current-kid`, `active-kids`, JWKs  | Request reads the Config Store of this name                                                       | Unsupported: discovery answers `500`, `/verify-signature` answers `verified: false`, and signing fails              | Request reads the `default` label (Spin ignores the store name)                                     | Unsupported: enabled signing fails startup; otherwise discovery answers `500` and verification `verified: false` |
| `signing_keys` (secrets)                                                               | `fastly.toml` (local)                                                                                                                            | Request-signing private keys by `kid`                         | Request reads the Secret Store of this name                                                       | Unsupported (see `jwks_store`)                                                                                      | Request reads encoded variables `v_signing_x5fkeys_v_<encoded kid>`, which `spin.toml` must declare | Unsupported: no snapshot for this store; enabled signing fails startup                                           |
| `request_signing.config_store_id`, `request_signing.secret_store_id`                   | App config                                                                                                                                       | Management targets for key rotation                           | Rotation writes through the Fastly API                                                            | Unsupported: rotation answers `501`                                                                                 | Unsupported: rotation answers `501`                                                                 | Unsupported: rotation answers `501`                                                                              |
| `api-keys` (secrets)                                                                   | `fastly.toml` (local)                                                                                                                            | Fastly API token for key rotation                             | Request reads it during rotation                                                                  | Not used                                                                                                            | Not used                                                                                            | Not used                                                                                                         |
| `ec.ec_store` (KV, `ec_identity_store` by default)                                     | App config, `fastly.toml` (local)                                                                                                                | Edge Cookie identity records                                  | Request reads and writes the KV Store of this name                                                | Unsupported: EC lookup is unavailable                                                                               | Unsupported: EC lookup is unavailable                                                               | Unsupported: EC lookup is unavailable                                                                            |
| `counter_store`                                                                        | Fastly service resource                                                                                                                          | Edge Rate Limiting counter for batch sync; not a KV store     | Request opens the rate counter of this name; Viceroy stubs rate counters, so no local declaration | Not used                                                                                                            | Not used                                                                                            | Not used                                                                                                         |
| `edgezero_logging` (config)                                                            | Fastly service resource                                                                                                                          | Optional `EDGEZERO__LOGGING__LEVEL` override                  | EdgeZero reads it once per instance; changes reach new instances                                  | Not used; the level is a Worker variable                                                                            | Not used; the level is a component variable                                                         | Not used; the level is an environment variable                                                                   |
| `datadome-ip-bypass` and `protection_excluded_ip_cidr_sources[].config_store` (config) | App config                                                                                                                                       | DataDome IP CIDR bypass lists                                 | Request reads the Config Store of this name                                                       | Unsupported                                                                                                         | Request reads the `default` label (Spin ignores the store name)                                     | Unsupported: enabled dynamic Config Store CIDR sources fail startup; use inline rules                            |
| Deprecated `secret_store` selectors                                                    | App config                                                                                                                                       | Superseded by `trusted_server_secrets`                        | Not read                                                                                          | Not read                                                                                                            | Not read                                                                                            | Not read                                                                                                         |
| Deprecated `auction.creative_store`                                                    | App config                                                                                                                                       | Superseded; creatives are delivered inline                    | Not read                                                                                          | Not read                                                                                                            | Not read                                                                                            | Not read                                                                                                         |

Axum declares config and secrets in `Hooks::stores()`, and declares
`trusted_server_kv` with the shared EC record layout for identity records;
generic runtime KV stays unavailable. Its `axum.toml` contains application and
logging metadata, with no store declarations. Identity records reach
`trusted_server_kv` through #1587 (shared backend over EdgeZero `KvStore`) and
#1574 (Axum identity). This generic KV wiring does not migrate the
existing Fastly identity record backend. Axum snapshot writes fail; publish
prepared inputs and restart to change config or secrets.

`cloudflare.toml` is a reference manifest: the Worker runtime does not read
it. The integration Viceroy template
(`crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml`)
declares test-only fixtures for the stores above.

On Spin, every request-time config read is served from the `default` label,
whatever store name the caller passes. Keep config keys, JWK `kid` values and
DataDome bypass keys distinct within that label.

## Blob and chunk flow

`config push` serializes validated settings into one EdgeZero
`BlobEnvelope`. Store-backed settings contain secret key names, not secret
values; the adapter resolves those keys from the logical secret store during
startup.

When a Fastly envelope exceeds one Config Store entry, the writer stores
bounded chunks and replaces the root value with a versioned
`fastly_config_chunks` pointer. Startup verifies each chunk's declared
length and SHA-256 and then verifies the reconstructed envelope. A missing,
oversized, reordered, or modified chunk fails configuration loading.

`ts config gc --adapter fastly` previews orphaned chunks by default. An
actual deletion requires `--yes --older-than WINDOW`. The age assertion
applies to the whole physical store, not one root key; verify the reported
store before deletion, especially when using `--store` or `--no-env`.

## Request handling

These rules hold on Fastly, Axum, Cloudflare, and Spin.

**Inbound requests.**

- A request target over 8192 bytes returns 414. More than 100 header fields,
  or more than 64 KiB of header bytes, returns 431.
- Ambiguous request framing returns 400: a duplicate or malformed
  `Content-Length`, `Content-Length` with `Transfer-Encoding`, or a
  `Transfer-Encoding` that is not chunked.
- Each endpoint reads its body under its own cap and returns 413 above it. A
  declared `Content-Length` over the cap returns 413 before any body byte is
  read. A body still unread 30 s after the request arrived returns 408.
- A publisher request streams its body to the origin under a 16 MiB cap, and a
  larger upload returns 413. A forwarded `GET` or `HEAD` that carries body bytes
  returns 400.

**Outbound requests.**

- Every request sets a 1 s connect timeout and a constant total timeout: the
  configured timeout where a setting names one, such as an auction provider's
  `timeout_ms`, 2 s for Tinybird, and otherwise 15 s for a buffered response or
  60 s for a streamed one. A redirect chain also has a 60 s deadline. EdgeZero
  enforces the connect timeout natively on Fastly only. On Axum, Cloudflare,
  and Spin it is best effort, and the total timeout still bounds the request.
- TLS is always verified on every outbound HTTPS request.
- A request that sets no `Accept-Encoding` carries `Accept-Encoding: identity`.
- Hop-by-hop fields, and fields that `Connection` names, never reach an
  upstream or come back from one. A request or response header whose value is
  not UTF-8 is dropped whole, with a warning that names the header.
- The request URI is normalized as a WHATWG URL. A publisher request whose
  target has a fragment, a backslash, or userinfo returns 400.
- On Fastly, EdgeZero registers each backend at request time and names it
  `ez_b31v1_` followed by a hash of the backend configuration.

**Responses.**

- Response egress strips hop-by-hop fields and never sends `Trailer`. A framing
  error answers 500 before the response is committed.
- A response body must finish within 60 s after egress starts.
- Framework errors, such as a 404, 405, 414, or 431, render as EdgeZero JSON
  and carry `x-ts-version`. A startup failure answers the fixed response of the
  [operational response contract](./api-reference#operational-probe-contract),
  which carries `x-ts-version`. Only EdgeZero's Fastly assembly-error
  response, for a method outside the publisher fallback set, carries none.

## Safe operator sequence

1. Put required secret values in the physical store mapped from
   `trusted_server_secrets`.
2. Run `ts config validate`.
3. Review `ts config diff --adapter ADAPTER`; use `--no-diff` when
   deliberately inline configuration must not appear in logs.
4. Run `ts config push --adapter ADAPTER`.
5. Start or deploy the adapter and verify a non-health publisher route. A
   healthy endpoint alone may not prove that application configuration loaded.
   Confirm that its `x-ts-version` response header reports the release you
   deployed.

See [Configuration](/guide/configuration) for the field contract, the
[CLI guide](/guide/cli) for exact command help, and
[deployment guides](/guide/integrations-overview#adapter-support) for target
behavior.
