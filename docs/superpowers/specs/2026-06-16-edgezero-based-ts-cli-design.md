# Trusted Server CLI — EdgeZero-Backed Product CLI

**Date:** 2026-06-16
**Status:** Draft design
**Scope:** Initial `ts` product CLI; audit is specified separately
**Related context:**

- `docs/superpowers/plans/2026-06-16-trusted-server-cli-respec-context.md`
- `docs/superpowers/specs/2026-06-16-edgezero-based-ts-audit-design.md`
- EdgeZero PR #269 CLI/config/provision work — implementation temporarily targets this PR branch/rev before repinning to the merged EdgeZero revision
- Future runtime-config-store spec for loading flattened `app_config` entries

---

## 1. Goal

Add a Trusted Server product CLI binary, `ts`, as the normal operator
entrypoint for Trusted Server workflows.

`ts` exposes Trusted Server-specific config commands and EdgeZero-backed
platform lifecycle commands through one binary. Trusted Server-specific commands
own Trusted Server behavior. Platform lifecycle commands are thin delegates to
EdgeZero and must not reimplement platform behavior.

The initial command surface is:

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

`ts config push` owns the Trusted Server app-config transformation:

```text
trusted-server.toml
  -> parse and validate as Trusted Server Settings
  -> serialize validated Settings to a JSON value
  -> flatten to EdgeZero-style deterministic key/value entries
  -> compute sha256 over the canonical entry map
  -> push config-store entries through EdgeZero platform primitives
```

EdgeZero owns adapter resolution, logical-store to platform-store resolution,
local-vs-remote push behavior, dry-run behavior, auth, provisioning, serving,
building, deployment, and all platform-specific writes.

---

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
- support app-config environment overrides;
- write request-signing key/bootstrap secrets;
- write secret-store entries of any kind;
- generate config signing / DSSE artifacts;
- support config diff/pull/inspect commands.

---

## 3. File ownership model

### 3.1 Source-controlled files

The repository tracks:

```text
edgezero.toml
trusted-server.example.toml
```

`edgezero.toml` is the EdgeZero platform manifest. It declares the Trusted
Server app, stores, adapters, and platform command metadata.

`trusted-server.example.toml` is the source-controlled app-config template.
It uses only example/placeholder values and is kept in sync with the Trusted
Server settings schema.

### 3.2 Operator-owned files

The repository ignores:

```text
trusted-server.toml
```

`trusted-server.toml` is operator-authored app config. It is never compiled into
the binary and is never a source-controlled deployment artifact.

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

---

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

---

## 5. Runtime payload contract

The runtime-config-store spec owns runtime loading. This CLI spec only defines
what `ts config push` publishes.

`ts config push` writes EdgeZero-style flattened config entries by default. It
does **not** store the whole Trusted Server config as one large JSON blob.

| Key pattern                     | Value                                                                                      |
| ------------------------------- | ------------------------------------------------------------------------------------------ |
| `<escaped-dotted-settings-key>` | Canonical JSON text for one flattened Trusted Server setting leaf                          |
| `ts-config-hash`                | `sha256:<hex>` over the canonical flattened settings entry map, excluding metadata entries |
| `ts-config-keys`                | Minified JSON array of flattened settings keys in sorted order, excluding metadata entries |

Flattening follows EdgeZero's config push model with Trusted Server key
escaping:

- Each JSON object key is treated as one path segment.
- Before joining path segments, each segment is escaped deterministically:
  - `\` becomes `\\`
  - `.` becomes `\.`
- Flattened keys are escaped path segments joined by an unescaped `.`.
- The canonical map, `ts-config-keys`, hash input, and pushed entry keys all use
  the escaped flattened keys.
- Runtime reconstruction must split only on unescaped `.` and then unescape in
  reverse order.
- JSON objects flatten recursively.
- Leaf values are stored as canonical JSON text so runtime reconstruction is
  lossless:
  - strings are JSON-quoted strings;
  - booleans and numbers use JSON scalar text;
  - arrays are stored as canonical minified JSON arrays under the array field's
    escaped dotted key. Any objects inside arrays must have recursively sorted
    keys before serialization.
- Null values are skipped.
- Metadata keys beginning with `ts-config-` are reserved for Trusted Server and
  must not be produced by app settings flattening.

Reserved future keys, not written in this initial spec:

| Key                   | Future purpose                                                                   |
| --------------------- | -------------------------------------------------------------------------------- |
| `ts-config-signature` | Optional signature/DSSE envelope over the canonical flattened settings entry map |
| `ts-config-metadata`  | Optional JSON metadata: version, published_at, valid_until, policy_id            |

The app config hash is computed only over flattened Trusted Server setting
entries, not over metadata entries and not over unrelated entries in the config
store.

Request-signing public/private state is intentionally out of scope for this
initial CLI. It will be revisited after EdgeZero exposes suitable secret-store
write primitives.

---

## 6. Flattened config entries

`trusted-server.toml` remains the human-authored source format. The deployed
runtime payload is an EdgeZero-style deterministic key/value entry set.

Flattening pipeline:

1. Read `trusted-server.toml` as UTF-8.
2. Parse as TOML.
3. Deserialize into the Trusted Server `Settings` schema with strict unknown-field
   rejection.
4. Run existing semantic validation.
5. Reject placeholder/default secrets using the same production safety rules as
   runtime validation.
6. Convert the validated settings into a JSON value.
7. Flatten the JSON value using EdgeZero's config push rules and Trusted Server's
   path-segment escaping rules.
8. Sort flattened entries lexicographically by escaped key.
9. Serialize the sorted settings-only entry map as minified JSON for hashing.
10. Compute SHA-256 over those exact UTF-8 bytes.

The flattened entries and hash must be stable for semantically identical config.
Reordered TOML input and TOML formatting/comment changes must not change the
hash if the resulting `Settings` value is identical.

If the settings schema contains maps or dynamic integration configuration, those
maps must be sorted during flattening by escaped key. Do not rely on parser
insertion order.

Strict schema validation is part of this CLI contract. Every non-map settings
struct reachable from `Settings` must reject unknown fields. Explicit map fields
remain the supported extension points for dynamic integration, response-header,
profile, or similar keyed configuration.

---

## 7. Command surface

### 7.1 EdgeZero delegate commands

```bash
ts auth login --adapter <adapter> [-- <edgezero-args>...]
ts auth status --adapter <adapter> [-- <edgezero-args>...]
ts auth logout --adapter <adapter> [-- <edgezero-args>...]

ts provision --adapter <adapter> [-- <edgezero-args>...]
ts serve --adapter <adapter> [-- <edgezero-args>...]
ts build --adapter <adapter> [-- <edgezero-args>...]
ts deploy --adapter <adapter> [-- <edgezero-args>...]
```

These commands provide a Trusted Server product CLI wrapper around EdgeZero
platform lifecycle behavior.

Behavior:

- Delegate to EdgeZero command handlers for the selected adapter.
- Preserve EdgeZero adapter semantics, validation, local/remote behavior, and
  platform-specific error handling.
- Forward supported command options and trailing passthrough args after `--` to
  EdgeZero without translating them into Trusted Server-owned platform logic.
- Do not read, validate, flatten, or push `trusted-server.toml` unless a
  delegated EdgeZero command explicitly requires app/manifest context.
- Do not construct Fastly, Wrangler, Spin, or other platform commands directly
  in Trusted Server code.
- Do not implement platform-specific REST/API writes in Trusted Server code.

Preferred implementation is to call EdgeZero Rust library APIs directly. Shelling
out to an `edgezero` binary is only acceptable as a temporary implementation
strategy if the required library API does not exist yet.

The command shape intentionally mirrors EdgeZero so product documentation can map
`ts` commands to EdgeZero-backed behavior one-to-one. Passthrough args are
forwarded verbatim; Trusted Server only parses product-level options such as
`--adapter`.

### 7.2 `ts config init`

```bash
ts config init [--config <path>] [--force]
```

Defaults:

| Option     | Default               |
| ---------- | --------------------- |
| `--config` | `trusted-server.toml` |

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
ts config validate [--config <path>] [--json]
```

Defaults:

| Option     | Default               |
| ---------- | --------------------- |
| `--config` | `trusted-server.toml` |

Behavior:

- Reads the local Trusted Server config file.
- Parses and validates it as Trusted Server app config.
- Builds flattened config entries.
- Computes the config hash over the canonical entry map.
- Does not read `edgezero.toml`.
- Does not contact any platform.
- Does not apply app-config environment overrides.

Human success output (`Config entries` counts flattened settings entries only,
excluding metadata):

```text
Config valid: /absolute/path/to/trusted-server.toml
Config entries: <count>
Config hash: sha256:<hex>
```

`--json` success output:

```json
{
  "valid": true,
  "config_path": "/absolute/path/to/trusted-server.toml",
  "entry_count": 42,
  "config_hash": "sha256:<hex>",
  "errors": []
}
```

On validation failure with `--json`, stdout still contains JSON and the process
exits non-zero:

```json
{
  "valid": false,
  "config_path": "/absolute/path/to/trusted-server.toml",
  "entry_count": null,
  "config_hash": null,
  "errors": ["publisher.domain is required"]
}
```

Human failure output goes to stderr and exits non-zero.

### 7.4 `ts config push`

```bash
ts config push \
  --adapter <adapter> \
  [--config <path>] \
  [--manifest <path>] \
  [--store <logical-config-store-id>] \
  [--local] \
  [--dry-run] \
  [--runtime-config <path>]
```

Defaults:

| Option       | Default               |
| ------------ | --------------------- |
| `--config`   | `trusted-server.toml` |
| `--manifest` | `edgezero.toml`       |
| `--store`    | `app_config`          |

Behavior:

1. Runs the same Trusted Server app-config validation and flattening as
   `ts config validate`.
2. Produces config entries:
   - one `<escaped-dotted-settings-key> = <canonical-json-value>` entry per flattened setting
   - `ts-config-keys = <minified JSON array of settings keys>`
   - `ts-config-hash = sha256:<hex>`
3. Delegates the entry write to EdgeZero's config-store push primitive using:
   - adapter from `--adapter`
   - manifest from `--manifest`
   - logical config store from `--store`
   - local mode from `--local`
   - dry-run mode from `--dry-run`
   - adapter runtime config from `--runtime-config`, when supplied

`--store` selects the logical config store for **all** Trusted Server config
entries written by this command.

`--dry-run` must not mutate platform or local adapter state. It should still
validate config, compute the hash, resolve the EdgeZero push target, and report
what would be written. Full values should not be printed by default; show key
names, entry count, and hash instead.

No `--json` is defined for `ts config push` in this spec. Machine-readable push
output should be added to EdgeZero upstream and then exposed here consistently.

---

## 8. EdgeZero integration boundary

The Trusted Server CLI must not implement platform-specific lifecycle behavior or
platform-specific writes.

Implementation starts by switching this repository's EdgeZero git dependencies
to the target PR #269 branch/rev that contains the needed CLI/config/provision
APIs. Before merging the Trusted Server work, repin to the merged EdgeZero
commit or release. Trusted Server must not add temporary platform-specific
writes while waiting for these EdgeZero APIs; missing APIs are upstream
prerequisites.

There are two integration modes:

1. Pure lifecycle delegation for `ts auth`, `ts provision`, `ts serve`,
   `ts build`, and `ts deploy`.
2. Trusted Server transformation plus EdgeZero write delegation for
   `ts config push`.

Pure lifecycle delegate commands should call EdgeZero command/library APIs with
the parsed CLI arguments and selected adapter. They should not perform Trusted
Server config flattening, direct platform API calls, or adapter-specific command
construction.

`ts config push` is intentionally different: it validates and transforms Trusted
Server app config first, then delegates flattened config-store entry writes to
EdgeZero.

Allowed `ts config push` implementation approaches:

1. Reuse EdgeZero's config push flattening and adapter push APIs directly, with
   Trusted Server supplying the typed `Settings` value and reserved metadata
   entries.
2. Call an EdgeZero Rust API that accepts already-flattened config entries and
   executes the adapter push.
3. Shell out to `edgezero config push` only if EdgeZero supports the same typed
   Trusted Server flattening path and metadata entries without introducing a
   separate platform write path in `ts`.
4. Add the required public flatten/push API to EdgeZero first, then consume it
   from `ts`.

Not allowed:

- direct Fastly REST API calls from `ts`;
- direct Wrangler/Fastly/Spin command construction in `ts`;
- TS-owned adapter registry for platform writes;
- duplicating EdgeZero store-name resolution logic beyond calling exposed
  EdgeZero helpers.

### 8.1 Required EdgeZero capability

Trusted Server needs an EdgeZero config push path that can write flattened
entries in the same shape EdgeZero already uses for app config:

```text
[
  ("publisher.domain", "example.com"),
  ("ec.partners", "[...]"),
  ("ts-config-keys", "[\"ec.partners\",\"publisher.domain\"]"),
  ("ts-config-hash", "sha256:<hex>")
]
```

EdgeZero then resolves and writes those entries for the selected
adapter/logical store.

If this public capability does not exist when implementation begins, it is an
upstream EdgeZero prerequisite, not a reason to implement platform-specific
writes in `ts`.

---

## 9. App-config environment variables

Trusted Server app config does not support environment overrides in this design.

Removed / unsupported:

```text
TRUSTED_SERVER__PUBLISHER__DOMAIN=...
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
```

No build-time env merge, push-time env overlay, or runtime env overlay applies
to app settings.

Environment variables remain valid for EdgeZero platform/runtime wiring only:

```text
EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME=...
EDGEZERO__ADAPTER__...
EDGEZERO__LOGGING__...
```

This keeps config hashes explainable: the hash is derived only from the local
config file's validated settings value.

---

## 10. Error behavior and exit codes

| Exit code | Meaning                        |
| --------- | ------------------------------ |
| `0`       | Command completed successfully |
| `1`       | Command failed                 |

Initial `ts` commands do not need a special cancellation code because no command
is interactive.

Failures with clear next steps should include hints:

| Failure                              | Hint                                                 |
| ------------------------------------ | ---------------------------------------------------- |
| missing `trusted-server.toml`        | run `ts config init` or pass `--config <path>`       |
| invalid app config                   | fix reported field/schema errors                     |
| missing `edgezero.toml` during push  | pass `--manifest <path>` or create EdgeZero manifest |
| EdgeZero push target missing         | run `ts provision --adapter <adapter>`               |
| adapter unsupported by EdgeZero push | use an adapter with config-store support             |

---

## 11. Security notes

- `ts config push` does not write secret-store entries in this initial spec.
- Request-signing bootstrap is omitted until EdgeZero exposes secret-store write
  primitives.
- Secret values must never be printed in logs, human output, dry-run output, or
  future JSON output.
- If the active Trusted Server settings schema still contains literal secret
  values in app config at implementation time, those values are written as
  individual flattened config-store entries. This is accepted v1 behavior.
  Secret-reference extraction/consolidation is a separate design track and
  should be coordinated with EdgeZero secret-store write primitives before
  production rollout where needed.
- Placeholder/default secrets must be rejected during validation/push using the
  existing Trusted Server safety checks.

---

## 12. Tests

### 12.1 `config init`

- writes `trusted-server.example.toml` contents to default path;
- writes custom `--config` path;
- creates parent directories;
- refuses overwrite without `--force`;
- overwrites with `--force`.

### 12.2 `config validate`

- accepts valid example config after replacing required placeholders as needed;
- rejects missing file with hint;
- rejects malformed TOML;
- rejects unknown fields;
- rejects semantic validation failures;
- rejects placeholder/default secrets;
- produces stable hash for reordered TOML input;
- `--json` success writes valid JSON and exits 0;
- `--json` failure writes valid JSON and exits non-zero.

### 12.3 flattened config entries

- nested objects flatten to escaped dotted keys;
- strings, booleans, numbers, arrays, and nulls follow EdgeZero flattening rules;
- arrays use canonical minified JSON with recursively sorted object keys;
- dynamic integration maps are stable;
- object/map keys containing `.` and `\` are escaped deterministically;
- escaped flattened keys can be split and unescaped without ambiguity;
- flattened entries are sorted before hashing;
- hash equals SHA-256 of the canonical settings-only entry map;
- metadata entries `ts-config-keys` and `ts-config-hash` are excluded from the
  hash input.

### 12.4 EdgeZero delegate commands

Use a fake EdgeZero delegate implementation or test hook. Do not contact real
platforms in unit tests.

- `ts auth login --adapter fastly` calls the EdgeZero auth login delegate with
  the selected adapter;
- `ts auth status --adapter fastly` calls the EdgeZero auth status delegate;
- `ts auth logout --adapter fastly` calls the EdgeZero auth logout delegate;
- `ts provision --adapter fastly` calls the EdgeZero provision delegate;
- `ts serve --adapter fastly` calls the EdgeZero serve delegate;
- `ts build --adapter fastly` calls the EdgeZero build delegate;
- `ts deploy --adapter fastly` calls the EdgeZero deploy delegate;
- delegate commands forward supported args/options without Trusted
  Server-specific platform translation;
- delegate commands surface missing/unsupported adapter errors from EdgeZero
  clearly.

### 12.5 `config push`

Use a fake EdgeZero push implementation or test hook. Do not contact real
platforms in unit tests.

- validates before pushing;
- passes flattened settings entries plus `ts-config-keys` and `ts-config-hash`;
- defaults `--store` to `app_config`;
- forwards `--adapter`, `--manifest`, `--store`, `--local`, `--dry-run`, and
  `--runtime-config` to EdgeZero push layer;
- `--dry-run` performs no mutation;
- does not write secret-store entries;
- does not print full config values by default.

---

## 13. Implementation sequencing

The full implementation plan is maintained in:

```text
docs/superpowers/plans/2026-06-16-edgezero-based-ts-cli-implementation-plan.md
```

Required sequencing:

1. Start by switching this repository to the target EdgeZero PR #269 branch/rev
   and verifying the required EdgeZero APIs.
2. Add the host-target `ts` CLI crate and testable runner/delegate boundaries.
3. Implement strict Trusted Server config parsing, deterministic escaping,
   flattening, hashing, and local `config init|validate` behavior.
4. Implement EdgeZero lifecycle delegation and config push using EdgeZero APIs.
5. Align repository file ownership with this spec by removing build-time config
   embedding, adding the EdgeZero manifest/template files, and ignoring
   operator-owned `trusted-server.toml`.
6. Update docs and run the repository verification gates.

---

## 14. Open follow-ups outside this spec

- Runtime config-store spec: runtime reads flattened `app_config` entries,
  reconstructs Trusted Server settings, computes/compares hash metadata, and
  `/health` fails when config is invalid.
- EdgeZero wishlist: secret-store write primitive, public flatten/push entry API
  if the current config push internals are not reusable, and JSON output for
  push/provision.
- Request-signing bootstrap spec after EdgeZero secret writes exist.
- Trusted Server audit CLI implementation is specified separately in
  `docs/superpowers/specs/2026-06-16-edgezero-based-ts-audit-design.md`.
- Secret-reference/config-secret consolidation spec if literal secrets should be
  removed from flattened config-store entries before production rollout.
