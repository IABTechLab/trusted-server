# Trusted Server CLI — EdgeZero-Backed Product CLI

**Date:** 2026-06-16
**Status:** Draft design, revised for blob app-config
**Scope:** Initial `ts` product CLI; audit is specified separately

## 1. Goal

Add a Trusted Server product CLI binary, `ts`, as the normal operator entrypoint
for Trusted Server workflows.

`ts` exposes Trusted Server-specific config initialization and EdgeZero-backed
platform lifecycle/config commands through one binary. Trusted Server-specific
commands own Trusted Server behavior. Platform lifecycle and config-store writes
are thin delegates to EdgeZero and must not reimplement platform behavior.

The command surface is:

```text
ts config init
ts config validate
ts config push

ts auth login --adapter <adapter>
ts auth status --adapter <adapter>
ts auth logout --adapter <adapter>

ts provision --adapter <adapter>
ts serve --adapter <adapter>
ts build --adapter <adapter>
ts deploy --adapter <adapter>
```

`ts` is the user-facing binary. EdgeZero is the platform execution engine.

`ts config push` owns Trusted Server validation, then delegates blob publication
to EdgeZero's typed config push path:

```text
trusted-server.toml
  -> parse as Trusted Server Settings
  -> apply EdgeZero app-config env overlay unless --no-env is passed
  -> validate as TrustedServerAppConfig
  -> serialize validated Settings to JSON
  -> wrap JSON in EdgeZero BlobEnvelope
  -> push the blob through EdgeZero platform primitives
```

The blob model is intentional. Full Trusted Server configs can exceed Fastly
config-store per-entry limits if flattened into one entry per setting. EdgeZero's
Fastly adapter may split the envelope into chunks and write a small pointer at
the logical config key; that adapter behavior is still owned by EdgeZero.

## 2. Non-goals

The initial `ts` CLI does **not** do any of the following:

- reimplement EdgeZero auth/provision/serve/build/deploy logic in Trusted Server;
- construct Fastly/Wrangler/Spin commands directly in `ts`;
- define a Trusted Server-owned platform adapter registry;
- require operators to call `edgezero` for normal Trusted Server workflows;
- include `ts dev`;
- include `ts audit` — separate spec;
- perform custom Fastly API provisioning;
- add a Trusted Server platform adapter layer;
- support runtime plugin/subcommand discovery;
- expose a public reusable `trusted-server-cli` library API;
- write request-signing key/bootstrap secrets;
- write secret-store entries of any kind;
- generate config signing / DSSE artifacts;
- support config pull/inspect commands.

## 3. File ownership model

### 3.1 Source-controlled files

The repository tracks:

```text
edgezero.toml
trusted-server.example.toml
```

`edgezero.toml` is the EdgeZero platform manifest. It declares the Trusted
Server app, stores, adapters, and platform command metadata.

`trusted-server.example.toml` is the source-controlled app-config template. It
uses only example/placeholder values and is kept in sync with the Trusted Server
settings schema.

### 3.2 Operator-owned files

The repository ignores:

```text
trusted-server.toml
```

`trusted-server.toml` is operator-authored app config. It is never committed as a
source-controlled deployment artifact.

### 3.3 App name

The EdgeZero app name is fixed for this product:

```toml
[app]
name = "trusted-server"
```

Because the app name is `trusted-server`, EdgeZero's app-config naming
convention and Trusted Server's historical config filename both resolve to:

```text
trusted-server.toml
```

## 4. EdgeZero manifest requirements

Trusted Server uses EdgeZero platform manifests and logical store IDs.

Minimum initial manifest store declarations:

```toml
[stores.config]
ids = ["app_config"]
default = "app_config"

[stores.secrets]
ids = ["secrets"]
default = "secrets"
```

The initial `ts config push` only writes config-store entries. The `secrets`
store is declared for runtime/future use but is not written by this CLI spec.

Platform store names are not stored in `trusted-server.toml`. They are resolved
by EdgeZero via its environment overlay, for example:

```text
EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME=publisher-a-ts-config
EDGEZERO__STORES__SECRETS__SECRETS__NAME=publisher-a-ts-secrets
```

## 5. Runtime payload contract

`ts config push` writes a single logical Trusted Server app-config blob by
default. It does **not** publish flattened per-setting entries.

| Key                                                     | Value                                                                                                              |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `app_config` by default, or `--key <key>` when supplied | Serialized `edgezero_core::blob_envelope::BlobEnvelope` whose `data` is the validated Trusted Server settings JSON |

The envelope contains:

- a version field owned by EdgeZero;
- the validated app-config JSON data;
- a SHA-256 hash over EdgeZero's canonical JSON form of `data`;
- generation timestamp metadata.

Runtime loading must verify the envelope hash before constructing `Settings`.
If an adapter must split a large envelope to satisfy platform limits, the entry
at the logical key may be an adapter-owned pointer that identifies chunks. The
adapter/runtime loader must reconstruct and verify the envelope before exposing
settings to application code.

Reserved future keys, not written in this initial spec:

| Key                   | Future purpose                                                        |
| --------------------- | --------------------------------------------------------------------- |
| `ts-config-signature` | Optional signature/DSSE envelope over the blob hash                   |
| `ts-config-metadata`  | Optional JSON metadata: version, published_at, valid_until, policy_id |

Request-signing public/private state is intentionally out of scope for this
initial CLI. It will be revisited after EdgeZero exposes suitable secret-store
write primitives.

## 6. Blob config pipeline

`trusted-server.toml` remains the human-authored source format. The deployed
runtime payload is an EdgeZero `BlobEnvelope`.

Pipeline:

1. Read `trusted-server.toml` as UTF-8.
2. Parse as TOML using EdgeZero's typed app-config loader.
3. Apply EdgeZero's app-config environment overlay unless `--no-env` is passed.
4. Deserialize into `TrustedServerAppConfig`, preserving the same top-level shape
   as `Settings`.
5. Run Trusted Server deploy-time validation:
   - strict unknown-field rejection from the settings schema;
   - validator rules and runtime preparation checks;
   - placeholder/default secret rejection;
   - enabled integration startup validation;
   - auction provider reference validation;
   - EC partner registry validation.
6. Serialize the validated settings to JSON.
7. Build an EdgeZero `BlobEnvelope` over that JSON value.
8. Delegate diff/read/write/consent/dry-run behavior to EdgeZero typed config
   push.

The pushed blob hash is stable for equivalent resolved settings values. Reordered
TOML input and formatting/comment changes should not change the envelope data
hash if they produce the same resolved `Settings` value. Environment overlays can
change the resolved value; pass `--no-env` when a file-only validation/push is
required.

## 7. Command surface

### 7.1 EdgeZero delegate commands

```bash
ts auth login --adapter <adapter>
ts auth status --adapter <adapter>
ts auth logout --adapter <adapter>

ts provision --adapter <adapter>
ts serve --adapter <adapter>
ts build --adapter <adapter>
ts deploy --adapter <adapter>
```

These commands provide a Trusted Server product CLI wrapper around EdgeZero
platform lifecycle behavior.

Behavior:

- Delegate to EdgeZero command handlers for the selected adapter.
- Preserve EdgeZero adapter semantics, validation, local/remote behavior, and
  platform-specific error handling.
- Do not read, validate, transform, or push `trusted-server.toml` unless the
  delegated EdgeZero command explicitly requires app/manifest context.
- Do not construct Fastly, Wrangler, Spin, or other platform commands directly in
  Trusted Server code.
- Do not implement platform-specific REST/API writes in Trusted Server code.

### 7.2 `ts config init`

```bash
ts config init [--app-config <path>] [--config <path>] [--force]
```

Defaults:

| Option         | Default               |
| -------------- | --------------------- |
| `--app-config` | `trusted-server.toml` |

`--config` is accepted as a compatibility alias for `--app-config`.

Behavior:

- Copies `trusted-server.example.toml` to the target config path.
- Creates parent directories when needed.
- Refuses to overwrite an existing file unless `--force` is passed.
- Does not read or validate `edgezero.toml`.
- Does not contact any platform.
- Does not run a wizard.
- May copy placeholder/example values. A successful init does not imply the
  resulting file passes `ts config validate`; validation and push still reject
  placeholder/default secrets until the operator replaces them.

Success output is concise, for example:

```text
Initialized config at trusted-server.toml
```

### 7.3 `ts config validate`

```bash
ts config validate [--app-config <path>] [--manifest <path>] [--no-env] [--strict]
```

Defaults:

| Option         | Default                                                      |
| -------------- | ------------------------------------------------------------ |
| `--app-config` | `<app name>.toml`, resolved by EdgeZero from `edgezero.toml` |
| `--manifest`   | `edgezero.toml`                                              |

Behavior:

- Loads and validates the local Trusted Server config through EdgeZero's typed
  app-config validation path.
- Applies app-config environment overlays unless `--no-env` is passed.
- Validates `edgezero.toml` and app-config compatibility.
- Does not contact any platform.
- Logs success through the EdgeZero CLI logger.

No Trusted Server-specific `--json` output is defined in this revision; machine
readable validation output should be added upstream in EdgeZero and then exposed
here consistently.

### 7.4 `ts config push`

```bash
ts config push \
  --adapter <adapter> \
  [--app-config <path>] \
  [--manifest <path>] \
  [--store <logical-config-store-id>] \
  [--key <config-entry-key>] \
  [--local] \
  [--dry-run] \
  [--no-env] \
  [--no-diff] \
  [--yes] \
  [--runtime-config <path>]
```

Defaults:

| Option         | Default                                                           |
| -------------- | ----------------------------------------------------------------- |
| `--app-config` | `<app name>.toml`, resolved by EdgeZero from `edgezero.toml`      |
| `--manifest`   | `edgezero.toml`                                                   |
| `--store`      | `[stores.config].default`, or the only configured config store id |
| `--key`        | resolved logical config store id, normally `app_config`           |

Behavior:

1. Runs the same Trusted Server typed app-config validation as
   `ts config validate`.
2. Builds a `BlobEnvelope` from the validated app-config JSON.
3. Delegates read/diff/consent/dry-run/write behavior to EdgeZero's typed config
   push primitive using:
   - adapter from `--adapter`;
   - manifest from `--manifest`;
   - logical config store from `--store`;
   - config entry key from `--key` or default;
   - local mode from `--local`;
   - dry-run mode from `--dry-run`;
   - adapter runtime config from `--runtime-config`, when supplied.

`--store` selects the logical config store for the Trusted Server config blob.
`--key` selects the entry key within that config store.

`--dry-run` must not mutate platform or local adapter state. It should still
validate config, compute the local envelope, resolve the EdgeZero push target,
and report what would be written. Full config values should not be printed by
default.

## 8. EdgeZero integration boundary

The Trusted Server CLI must not implement platform-specific lifecycle behavior or
platform-specific writes.

There are two integration modes:

1. Pure lifecycle delegation for `ts auth`, `ts provision`, `ts serve`,
   `ts build`, and `ts deploy`.
2. Trusted Server config initialization/validation plus EdgeZero typed blob push
   for `ts config validate` and `ts config push`.

Pure lifecycle delegate commands should call EdgeZero command/library APIs with
the parsed CLI arguments and selected adapter. They should not perform Trusted
Server config transformation, direct platform API calls, or adapter-specific
command construction.

`ts config push` is intentionally different: it validates Trusted Server app
config first, then delegates blob config-store writes to EdgeZero.

Allowed implementation approach:

- use `edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>` and
  `edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>`.

Not allowed:

- direct Fastly REST API calls from `ts`;
- direct Wrangler/Fastly/Spin command construction in `ts`;
- TS-owned adapter registry for platform writes;
- duplicating EdgeZero store-name resolution logic beyond calling exposed
  EdgeZero helpers.

## 9. App-config environment variables

Trusted Server app config follows EdgeZero's typed app-config env overlay
behavior by default. For app name `trusted-server`, overlay variables use the
`TRUSTED_SERVER__...` prefix.

Examples:

```text
TRUSTED_SERVER__PUBLISHER__DOMAIN=example.com
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
```

Pass `--no-env` to `ts config validate` or `ts config push` when the resolved
blob should be derived from the file only.

Environment variables remain valid for EdgeZero platform/runtime wiring:

```text
EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME=publisher-a-ts-config
EDGEZERO__ADAPTER__...
EDGEZERO__LOGGING__...
```

## 10. `edgezero_enabled` rollout flag

This spec preserves pre-PR Fastly rollout behavior.

The `edgezero_enabled` flag is **not** part of the Trusted Server app-config
blob. It remains a separate Fastly bootstrap value in the existing
`trusted_server_config` config store:

```text
store: trusted_server_config
key: edgezero_enabled
```

Missing, unreadable, `false`, or any value other than `true` / `1` falls back to
the legacy Fastly-native path. `true` / `1` routes through the EdgeZero path.

Moving or removing this flag is a later EdgeZero cutover cleanup and is out of
scope for this PR.

## 11. Error behavior and exit codes

| Exit code | Meaning                        |
| --------- | ------------------------------ |
| `0`       | Command completed successfully |
| non-zero  | Command failed                 |

Failures with clear next steps should include hints:

| Failure                              | Hint                                                 |
| ------------------------------------ | ---------------------------------------------------- |
| missing `trusted-server.toml`        | run `ts config init` or pass `--app-config <path>`   |
| invalid app config                   | fix reported field/schema errors                     |
| missing `edgezero.toml` during push  | pass `--manifest <path>` or create EdgeZero manifest |
| EdgeZero push target missing         | run `ts provision --adapter <adapter>`               |
| adapter unsupported by EdgeZero push | use an adapter with config-store support             |

## 12. Security notes

- `ts config push` does not write secret-store entries in this initial spec.
- Request-signing bootstrap is omitted until EdgeZero exposes secret-store write
  primitives.
- Secret values must never be printed in logs, human output, dry-run output, or
  future JSON output.
- If the active Trusted Server settings schema still contains literal secret
  values in app config at implementation time, those values are included in the
  single blob envelope. This is accepted v1 behavior.
- Placeholder/default secrets must be rejected during validation/push using the
  existing Trusted Server safety checks.

## 13. Tests

### 13.1 `config init`

- writes `trusted-server.example.toml` contents to the default path;
- writes a custom `--app-config` / `--config` path;
- creates parent directories;
- refuses overwrite without `--force`;
- overwrites with `--force`.

### 13.2 `config validate`

- accepts valid config after replacing required placeholders as needed;
- rejects missing file with hint;
- rejects malformed TOML;
- rejects unknown fields;
- rejects semantic validation failures;
- rejects placeholder/default secrets;
- runs EdgeZero typed validation with env overlays by default;
- supports `--no-env` for file-only validation.

### 13.3 blob config payload

- `TrustedServerAppConfig` serializes to the same JSON shape as `Settings`;
- valid settings round-trip through `BlobEnvelope` and runtime reconstruction;
- tampered blob hashes are rejected;
- Fastly chunk pointers reconstruct the exact envelope before verification;
- strings that look like JSON scalars remain strings after round-trip.

### 13.4 EdgeZero delegate commands

Use parser/unit tests where possible and rely on EdgeZero's own tests for
platform dispatch behavior.

- `ts auth login --adapter fastly` parses as EdgeZero auth login;
- `ts auth status --adapter fastly` parses as EdgeZero auth status;
- `ts auth logout --adapter fastly` parses as EdgeZero auth logout;
- `ts provision --adapter fastly` delegates to EdgeZero provision;
- `ts serve --adapter fastly` delegates to EdgeZero serve;
- `ts build --adapter fastly` delegates to EdgeZero build;
- `ts deploy --adapter fastly` delegates to EdgeZero deploy.

### 13.5 `config push`

Use EdgeZero typed config push tests and Trusted Server wrapper tests. Do not
contact real platforms in unit tests.

- validates before pushing;
- builds a `BlobEnvelope` with settings JSON as data;
- defaults `--store`/`--key` through EdgeZero resolution;
- forwards `--adapter`, `--manifest`, `--store`, `--key`, `--local`,
  `--dry-run`, `--no-env`, `--no-diff`, `--yes`, and `--runtime-config` to
  EdgeZero;
- `--dry-run` performs no mutation;
- does not write secret-store entries;
- does not print full config values by default.

## 14. Implementation sequencing

1. Update this spec and docs to the blob app-config contract.
2. Add the `TrustedServerAppConfig` wrapper in core and centralize deploy-time
   validation.
3. Collapse `crates/trusted-server-cli` to the thin downstream-CLI shape:
   direct EdgeZero args/run functions plus TS-owned `config init`.
4. Route `config validate` and `config push` through EdgeZero typed blob APIs.
5. Keep `edgezero_enabled` in `trusted_server_config` and restore any accidental
   coupling to `app_config`.
6. Keep runtime blob loading verified and avoid Trusted Server-owned platform
   writes.
7. Run repository verification gates.

## 15. Open follow-ups outside this spec

- Remove `edgezero_enabled` after EdgeZero path cutover is complete.
- EdgeZero wishlist: secret-store write primitive and machine-readable config
  validate/push output.
- Request-signing bootstrap spec after EdgeZero secret writes exist.
- Trusted Server audit CLI implementation is specified separately.
- Secret-reference/config-secret consolidation spec if literal secrets should be
  removed from the blob before production rollout.
