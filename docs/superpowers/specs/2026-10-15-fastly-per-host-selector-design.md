# Design Spec: The Fastly adapter keeps its own selector for a document per host

**Status:** Proposed, 2026-10-15
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-config-by-host-design.md`, `2026-10-13-request-addressing-design.md` and `2026-10-15-fastly-names-in-core-design.md`, all in IABTechLab/trusted-server#1342, which holds every design document of the stack, and `2026-09-16-edgezero-fastly-store-selectors-design.md` on Tech Lab main
**Related PRs:** none yet. IABTechLab/trusted-server#1175 is the change on main that this design answers.
**Issues:** IABTechLab/trusted-server#1270 (config by host)
**Last updated:** 2026-10-15

> **Why this spec exists.** Tech Lab main now reads the Fastly config entry under its logical store ID on every target, which removes the per-service `__KEY` selector that the stack's one document per host is chosen by, so the stack cannot take main until the Fastly adapter reads a selector of its own.

Line references are at these commits.

| Code                                                | Commit                               |
| --------------------------------------------------- | ------------------------------------ |
| The stack, the head of `split/50-cookies-by-path`   | `3aaf720c0`                          |
| Tech Lab main                                       | `fa9837f2b`, which holds `a0b00c727` |
| EdgeZero as the stack pins it                       | `683202c`                            |
| EdgeZero as Tech Lab main pins it, the `v0.0.9` tag | `f717674`                            |

## 1. Problem

Tech Lab main took EdgeZero `v0.0.9` in #1175 (`Cargo.toml:59-64` on main). That release removes the `edgezero_runtime_env` config store and the `runtime_env_config` function that read it. A deploy now links each physical store to the service version under its logical ID, chosen in the deploy's environment by `EDGEZERO__STORES__<KIND>__<ID>__NAME` (`desired_resource_links`, `crates/edgezero-adapter-fastly/src/cli.rs:624-656` at `v0.0.9`). EdgeZero's CLI refuses any Fastly config key other than the logical store ID (`validate_fastly_config_key`, `cli.rs:5728-5744`), both when `ts config push` or `ts config diff` resolves its key (`resolve_config_store_and_key`, `crates/edgezero-cli/src/config.rs:1441-1452`, asking the Fastly adapter at `cli.rs:2149-2157`) and when a deploy is planned (`cli.rs:788`).

Main's Fastly adapter follows that model.

- It opens `trusted_server_config` and `trusted_server_secrets` by logical ID and reads the document under the key `trusted_server_config` on every target (`RuntimeStoreConfig::logical`, `crates/trusted-server-adapter-fastly/src/app.rs:163-177` on main, called at `main.rs:326`).
- Staging is kept apart only by the physical store a staging version links, never by a different key (the doc comment at `app.rs:163-170` on main).
- The sandbox bounds `TS__SANDBOX__*` moved from service-scoped keys in `edgezero_runtime_env` to unscoped keys in the application's own config store (`sandbox.rs:265-271` and `:341-342` on main).

The stack is pinned at `683202c` and chooses a document for each host through the removed store.

| What the stack reads                                                     | Where, at `3aaf720c0`                                                                                                                                                           |
| ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| The `__KEY` selector, through `runtime_env_config`                       | `crates/trusted-server-adapter-fastly/src/lib.rs:13` and `:591-599`, `app.rs:88`, `:176-184` and `:1710-1714`                                                                   |
| The request's host in place of `{host}`, before any settings are read    | `config_selection.rs:107-124`, `lib.rs:107-118`                                                                                                                                 |
| The selector values of row 35's request addressing in place of `{host}`  | `config_selection.rs:97-103`, `lib.rs:130-144` and `:191-205`                                                                                                                   |
| The sandbox bounds, under service-scoped keys in `edgezero_runtime_env`  | `sandbox.rs:421-460`                                                                                                                                                            |
| Local stores and tests that write a selector into `edgezero_runtime_env` | `fastly.toml:62-69`, `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml:124-128`, `scripts/config-by-host-local-test.sh:47`, `:224` and `:294-296` |
| The guide's commands for a document per host                             | `docs/guide/configuration.md`, the section that starts at line 3809                                                                                                             |

Taking main therefore fails in three ways.

1. **Row 11 stops building.** `split/11-config-by-host` merges with main without a textual conflict, but `runtime_env_config` no longer exists, so neither `lib.rs` nor `app.rs` compiles.
2. **A service with a document per host loses its selector.** If the selector were simply dropped, as main drops it, every host would read the one document at `trusted_server_config`. That is the shared fallback the config by host design refuses, because it serves one publisher's settings for a host nobody configured.
3. **The guide's way of writing a host's document is refused.** `ts config push --adapter fastly --key www.publisher-a.example` reaches `validate_fastly_config_key` and stops.

Two existing deployments were read through the Fastly API on 2026-10-15, being their config stores' key names and selectors, their versions and each active version's links. Each keeps one document per host in one physical store, with a selector that carries `{host}` and is written twice in that store, once for the production service id and once for the service id of Fastly's staging environment. In each, both entries carry the same selector. Each active version links that one store twice, as `trusted_server_config` and as `edgezero_runtime_env`, so the selector entries already sit in the store the version opens as `trusted_server_config`. In each, the staging environment runs the same version as production. Neither store holds sandbox bounds.

## 2. Design

The Fastly adapter reads its selector from one entry in the application's own config store. The selector's text, the substitution of `{host}` and of the selector values, and every answer stay as they are today. Only where the selector is read from changes, and the stack takes the rest of main's store model as main has it. Core is not touched, because core names no hosting platform and the selector is the Fastly adapter's own.

### 2.1 Where the selector is configured

The selector is the entry `TS__CONFIG__KEY` in the config store the running version links under the logical ID `trusted_server_config`. It sits beside the documents it names and beside the sandbox bounds, which main already reads from the same store. The entry is not scoped by service id, as the sandbox bounds are not.

```bash
fastly config-store-entry update --upsert \
  --store-id <id of the store linked as trusted_server_config> \
  --key TS__CONFIG__KEY \
  --value '{host}'
```

| `TS__CONFIG__KEY` holds            | The document read                                                |
| ---------------------------------- | ---------------------------------------------------------------- |
| Nothing, the entry is absent       | `trusted_server_config` for every request, as on Tech Lab main   |
| `site_a`, with no placeholder      | `site_a` for every request, with no lookup                       |
| `{host}`                           | The request's host, such as `www.publisher-a.example`            |
| `{host}_v2`                        | The host followed by `_v2`, such as `www.publisher-a.example_v2` |
| A malformed selector (section 2.4) | None, and the request is answered `500`                          |

The name follows main's `TS__SANDBOX__*` entries and keeps `__KEY`, the name the stack's code, tests and guide already give the selector. A host is lowercased letters, digits, hyphens and dots (`normalized_host`, `config_selection.rs:39-56`), so no host can stand for `TS__CONFIG__KEY` or a sandbox bound, which carry capitals and underscores.

The selector is written in the store and never in the deploy's environment. A deploy refuses `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY` set to anything other than the logical store ID (`cli.rs:788` at `v0.0.9`).

### 2.2 How the selector is read on each request

The entry point keeps its order. `/health` is answered first (`lib.rs:439`), before `edgezero_main` is called (`lib.rs:468`) and so before the selector is read. Then, in place of `runtime_env_config` at `lib.rs:591-599`, these steps run.

1. The stores are `RuntimeStoreConfig::logical()`, as on main.
2. The adapter opens `trusted_server_config` and reads `TS__CONFIG__KEY` with `try_get`, never `get`, because `get` panics on a lookup error.
3. The text is checked (section 2.4) and becomes `config_key`, the selector that `address_request` (`lib.rs:191-205`) hands to `resolve_key` or `resolve_addressed_key` exactly as today.

The entry is read on every request and not kept by a sandbox that serves several requests, so a changed selector takes effect on the next request, as a changed `edgezero_runtime_env` entry does today. That costs one lookup, where `runtime_env_config` made one for each key on its allowlist (`crates/edgezero-adapter-fastly/src/lib.rs:302-330` at `683202c`), ten for the stack's three declared stores.

`RuntimeStoreConfig::from_env`, its tests and the `runtime_env_config` imports go, as main removes them. `TrustedServerApp::routes()` (`app.rs:1710-1714`) builds a router without a request, so it reads the logical key as on main. The entry point builds the application through `build_app_with_state` (`lib.rs:675-681`) and not through `routes()`.

### 2.3 How `{host}` and the selector values are substituted

Nothing changes in the substitution, which `resolve_key_from` does for both callers (`config_selection.rs:107-124`).

- A selector without `{host}` is the key as it stands, and nothing is looked up (`:112-114`).
- Otherwise the request's host, lowercased with its port and one trailing dot removed, takes the placeholder's place, and the store is asked whether a document exists at that key.
- Under row 35's request addressing, each selector value takes the placeholder's place in turn and is keyed as a host is. The first value whose document exists is read, a value that is not a host name is passed over without a lookup, and a failed lookup stops the search.

One rule is added. A key longer than 255 characters, the longest key a Fastly config store entry takes as EdgeZero reads the limit (`FASTLY_CONFIG_KEY_LIMIT`, `crates/edgezero-adapter-fastly/src/chunked_config.rs:36` at `v0.0.9`), is not looked up and counts as no document. A host may be 253 characters, so `{host}_v2` could otherwise ask the store for a key it cannot hold. The rule answers such a request `421` before any lookup, whatever the store would have said.

### 2.4 The answers

| A request                                                                           | Is answered                                                                          |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `/health`, for any host                                                             | `200`, before the selector is read                                                   |
| Any host, with no `TS__CONFIG__KEY`                                                 | From the document at `trusted_server_config`, as on main                             |
| A host, or every selector value, with no document at its key                        | `421 Misdirected Request` with `Cache-Control: private, no-store` (`lib.rs:249-255`) |
| A `Host` that is not a host name, or no `Host`, where the selector carries `{host}` | `421`, with no lookup                                                                |
| A key over 255 characters                                                           | `421`, with no lookup                                                                |
| Any host, where the store cannot be opened or a lookup fails                        | `500` (`lib.rs:256-259`)                                                             |
| Any host, where the selector is malformed                                           | `500`                                                                                |

A selector is malformed when it is any of these.

1. Empty, or carrying whitespace anywhere.
2. Carrying `{host}` more than once.
3. Carrying a `{` or a `}` that is not part of its one `{host}`. This catches `{Host}`, `{hostname}` and an unclosed brace, each of which would otherwise be read as one fixed key for every host.
4. Longer than 255 characters.

`Refusal` (`config_selection.rs:24-31`) gains a third case, `MalformedSelector`, answered `500`. The log line names the selector and the rule it breaks. The selector is written by the operator, so it may be logged, which keys built from a request are not.

A malformed selector is answered `500` and not `421` for three reasons. The fault is the operator's, every request meets it, and `421` tells a client to retry on another connection. It never falls back to the logical key, because a service that chooses by host must never serve one document for every host. A blank entry is treated the same way, where EdgeZero treated a blank `__KEY` as absent (`EnvConfig::store_key`, `crates/edgezero-core/src/env_config.rs:112-121` at `683202c`), because a blank written over a working selector is far more likely a failed write than a choice.

### 2.5 Staging

At `683202c` EdgeZero's staged deploy kept staging apart by key. It copied the service's runtime entries into a staging store of their own and pointed each config selector at `<logical store id>_staging` (`staging_entries_from_production`, `cli.rs:3570-3603` at `683202c`), so a staged version of a service that chose by host read one staging document for every host. At `v0.0.9` there is no suffix, and staging is kept apart only by the physical store the staging version links.

This design follows `v0.0.9`. A staged version reads `TS__CONFIG__KEY` and the documents from whichever store its version links as `trusted_server_config`, which gives two cases.

1. **The staging version links the production store.** A staged version cloned from the active one does, and so does each existing deployment, whose staging environment runs the same version as production (section 1). It reads the production selector and the production documents. Staging is told apart by host, because the staging environment answers names of its own with documents of their own, and by the platform's staging declaration (`platform::declare_platform_staging`, `2026-10-15-fastly-names-in-core-design.md` section 2.2).
2. **The staging version links a store of its own,** through `ts deploy --staging` with `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` naming that store. It reads that store's selector and documents, and the staging selector may carry text of its own, such as `{host}_staging`.

Today's staging keeps working wherever production and staging carry the same selector, which is every existing deployment read for this design. A deployment whose two entries differ cannot keep both in one store, because the entry is no longer scoped by service id. Such a deployment moves its staging selector and its staging documents into a store that its staging versions link, and those documents move once. One caution carries over from main's guide. `ts config push --staging` on Fastly writes the logical key in whichever store `__NAME` names, so a staging push with the production store selected writes into production.

### 2.6 Writing a host's document

EdgeZero refuses a Fastly key other than the logical store ID, so the `ts` CLI writes such a key itself. `ts config push --adapter fastly --key <key>` keeps its form, and a key other than `trusted_server_config` is handed to the `ts` CLI's own writer rather than to EdgeZero.

1. The document is validated as any push validates it (`run_config_validate_typed`).
2. It is wrapped in the same envelope EdgeZero writes (`edgezero_core::blob_envelope`, unchanged between `683202c` and `v0.0.9`).
3. A document over 8,000 characters is split into EdgeZero's chunk format, which is also unchanged between the two versions (`chunked_config.rs`). The chunks are written first and the pointer last, so a reader never meets a pointer to a chunk that is missing.
4. Each entry is written to the store `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` names, through `fastly config-store-entry update`, as the CLI already writes signing keys (`crates/trusted-server-cli/src/commands/keys.rs:197-202`).
5. `--dry-run` prints the keys it would write and writes nothing.

The adapter's reader from row 49 (`config_chunks.rs`) checks every chunk's length and hash before it returns a document, so a document written this way is read back whole or refused. Each key is pushed in lower case, as the guide already says, because the runtime lowercases the host.

### 2.7 Axum, Cloudflare and Spin

Nothing changes on the three adapters that read one document. The Axum and Spin adapters take the key from `default_config_key()` (`crates/trusted-server-core/src/settings_data.rs:43-45`, called at `crates/trusted-server-adapter-axum/src/app.rs:164` and `crates/trusted-server-adapter-spin/src/app.rs:119`). At `v0.0.9` that still honors `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY` where the environment sets it (`EnvConfig::store_key`, `crates/edgezero-core/src/env_config.rs:122`), as main's doc comment on `default_config_key` says (`settings_data.rs:54-59` on main). The Cloudflare adapter reads the document its binding holds (`crates/trusted-server-adapter-cloudflare/src/app.rs:192`). None of the three reads `TS__CONFIG__KEY` or substitutes `{host}`, and each ignores the selector values a request addressing hook names (`addressed_router`, `2026-10-13-request-addressing-design.md`).

### 2.8 How a deployment moves over

No document moves, and the steps run in this order.

1. **The selector.** Write the value of the service's current `__KEY` entry into `TS__CONFIG__KEY`, in the store the version links as `trusted_server_config`. A build from before this change does not read the entry, so it can be written while that build serves. Where the production and staging entries differ, section 2.5 applies first.
2. **The documents.** Confirm that the store linked as `trusted_server_config` holds a document at the selector's key for every host the service answers, because a host without one is answered `421` by the new build. In each existing deployment the documents are already in that store. A version that opened its documents under another link name, through a `__NAME` entry in `edgezero_runtime_env`, links that store as `trusted_server_config` instead. A managed deploy does that when `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` names the store.
3. **The build.** Stage the new build on a version that links the same store, request each host on the staging environment, and then activate that version.

Rolling back stays possible. A build from before this change reads its old `EDGEZERO__SERVICES__<SERVICE_ID>__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY` entries through the `edgezero_runtime_env` link. A cloned version keeps that link, and a managed deploy keeps it as an inherited link, because it deletes only a link whose logical ID it declares and whose store has changed (`plan_link_reconciliation`, `cli.rs:714-727` at `v0.0.9`). The old entries and the link are removed only once no active, staged or rollback version reads them.

### 2.9 Taking Tech Lab main, row by row

Main is merged into row 1, and each row then takes the row below it, in the stack's order. A trial merge (`git merge-tree --write-tree`) of main's `fa9837f2b` with each row's head found the conflicts in the table. A row that is not listed merged without a conflict in the trial.

| Row                                | What the merge meets                                                                                                                                                                                          | How it is resolved                                                                                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1 to 5                             | No conflict                                                                                                                                                                                                   | Each takes main's logical stores, its `trusted_server_secrets` local store, its sandbox bounds and the `v0.0.9` pin as they are.                                                                       |
| 7, `split/7-integration-seam-impl` | `crates/trusted-server-adapter-fastly/src/main.rs`, because row 7 moved `edgezero_main` into `lib.rs` and main changed the function where it was                                                              | Keep row 7's thin binary and make main's change in `lib.rs`, being `RuntimeStoreConfig::logical()` in place of `runtime_env_config`.                                                                   |
| 7                                  | `crates/cmp/sourcepoint/src/lib.rs`, because main's fa9837f2b (#1183) rewrote `crates/trusted-server-core/src/integrations/sourcepoint.rs`, 960 changed lines, and row 7 moved that module into its own crate | Carry #1183 into the crate. Its 21 changed lines in `docs/guide/integrations/sourcepoint.md` merge cleanly. Rows 8, 10, 21 and 43 also change the crate, so each of their merges is read in that file. |
| 11, `split/11-config-by-host`      | No textual conflict, but the row does not build, because `runtime_env_config` is gone                                                                                                                         | This design, made on row 11 in the merge that reaches it, with the guide's section for a document per host, `scripts/config-by-host-local-test.sh` and the tests of section 4.                         |
| 13, `split/13-removals`            | `fastly.toml`, `viceroy-template.toml`, `scripts/smoke-fastly.sh`, `scripts/template-cache-local-test.sh` and `docs/guide/fastly.md`                                                                          | Main's local stores, with `trusted_server_secrets` and no `edgezero_runtime_env`, and row 13's removals.                                                                                               |
| 15, `split/15-diagnostic-headers`  | `docs/guide/getting-started.md`                                                                                                                                                                               | Main's managed deploy in place of `fastly compute publish`, and row 15's text.                                                                                                                         |
| 35, `split/35-request-addressing`  | No conflict                                                                                                                                                                                                   | Builds once row 11 does. Its guide and its design document say "the selector" where they say "the `__KEY` selector".                                                                                   |
| 49, `split/49-host-features`       | `crates/trusted-server-core/src/settings_data.rs`, the doc comments that main rewrote for Fastly and row 49 made neutral                                                                                      | Keep row 49's neutral text and drop `edgezero_runtime_env`, which nothing reads any more (`settings_data.rs:23-45`).                                                                                   |
| 6, the design documents            | Not in the chain of merges                                                                                                                                                                                    | `2026-10-12-config-by-host-design.md` and `2026-10-13-request-addressing-design.md` point here for where the selector is read.                                                                         |

`Cargo.lock` merged without a conflict at every row in the trial, and each row's build confirms that the lock still resolves.

This design is made on row 11, the row where a document per host enters the stack, and not on a row of its own. A row of its own after row 11 would leave row 11 with a placeholder that nothing could set. Rows 1 to 10 take main with no selector at all, as main has none.

### 2.10 Options not taken

- **Keep reading `edgezero_runtime_env` from the adapter.** The adapter could copy EdgeZero's removed lookup and keep the old entries. That keeps a store EdgeZero and main have both removed, which a managed deploy keeps only as an inherited link and never creates, and a deployment would have two stores to keep in step.
- **One entry for each Fastly service id, as today.** One store could then hold different selectors for production and staging. Main removed exactly that scoping from the sandbox bounds when the store became the application's own, the staging environment's service id is a detail an operator has to find, and EdgeZero now keeps staging apart by store.
- **A physical store for each host,** chosen at deploy time as EdgeZero intends. Every document would move into a store of its own, and every host would need another link on every version.
- **The selector inside the document at `trusted_server_config`.** That key holds a settings document on every service that reads one, and `ts config push` writes there, so one push could overwrite the selector or the selector could stand where settings are expected.
- **The selector built into the package.** One package serves production and staging, and changing a selector would need a build and a deploy, where today it takes effect on the next request.
- **A staging selector chosen by the platform's staging signal,** such as adding `_staging` when `FASTLY_IS_STAGING` is `1`. That brings back the key suffix EdgeZero removed, and existing deployments read the production keys in staging by design.
- **Asking EdgeZero to accept the keys an application declares.** That would let `ts config push` write a host's document without a writer of the stack's own, but the merge would then wait for an EdgeZero release. It is worth raising with EdgeZero separately.

## 3. What is not in this change

- **Different selectors for production and staging in one store.** Section 2.5 gives the way through, a store for staging.
- **Removing the chunks a re-pushed host's document leaves behind.** `ts config gc` is EdgeZero's, and how it treats keys other than the logical store ID was not checked for this design.
- **`ts config diff` for a key other than the logical store ID**, which EdgeZero refuses as it refuses a push (`run_config_diff_typed`, `crates/edgezero-cli/src/config.rs:609` and `:655` at `v0.0.9`).
- **Choosing a document for each request on Axum, Cloudflare and Spin**, which read one document, as the config by host design records.
- **Removing the old `__KEY` entries and the `edgezero_runtime_env` link** from a live service, which waits until no version reads them (section 2.8).

## 4. Acceptance

Unit tests in `crates/trusted-server-adapter-fastly/src/config_selection.rs`, with no store.

- An absent entry gives `trusted_server_config`, and `{host}`, `{host}_v2` and a fixed key are accepted as written.
- Each malformed shape in section 2.4 is refused as `MalformedSelector`, among them `""`, `" {host}"`, `{host}{host}`, `{Host}`, `{hostname}`, `{host`, `host}` and a selector of 256 characters.
- A key over 255 characters after substitution is refused as `NoConfigForHost`, with no lookup.
- The substitution tests at `config_selection.rs:180-477` pass unchanged, because they take the selector's text.

Viceroy tests in `crates/trusted-server-adapter-fastly/src/lib.rs`, whose local `trusted_server_config` store holds `TS__CONFIG__KEY`.

- With `{host}_v2`, a request for `a.example` is answered from `a.example_v2`, one for `b.example` from `b.example_v2`, and one for `unknown.example` is answered `421`, while a document at `trusted_server_config` in the same store is never served for a host.
- With no entry, every host is answered from `trusted_server_config`.
- With a malformed entry, every route is answered `500` and `/health` is answered `200`.
- A selector written under `EDGEZERO__SERVICES__0000000000000000000000__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY` in a local `edgezero_runtime_env` store is not read, which proves the old source is gone.
- A request addressing hook's selector values take the placeholder's place in the entry's selector, and the first value whose document exists is read.
- `scripts/config-by-host-local-test.sh` writes `TS__CONFIG__KEY` into the local `trusted_server_config` store and passes.

Tests in `crates/trusted-server-cli`.

- `ts config push --adapter fastly --key a.example` writes through the stack's writer, and `--key trusted_server_config` still goes to EdgeZero.
- A document over 8,000 characters is written as chunks and then a pointer, and the adapter's reader in `config_chunks.rs` returns it whole from a fixture the writer produced. A fixture with one chunk's hash changed is refused.

For a live service, a staged version of the new build, linking the store that holds today's entries, answers every host the active version answers from the same document. The pull request's description records that check with the CI run.

Not tested.

- The `500` for a store that opens and then fails a lookup. No test makes a lookup fail on a store that opens, so only a store that cannot be opened is tested.
- The rollback path of section 2.8 in CI. It depends on a live service's links and is checked by hand before the old entries are removed.

## Review log

No review rounds yet.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-15 | First draft. |
