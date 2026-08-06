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
ts config gc

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

The initial `ts config push` writes the immutable config object and stages its
settings candidate through the deployment-metadata capabilities specified in
§5. It does not write a secret-store entry. The `secrets` store is declared for
runtime/future use but is not written by this CLI spec.

Platform store names are not stored in `trusted-server.toml`. They are resolved
by EdgeZero via its environment overlay, for example:

```text
EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME=publisher-a-ts-config
EDGEZERO__STORES__SECRETS__SECRETS__NAME=publisher-a-ts-secrets
```

## 5. Runtime payload contract

`ts config push` publishes one logical Trusted Server app-config snapshot by
default. It does **not** publish flattened per-setting entries; each snapshot
is a new immutable versioned object as defined below.

| Logical root                                            | Value                                                                                                              |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `app_config` by default, or `--key <key>` when supplied | Serialized `edgezero_core::blob_envelope::BlobEnvelope` whose `data` is the validated Trusted Server settings JSON |

Publication is versioned, not an overwrite of one live blob. The adapter maps
the logical identity `(root, push_sequence)` to an immutable physical object;
the object is written once, read back, and hash-verified. The strong
policy/config/model activation register names the sole active object. Runtimes never
treat the mutable logical root's latest value as active configuration. This
indirection is required even for adapters whose native config store exposes
only `put`: a globally unique, never-reused sequence gives every publication a
new object, while the deployment-metadata CAS supplies the active pointer.

The envelope contains:

- a version field owned by EdgeZero;
- `push_sequence: u64` constrained to `0..=2^53-1`, scoped once per deployment/application across all
  logical config-blob keys, allocated
  exactly once from Trusted Server's linearizable deployment-metadata
  config-sequence register before publication;
- the validated app-config JSON data;
- a SHA-256 hash over EdgeZero's canonical JSON form of `data`;
- a sequence-binding hash
  `SHA-256("tscfgseq1|" || push_sequence.to_be_bytes() || data_hash_bytes)`,
  where the domain tag is UTF-8, the integer is unsigned 64-bit big-endian,
  and `data_hash_bytes` is the 32 decoded bytes of the preceding hash; the
  known-answer vector is in
  `docs/superpowers/specs/revision-canonicalization-vectors.json`;
- generation timestamp metadata.

Runtime loading must verify both the data hash and sequence-binding hash before
constructing `Settings`.
The sequence is metadata, not part of `data`, and is cryptographically bound
to that exact data by `sequence_binding_hash`; a future envelope signature
signs that binding hash rather than the data hash alone. Allocation may leave a gap if publication fails but
never reuses a value. Concurrent pushes CAS the sequence register; a loser
re-reads and retries. A rollback republishes old `data` with a new sequence.
Allocation at the portable maximum is a hard deployment error, never wrap or a
larger JSON integer.
An adapter without the deployment-metadata allocator rejects multi-instance
config/policy activation; ordinary config-store `put` is not treated as CAS.
If an adapter must split a large envelope to satisfy platform limits, the entry
for that immutable logical identity may be an adapter-owned manifest that
identifies immutable chunks. The adapter/runtime loader must reconstruct and
verify the envelope before acknowledging readiness or exposing settings to
application code. A failed candidate or aborted activation leaves only an
unreferenced immutable object; a garbage collector may remove it after the
activation register's operational history and the permission spec §5.5
time-based retention rules both permit removal, never while `active`,
`candidate`, operational history, or a retained activation-journal record
references it. Register eviction alone is never evidence that an object is old
enough to collect. The minimum journal/blob retention is 30 days and grows to
the longest processed-artifact, cookie-scope migration, rollback, or audit
horizon. Its not-before time uses the journal store's timestamp plus the
permission spec's 60-second promotion allowance; CLI/process wall time never
shortens it. `ts config gc` obtains the qualified journal inventory, computes the
reachable set, and refuses deletion if journal listing is incomplete or its
retention clock is uncertain.

Before installing a candidate, the deployment controller obtains the
authoritative `{membership_epoch, members[]}` snapshot defined by the
permission spec §5.5. The CLI cannot synthesize, shrink, or override that set.
A membership change aborts and restages the candidate; `--force` never bypasses
unanimous readiness, the bounded serve-admission lease drain, the all-request
quiescence barrier, or serve admission. The candidate snapshots the positive
deployment-qualified `serve_admission_lease_bound_ms`. The draining CAS
records, from its qualified store clock at the CAS linearization point, a
`promotion_not_before_unix_ms`; that clock cannot satisfy the gate before the
full real interval has elapsed. The register rejects an early promotion even
if all member acknowledgments are present. Promotion is allowed only after the
controller has written and read-verified the immutable activation-journal
entry and the active-register CAS binds its ID as the new journal head.

`ts config push` can stage and promote only a **settings candidate** and copies
the active model epoch, minimum binary generation, and row schema floor
unchanged. It cannot construct or promote a model candidate, and no `--force`
or config value can cross that boundary. The one-time
`pre_epic_v1` → `permissions_v2` transition is an authenticated deployment-
controller operation executed by the migration runbook: it stages the exact
model candidate, collects the fleet proof, and commits the single-register CAS
specified in permission §5.5. That controller operation is deliberately not a
general-purpose initial CLI command; exposing it later requires its own typed
command and cannot be emulated by raw config-store or metadata writes.
After that CAS, the same authenticated controller owns mirror completion: it
strong-reads active and `m00`; a missing or lower mirror is CAS-set to exactly
`active.row_schema_floor`, equality is an idempotent no-op, and an unreadable
mirror or failed CAS/read-verification remains closed for retry. The controller
then strong-reads and verifies exact equality before declaring the operation
complete. Retrying after a crash is idempotent. The operation never lowers
`m00` and never changes or authorizes active; a mirror higher than active is
rejected before any write as an inconsistency that fails closed for
investigation.

### 5.1 Activation journal object and GC protocol

The journal uses the same qualified immutable config-object service under the
reserved logical root `ts_activation_journal`, never the mutable app-config
root or the identity graph. Its logical object ID is lowercase
`SHA-256("tsactj1|" || RFC8785-JCS-UTF8(journal))`; adapters map
`("ts_activation_journal", object_id)` to a write-once physical object. The
object materializes exactly these fields and rejects unknown/missing fields:

Every JSON number in the journal, including every number nested in an active
tuple, is an integer in `0..=9,007,199,254,740,991` (2^53 − 1). Booleans,
floats, negative values, and larger otherwise-valid `u64` values are rejected
before JCS; implementations may use wider internal integers but cannot emit
them here. Store-supplied lifecycle timestamps use the same portable range,
and addition that would exceed it fails closed. This profile makes the JCS
object ID identical in JavaScript, Rust, and every adapter rather than relying
on a language's larger integer type.

- `schema_version = 1`; `attempt_id` as 32 lowercase hex characters from 16
  CSPRNG bytes, allowing a timed-out attempt to publish a new object;
- `candidate_incarnation` as the exact candidate's never-reused 32 lowercase
  hex CSPRNG identity for `config`/`model`, or null for `checkpoint`;
- `previous_journal_id` and `pruned_through_journal_id`, each 64 lowercase hex
  or null under the link/pruning rules below;
- `expected_activation_generation: u64` and `transition_kind` exactly
  `config`, `model`, or `checkpoint`;
- `drain_attempt: u64`, which is the exact nonzero candidate drain attempt for
  `config`/`model` and zero for `checkpoint`;
- `serve_admission_lease_bound_ms: u64`, the exact positive
  deployment-qualified bound snapshotted by the candidate, and
  `promotion_not_before_unix_ms: u64`, the exact store-clock gate written by
  that drain attempt; both are zero only for a `checkpoint`;
- complete `displaced_active` and `activated_active` tuples from permission
  §5.5, including settings bindings, policy identity, model epoch, minimum
  binary generation, row schema floor, and logical activation generation;
- `membership_epoch: u64`, sorted unique `ready_members` and
  `quiesced_members` using the stable member grammar, authenticated
  `controller_id`, and `retain_for_ms: u64` constrained
  to at least 2,592,000,000 and the longest applicable artifact, cookie-scope,
  rollback, and audit horizon.

The cross-language known-answer vector is
`docs/superpowers/specs/activation-journal-vectors.json`; every controller,
runtime verifier, and GC must reproduce both JCS bytes and object ID and reject
every numeric boundary vector. For the
first promotion, `previous_journal_id` is null only when the register head is
null and expected generation is zero. Every later config/model promotion must
name the exact current head and has null `pruned_through_journal_id`; the active
register CAS rejects any link/generation mismatch. For config/model entries,
`expected_activation_generation` must equal current active's logical
activation generation, and activated active must set it to that value + 1;
both member lists must equal the candidate snapshot's complete sorted member
list, `membership_epoch` must equal that snapshot's epoch, `drain_attempt` must
equal the candidate's current attempt and every quiescence acknowledgment, and
`candidate_incarnation` must equal every readiness/quiescence binding,
`serve_admission_lease_bound_ms` and `promotion_not_before_unix_ms` must equal
the candidate's exact drain fields, the admission-lease bound must be positive,
the immutable-store `created_at` for the journal must be at or after the
promotion-not-before time and no more than 60 seconds before the promotion CAS,
and the promotion CAS must independently enforce that its register store clock
has reached that time. These comparisons are defined only because the
activation register and immutable object service expose the same qualified,
authenticated Unix-millisecond time domain; adapters with incomparable clocks
fail activation qualification rather than comparing local timestamps.
Independently, `displaced_active` must equal current active and
`activated_active` must equal the candidate's computed post-CAS tuple. Overflow
is a hard error. A checkpoint
uses the current membership epoch, empty `ready_members` and
`quiesced_members` lists, and identical displaced and activated tuples
(including unchanged activation generation), with null
`candidate_incarnation`, `drain_attempt = 0`,
`serve_admission_lease_bound_ms = 0`, and
`promotion_not_before_unix_ms = 0`; it cannot stand in for fleet readiness or
quiescence.

The immutable store returns authenticated `created_at_unix_ms` object metadata
from the shared qualified activation time domain and maintains a separate extend-only
`delete_not_before_unix_ms` lifecycle value. On every config or journal object
write, the adapter atomically initializes deletion protection to at least store
creation time + 30 days. For a promotion journal it extends protection for the
journal and both named blobs to at least `created_at + 60 seconds +
retain_for_ms` before the active CAS may bind the journal. These lifecycle
values can only increase. Therefore failed publication, aborted candidates,
losing journal attempts, and other unreferenced objects still have a store-clock
not-before value even though no successful promotion names them.

The object service's qualification supplies snapshot-consistent complete
listing for both logical roots: a listing returns one snapshot generation and
opaque pagination token; every page is from that generation, and mutation or
expiry of the token forces GC to restart without deleting. GC first completes
the listing, traverses and verifies the journal from the active head, and builds
the active/candidate/history/journal reachable set. Missing objects, broken
hashes/links, unknown schema, cycles, incomplete pages, or uncertain lifecycle
metadata abort the run. Deletion then uses object-ID CAS and is allowed only
when the object is unreachable and its store-enforced not-before has passed.

Journal pruning is an authenticated controller operation, never implicit GC.
Only when every record reachable from the current head is older than its full
retention horizon may the controller publish a `checkpoint` whose displaced
and activated tuples both equal current active, whose previous ID is null, and
whose `pruned_through_journal_id` is the old head. One register CAS verifies the
unchanged active tuple/generation and replaces only the journal head. The
checkpoint names and protects the current active blob; old journal objects
remain until their individual not-before values pass. Frequent activation can
therefore retain a longer chain but can never cut a still-required segment.

Reserved future keys, not written in this initial spec:

| Key                   | Future purpose                                                        |
| --------------------- | --------------------------------------------------------------------- |
| `ts-config-signature` | Optional signature/DSSE envelope over the sequence-binding hash       |
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
2. Allocates `push_sequence` through the selected adapter's Trusted Server
   deployment-metadata capability (dry-run reads and reports the next value
   but does not reserve it).
3. Builds a `BlobEnvelope` from the validated app-config JSON and allocated
   sequence.
4. Writes the envelope under the new immutable `(logical root, push_sequence)`
   identity, reads it back, and verifies both hashes. It never
   overwrites an object for an already allocated sequence.
5. CAS-installs the exact immutable object as the sole activation candidate;
   the candidate has a new never-reused CSPRNG incarnation, binds current
   active's complete tuple and logical activation generation, includes logical root, source version, data hash,
   effective-config revision, policy digest, proposed policy ordinal, and the
   unchanged active model fields. A competing or existing candidate makes the
   CAS fail. The newly written unreferenced object is safe to collect later; it
   never becomes live by being the most recently written blob.
6. Fleet readiness and controller promotion follow the permission spec §5.5.
   Only promotion changes the active configuration. A config-only push still
   takes this path but retains the policy ordinal.

The underlying immutable-object read/diff/consent/dry-run/write behavior
delegates to EdgeZero's typed config push primitive using:

- adapter from `--adapter`;
- manifest from `--manifest`;
- logical config store from `--store`;
- config entry key from `--key` or default;
- local mode from `--local`;
- dry-run mode from `--dry-run`;
- adapter runtime config from `--runtime-config`, when supplied.

`--store` selects the logical config store for the Trusted Server config blob.
`--key` selects the entry key within that config store.

`--dry-run` must not allocate a sequence, write an immutable object, or mutate
the activation register. It validates config, computes a provisional envelope
using the reported next sequence, resolves the EdgeZero push target, and
reports the immutable identity and candidate tuple that would be written.
Because another push may win, that sequence is explicitly advisory. Full
config values should not be printed by default.

### 7.5 `ts config gc`

```bash
ts config gc \
  --adapter <adapter> \
  [--manifest <path>] \
  [--store <logical-config-store-id>] \
  [--key <config-entry-key>] \
  [--dry-run] \
  [--yes] \
  [--runtime-config <path>]
```

The command applies only §5.1's qualified immutable-object inventory and
deletion protocol; it never guesses physical keys or prunes the journal head.
It resolves the app-config root from `--key` (default `app_config`) and the
fixed `ts_activation_journal` root in the same selected config store, completes
one snapshot-consistent paginated inventory, verifies all hashes, lifecycle
metadata, active/candidate/history references, and the journal chain, then
computes unreachable objects whose store-enforced not-before has passed.
`--dry-run` prints only object IDs, roots, reasons, and lifecycle timestamps and
does not delete. Without `--dry-run`, deletion requires `--yes` or interactive
confirmation and uses the object-ID CAS from §5.1. Any uncertainty aborts the
entire run before the first delete; a partial platform deletion error stops the
run, reports exact completed IDs, and is safe to retry because reachability and
object-ID CAS are re-evaluated. Publishing a checkpoint is a separate
authenticated controller operation and is never an implicit side effect of
this command.

## 8. EdgeZero integration boundary

The Trusted Server CLI must not implement platform-specific lifecycle behavior or
platform-specific writes.

There are two integration modes:

1. Pure lifecycle delegation for `ts auth`, `ts provision`, `ts serve`,
   `ts build`, and `ts deploy`.
2. Trusted Server config initialization/validation plus EdgeZero typed blob
   push for `ts config validate` and `ts config push`, and the qualified
   immutable-object inventory/CAS-delete path for `ts config gc`.

Pure lifecycle delegate commands should call EdgeZero command/library APIs with
the parsed CLI arguments and selected adapter. They should not perform Trusted
Server config transformation, direct platform API calls, or adapter-specific
command construction.

`ts config push` is intentionally different: it validates Trusted Server app
config first, then delegates blob config-store writes to EdgeZero. `ts config
gc` delegates listing, lifecycle metadata, and object-ID CAS deletion but owns
the Trusted Server reachability/journal validation in §5.1.

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
- stages only a settings candidate bound to the complete active tuple and
  activation generation; config push cannot alter model fields;
- does not write secret-store entries;
- does not print full config values by default.

### 13.6 `config gc`

- complete snapshot pagination is required before the first delete;
- active, candidate, history, journal-chain, and protected-blob reachability
  each prevent deletion;
- broken hash/link, cycle, expired pagination token, uncertain store clock, or
  missing lifecycle metadata aborts with zero deletion;
- not-before boundary, dry-run, object-ID CAS conflict, partial-error retry, and
  interactive/`--yes` confirmation follow §7.5;
- GC cannot publish a checkpoint or alter active/model state;
- the activation-journal known-answer vector verifies identically in runtime,
  controller, and GC tests.

## 14. Implementation sequencing

1. Update this spec and docs to the blob app-config contract.
2. Add the `TrustedServerAppConfig` wrapper in core and centralize deploy-time
   validation.
3. Collapse `crates/trusted-server-cli` to the thin downstream-CLI shape:
   direct EdgeZero args/run functions plus TS-owned `config init`.
4. Route `config validate` and `config push` through EdgeZero typed blob APIs;
   add the qualified listing/lifecycle/object-CAS surface required by `config
gc` without platform-specific logic in Trusted Server.
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
