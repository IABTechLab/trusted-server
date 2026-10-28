# Server-side Identity Providers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Trusted Server call identity providers (ID5, LiveRamp ATS, Lockr, UID2 and generic pull partners) from the edge through one `IdentityProvider` trait and one coordinator. Every page load makes at most one KV write, apart from the removal writes, and removals reach KV on the request that carries them. A provider can return complete EIDs, with several entries and UIDs per source and their EID-level fields, and TS stores and sends them whole. Every path that predates this work keeps its one-UID behaviour (G13).

**Architecture:** New code lives in `crates/trusted-server-core/src/ec/identity/`. Providers are pure. They describe themselves with a `ProviderDescriptor`, build `ProviderRequest`s and parse buffered `ProviderResponse`s into `Resolution`s. The `IdentityCoordinator` owns eligibility, consent, transport limits, backoff and turning results into changes. Each request is either the **writer** (a real-browser top-level document navigation) or a **stager**. Resolving providers run only on the writer of a returning EC. The writer adds every KV change to one request-scoped `IdentityMutation` and applies it with one conditional write after the response is sent. Stagers persist no positive ID: they read KV and serve from it. The two removal writes are the exceptions: a failed flush retries its removals alone, and a capture's revocation is written on the proxied request through the shared CAS loop in `removal.rs`. The server sets no identity cookie of its own beyond `ts-ec`. `/auction` and `/_ts/page-bids` return a `ts-eids-s` mirror payload that the shim writes, and the server never reads that cookie. KV schema version 2 stores every record as an entry set (`eids`), and reads version 1 rows as one-entry records. Today's pull sync becomes `GenericPullProvider`.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `trusted-server-adapter-fastly`, `error-stack`, `derive_more`, `serde`/`serde_json`, `hmac`/`sha2`, `base64`, `rand`, `flate2`/`brotli` (all already dependencies). The shim is TypeScript in `crates/trusted-server-js/lib`. Fastly Compute (Viceroy for tests).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md` (revision 15 with the round 15 amendments, branch `spec/1246-server-side-identity-providers`). Read it before starting. "Spec 5.4" means section 5.4 of that file. Where this plan and the spec disagree, the spec wins. Stop and report the disagreement instead of choosing one. The spec cites `file:line` at commit `c84a73da7`. Line numbers have drifted slightly since then, so find code by the function names given here, not by line number.

## How this plan is split

The spec's rollout (spec 14) has two steps, and each step can ship on its own. This plan has one **Part** per step, and each Part is one pull request. Each PR's branch is stacked on the previous one:

| Part | Spec step                | Branch                            | Base                                       | Tasks         |
| ---- | ------------------------ | --------------------------------- | ------------------------------------------ | ------------- |
| 1    | 1. One write, no vendors | `feat/1246-identity-single-write` | `spec/1246-server-side-identity-providers` | 1 to 31       |
| 2    | 2. Providers             | `feat/1246-identity-providers`    | `feat/1246-identity-single-write`          | 32 to 41, 39b |

A task with a letter suffix, such as Task 39b, was added by a later spec revision. It runs right after the task with the same number. Task numbers 4b, 16 and 31b are retired: revision 15 removed the cookie keyring, the sealed cookies and the first-auction race metric. Task 27 is reused: it held the removal overflow write, which revision 15 removed, and now holds the withdrawal tombstone's after-send attempt. Part 1 starts with the outbound cookie strips (Tasks 1 to 4), which spec 14 folds into rollout step 1.

Do not start Part 2 until Part 1's one-write invariant tests (Task 30) pass.

**Rolling back past Part 1 is unsupported** (spec 14). Part 1 writes KV schema version 2, and today's reader rejects every version but 1, so a build without Part 1 cannot read a row Part 1 has written. Part 1's PR description and changelog say so.

## Global Constraints

Copied from the spec and `AGENTS.md`. Every task implicitly includes these.

- Follow `AGENTS.md` for commands, style, error handling and CI gates. Bare `cargo test`, `cargo build` and `cargo check` fail at the workspace root by design. Use the aliases (`cargo test-fastly`, `cargo clippy-fastly`, and the others).
- Errors use `error-stack` `Report<E>` with `derive_more::Display` enums and `impl core::error::Error`. No `anyhow` and no `thiserror`. Use `expect("should ...")`, never `unwrap()`, in production and in tests.
- No local imports inside functions. No wildcard imports except `use super::*` in test modules. Comments go on their own line, above the code.
- Every public item has a doc comment, and every fallible public function has `# Errors`. Every builder method and getter is `#[must_use]`. Spec 4.2, "Conventions for the listings", gives the derive list for each type. Copy it exactly.
- Never log UIDs, envelopes, HEMs, provider state, consent strings, cookie values, or provider URLs with a query string. EC IDs go through `log_id`. Log provider errors with `current_context()` only, never `{:?}` of the report (spec 9.3).
- Use only example or fictional data in code, tests, comments and docs (`example.com` hosts). Public protocol source domains (`id5-sync.com`, `liveramp.com`, `uidapi.com`, `sharedid.org`) may appear where the spec uses them.
- **Fixed values from the spec. Do not change them:**
  - `ts-eids-s` (`__Host-ts-eids-s`) is client-only: the shim writes it with `Path=/; Secure; SameSite=Lax`, no `Domain` and no `HttpOnly`, with the `Max-Age` the payload gives. The server never sets, opens, parses or trusts it. The mirror payload is `ext.ts_eids_s` on `/auction` and a `ts_eids_s` member of the `/_ts/page-bids` body: `{value, max_age}`, `{clear: true}`, or absent. `value` is base64url without padding of compact JSON `{"v":1,"eids":[...]}`, at most **2,048** bytes. `max_age` is at most **86,400** and never past the earliest `exp` in the mirrored set (spec 6.2).
  - `TS_IDENTITY_COOKIE_NAMES` = `ts-ec`, `ts-eids`, `__Host-ts-eids-s`, stripped from Prebid Server requests. `TS_ORIGIN_STRIPPED_COOKIE_NAMES` = `__Host-ts-eids-s`, stripped from origin requests by default; `TS_ORIGIN_STRIPPED_COOKIE_NAMES_WITH_EIDS` = `ts-eids`, `__Host-ts-eids-s`, stripped instead when `[ec] strip_eids_cookie_from_origin = true`. Exact names only: there are no cookie prefixes.
  - Hashes: `ps.iv` and `ps.ov` hold the first **16** hex digits of the unkeyed SHA-256 of each UID. The consent fingerprint `fp` is a plain bitmask written as hex. The only keyed value is the rate-limit visitor key, an HMAC of the full EC ID with a key derived from `ec.passphrase` (spec 9.3). Nothing stored in the row is keyed.
  - KV: `SCHEMA_VERSION` is **2**. Every record is `{w?, rv?, at?, exp?, ra?, ru?, eids: [{uids: [{id, atype?, ext?}], inserter?, matcher?, mm?, ext?}]}`, and `eids` is never empty. A version 1 record (`{uid, ...}`) reads as one entry holding that UID, and the row's next write stores version 2. Every UID-level and EID-level `ext` is a JSON object of at most **2,048** bytes as compact JSON. `inserter` and `matcher` are not blank and at most **255** bytes. A source holds at most **8** entries and **32** UIDs, and its entry set is at most **4,096** bytes as compact JSON (`MAX_ENTRY_SET_BYTES`). `ps.st` is at most **1,024** bytes of embedded JSON, a row has at most **32** `ps` entries, and `ps.iv` and `ps.ov` are each an ordered list of at most **128** hashes (`MAX_INVALIDATED_UIDS`), oldest first, truncated from the front before validation. One function, `entry_set_bytes`, measures `MAX_ENTRY_SET_BYTES` on the stored `eids` array, for both the coordinator and `KvEntry::validate`. Expired means `now >= exp`. `rv` is **16** hex digits (64 random bits). On read, a record that breaks a limit is dropped from the row and logged with its source only; only `serialize_entry` rejects.
  - `ProviderRequest` call context: at most **1,024** bytes of JSON. `IdentityProviderId`: `^[a-z][a-z0-9_-]{0,62}$`.
  - `[ec.identity]` defaults: `max_concurrency = 4`, `hold_budget_ms = 100`, `hold_retry_sec = 300`, `max_response_bytes = 65536`. Per-provider defaults: `mode = "after_send"`, `timeout_ms` = `hold_budget_ms`, `after_send_timeout_ms = 1000`, `refresh_after_sec = 86400`, `max_calls_per_minute = 1`, `breaker_failures_per_minute = 50`, `cookie_priority = 0`, `sources` = every source the descriptor declares (required for a capture-only provider). For a provider that declares `captures`: `revoke_scope = "provider"`.
  - Removed config keys, each rejected with an error that names its replacement (spec 10.2): `ec.pull_sync_concurrency` (`[ec.identity].max_concurrency`), and `pull_sync_ttl_sec` and `pull_sync_rate_limit` on `[[ec.partners]]` (`refresh_after_sec` and `max_calls_per_minute = ceil(N / 60)` in `[ec.identity.providers.<pull-id>]`).
  - Backoff (spec 8): a `found` sets `f = 0`. A `no_match` waits `n = now + retry_in`, default 24 h and at most 7 d. `with_retry_in` on any other resolution makes `n` at least `now + retry_in`, after the row's own rule (so an `invalidate` gets `n = max(now, now + retry_in)`). An input-caused failure waits `min(1 h × 2^(f-1), 24 h)`. A transport, 429 or 5xx failure waits `min(5 min × 2^(f-1), 1 h)`. A cut-off hold call waits `hold_retry_sec`. A skip waits `refresh_after_sec`. A due provider the burst cap stops gets `ps.n = max(ps.n, now + 60 s)`. A provider the breaker or a limiter error stops is not dispatched and its `ps` is unchanged.
- **G4 is strict.** A page load makes at most one KV write, never retried as a whole. The only exceptions (spec 5.1):
  - the consent-withdrawal tombstone path (a CAS with up to five attempts, then its completion marker, per withdrawn EC ID), which also runs for a `revoke_scope = "global"` capture from any client, and gets one more attempt after send when it fails before send (Task 27)
  - the removal-only retry of a failed flush (spec 5.2, phase 4, step 5; Task 29)
  - the capture removal write (spec 5.3; Task 38)

  The last two share one CAS loop, `ec/identity/removal.rs` (Task 18): up to five attempts, retrying only a generation mismatch, and `removal_lost` when every attempt fails.

- **Removals win.** No filter (consent re-gate, staleness, ownership, the provider no longer being enabled, size cap) may drop a removal, an opt-out or a clear. No client gate applies to a removal or an opt-out; only a capture's clear needs a real browser (spec 4.7, 5.6).
- **Keep `ts-eids` and `x-ts-eids`.** Do not remove either (product requirement).
- **Behaviour that predates the spec does not change (G13).** G13 itself changes none of the paths that predate the spec. Browser ingest, `ts-eids`, batch sync, generic pull, `/auction` body forwarding and the `/identify` response keep the behaviour the rest of the spec gives them, and every record they write is one entry with one UID: one UID per registered source, picked by today's rule (`collect_eid_updates`, `ec/prebid_eids.rs:146-151`: for a repeated source, the first valid UID of its last entry, with the sharedId cookie applied last), and nothing for an unregistered source. Only a provider result writes more, and no Part 1 code writes more. A version 1 row goes out to bidders exactly as it does on `main`. Golden tests, named `golden_*` so `cargo test-fastly -- golden` runs them all, pin these paths (Tasks 6, 7, 28 and 30).
- **Feature gate for test support.** Gate test-only code on `#[cfg(any(test, feature = "test-utils"))]`, the feature `trusted-server-core` already has (`crates/trusted-server-core/Cargo.toml`) and spec 13 names. `ec::identity::test_support` is a `pub mod` under that gate, and every item in it is `pub`, from the task that creates it, as existing `test-utils` items are (`IntegrationRegistry::from_request_filters`). A `pub(crate)` item behind the feature is dead code in the non-test library build that `cargo clippy-fastly` lints with `--all-features`, so `-D warnings` fails it. The Fastly adapter's dev-dependencies already enable the feature (`crates/trusted-server-adapter-fastly/Cargo.toml`).
- **Adapter-facing API.** The Fastly adapter is a separate crate. It reaches identity state only through `pub struct IdentityRuntime` (Task 23) and `pub` opaque wrappers such as `BrowserSeed` (Task 25). Core types stay `pub(crate)`, and no `pub` item exposes a `pub(crate)` type: `private_interfaces` is an error under `-D warnings`.
- **Lints that bite plan code.** Backtick `OpenRTB`, CamelCase vendor names such as `LiveRamp` and `SharedID`, and other code-like words in doc comments (`clippy::doc_markdown`). `clippy::panic` is denied in production code; `clippy.toml` allows `panic!` in tests. On `wasm32-wasip1` a panic aborts the whole test binary under Viceroy and `#[should_panic]` tests are ignored, so a check that a test must see fail returns a `Result` instead of panicking.
- Test discipline:
  1. **Red first.** Run each new test against unchanged code and watch it fail for the expected reason before you implement.
  2. **Pair absence with presence.** Each "X is gone" assertion sits next to a "control Y is still there" assertion on the same value.
  3. Compare structured outputs whole (full `KvEntry`, full `serde_json::Value`). Do not use `..Default::default()` in exhaustiveness tests. List every field.
  4. Use `json!` for JSON fixtures, except where exact byte lengths matter.
- Commits: sentence case, imperative mood, no `fix:`/`feat:` prefixes, no bracketed tags, `git commit --signoff -S`, one `-m` flag per paragraph, no heredoc, no AI attribution lines.

## Test commands

| What                                  | Command                                                                                                                 |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Core and Fastly adapter (Viceroy)     | `cargo test-fastly`                                                                                                     |
| One core test by name                 | `cargo test-fastly -- <test_name_substring>`                                                                            |
| Fastly adapter with reusable sandbox  | `cargo test-fastly-reuse`                                                                                               |
| Other adapters (strips, Task 31)      | `cargo test-axum && cargo test-cloudflare && cargo test-spin`                                                           |
| Lint, Fastly target                   | `cargo clippy-fastly`                                                                                                   |
| Docs                                  | `cargo doc --no-deps --all-features -p trusted-server-core`                                                             |
| Parity crate                          | `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`                           |
| CLI (config validation, Tasks 19, 33) | `./scripts/test-cli.sh`                                                                                                 |
| JS shim (Tasks 26, 30, 31)            | `cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run && mise exec node@24.12.0 -- npm run lint` |
| Full gate before each PR              | `/check-ci`                                                                                                             |

`cargo test-fastly -- <filter>` runs only the tests whose path contains `<filter>`. Every "Run" step below gives the exact filter.

## File map

| File                                                                                                                                                    | Responsibility                                                                                                                                                                                              | Tasks                                   |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| `crates/trusted-server-core/src/constants.rs`, `src/cookies.rs`                                                                                         | Cookie names, identity cookie lists, name strip                                                                                                                                                             | 1                                       |
| `crates/trusted-server-core/src/auction/routing.rs`, `src/integrations/prebid.rs`                                                                       | Strip identity cookies from Prebid Server requests                                                                                                                                                          | 2                                       |
| `crates/trusted-server-core/src/publisher.rs`                                                                                                           | Origin strip; servability callers; `is_prefetch_request` move; finalize callers; page-bids stops ingest and returns the mirror; navigation merge and denials; preload decision; owned precedence; hold mode | 3, 14, 23, 25, 26, 28, 29, 36, 37       |
| `crates/trusted-server-core/src/cookies/template_cache_policy.rs`                                                                                       | `ts-eids-s` refused as a cache key                                                                                                                                                                          | 4                                       |
| `crates/trusted-server-core/src/settings.rs`, `src/config.rs`                                                                                           | `[ec.identity]`: `IdentitySettings`, `ProviderSettings`, `RevokeScope`; removed keys; deploy and runtime validation; `ADMIN_ENDPOINTS`                                                                      | 19, 22, 33, 36                          |
| `crates/trusted-server-core/src/ec/identity/mod.rs`, `ids.rs`, `resolution.rs` (new)                                                                    | Module, ID newtypes, `UnixTime`, error type, results (`ResolvedEid`, `ResolvedUid`, `entry_sets`)                                                                                                           | 5, 10                                   |
| `crates/trusted-server-core/src/ec/kv_types.rs`                                                                                                         | Schema version 2: `KvPartnerId` with `eids`, `KvEid`, `KvUid`, `entry_set_bytes`, `KvProviderState` with `iv` and `ov`, `KvEntry.ps`, validation, upgrade on read                                           | 6                                       |
| `crates/trusted-server-core/src/ec/kv.rs`                                                                                                               | Constructors; upgrade on read; `PartnerIdUpdate.acquired_at`; `create_if_absent` seed; `compute_cluster_size`; withdrawal retry after send; counting store                                                  | 6, 7, 22, 24, 25, 27                    |
| `crates/trusted-server-core/src/ec/prebid_eids.rs`                                                                                                      | Browser ingest records `at` and stays one UID                                                                                                                                                               | 7                                       |
| `crates/trusted-server-core/src/ec/admin.rs`                                                                                                            | Redact `ps.st`; upgrade on read; servability; operator opt-out clear                                                                                                                                        | 6, 14, 36                               |
| `crates/trusted-server-core/src/ec/identity/descriptor.rs` (new)                                                                                        | Descriptor, builder, `Resolves`, `Captures`, `ConsentPolicy`, `EidProvenance`, `Input`, `Access`, `ResolveKind`, `UsOptOut`                                                                                 | 5, 8                                    |
| `crates/trusted-server-core/src/ec/identity/input.rs` (new)                                                                                             | `ResolveInput`, `CaptureInput`, `StoredId`, `Region`, `HashedEmail`, `ProviderConsent`, randomness                                                                                                          | 9, 10                                   |
| `crates/trusted-server-core/src/ec/identity/exchange.rs` (new)                                                                                          | `ProviderRequest`, `ProviderResponse`                                                                                                                                                                       | 10                                      |
| `crates/trusted-server-core/src/ec/identity/provider.rs` (new)                                                                                          | `IdentityProvider`, `IdentityProviderConfig`                                                                                                                                                                | 11                                      |
| `crates/trusted-server-core/src/ec/identity/policy.rs` (new), `src/consent/*`                                                                           | Policy evaluation, GPP opt-outs, the plain consent fingerprint, redacting `Debug`                                                                                                                           | 12, 20, 34                              |
| `crates/trusted-server-core/src/ec/identity/revision.rs` (new)                                                                                          | `Revision`, `RecordRevision`, `StateRevision`, the unkeyed UID hash, the visitor key                                                                                                                        | 13                                      |
| `crates/trusted-server-core/src/ec/identity/catalog.rs`, `servable.rs` (new)                                                                            | Provider catalog, narrowed sources, ownership, `validate_identity_catalog`; the servability check                                                                                                           | 14, 22, 32, 33                          |
| `crates/trusted-server-core/src/ec/eids.rs`, `ec/identify.rs`, `src/openrtb.rs`                                                                         | Servability for KV IDs; one EID per stored entry; provenance defaults; `/identify` stops writing                                                                                                            | 14, 24, 28                              |
| `crates/trusted-server-core/src/ec/identity/mutation.rs` (new)                                                                                          | `IdentityMutation` and its apply algorithm, including the removals-only apply                                                                                                                               | 15                                      |
| `crates/trusted-server-core/src/ec/identity/mirror.rs` (new)                                                                                            | The `ts-eids-s` mirror payload and its size cap                                                                                                                                                             | 17                                      |
| `crates/trusted-server-core/src/ec/identity/removal.rs` (new)                                                                                           | The shared removal write: CAS loop, after-send attempt, `removal_lost`                                                                                                                                      | 18, 29, 38                              |
| `crates/trusted-server-core/src/ec/identity/coordinator.rs` (new)                                                                                       | Eligibility, outcomes, dispatch, breaker, hold, capture, re-grants                                                                                                                                          | 20, 21, 35, 36, 37, 38                  |
| `crates/trusted-server-core/src/ec/identity/generic_pull.rs` (new), `ec/pull_sync.rs`, `ec/pull_sync_marker.rs`                                         | `GenericPullProvider`; dispatch order; pull sync dispatcher and the pull-complete marker removed                                                                                                            | 21, 22, 29                              |
| `crates/trusted-server-core/src/ec/identity/test_support.rs` (new, `pub` under `test-utils`)                                                            | Shared fixtures, counting store, fake HTTP client and limiter, `ProviderHarness`, `MultiEntryTestProvider`                                                                                                  | 14, 15, 18, 20, 21, 22, 25, 35, 39, 39b |
| `crates/trusted-server-core/src/ec/identity/runtime.rs` (new)                                                                                           | `pub struct IdentityRuntime`: catalog and visitor key for the adapter                                                                                                                                       | 23, 29                                  |
| `crates/trusted-server-core/src/ec/identity/request.rs` (new), `src/ec/mod.rs`, `src/http_util.rs`                                                      | Writer decision, phase-1 captures, request identity state, removal counters                                                                                                                                 | 5, 23, 25, 29, 38                       |
| `crates/trusted-server-core/src/ec/finalize.rs`                                                                                                         | Mutation instead of writes; `browser_seed`; global revoke; withdrawal retry; metrics                                                                                                                        | 7, 25, 27, 29, 31, 38                   |
| `crates/trusted-server-core/src/auction/endpoints.rs`, `src/openrtb.rs`                                                                                 | Servability callers; finalize test callers; mirror payload in `ext`; server set and denials in the merge; owned precedence; hold mode                                                                       | 14, 25, 26, 28, 36, 37                  |
| `crates/trusted-server-core/src/ec/identity/flush.rs` (new)                                                                                             | Phase 4 `run_after_send`                                                                                                                                                                                    | 29                                      |
| `crates/trusted-server-adapter-fastly/src/app.rs`, `src/main.rs`, `src/rate_limiter.rs`                                                                 | `IdentityRuntime` in `AppState`; writer decision; status capture; testable post-route function and phase 4; `record`/`count`; request-level tests (`drive_request_for_test`); admin route                   | 23, 25, 27, 29, 30, 35, 36, 39b         |
| `crates/trusted-server-adapter-axum/src/app.rs`, `crates/trusted-server-adapter-cloudflare/src/app.rs`, `crates/trusted-server-adapter-spin/src/app.rs` | Admin opt-out route parity                                                                                                                                                                                  | 36                                      |
| `crates/trusted-server-core/src/integrations/registry.rs`                                                                                               | `BrowserSeed` and `is_real_browser` on `ProxyDispatchInput`; provider registration; capture-path check; capture plumbing                                                                                    | 23, 25, 32, 38                          |
| `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`                                                                                        | `is_prefetch_request` caller                                                                                                                                                                                | 23                                      |
| `crates/trusted-server-core/src/integrations/lockr.rs`                                                                                                  | Upstream log without the query string                                                                                                                                                                       | 38                                      |
| `crates/trusted-server-js/lib/src/core/*.ts`, `src/integrations/prebid/*`, `test/**`                                                                    | The mirror writer; registered-only, per-source `ts-eids` merge; `ortb2` G10 test                                                                                                                            | 26, 30, 31                              |
| `crates/trusted-server-core/src/ec/rate_limiter.rs`                                                                                                     | `record` and `count`                                                                                                                                                                                        | 35                                      |
| `crates/trusted-server-core/src/ec/batch_sync.rs` (and its adapter call site)                                                                           | Owned sources fill only when absent                                                                                                                                                                         | 40                                      |
| `crates/trusted-server-core/src/ec/identity/examples.rs` (new, tests only)                                                                              | Spec 4.6 reference providers                                                                                                                                                                                | 32, 33, 39, 39b                         |
| `crates/trusted-server-core/src/migration_guards.rs`                                                                                                    | Every new `ec/identity/*.rs` file                                                                                                                                                                           | 5 onward                                |
| `docs/guide/*.md`, `CHANGELOG.md`, `trusted-server.example.toml`                                                                                        | Docs, removed keys and the rollback note                                                                                                                                                                    | 4, 19, 31, 41                           |

This layout is the one spec 4.1's file table lists, with one difference: `[ec.identity]` settings live in `settings.rs` with the rest of the settings, and their validation in `config.rs`, rather than in a new `ec/identity/config.rs`. `UnixTime` lives in `ids.rs`.

---

# Part 1: One write, no new vendors (spec 14, step 1)

Part 1 starts by removing TS's identity cookies from outbound requests (Tasks 1 to 4), then builds the provider framework without registering any vendor, moves KV to schema version 2, and makes a page load write KV once. Generic pull partners are its only providers. It reads, preserves, validates and serves complete entry sets, but nothing in it writes a set with more than one entry or UID, so a rollback from Part 2 to Part 1 keeps every set a provider wrote. No vendor provider is registered yet, and no provider runs in hold mode or captures. Those come in Part 2.

Tasks 1 to 4 strip the identity cookies from outbound requests. Tasks 5 to 22 build unit-tested modules. Most have no request wiring: Task 6 moves KV to schema version 2, Task 7 changes what finalize's browser ingest stores, and Task 14 moves the `/auction`, admin and `/identify` readers onto the servability check. Tasks 23 to 29 wire the rest into requests. Task 26 adds the `ts-eids-s` mirror payload and the shim code that writes it. Task 30 proves the one-write invariant, and Task 31 ships the metrics and the docs.

Before you start: create a worktree from the spec branch, `git worktree add -b feat/1246-identity-single-write .claude/worktrees/1246-identity-providers spec/1246-server-side-identity-providers`. The implementation stacks on its spec branch, which already carries the Prebid EID-fidelity work this spec depends on. Build with the toolchain that branch pins in `rust-toolchain.toml`.

### Task 1: Identity cookie names and a cookie strip helper

**Files:**

- Modify: `crates/trusted-server-core/src/constants.rs` (next to `COOKIE_TS_EIDS`, line 8)
- Modify: `crates/trusted-server-core/src/cookies.rs` (after `CONSENT_COOKIE_NAMES`, and after `strip_cookies`)
- Test: `crates/trusted-server-core/src/cookies.rs` (`mod tests`)

**Interfaces:**

- Produces:
  - `pub const COOKIE_TS_EIDS_S: &str = "__Host-ts-eids-s";`
  - `pub const TS_IDENTITY_COOKIE_NAMES: &[&str]` = `ts-ec`, `ts-eids`, `__Host-ts-eids-s` (spec 6.2)
  - `pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str]` = `ts-eids`, `__Host-ts-eids-s`
  - `pub fn strip_cookie_pairs(cookie_header: &str, names: &[&str]) -> String`
  - `pub fn strip_cookie_headers(headers: &mut http::HeaderMap, names: &[&str])`

The lists match exact names only: no TS identity cookie is a family with a name prefix (spec 6.2). `ts-ec-pull-complete` is not on either list. Task 29 stops setting it, and a browser's copy expires within its one-hour `Max-Age`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `cookies.rs`:

```rust
    #[test]
    fn strip_cookie_pairs_removes_exact_names_only() {
        let header = "ts-ec=abc; ts-eids=W10; __Host-ts-eids-s=eyJ2IjoxfQ; other=1; ts-eids-extra=keep";

        let stripped = strip_cookie_pairs(header, TS_ORIGIN_STRIPPED_COOKIE_NAMES);

        assert_eq!(
            stripped, "ts-ec=abc; other=1; ts-eids-extra=keep",
            "should drop the ID cookies and keep ts-ec and a cookie that only shares a prefix"
        );
    }

    #[test]
    fn strip_cookie_headers_joins_every_cookie_field_and_drops_non_utf8() {
        let mut headers = http::HeaderMap::new();
        headers.append(header::COOKIE, http::HeaderValue::from_static("ts-ec=abc; ts-eids=W10"));
        headers.append(
            header::COOKIE,
            http::HeaderValue::from_bytes(b"bad=\xff").expect("should build an opaque header"),
        );
        headers.append(header::COOKIE, http::HeaderValue::from_static("other=1"));

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES);

        let values: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
        assert_eq!(values.len(), 1, "should collapse into one Cookie header");
        assert_eq!(values[0], "other=1", "should keep only the unrelated cookie");
    }

    #[test]
    fn strip_cookie_headers_removes_header_when_nothing_is_left() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::COOKIE,
            http::HeaderValue::from_static("ts-ec=abc; __Host-ts-eids-s=eyJ2IjoxfQ"),
        );
        headers.insert(header::USER_AGENT, http::HeaderValue::from_static("ExampleBrowser/1.0"));

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES);

        assert!(headers.get(header::COOKIE).is_none(), "should remove the empty Cookie header");
        assert!(headers.get(header::USER_AGENT).is_some(), "should leave other headers alone");
    }
```

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test-fastly -- cookies::tests::strip_cookie`
Expected: compile errors, because `strip_cookie_pairs`, `strip_cookie_headers` and the constants do not exist.

- [ ] **Step 3: Implement**

In `constants.rs`, under `COOKIE_TS_EIDS`:

```rust
/// Client-only mirror of the servable server-side EIDs, written by the shim
/// and never read by the server (spec 6.2).
pub const COOKIE_TS_EIDS_S: &str = "__Host-ts-eids-s";
```

In `cookies.rs`, extend the `crate::constants` import with `COOKIE_TS_EC`, `COOKIE_TS_EIDS` and `COOKIE_TS_EIDS_S`. Then add, after `CONSENT_COOKIE_NAMES`:

```rust
/// Trusted Server identity cookies that never reach a third party.
///
/// Stripped from Prebid Server requests and never added to proxy or provider
/// requests.
pub const TS_IDENTITY_COOKIE_NAMES: &[&str] = &[COOKIE_TS_EC, COOKIE_TS_EIDS, COOKIE_TS_EIDS_S];

/// Identity cookies removed from publisher-origin requests.
///
/// `ts-ec` still reaches the origin. The ID cookies
/// are removed so that they cannot push the origin's `Cookie` header past
/// common 8 KB header limits.
pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str] = &[COOKIE_TS_EIDS, COOKIE_TS_EIDS_S];
```

After `strip_cookies`:

```rust
/// Strips cookies whose name is exactly one of `names` from a `Cookie` header
/// value.
///
/// Returns an empty string when nothing is left.
#[must_use]
pub fn strip_cookie_pairs(cookie_header: &str, names: &[&str]) -> String {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default().trim();
            !names.contains(&name)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Strips matching cookies from every `Cookie` field in `headers`.
///
/// All `Cookie` fields are read, matching pairs are removed, and the rest is
/// written back as one field. A field that is not valid UTF-8 is dropped
/// rather than forwarded, because it cannot be checked. The header is removed
/// when nothing is left.
pub fn strip_cookie_headers(headers: &mut http::HeaderMap, names: &[&str]) {
    let retained = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| strip_cookie_pairs(value, names))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("; ");

    headers.remove(header::COOKIE);
    if retained.is_empty() {
        return;
    }
    match http::HeaderValue::from_str(&retained) {
        Ok(value) => {
            headers.insert(header::COOKIE, value);
        }
        Err(_) => log::debug!("Dropping Cookie header that is invalid after stripping"),
    }
}
```

`unwrap_or_default()` on an `Option<&str>` is not `unwrap()`, and `split` always yields at least one item, so it is safe here.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test-fastly -- cookies::tests`
Expected: PASS, including the existing `strip_cookies` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/constants.rs crates/trusted-server-core/src/cookies.rs
git commit --signoff -S -m "Add identity cookie lists and a cookie strip by name"
```

### Task 2: Strip identity cookies from Prebid Server requests in every mode

**Files:**

- Modify: `crates/trusted-server-core/src/auction/routing.rs` (`PrebidTransportHeaders`, around line 205)
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs` (`apply_prebid_header_values` and both callers, around lines 2025-2108)
- Test: `crates/trusted-server-core/src/integrations/prebid.rs` (`mod tests`)

**Interfaces:**

- Consumes: `strip_cookie_pairs`, `TS_IDENTITY_COOKIE_NAMES`, `CONSENT_COOKIE_NAMES` (Task 1)
- Produces: `PrebidTransportHeaders::cookies(&self) -> &[HeaderValue]`, which replaces `cookie()`. `apply_prebid_header_values` takes `cookies: &[HeaderValue]` as its first argument.

Today `apply_prebid_header_values` forwards the raw `Cookie` header unchanged whenever `consent_forwarding.strips_consent_cookies()` is false, and both callers pass only the first `Cookie` field. After this task, identity cookies are stripped in every mode, every field is read, and a non-UTF-8 field is dropped (spec 6.2, "Prebid Server").

- [ ] **Step 1: Write the failing tests**

Find the existing tests of `apply_prebid_header_values` with `grep -n "apply_prebid_header_values\|strips_consent_cookies" crates/trusted-server-core/src/integrations/prebid.rs`, and add next to them. Build the target request the way those tests do. The `ConsentForwardingMode` variants are listed in `consent_config.rs:137-150`. Use the one that does **not** strip consent cookies for the first test and the one that does for the second.

```rust
    fn cookie_values(request: &http::Request<EdgeBody>) -> Vec<String> {
        request
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should be ASCII").to_owned())
            .collect()
    }

    #[test]
    fn prebid_headers_strip_identity_cookies_when_consent_cookies_are_forwarded() {
        let cookies = [
            HeaderValue::from_static("ts-ec=abc; euconsent-v2=tcf; other=1"),
            HeaderValue::from_static("__Host-ts-eids-s=eyJ2IjoxfQ; partner=p"),
            HeaderValue::from_static("ts-eids=W10; second=2"),
        ];
        let mut to = http::Request::new(EdgeBody::empty());

        apply_prebid_header_values(
            &cookies,
            None,
            None,
            None,
            &mut to,
            ConsentForwardingMode::Both,
            None,
        );

        assert_eq!(
            cookie_values(&to),
            vec!["euconsent-v2=tcf; other=1; partner=p; second=2".to_owned()],
            "should drop identity cookies, keep consent and unrelated cookies, and read every field"
        );
    }

    #[test]
    fn prebid_headers_strip_identity_and_consent_cookies_in_openrtb_only_mode() {
        let cookies = [HeaderValue::from_static("ts-ec=abc; euconsent-v2=tcf; other=1")];
        let mut to = http::Request::new(EdgeBody::empty());

        apply_prebid_header_values(
            &cookies,
            None,
            None,
            None,
            &mut to,
            ConsentForwardingMode::OpenrtbOnly,
            None,
        );

        assert_eq!(cookie_values(&to), vec!["other=1".to_owned()], "should keep only other=1");
    }

    #[test]
    fn prebid_headers_drop_non_utf8_cookie_fields_and_empty_results() {
        let cookies = [
            HeaderValue::from_bytes(b"x=\xff").expect("should build an opaque header"),
            HeaderValue::from_static("ts-ec=abc"),
        ];
        let mut to = http::Request::new(EdgeBody::empty());

        apply_prebid_header_values(
            &cookies,
            None,
            None,
            None,
            &mut to,
            ConsentForwardingMode::Both,
            None,
        );

        assert!(to.headers().get(header::COOKIE).is_none(), "should send no Cookie header");
    }
```

If the mode variants are named differently, use the real names and keep the meaning: one mode that forwards consent cookies and `OpenrtbOnly`.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test-fastly -- integrations::prebid::tests::prebid_headers_`
Expected: compile error, because the first argument is still `Option<&HeaderValue>`.

- [ ] **Step 3: Implement**

In `auction/routing.rs`, change the field and its accessor:

```rust
pub(crate) struct PrebidTransportHeaders {
    cookies: Vec<HeaderValue>,
    user_agent: Option<HeaderValue>,
    referer: Option<HeaderValue>,
    accept_language: Option<HeaderValue>,
}

impl PrebidTransportHeaders {
    /// Every inbound `Cookie` field, in order.
    pub(crate) fn cookies(&self) -> &[HeaderValue] {
        &self.cookies
    }
    // user_agent, referer and accept_language unchanged
```

and in `snapshot`:

```rust
            cookies: request.headers().get_all(header::COOKIE).iter().cloned().collect(),
```

Fix every other use of `cookie:` or `.cookie()` in `routing.rs` that the compiler reports, including the tests at about lines 1300-1315 that append two `Cookie` fields. Those tests should now see both fields.

In `integrations/prebid.rs`, change the import to `use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES, strip_cookie_pairs};`. Then replace the cookie half of `apply_prebid_header_values`:

```rust
fn apply_prebid_header_values(
    cookies: &[HeaderValue],
    user_agent: Option<&HeaderValue>,
    referer: Option<&HeaderValue>,
    accept_language: Option<&HeaderValue>,
    to: &mut http::Request<EdgeBody>,
    consent_forwarding: ConsentForwardingMode,
    client_ip: Option<std::net::IpAddr>,
) {
    // ... unchanged header loop and x-forwarded-for ...

    // Identity cookies are stripped in every mode, before the consent mode is
    // checked, so no mode forwards the raw header (spec 6.2).
    let consent_names: &[&str] = if consent_forwarding.strips_consent_cookies() {
        CONSENT_COOKIE_NAMES
    } else {
        &[]
    };
    let retained = cookies
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| strip_cookie_pairs(value, TS_IDENTITY_COOKIE_NAMES))
        .map(|value| strip_cookie_pairs(&value, consent_names))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if !retained.is_empty()
        && let Ok(cookie_header) = HeaderValue::from_str(&retained)
    {
        to.headers_mut().insert(header::COOKIE, cookie_header);
    }
}
```

Update both callers. `copy_request_headers`-style callers pass a local `let cookies: Vec<HeaderValue> = from.headers().get_all(header::COOKIE).iter().cloned().collect();` and then `&cookies`. `apply_prebid_transport_headers` passes `from.cookies()`. Remove `strip_cookies` from the import if nothing else uses it.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test-fastly -- integrations::prebid` and then `cargo test-fastly -- auction::routing`
Expected: PASS. If an existing test asserted that the raw header is forwarded unchanged, update its expectation to the stripped value and say why in the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction/routing.rs crates/trusted-server-core/src/integrations/prebid.rs
git commit --signoff -S -m "Strip identity cookies from Prebid Server requests in every mode" -m "Every Cookie field is now read and checked, and a field that is not valid UTF-8 is dropped instead of forwarded."
```

### Task 3: Strip the ID cookies from publisher-origin requests

> Revised after the round 14 review: by default only `__Host-ts-eids-s` is stripped, and `ts-eids` keeps reaching the origin as today. `rewrite_origin_request` takes a `strip_eids: bool` from `settings.ec.strip_eids_cookie_from_origin` (`Ec` field, `#[serde(default)]`), which strips `ts-eids` too. Tests: `rewrite_origin_request_strips_only_the_mirror_by_default` and `rewrite_origin_request_strips_ts_eids_when_the_operator_asks`. The steps below are the original task; read their `ts-eids` strip as the opt-in case.

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (`rewrite_origin_request`, around line 3124)
- Test: `crates/trusted-server-core/src/publisher.rs` (`mod tests`)

**Interfaces:**

- Consumes: `strip_cookie_headers`, `TS_ORIGIN_STRIPPED_COOKIE_NAMES` (Task 1)

- [ ] **Step 1: Write the failing test**

Add to `publisher.rs` tests, next to any existing `rewrite_origin_request` test (`grep -n "fn .*rewrite_origin_request" crates/trusted-server-core/src/publisher.rs`):

```rust
    #[test]
    fn rewrite_origin_request_strips_id_cookies_and_keeps_ts_ec() {
        let mut req = Request::builder()
            .uri("https://www.example.com/article")
            .header(header::COOKIE, "ts-ec=abc; ts-eids=W10; session=1")
            .header(
                header::COOKIE,
                "__Host-ts-eids-s=eyJ2IjoxfQ; ts-ec-pull-complete=v1",
            )
            .body(EdgeBody::empty())
            .expect("should build request");

        rewrite_origin_request(
            &mut req,
            "https://origin.example.com/article".parse().expect("should parse uri"),
            "origin.example.com",
        )
        .expect("should rewrite request");

        let cookies: Vec<_> = req.headers().get_all(header::COOKIE).iter().collect();
        assert_eq!(cookies.len(), 1, "should send one Cookie header");
        assert_eq!(
            cookies[0], "ts-ec=abc; session=1; ts-ec-pull-complete=v1",
            "should keep ts-ec, the pull marker and unrelated cookies, and drop the ID cookies"
        );
    }

    #[test]
    fn rewrite_origin_request_drops_cookie_header_when_only_id_cookies() {
        let mut req = Request::builder()
            .uri("https://www.example.com/")
            .header(header::COOKIE, "ts-eids=W10; __Host-ts-eids-s=eyJ2IjoxfQ")
            .body(EdgeBody::empty())
            .expect("should build request");

        rewrite_origin_request(
            &mut req,
            "https://origin.example.com/".parse().expect("should parse uri"),
            "origin.example.com",
        )
        .expect("should rewrite request");

        assert!(req.headers().get(header::COOKIE).is_none(), "should drop the empty Cookie header");
        assert_eq!(req.headers()[header::HOST], "origin.example.com", "should still set Host");
    }
```

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- publisher::tests::rewrite_origin_request_`
Expected: FAIL. The first assertion sees two `Cookie` fields that still hold `ts-eids`.

- [ ] **Step 3: Implement**

In `rewrite_origin_request`, after `req.headers_mut().remove("fastly-ssl");`:

```rust
    // The publisher origin keeps `ts-ec` but never receives the ID cookies,
    // which can push its `Cookie` header past common 8 KB limits (spec 6.2).
    strip_cookie_headers(req.headers_mut(), TS_ORIGIN_STRIPPED_COOKIE_NAMES);
```

Add `use crate::cookies::{TS_ORIGIN_STRIPPED_COOKIE_NAMES, strip_cookie_headers};` to the imports at the top of `publisher.rs`.

- [ ] **Step 4: Run and see it pass, then run the other adapters**

Run: `cargo test-fastly -- publisher::tests` then `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS. The strip lives in core, so every adapter gets it (spec 11). If an adapter test asserted that `ts-eids` reaches the origin, update it and note the behaviour change in the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit --signoff -S -m "Strip the mirror cookie from publisher origin requests" -m "The origin keeps ts-ec, ts-eids and every unrelated cookie, and never receives __Host-ts-eids-s. [ec] strip_eids_cookie_from_origin strips ts-eids too, for origins that reject large Cookie headers."
```

### Task 4: Refuse the mirror cookie as a template-cache key, and document the strips

**Files:**

- Modify: `crates/trusted-server-core/src/cookies/template_cache_policy.rs` (around line 35)
- Modify: `docs/guide/edge-cookies.md`, `CHANGELOG.md`
- Test: `crates/trusted-server-core/src/cookies/template_cache_policy.rs` (`mod tests`, line 172)

- [ ] **Step 1: Write the failing tests**

Find the test that rejects `ts-eids` as a key (`grep -n "identity cookie" crates/trusted-server-core/src/cookies/template_cache_policy.rs`). It shows how the validating function is called. Add two cases next to it with the same call shape:

```rust
    #[test]
    fn template_cache_key_rejects_the_mirror_cookie() {
        let error = validate_names_for_test(&["__Host-ts-eids-s".to_owned()], &[])
            .expect_err("should reject the mirror cookie as a cache key");
        assert!(
            error.contains("must not key on Trusted Server identity cookie"),
            "should explain the rejection, got {error}"
        );
        validate_names_for_test(&["session".to_owned()], &[])
            .expect("should still accept an unrelated cookie");
    }
```

`validate_names_for_test` stands for the call the existing identity-cookie test makes. Use that exact call, not a new helper.

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- template_cache_policy::tests::template_cache_key_rejects_the_mirror`
Expected: FAIL, because the name is accepted today.

- [ ] **Step 3: Implement**

Replace the condition at line 35:

```rust
            if field == "template_cache_key_cookies"
                && [COOKIE_TS_EC, COOKIE_TS_EIDS, COOKIE_TS_EIDS_S, COOKIE_SHAREDID].contains(&name.as_str())
            {
```

and extend the `crate::constants` import with `COOKIE_TS_EIDS_S`.

- [ ] **Step 4: Run and see it pass**

Run: `cargo test-fastly -- template_cache_policy`
Expected: PASS.

- [ ] **Step 5: Docs**

In `docs/guide/edge-cookies.md`, in the section that lists TS cookies, add a short "Cookies forwarded to the origin" paragraph. Say that `ts-ec` and `ts-eids` still reach the publisher origin, that `__Host-ts-eids-s` does not, and that `[ec] strip_eids_cookie_from_origin = true` strips `ts-eids` too. Also say that Prebid Server never receives any TS identity cookie.

In `CHANGELOG.md` under `## [Unreleased]` / `### Changed`, add:

```markdown
- **Breaking:** Prebid Server no longer receives any Trusted Server identity cookie (`ts-ec`, `ts-eids`, and the `__Host-ts-eids-s` mirror that this release's shim writes); today it receives `ts-ec` and `ts-eids`. The publisher origin still receives `ts-ec` and `ts-eids` but never the mirror. `[ec] strip_eids_cookie_from_origin = true` strips `ts-eids` from origin requests too.
```

Run: `cd docs && npm run format && cd ..` and `docs/node_modules/.bin/prettier --config docs/.prettierrc --write CHANGELOG.md`

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/cookies/template_cache_policy.rs docs/guide/edge-cookies.md CHANGELOG.md
git commit --signoff -S -m "Refuse the identity mirror cookie as a template cache key"
```

### Task 5: The `ec::identity` module, its ID newtypes and its error type

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/mod.rs`
- Create: `crates/trusted-server-core/src/ec/identity/ids.rs`
- Create: `crates/trusted-server-core/src/ec/identity/resolution.rs` (error type only in this task, extended in Task 10)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (module list, around line 32)
- Modify: `crates/trusted-server-core/src/migration_guards.rs` (`checked_sources`)

**Interfaces:**

- Produces:
  - `IdentityProviderId`: `FromStr<Err = Report<IdentityProviderError>>`, `as_str(&self) -> &str`, serde as a string. Derives `Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display`.
  - `SourceDomain`: `FromStr` through `normalize_partner_source_domain`, `as_str`, `matches(&self, name: &str) -> bool`, serde as a string. Same derives.
  - `UnixTime`: `from_secs(u64)`, `from_millis(u64)`, `as_secs(self) -> u64`, `Add<Duration>`. Derives `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`.
  - `IdentityProviderError` (all seven variants from spec 4.5) with `config`, `rejected`, `malformed` and `not_implemented` constructors. `Access` and `Input` are referenced by two variants, so this task adds a minimal `descriptor.rs` with just those two enums. Task 8 fills in the rest of the file.

- [ ] **Step 1: Write the failing tests**

Create `ec/identity/ids.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_accepts_the_documented_grammar() {
        for id in ["id5", "pull-ssp-example-com", "lockr_aim", "a"] {
            assert!(id.parse::<IdentityProviderId>().is_ok(), "should accept `{id}`");
        }
        let longest = format!("a{}", "b".repeat(62));
        longest.parse::<IdentityProviderId>().expect("should accept 63 characters");
    }

    #[test]
    fn provider_id_rejects_everything_else() {
        let too_long = format!("a{}", "b".repeat(63));
        for id in ["", "1id", "Id5", "id 5", "id.5", too_long.as_str()] {
            let error = id.parse::<IdentityProviderId>().expect_err("should reject the id");
            assert!(
                matches!(error.current_context(), IdentityProviderError::Config { .. }),
                "should be a Config error for `{id}`"
            );
        }
    }

    #[test]
    fn source_domain_normalizes_and_matches() {
        let source: SourceDomain = "ID5-Sync.com.".parse().expect("should parse");
        assert_eq!(source.as_str(), "id5-sync.com", "should lower-case and drop the trailing dot");
        assert!(source.matches("id5-sync.COM"), "should match another spelling");
        assert!(!source.matches("example.com"), "should not match another domain");
        assert!(!source.matches("https://id5-sync.com"), "should not match a URL");
    }

    #[test]
    fn source_domain_round_trips_through_serde() {
        let source: SourceDomain = "example.com".parse().expect("should parse");
        let json = serde_json::to_string(&source).expect("should serialize");
        assert_eq!(json, "\"example.com\"", "should serialize as a bare string");
        let back: SourceDomain = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(back, source, "should round-trip");
        serde_json::from_str::<SourceDomain>("\"not a domain\"")
            .expect_err("should reject an invalid domain on deserialize");
    }

    #[test]
    fn unix_time_converts_millis_and_adds_durations() {
        assert_eq!(UnixTime::from_millis(1_760_000_000_999).as_secs(), 1_760_000_000, "should floor millis");
        let later = UnixTime::from_secs(100) + Duration::from_secs(50);
        assert_eq!(later, UnixTime::from_secs(150), "should add seconds");
        let saturated = UnixTime::from_secs(u64::MAX) + Duration::from_secs(1);
        assert_eq!(saturated, UnixTime::from_secs(u64::MAX), "should saturate");
    }
}
```

Create `ec/identity/mod.rs`:

```rust
//! Server-side identity providers.
//!
//! An [`IdentityProvider`](provider::IdentityProvider) turns request signals
//! into `OpenRTB` EIDs. Providers only build requests and parse buffered
//! responses. The coordinator owns eligibility, consent, transport limits and
//! KV write-back, so every page load makes at most one browser-driven KV
//! write. See `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`.

pub mod descriptor;
pub mod ids;
pub mod resolution;

pub use descriptor::{Access, Input};
pub use ids::{IdentityProviderId, SourceDomain, UnixTime};
pub use resolution::IdentityProviderError;
```

Add `pub mod identity;` to the module list in `ec/mod.rs`, in alphabetical order after `pub mod identify;`.

- [ ] **Step 2: Run and see the tests fail**

Run: `cargo test-fastly -- ec::identity::ids`
Expected: compile errors, because the types do not exist.

- [ ] **Step 3: Implement `resolution.rs` (error only) and the minimal `descriptor.rs`**

`ec/identity/descriptor.rs` for now:

```rust
//! What a provider is, may write, needs and must respect.

/// An input a provider can ask for.
///
/// Consent strings, the provider's stored IDs, the publisher domain, the
/// time and the declared endpoint are always available. Private state is
/// available to providers declared `with_private_state()`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display,
)]
#[non_exhaustive]
pub enum Input {
    /// The platform's client address.
    #[display("client_ip")]
    ClientIp,
    /// The request's `User-Agent`.
    #[display("user_agent")]
    UserAgent,
    /// The top-level page URL without query string or fragment.
    #[display("page_url")]
    PageUrl,
    /// Country and subdivision only.
    #[display("region")]
    Region,
    /// A SHA-256 of a normalized email address. Never available in v1.
    #[display("hashed_email")]
    HashedEmail,
}

/// Something a provider read without declaring it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display,
)]
#[non_exhaustive]
pub enum Access {
    /// An undeclared [`Input`].
    #[display("{_0}")]
    Input(Input),
    /// The endpoint, when the descriptor declares no `resolves`.
    #[display("endpoint")]
    Endpoint,
    /// Private state, without `with_private_state()`.
    #[display("private_state")]
    PrivateState,
}
```

`ec/identity/resolution.rs` for now:

```rust
//! Provider results and errors.

use error_stack::Report;
use http::StatusCode;

use super::descriptor::{Access, Input};

/// Marker message of [`IdentityProviderError::not_implemented`]. The harness
/// matches on it to tell a missing override from a real failure.
pub(crate) const NOT_IMPLEMENTED_MARKER: &str = "capability declared but not implemented: ";

/// A provider failure. Built through the constructors below.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum IdentityProviderError {
    /// Configuration or provider code makes the call impossible.
    #[display("identity provider configuration error: {message}")]
    Config {
        /// What is wrong. Never contains a secret, UID or URL query.
        message: String,
    },
    /// The provider read something its descriptor did not declare.
    #[display("`{access}` was read but not declared")]
    UndeclaredInput {
        /// What was read.
        access: Access,
    },
    /// A declared input is not available for this request.
    #[display("input `{input}` is unavailable for this request")]
    InputUnavailable {
        /// The missing input.
        input: Input,
    },
    /// The provider refused the request.
    #[display("provider rejected the request with status {status}")]
    Rejected {
        /// The response status.
        status: StatusCode,
    },
    /// The provider's response could not be parsed.
    #[display("provider response could not be parsed")]
    Malformed,
    /// The stored private state does not decode as the provider's type.
    #[display("stored provider state could not be decoded")]
    InvalidState,
    /// Set by the coordinator only. Providers never see transport failures.
    #[display("provider request failed in transport")]
    Transport,
}

impl core::error::Error for IdentityProviderError {}

impl IdentityProviderError {
    /// A [`Self::Config`] error.
    #[must_use]
    pub fn config(message: impl Into<String>) -> Report<Self> {
        Report::new(Self::Config { message: message.into() })
    }

    /// A [`Self::Rejected`] error.
    #[must_use]
    pub fn rejected(status: StatusCode) -> Report<Self> {
        Report::new(Self::Rejected { status })
    }

    /// A [`Self::Malformed`] error.
    #[must_use]
    pub fn malformed() -> Report<Self> {
        Report::new(Self::Malformed)
    }

    /// The default body of a capability method: a `Config` error with a
    /// fixed marker message, so the harness can tell a missing override from
    /// a real failure.
    #[must_use]
    pub fn not_implemented(method: &'static str) -> Report<Self> {
        Self::config(format!("{NOT_IMPLEMENTED_MARKER}{method}"))
    }

    /// Whether this is the [`Self::not_implemented`] marker.
    #[must_use]
    pub(crate) fn is_not_implemented(&self) -> bool {
        matches!(self, Self::Config { message } if message.starts_with(NOT_IMPLEMENTED_MARKER))
    }
}
```

- [ ] **Step 4: Implement `ids.rs`**

Put this above the test module:

```rust
//! Identifiers shared by the identity provider API.

use core::ops::Add;
use core::str::FromStr;
use std::time::Duration;

use error_stack::Report;
use serde::{Deserialize, Serialize};

use super::resolution::IdentityProviderError;
use crate::ec::partner::normalize_partner_source_domain;

/// An identity provider's ID, such as `id5` or `pull-ssp-example-com`.
///
/// Matches `^[a-z][a-z0-9_-]{0,62}$`, so it can be a bare TOML key and can
/// equal an integration ID that uses `_`. Separate from
/// [`crate::auction::ProviderId`].
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct IdentityProviderId(String);

impl IdentityProviderId {
    /// The ID as written in config.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for IdentityProviderId {
    type Err = Report<IdentityProviderError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut bytes = value.bytes();
        let valid_first = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let valid_rest = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        });
        if valid_first && valid_rest && value.len() <= 63 {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityProviderError::config(format!(
                "identity provider id `{value}` must match ^[a-z][a-z0-9_-]{{0,62}}$"
            )))
        }
    }
}

impl TryFrom<String> for IdentityProviderId {
    type Error = Report<IdentityProviderError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<IdentityProviderId> for String {
    fn from(id: IdentityProviderId) -> Self {
        id.0
    }
}

/// An EID `source` domain, normalized. Also the key of the KV `ids` map.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct SourceDomain(String);

impl SourceDomain {
    /// The normalized domain.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a vendor's spelling of a source names this one, after
    /// normalization.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        normalize_partner_source_domain(name).is_ok_and(|normalized| normalized == self.0)
    }
}

impl FromStr for SourceDomain {
    type Err = Report<IdentityProviderError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize_partner_source_domain(value)
            .map(Self)
            .map_err(IdentityProviderError::config)
    }
}

impl TryFrom<String> for SourceDomain {
    type Error = Report<IdentityProviderError>;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SourceDomain> for String {
    fn from(source: SourceDomain) -> Self {
        source.0
    }
}

/// Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixTime(u64);

impl UnixTime {
    /// From Unix seconds.
    #[must_use]
    pub fn from_secs(secs: u64) -> Self {
        Self(secs)
    }

    /// From Unix milliseconds, for vendors such as Lockr and `UID2`. Floors to
    /// whole seconds.
    #[must_use]
    pub fn from_millis(millis: u64) -> Self {
        Self(millis / 1000)
    }

    /// As Unix seconds.
    #[must_use]
    pub fn as_secs(self) -> u64 {
        self.0
    }
}

impl Add<Duration> for UnixTime {
    type Output = UnixTime;

    fn add(self, rhs: Duration) -> UnixTime {
        UnixTime(self.0.saturating_add(rhs.as_secs()))
    }
}
```

serde's `try_from` needs the error to implement `Display`, and `Report<C>` does. Check that `normalize_partner_source_domain` rejects `"not a domain"` (spaces fail the label check). If it does not, use `"not/a/domain"` in the test.

Add both new files to `checked_sources()` in `migration_guards.rs`, in alphabetical order, following the `ec/client_eids.rs` entry:

```rust
        ("ec/identity/descriptor.rs", include_str!("ec/identity/descriptor.rs")),
        ("ec/identity/ids.rs", include_str!("ec/identity/ids.rs")),
        ("ec/identity/mod.rs", include_str!("ec/identity/mod.rs")),
        ("ec/identity/resolution.rs", include_str!("ec/identity/resolution.rs")),
```

Every later task that creates a file under `ec/identity/` adds it to this list the same way.

- [ ] **Step 5: Run and see the tests pass**

Run: `cargo test-fastly -- ec::identity` and `cargo test-fastly -- migration_guards`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/mod.rs crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add the identity module with provider and source ID newtypes"
```

### Task 6: KV schema version 2: one record shape, provider state, `Default` and constructors

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv_types.rs` (`SCHEMA_VERSION`, `KvEntry`, `KvConsent`, `KvGeo`, `KvPartnerId`, `validate`, `upgrade_for_read`, constructors, the size estimate in the `KvEntry` doc comment)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`deserialize_entry`, every production `.uid` reader, and the tests at about lines 1736 and 1758)
- Modify: every file with a `KvPartnerId {` literal or a `.uid` read of a `KvPartnerId` (find them with `grep -rn "KvPartnerId {" crates/` and the compiler)
- Modify: `crates/trusted-server-core/src/ec/admin.rs` (`build_lookup_response`, around `admin.rs:375-415`, and the `.uid` readers at about lines 490 and 616)
- Modify: `crates/trusted-server-core/src/ec/eids.rs` (the `.uid` readers at about lines 40, 53 and 71; Task 14 rewrites this file's output path)
- Test: `crates/trusted-server-core/src/ec/kv_types.rs`, `crates/trusted-server-core/src/ec/kv.rs`, `crates/trusted-server-core/src/ec/admin.rs`

**Interfaces:**

- Produces:
  - `pub const SCHEMA_VERSION: u8 = 2;`. `KvMetadata.v` mirrors it on every write.
  - `KvPartnerId { w: Option<String>, rv: Option<String>, at: Option<u64>, exp: Option<u64>, ra: Option<u64>, ru: Option<u64>, eids: Vec<KvEid> }`, deriving `Debug, Clone, Default, PartialEq, Serialize, Deserialize`, with `#[serde(from = "KvPartnerIdWire")]`. It has no `uid`, `atype` or `ext` field. `eids` is always serialized and never empty in a valid record. Each optional field is skipped when `None`.
  - `KvPartnerId::new(uid: impl Into<String>) -> Self` (one entry with one UID and no metadata), `KvPartnerId::browser(uid: impl Into<String>, acquired_at: Option<u64>) -> Self`, and `KvPartnerId::first_uid(&self) -> Option<&str>` (the first UID of the first entry)
  - `KvEid { uids: Vec<KvUid>, inserter: Option<String>, matcher: Option<String>, mm: Option<i32>, ext: Option<serde_json::Value> }` (no `source`: the `ids` key holds it) and `KvUid { id: String, atype: Option<i32>, ext: Option<serde_json::Value> }`, both deriving `Debug, Clone, Default, PartialEq, Serialize, Deserialize`, with every optional field skipped when `None`, and neither denying unknown fields
  - `KvProviderState { n: Option<u64>, f: u32, o: Option<KvOptOut>, oc: Option<u64>, iv: Vec<String>, ov: Vec<String>, st: Option<serde_json::Value> }`, deriving `Default`
  - `KvOptOut { at: u64, fp: String }`. `fp` is the consent bitmask of spec 8 as lowercase hex, at most 16 digits (Task 20 defines the bits).
  - `KvEntry.ps: BTreeMap<String, KvProviderState>`, and `KvEntry` derives `Default`
  - Constants `MAX_PARTNER_EXT_BYTES = 2048` (UID-level and EID-level `ext`), `MAX_EID_FIELD_BYTES = 255` (`inserter`, `matcher`), `MAX_ENTRIES_PER_SOURCE = 8`, `MAX_UIDS_PER_SOURCE = 32`, `MAX_ENTRY_SET_BYTES = 4096` (compact JSON of the stored set), `MAX_PROVIDER_STATE_BYTES = 1024`, `MAX_PROVIDER_STATES = 32`, `MAX_INVALIDATED_UIDS = 128` (the cap of `ps.iv` and of `ps.ov`, each an ordered list, oldest first), `RECORD_REVISION_HEX_LEN = 16`
  - `pub(crate) fn entry_set_bytes(set: &[KvEid]) -> usize`: the length of `serde_json::to_vec(set)`, the stored array without `source`, after the field rules and without provenance defaults. `KvEntry::validate` and the coordinator (Tasks 10 and 20) both measure `MAX_ENTRY_SET_BYTES` with it, so they never disagree.
  - `pub(crate) fn KvEntry::upgrade_for_read(&mut self) -> Result<Vec<String>, String>`: accepts `v` of 1 or 2 and sets it to 2, rejecting any other version as today's check does. It then drops each record that breaks a rule of `validate_record`, and returns one message per dropped record, naming the source and the rule, never a UID (spec 6.1). `deserialize_entry` calls it before `validate` and logs each message at warn. Only `serialize_entry` rejects a bad record.

Spec 6.1: every record stores its source's entry set in `eids`. A record that browser ingest, generic pull or batch sync writes is one entry with one UID and no EID-level fields. Only a provider result writes more, and nothing in Part 1 does. This task makes larger sets read, preserved and validated, so a rollback from Part 2 to Part 1 keeps every set a provider wrote readable. Such a set is not served while its writer is not configured (spec 14).

Every row on `main` is version 1, whose records hold one `uid`. The reader accepts both versions: a version 1 record reads as one entry holding that `uid`, with no `w`, no `rv` and no timing beyond `at`, and the row's next write stores version 2. Today's reader rejects every version but its own (`ec/kv_types.rs:376-381`), so a build without this task cannot read a row this task has written. Rolling back past Part 1 is unsupported (spec 14), and Task 31's changelog says so.

`validate` runs on every read (`ec/kv.rs:366`, `deserialize_entry`) as well as on every write. One bad record must not make its whole row unreadable, because nothing could then write the row again. So the read path first drops such a record, and only the write path rejects it.

- [ ] **Step 1: Write the failing tests**

Add to `kv_types.rs` tests. The test module has `use super::*;` only, so also add `use serde_json::json;`:

```rust
    fn kv_uid(id: &str, atype: Option<i32>) -> KvUid {
        KvUid {
            id: id.to_owned(),
            atype,
            ext: None,
        }
    }

    fn kv_eid(uids: Vec<KvUid>) -> KvEid {
        KvEid {
            uids,
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
        }
    }

    /// The set of spec 6.1's `example-ids.example.com` record.
    fn example_full_set() -> Vec<KvEid> {
        vec![
            KvEid {
                uids: vec![
                    kv_uid("example-a", Some(1)),
                    KvUid {
                        id: "example-b".to_owned(),
                        atype: Some(3),
                        ext: Some(json!({ "stype": "ppuid" })),
                    },
                ],
                inserter: Some("example.com".to_owned()),
                matcher: None,
                mm: Some(3),
                ext: None,
            },
            KvEid {
                uids: vec![kv_uid("example-c", Some(1))],
                inserter: None,
                matcher: Some("partner.example.com".to_owned()),
                mm: None,
                ext: None,
            },
        ]
    }

    fn with_set(set: Vec<KvEid>) -> KvEntry {
        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").eids = set;
        entry
    }

    #[test]
    fn golden_version_1_row_reads_as_one_entry_records_and_writes_version_2() {
        let main_row = json!({
            "v": 1,
            "created": 1_760_000_000,
            "consent": { "ok": true, "updated": 1_760_000_000 },
            "geo": { "country": "US" },
            "ids": {
                "id5-sync.com": { "uid": "ID5*example" },
                "sharedid.org": { "uid": "example-shared-id" }
            }
        });

        let mut entry: KvEntry = serde_json::from_value(main_row).expect("should read a row from main");
        let messages = entry.upgrade_for_read().expect("should accept version 1");
        entry.validate().expect("should be valid once upgraded");

        assert!(messages.is_empty(), "should drop nothing from a valid version 1 row");
        assert_eq!(entry.ids["id5-sync.com"], KvPartnerId::new("ID5*example"), "should read uid as one entry");
        assert_eq!(
            serde_json::to_value(&entry).expect("should write"),
            json!({
                "v": 2,
                "created": 1_760_000_000,
                "consent": { "ok": true, "updated": 1_760_000_000 },
                "geo": { "country": "US" },
                "ids": {
                    "id5-sync.com": { "eids": [{ "uids": [{ "id": "ID5*example" }] }] },
                    "sharedid.org": { "eids": [{ "uids": [{ "id": "example-shared-id" }] }] }
                }
            }),
            "should write version 2, with no uid key and no ps"
        );
    }

    #[test]
    fn version_2_row_and_provider_state_round_trip() {
        let stored = json!({
            "v": 2,
            "created": 1_760_000_000,
            "consent": { "ok": true, "updated": 1_760_000_000 },
            "geo": { "country": "US" },
            "ids": {
                "id5-sync.com": {
                    "w": "id5",
                    "rv": "9c1d04b7e6a83f2a",
                    "at": 1_760_000_000,
                    "exp": 1_767_776_000,
                    "ra": 1_760_086_400,
                    "ru": 1_770_000_000,
                    "eids": [{ "uids": [{ "id": "ID5*example", "atype": 1, "ext": { "linkType": 2 } }] }]
                },
                "example-ids.example.com": {
                    "w": "exampleid",
                    "rv": "4be71f0c2d9a6e35",
                    "at": 1_760_000_000,
                    "eids": [
                        {
                            "uids": [
                                { "id": "example-a", "atype": 1 },
                                { "id": "example-b", "atype": 3, "ext": { "stype": "ppuid" } }
                            ],
                            "inserter": "example.com",
                            "mm": 3
                        },
                        {
                            "uids": [{ "id": "example-c", "atype": 1 }],
                            "matcher": "partner.example.com"
                        }
                    ]
                }
            },
            "ps": {
                "id5": {
                    "n": 1_760_003_600,
                    "f": 1,
                    "o": { "at": 1_759_000_000, "fp": "1a" },
                    "oc": 1_759_500_000,
                    "iv": ["aaaaaaaaaaaaaaaa"],
                    "ov": ["bbbbbbbbbbbbbbbb"],
                    "st": { "signature": "example-opaque-state" }
                }
            }
        });

        let entry: KvEntry = serde_json::from_value(stored.clone()).expect("should read");
        entry.validate().expect("should be valid");
        assert_eq!(serde_json::to_value(&entry).expect("should write"), stored, "should round-trip exactly");
        assert_eq!(
            entry.ids["example-ids.example.com"].eids,
            example_full_set(),
            "should read the set whole and in order"
        );
    }

    #[test]
    fn a_record_reads_its_eids_and_its_version_1_uid_only_without_them() {
        let both: KvPartnerId = serde_json::from_value(json!({
            "uid": "stale-copy",
            "eids": [{ "uids": [{ "id": "example-a" }] }]
        }))
        .expect("should read a record with both keys");
        assert_eq!(both.first_uid(), Some("example-a"), "should take eids over a uid copy");

        let neither: KvPartnerId = serde_json::from_value(json!({ "w": "exampleid" })).expect("should read");
        assert!(neither.eids.is_empty(), "should read no entry from a record with neither key");
        assert_eq!(neither.first_uid(), None, "should have no first UID");
        with_set(Vec::new()).validate().expect_err("should reject a record with an empty eids on write");
    }

    #[test]
    fn upgrade_for_read_drops_bad_records_and_names_the_source_only() {
        let mut set = example_full_set();
        set[1].matcher = Some("  ".to_owned());
        let mut entry = with_set(set);
        entry.ids.insert("sharedid.org".to_owned(), KvPartnerId::new("example-shared-id"));
        entry.v = 1;

        let messages = entry.upgrade_for_read().expect("should accept the row");

        assert_eq!(entry.v, SCHEMA_VERSION, "should upgrade the version");
        assert_eq!(messages.len(), 1, "should report one dropped record");
        assert!(messages[0].contains("example.com"), "should name the source");
        assert!(!messages[0].contains("example-a"), "should never name a UID");
        assert!(!entry.ids.contains_key("example.com"), "should drop the bad record");
        assert_eq!(
            entry.ids["sharedid.org"],
            KvPartnerId::new("example-shared-id"),
            "should leave a valid record alone"
        );
        entry.validate().expect("should validate once upgraded");

        let mut future = KvEntry::minimal("example.com", "example-uid", 1);
        future.v = SCHEMA_VERSION + 1;
        future.upgrade_for_read().expect_err("should reject a version it does not know");
    }

    #[test]
    fn entry_set_bytes_measures_the_stored_array() {
        let set = example_full_set();
        let bytes = serde_json::to_vec(&set).expect("should serialize the set");

        assert_eq!(entry_set_bytes(&set), bytes.len(), "should measure the stored array");
        assert!(
            !String::from_utf8(bytes).expect("should be UTF-8").contains("\"source\""),
            "should measure entries without source"
        );
    }

    #[test]
    fn validate_rejects_entry_set_limits() {
        let rejects = |entry: &KvEntry, field: &str| {
            let error = entry.validate().expect_err(&format!("should reject {field}"));
            assert!(error.contains(field), "should name {field}, got {error}");
        };

        rejects(&with_set(Vec::new()), "eids");

        let mut set = example_full_set();
        set[0].ext = Some(json!({ "pad": "x".repeat(MAX_PARTNER_EXT_BYTES) }));
        rejects(&with_set(set), "ext");

        let mut set = example_full_set();
        set[0].uids[0].ext = Some(json!("not an object"));
        rejects(&with_set(set), "ext");

        let mut set = example_full_set();
        set[0].inserter = Some("  ".to_owned());
        rejects(&with_set(set), "inserter");

        let mut set = example_full_set();
        set[1].matcher = Some("m".repeat(MAX_EID_FIELD_BYTES + 1));
        rejects(&with_set(set), "matcher");

        let mut set = example_full_set();
        set[0].mm = Some(-1);
        rejects(&with_set(set), "mm");

        let mut set = example_full_set();
        set[0].uids[1].atype = Some(-1);
        rejects(&with_set(set), "atype");

        let mut set = example_full_set();
        set[1].uids[0].id = " ".to_owned();
        rejects(&with_set(set), "uid");

        let mut set = example_full_set();
        set[1].uids.clear();
        rejects(&with_set(set), "uid");

        let nine_entries = (0..=MAX_ENTRIES_PER_SOURCE)
            .map(|index| kv_eid(vec![kv_uid(&format!("example-{index}"), None)]))
            .collect();
        rejects(&with_set(nine_entries), "entries");

        let thirty_three_uids = (0..=MAX_UIDS_PER_SOURCE)
            .map(|index| kv_uid(&format!("example-{index}"), None))
            .collect();
        rejects(&with_set(vec![kv_eid(thirty_three_uids)]), "uids");

        let oversized = (0..4)
            .map(|index| KvEid {
                ext: Some(json!({ "pad": "x".repeat(1_100) })),
                ..kv_eid(vec![kv_uid(&format!("example-{index}"), None)])
            })
            .collect();
        rejects(&with_set(oversized), "entry set");

        let mut old = with_set(example_full_set());
        old.v = 1;
        rejects(&old, "version");

        with_set(example_full_set()).validate().expect("should accept the control set");
    }

    #[test]
    fn validate_rejects_oversized_state_and_too_many_providers() {
        let with_state = |state: KvProviderState| {
            let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
            entry.ps.insert("id5".to_owned(), state);
            entry
        };
        let hashes = |count: usize| vec!["a".repeat(16); count];

        let error = with_state(KvProviderState { st: Some(json!("x".repeat(MAX_PROVIDER_STATE_BYTES))), ..KvProviderState::default() })
            .validate()
            .expect_err("should reject st");
        assert!(error.contains("st"), "should name st");

        for (name, state) in [
            ("iv", KvProviderState { iv: hashes(MAX_INVALIDATED_UIDS + 1), ..KvProviderState::default() }),
            ("ov", KvProviderState { ov: hashes(MAX_INVALIDATED_UIDS + 1), ..KvProviderState::default() }),
            ("fp", KvProviderState { o: Some(KvOptOut { at: 1, fp: "XYZ".to_owned() }), ..KvProviderState::default() }),
        ] {
            let error = with_state(state).validate().expect_err(&format!("should reject {name}"));
            assert!(error.contains(name), "should name {name}, got {error}");
        }
        with_state(KvProviderState {
            iv: hashes(MAX_INVALIDATED_UIDS),
            ov: hashes(MAX_INVALIDATED_UIDS),
            o: Some(KvOptOut { at: 1, fp: "1a".to_owned() }),
            ..KvProviderState::default()
        })
        .validate()
        .expect("should accept 128 hashes in each list and a hex fingerprint");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        for index in 0..=MAX_PROVIDER_STATES {
            entry.ps.insert(format!("p{index}"), KvProviderState::default());
        }
        assert!(entry.validate().expect_err("should reject ps size").contains("ps"), "should name ps");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").rv = Some("not-hex".to_owned());
        assert!(entry.validate().expect_err("should reject rv").contains("rv"), "should name rv");

        KvEntry::minimal("example.com", "example-uid", 1).validate().expect("should accept the control entry");
    }
```

The validation tests build `KvProviderState` with `..KvProviderState::default()` and `KvEid` with struct update syntax because they test one field at a time. They are not exhaustiveness tests. The oversized-set case keeps every `ext` under 2,048 bytes and every other limit intact, so only the 4,096-byte set limit applies.

In `kv.rs` tests, rewrite the two tests that pin today's version and UID checks:

- `deserialize_entry_rejects_invalid_legacy_values` (about line 1736) becomes `deserialize_entry_drops_an_invalid_record`: a body whose record for one source has a UID over `MAX_UID_LENGTH` deserializes, the row reads without that source, and a second, valid source is kept. A row-level fault, such as a malformed `consent`, still fails the read as a whole, as today.
- The test that sets `SCHEMA_VERSION + 1` (about line 1758) keeps its expectation: the read fails. Add a sibling, `deserialize_entry_reads_a_version_1_body`, whose body is a row from `main`.

Add to `admin.rs` tests, next to the existing raw-lookup test (`grep -n "lookup_raw\|fn .*lookup" crates/trusted-server-core/src/ec/admin.rs`). Use the same setup that test uses to put an entry in the in-memory graph and call the handler:

```rust
    #[test]
    fn admin_lookup_redacts_provider_state() {
        let mut entry = KvEntry::minimal("example.com", "example-uid", 1_760_000_000);
        entry.ps.insert(
            "id5".to_owned(),
            KvProviderState { n: Some(5), st: Some(json!({ "signature": "secret-state" })), ..KvProviderState::default() },
        );
        let body = admin_lookup_body_for_test(&entry);

        assert_eq!(body["entry"]["ps"]["id5"]["st"], json!("[redacted]"), "should redact the state");
        assert_eq!(body["entry"]["ps"]["id5"]["n"], json!(5), "should keep the rest of ps");
        assert_eq!(
            body["entry"]["ids"]["example.com"]["eids"][0]["uids"][0]["id"],
            json!("example-uid"),
            "should keep the ids"
        );
    }
```

`admin_lookup_body_for_test` stands for whatever the existing lookup tests do to get the JSON body: insert the row, call the handler and parse the body. Write it as a small helper in the test module that reuses that setup. The stored row sits under `"entry"` in the response, as the existing assertions such as `json["entry"]["ids"]["bidstream.example"]["uid"]` (`admin.rs:998`) show. Those existing assertions read `uid`; move each to `eids[0].uids[0].id`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::kv_types::tests`, `cargo test-fastly -- ec::kv::tests::deserialize_entry` and `cargo test-fastly -- ec::admin::tests::admin_lookup_redacts`
Expected: compile errors (`ps`, `KvProviderState`, `KvEid`, `KvUid`, `KvPartnerId::new`, `first_uid`, `entry_set_bytes` and `upgrade_for_read` do not exist).

- [ ] **Step 3: Implement the schema**

In `kv_types.rs`, set `SCHEMA_VERSION` to 2 and add the constants after `MAX_UID_LENGTH`:

```rust
/// Maximum serialized bytes of a stored UID-level or EID-level `ext`.
pub const MAX_PARTNER_EXT_BYTES: usize = 2048;
/// Maximum bytes of a stored `inserter` or `matcher`.
pub const MAX_EID_FIELD_BYTES: usize = 255;
/// Maximum number of EID entries stored for one source.
pub const MAX_ENTRIES_PER_SOURCE: usize = 8;
/// Maximum number of UIDs stored for one source, across its entries.
pub const MAX_UIDS_PER_SOURCE: usize = 32;
/// Maximum bytes of one source's stored entry set, as measured by
/// [`entry_set_bytes`].
pub const MAX_ENTRY_SET_BYTES: usize = 4096;
/// Maximum serialized bytes of one provider's private state (`ps.st`).
pub const MAX_PROVIDER_STATE_BYTES: usize = 1024;
/// Maximum number of providers with a `ps` entry in one row.
pub const MAX_PROVIDER_STATES: usize = 32;
/// Maximum number of UID hashes kept in each of a provider's `ps.iv` and
/// `ps.ov` lists, enough for four whole 32-UID sets.
pub const MAX_INVALIDATED_UIDS: usize = 128;
/// Length of a record revision (`rv`) in lowercase hex digits.
pub const RECORD_REVISION_HEX_LEN: usize = 16;
```

Change the size estimate in the `KvEntry` doc comment from `(max ~5KB)` to the typical and at-every-cap figures spec 6.1 gives, citing spec 6.1.

Add `Default` to the derives of `KvEntry`, `KvConsent` and `KvGeo`. Add the `ps` field at the end of `KvEntry`:

```rust
    /// Per-provider scheduling, opt-out, removal and private state, keyed by
    /// identity provider ID. Never sent to bidders.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ps: BTreeMap<String, KvProviderState>,
```

Replace `KvPartnerId`:

```rust
/// A source's record within a KV entry (spec 6.1).
///
/// Every record stores its source's entry set in `eids`. A version 1 record,
/// which holds one `uid`, reads as one entry holding that UID.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "KvPartnerIdWire")]
pub struct KvPartnerId {
    /// Writer: an identity provider ID. Absent for browser ingest and
    /// version 1 records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
    /// Record revision: 16 lowercase hex digits, random on every write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rv: Option<String>,
    /// Unix seconds when the ID was acquired or last refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// Unix seconds at or after which the ID must not be sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<u64>,
    /// Unix seconds after which the ID is due for refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<u64>,
    /// Unix seconds after which the ID can no longer be refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ru: Option<u64>,
    /// The source's entry set, in order. Never empty in a valid record.
    pub eids: Vec<KvEid>,
}

/// The stored shape of [`KvPartnerId`], which also reads version 1's `uid`.
#[derive(Deserialize)]
struct KvPartnerIdWire {
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    w: Option<String>,
    #[serde(default)]
    rv: Option<String>,
    #[serde(default)]
    at: Option<u64>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    ra: Option<u64>,
    #[serde(default)]
    ru: Option<u64>,
    #[serde(default)]
    eids: Vec<KvEid>,
}

impl From<KvPartnerIdWire> for KvPartnerId {
    fn from(wire: KvPartnerIdWire) -> Self {
        let eids = match (wire.eids.is_empty(), wire.uid) {
            (true, Some(uid)) => vec![KvEid::one_uid(uid)],
            (_, _) => wire.eids,
        };
        Self { w: wire.w, rv: wire.rv, at: wire.at, exp: wire.exp, ra: wire.ra, ru: wire.ru, eids }
    }
}

impl KvPartnerId {
    /// A record holding one entry with one UID, and no metadata.
    #[must_use]
    pub fn new(uid: impl Into<String>) -> Self {
        Self { eids: vec![KvEid::one_uid(uid.into())], ..Self::default() }
    }

    /// A browser-ingested record: no writer, with its acquisition time.
    #[must_use]
    pub fn browser(uid: impl Into<String>, acquired_at: Option<u64>) -> Self {
        Self { at: acquired_at, ..Self::new(uid) }
    }

    /// The first UID of the first entry, which `/identify` returns and
    /// today's one-UID readers use.
    #[must_use]
    pub fn first_uid(&self) -> Option<&str> {
        self.eids.first().and_then(|entry| entry.uids.first()).map(|uid| uid.id.as_str())
    }
}

/// One EID entry of a record (spec 6.1).
///
/// The `OpenRTB` EID object without `source`, which the `ids` key holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvEid {
    /// The entry's UIDs, in order. Never empty in a valid record.
    pub uids: Vec<KvUid>,
    /// `eid.inserter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserter: Option<String>,
    /// `eid.matcher`. Absent means `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// `eid.mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mm: Option<i32>,
    /// EID-level `ext`, at most [`MAX_PARTNER_EXT_BYTES`] serialized bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

impl KvEid {
    /// One entry holding one UID and nothing else: the shape browser
    /// ingest, generic pull and batch sync write.
    fn one_uid(id: String) -> Self {
        Self { uids: vec![KvUid { id, atype: None, ext: None }], ..Self::default() }
    }
}

/// One UID of a stored entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvUid {
    /// The user identifier.
    pub id: String,
    /// `uid.atype`. Absent: use the partner's `openrtb_atype`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atype: Option<i32>,
    /// UID-level `ext`, at most [`MAX_PARTNER_EXT_BYTES`] serialized bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

/// A provider's sticky opt-out (spec 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvOptOut {
    /// Unix seconds when the opt-out was reported.
    pub at: u64,
    /// The consent bits the provider's policy read then, as a lowercase hex
    /// bitmask of at most 16 digits. Plain, not keyed (spec 8).
    pub fp: String,
}

/// Per-provider state in a KV row (`ps.<provider>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvProviderState {
    /// Unix seconds before which the provider must not be called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u64>,
    /// Consecutive failures, for backoff.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub f: u32,
    /// Sticky opt-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub o: Option<KvOptOut>,
    /// When the provider's opt-out was last cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc: Option<u64>,
    /// Unkeyed hashes of UIDs the owner invalidated, oldest first, at most
    /// [`MAX_INVALIDATED_UIDS`]. Never emptied (spec 5.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iv: Vec<String>,
    /// Unkeyed hashes of UIDs the provider's opt-out removed, oldest first,
    /// at most [`MAX_INVALIDATED_UIDS`]. Emptied when the opt-out clears.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ov: Vec<String>,
    /// The provider's private state as embedded JSON. KV only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub st: Option<serde_json::Value>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Bytes of a stored entry set as compact JSON: the array of entries without
/// `source`, as `eids` stores it. Both `validate` and the coordinator measure
/// [`MAX_ENTRY_SET_BYTES`] with this function.
pub(crate) fn entry_set_bytes(set: &[KvEid]) -> usize {
    serde_json::to_vec(set).map_or(usize::MAX, |bytes| bytes.len())
}
```

`KvPartnerId`, `KvEid` and `KvUid` do not deny unknown fields, so a field a later build adds never makes this build fail to read the row (spec 6.1).

The listed constructors (`KvEntry::new`, `minimal`, `tombstone`) gain `ps: BTreeMap::new()` and write `v: SCHEMA_VERSION`, which is now 2. `minimal` uses `KvPartnerId::new(uid)`. `KvMetadata.v` takes the entry's `v`, so metadata written from now on says 2. Find every reader of the metadata version (`grep -rn "\.v\b\|SCHEMA_VERSION" crates/trusted-server-core/src/ec/`) and make each accept 1 and 2 where it reads, and write 2.

Extend `validate()`. It still rejects `v != SCHEMA_VERSION`, now 2, with a message containing `version`. Inside the existing `for (partner_id, partner_uid) in &self.ids` loop, replace the UID length check with `validate_record(partner_id, partner_uid)?;`. After the loop:

```rust
        if self.ps.len() > MAX_PROVIDER_STATES {
            return Err(format!("ps exceeds MAX_PROVIDER_STATES ({})", self.ps.len()));
        }
        for (provider, state) in &self.ps {
            for (name, hashes) in [("iv", &state.iv), ("ov", &state.ov)] {
                if hashes.len() > MAX_INVALIDATED_UIDS {
                    return Err(format!("ps '{provider}' {name} exceeds {MAX_INVALIDATED_UIDS} hashes"));
                }
            }
            if let Some(opt_out) = &state.o
                && (opt_out.fp.is_empty()
                    || opt_out.fp.len() > 16
                    || !opt_out.fp.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            {
                return Err(format!("ps '{provider}' o.fp is not a hex bitmask"));
            }
            if let Some(st) = &state.st
                && serde_json::to_vec(st).map_or(true, |bytes| bytes.len() > MAX_PROVIDER_STATE_BYTES)
            {
                return Err(format!("ps '{provider}' st exceeds {MAX_PROVIDER_STATE_BYTES} bytes"));
            }
        }
```

Add the helpers after `impl KvEntry`:

```rust
/// Checks one record against the limits of spec 4.5 and 6.1.
fn validate_record(partner_id: &str, record: &KvPartnerId) -> Result<(), String> {
    if record.eids.is_empty() {
        return Err(format!("partner ID '{partner_id}' has an empty eids"));
    }
    if let Some(rv) = &record.rv
        && (rv.len() != RECORD_REVISION_HEX_LEN
            || !rv.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(format!("partner ID '{partner_id}' rv is not {RECORD_REVISION_HEX_LEN} hex digits"));
    }
    if record.eids.len() > MAX_ENTRIES_PER_SOURCE {
        return Err(format!("partner ID '{partner_id}' has more than {MAX_ENTRIES_PER_SOURCE} entries"));
    }
    let mut uid_count = 0;
    for entry in &record.eids {
        if entry.uids.is_empty() {
            return Err(format!("partner ID '{partner_id}' has an entry without a uid"));
        }
        for (name, value) in [("inserter", &entry.inserter), ("matcher", &entry.matcher)] {
            if let Some(value) = value
                && (value.trim().is_empty() || value.len() > MAX_EID_FIELD_BYTES)
            {
                return Err(format!("partner ID '{partner_id}' {name} is blank or over {MAX_EID_FIELD_BYTES} bytes"));
            }
        }
        if entry.mm.is_some_and(|mm| mm < 0) {
            return Err(format!("partner ID '{partner_id}' mm is negative"));
        }
        validate_ext(partner_id, entry.ext.as_ref())?;
        for uid in &entry.uids {
            if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
                return Err(format!("partner ID '{partner_id}' has a blank or oversized uid"));
            }
            if uid.atype.is_some_and(|atype| atype < 0) {
                return Err(format!("partner ID '{partner_id}' atype is negative"));
            }
            validate_ext(partner_id, uid.ext.as_ref())?;
        }
        uid_count += entry.uids.len();
    }
    if uid_count > MAX_UIDS_PER_SOURCE {
        return Err(format!("partner ID '{partner_id}' has more than {MAX_UIDS_PER_SOURCE} uids"));
    }
    if entry_set_bytes(&record.eids) > MAX_ENTRY_SET_BYTES {
        return Err(format!("partner ID '{partner_id}' entry set exceeds {MAX_ENTRY_SET_BYTES} bytes"));
    }
    Ok(())
}

/// Checks a UID-level or EID-level `ext`: a JSON object of at most
/// [`MAX_PARTNER_EXT_BYTES`] bytes as compact JSON.
fn validate_ext(partner_id: &str, ext: Option<&serde_json::Value>) -> Result<(), String> {
    let Some(ext) = ext else {
        return Ok(());
    };
    let oversized = serde_json::to_vec(ext).map_or(true, |bytes| bytes.len() > MAX_PARTNER_EXT_BYTES);
    if !ext.is_object() || oversized {
        return Err(format!("partner ID '{partner_id}' ext is not an object of at most {MAX_PARTNER_EXT_BYTES} bytes"));
    }
    Ok(())
}
```

Today's UID check (`kv_types.rs:375`) decides whether a version 1 `uid` is valid. Read it before writing `validate_record`, and keep accepting every `uid` it accepts, so no row that reads on `main` loses a record here. If it accepts a blank `uid`, accept it too and say so in the commit body; the spec's non-blank rule then applies only to UIDs a provider writes (Task 10).

Add `upgrade_for_read` to `impl KvEntry`:

```rust
    /// Brings a row read from KV to the current schema (spec 6.1).
    ///
    /// Accepts schema versions 1 and 2 and sets the version to 2. Drops each
    /// record that breaks a rule of spec 4.5 or 6.1, and returns one message
    /// per dropped record, naming the source and the rule, never a UID.
    /// Called on read only, before [`Self::validate`], so one bad record
    /// never makes the row unreadable.
    ///
    /// # Errors
    ///
    /// Returns an error naming the version when the row's version is
    /// neither 1 nor 2.
    pub(crate) fn upgrade_for_read(&mut self) -> Result<Vec<String>, String> {
        if !(1..=SCHEMA_VERSION).contains(&self.v) {
            return Err(format!("unsupported schema version {}", self.v));
        }
        self.v = SCHEMA_VERSION;
        let mut messages = Vec::new();
        self.ids.retain(|partner_id, record| match validate_record(partner_id, record) {
            Ok(()) => true,
            Err(message) => {
                messages.push(message);
                false
            }
        });
        Ok(messages)
    }
```

Every `validate_record` message names the partner ID and the rule and never a UID, so it is safe to log. In `ec/kv.rs` `deserialize_entry` (`kv.rs:355`), make `entry` mutable and call `entry.upgrade_for_read()` right before `entry.validate()`. Map its `Err` to the same error today's version check gives, and log each message with `log::warn!("Dropping an invalid record on read for key '{}': {message}", log_id(ec_id))`. `serialize_entry` keeps calling only `validate`, so a bad record is never written. In `ec/admin.rs` `build_lookup_response`, call `upgrade_for_read` on the parsed `KvEntry` before its `validate()` too, so the auction view matches what the read path serves, and when it returns messages set `payload.entry_error` to `dropped on read: ` followed by the messages joined with `; `.

- [ ] **Step 4: Move every literal and `.uid` reader**

Run `grep -rn "KvPartnerId {" crates/` and change each `KvPartnerId { uid: X }` to `KvPartnerId::new(X)`. That covers about 30 sites in 8 files, including `ec/kv.rs` (`apply_cookie_partner_id_updates`, `apply_partner_id_updates` and others), the batch sync and pull sync writers, the admin tests and `kv_types.rs`. None of them copies an existing record, so none loses metadata or entries. If one does, clone the record instead of rebuilding it. Run `grep -rn "KvEntry {" crates/` too, and add `ps: BTreeMap::new()` to any full literal the compiler flags.

Then let the compiler list every read of the removed `uid` field. Each production reader moves to `first_uid()`, keeping today's meaning, because every record a Part 1 path reads that way is one entry with one UID:

- `ec/kv.rs`, about lines 147, 186, 201 and 916, and the "is it the same UID" check at about line 988, which becomes `existing.first_uid() == Some(uid)`
- `ec/admin.rs`, about lines 490 and 616
- `ec/eids.rs`, about lines 40, 53 and 71 (Task 14 replaces these with the stored set)

Test readers in `ec/kv.rs`, `auction/endpoints.rs`, `ec/finalize.rs` and `ec/pull_sync.rs` move to `first_uid()` too, or compare whole records against `KvPartnerId::new(..)`. Do not change what they assert.

- [ ] **Step 5: Redact `ps.st` in the admin lookup**

`build_lookup_response` (`ec/admin.rs:375`) already parses the body into a `JsonValue`, adds the `*_iso` companions and returns it as `payload.entry`. Redact that value in place, right after the parse and before `add_iso_timestamp_companions`:

```rust
/// Replaces every `ps.<provider>.st` with a marker. Provider state is never
/// returned by the admin lookup (spec 6.1, 9.3).
fn redact_provider_state(entry: &mut JsonValue) {
    let Some(states) = entry.get_mut("ps").and_then(JsonValue::as_object_mut) else {
        return;
    };
    for state in states.values_mut() {
        if let Some(st) = state.get_mut("st") {
            *st = JsonValue::String("[redacted]".to_owned());
        }
    }
}
```

A body that is not JSON goes into `raw_body` unchanged, as today, because the lookup exists to inspect corrupt rows. Check that `raw_body` is only set for a body that does not parse, so it can never carry `ps.st`. The stored JSON is returned as stored, so a version 1 row shows `uid` until its next write.

- [ ] **Step 6: Run and see the tests pass**

Run: `cargo test-fastly -- ec::` and then `cargo clippy-fastly`
Expected: PASS. Every existing KV, batch sync and pull sync test passes once its readers moved in Step 4, with its assertions unchanged.

- [ ] **Step 7: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Move the KV schema to version 2 with one record shape" -m "Every record stores its source's entry set in eids, with optional writer, revision and timing fields, and the row gains per-provider state. Version 1 rows read as one-entry records and are written as version 2. A build without this change cannot read a version 2 row, so rolling back past it is unsupported." -m "A record that breaks a limit is dropped on read and logged with its source only, so one bad record never makes a row unreadable; only the write path rejects it." -m "KvPartnerId and KvEntry derive Default, and literals move to constructors. The admin lookup redacts provider state."
```

### Task 7: Browser ingest records when it acquired an ID, and stores one UID

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`PartnerIdUpdate`, `apply_cookie_partner_id_updates`, `apply_partner_id_updates`)
- Modify: `crates/trusted-server-core/src/ec/prebid_eids.rs` (`collect_eid_updates`, `dedupe_partner_updates`)
- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (both `collect_eid_updates` callers), and every other `PartnerIdUpdate::new` or `collect_eid_updates` caller the compiler reports
- Test: `crates/trusted-server-core/src/ec/kv.rs`, `crates/trusted-server-core/src/ec/prebid_eids.rs`

**Interfaces:**

- Produces: `PartnerIdUpdate { partner_id, uid, acquired_at: Option<u64> }`, `PartnerIdUpdate::new(partner_id, uid)` (`acquired_at = None`), `PartnerIdUpdate::with_acquired_at(self, at: u64) -> Self`. `collect_eid_updates` gains a last parameter `now: Option<u64>`. A record that browser ingest inserts now carries `at`.

What browser ingest stores does not change (G13): one UID per registered source, as one entry with one UID and no `atype`, `ext` or EID-level field, picked by today's rule, and nothing for an unregistered source, whatever extra UIDs or EID-level fields the browser sent. Today's rule is in the `collect_eid_updates` doc comment (`ec/prebid_eids.rs:146-151`): for a repeated source, the first valid UID of its last entry wins, and the sharedId cookie is applied last, so it wins over a Prebid EID for the same partner. The golden test pins that for `ts-eids` and the sharedId cookie, the only input browser ingest keeps once Task 26 stops `/auction` and `/_ts/page-bids` from ingesting their bodies (spec G10). Until then the body input still goes through `collect_eid_updates` unchanged.

- [ ] **Step 1: Write the failing tests**

In `kv.rs` tests:

```rust
    #[test]
    fn browser_ingest_records_acquisition_time() {
        let mut entry = KvEntry::minimal("sharedid.org", "existing", 1);
        let updates = [PartnerIdUpdate::new("example.com", "new-uid").with_acquired_at(1_760_000_000)];

        let changed = apply_partner_id_updates(&mut entry, &updates);

        assert!(changed, "should report a change");
        assert_eq!(
            entry.ids["example.com"],
            KvPartnerId::browser("new-uid", Some(1_760_000_000)),
            "should store the UID with at and no writer"
        );
        assert_eq!(entry.ids["sharedid.org"], KvPartnerId::new("existing"), "should leave other sources alone");
    }
```

In `prebid_eids.rs` tests, next to `collect_eid_updates_merges_prebid_and_sharedid_without_kv` (`make_registry` and `encode_json` already exist there). Add `use crate::ec::kv::apply_partner_id_updates;` and `use crate::ec::kv_types::KvEntry;` to the test module's imports if they are not there:

```rust
    #[test]
    fn golden_browser_ingest_stores_one_uid_per_registered_source() {
        let registry = make_registry(vec![("id5", "id5-sync.com"), ("sharedid", "sharedid.org")]);
        let eids_cookie = encode_json(&json!([
            { "source": "id5-sync.com", "uids": [{ "id": "ID5_earlier_entry", "atype": 1 }] },
            {
                "source": "id5-sync.com",
                "uids": [
                    { "id": " ", "atype": 1 },
                    { "id": "ID5_first", "atype": 1, "ext": { "linkType": 2 } },
                    { "id": "ID5_second", "atype": 1 }
                ],
                "inserter": "example.com",
                "matcher": "partner.example.com",
                "mm": 3,
                "ext": { "segments": ["example-segment"] }
            },
            { "source": "sharedid.org", "uids": [{ "id": "prebid-shared-id", "atype": 1 }] },
            { "source": "unregistered.example.com", "uids": [{ "id": "other", "atype": 1 }] }
        ]));
        let updates =
            collect_eid_updates(Some(&eids_cookie), Some(" cookie-shared-id "), None, &registry, Some(1_760_000_000));
        let mut entry = KvEntry::minimal("example.com", "existing", 1);
        apply_partner_id_updates(&mut entry, &updates);
        let stored = serde_json::to_value(&entry.ids).expect("should serialize ids");

        assert_eq!(
            stored,
            json!({
                "example.com": { "eids": [{ "uids": [{ "id": "existing" }] }] },
                "id5-sync.com": { "at": 1_760_000_000, "eids": [{ "uids": [{ "id": "ID5_first" }] }] },
                "sharedid.org": { "at": 1_760_000_000, "eids": [{ "uids": [{ "id": "cookie-shared-id" }] }] }
            }),
            "should store one UID per registered source by today's rule, with no EID-level field, atype or ext"
        );
    }
```

The expected values are today's rule, not a new one. Before Step 3, check them against the code as Task 6 leaves it: call the four-argument `collect_eid_updates`, drop every `"at"` from the expectation, and the assertion must pass. If it does not, the expectation is wrong, not the code: fix the expectation to what `main` stores and record the difference in the PR description.

Update the existing `collect_eid_updates(...)` calls in `prebid_eids.rs` tests to pass `None` as the new last argument. Their expectations do not change.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::kv::tests::browser_ingest_records_acquisition_time` and `cargo test-fastly -- ec::prebid_eids::tests::golden_browser_ingest`
Expected: compile errors, because `with_acquired_at` and the `now` parameter do not exist.

- [ ] **Step 3: Implement**

Add the field and builder:

```rust
pub(crate) struct PartnerIdUpdate {
    pub(crate) partner_id: String,
    pub(crate) uid: String,
    /// Unix seconds this request saw the ID. Stored as the record's `at`.
    pub(crate) acquired_at: Option<u64>,
}

impl PartnerIdUpdate {
    pub(crate) fn new(partner_id: impl Into<String>, uid: impl Into<String>) -> Self {
        Self { partner_id: partner_id.into(), uid: uid.into(), acquired_at: None }
    }

    /// Records when this request saw the ID.
    #[must_use]
    pub(crate) fn with_acquired_at(mut self, at: u64) -> Self {
        self.acquired_at = Some(at);
        self
    }
}
```

In both `apply_*` functions, the de-duplicating map now keeps the whole update: `BTreeMap<&str, &PartnerIdUpdate>`. Insert `KvPartnerId::browser(update.uid.clone(), update.acquired_at)`. Equality checks still compare only `uid`, so an unchanged UID is not rewritten just to bump `at`.

In `prebid_eids.rs`, add the `now: Option<u64>` parameter last in `collect_eid_updates`. When it is `Some`, call `.with_acquired_at(now)` on each update before `dedupe_partner_updates`. `dedupe_partner_updates` rebuilds every update from its `partner_id` and `uid` today, which would drop `acquired_at`, so make it keep the whole update:

```rust
pub(crate) fn dedupe_partner_updates(updates: Vec<PartnerIdUpdate>) -> Vec<PartnerIdUpdate> {
    let mut latest = std::collections::BTreeMap::new();
    for update in updates {
        latest.insert(update.partner_id.clone(), update);
    }

    latest.into_values().collect()
}
```

The later update for a partner still wins, and the output is still ordered by partner ID. Pass `checked_current_timestamp()` as `now` from both callers in `finalize.rs`. Leave batch sync and pull sync without `acquired_at`. They get it through the mutation in Tasks 15 and 22.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::`
Expected: PASS, including every existing `prebid_eids` and `finalize` test unchanged apart from the extra `None` argument.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src/ec
git commit --signoff -S -m "Record when browser ingest acquired each ID" -m "Ingest still stores one UID per registered source, by today's rule: the first valid UID of a repeated source's last entry, with the sharedId cookie applied last. A golden test pins that extra UIDs and EID-level fields from the browser are not stored."
```

### Task 8: The provider descriptor

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/descriptor.rs` (Task 5 created it with `Input` and `Access`)
- Modify: `crates/trusted-server-core/src/ec/identity/mod.rs` (re-exports)
- Test: `crates/trusted-server-core/src/ec/identity/descriptor.rs`

**Interfaces:**

- Consumes: `IdentityProviderId`, `SourceDomain`, `IdentityProviderError` (Task 5)
- Produces, exactly as spec 4.3 lists them:
  - `ProviderDescriptor` and its getters: `builder`, `id`, `sources`, `primary_source`, `source_named`, `resolves`, `endpoint`, `captures`, `consent_policy`, `provenance`, `is_expiring`
  - `ProviderDescriptorBuilder`: `with_source`, `with_resolves`, `with_captures`, `with_consent_policy`, `with_expiring_ids`, `with_expiring_source`, `with_provenance`, `build`
  - `Resolves`: `new`, `with_endpoint(&str)`, `with_host(&str)`, `with_needs`, `with_needs_for`, `with_wants`, `with_kinds`, `with_private_state`, plus `pub(in crate::ec::identity) fn with_ec_id(self)` and crate-visible getters `hosts()`, `needs(kind)`, `wants()`, `kinds()`, `has_private_state()`, `gets_ec_id()`
  - `Captures`: `new`, `with_path(Method, &str)`, `with_path_prefix(&str)`, plus crate-visible `matches(&Method, &str) -> bool`, `paths()`, `prefixes()`. There is no body-consent rule: capture is gated by the request's own consent, from its cookies and headers (spec 4.7).
  - `EidProvenance`: `new`, `with_inserter`, `with_matcher`, `with_mm`, `inserter`, `matcher`, `mm`
  - `ConsentPolicy`: `strict`, `with_tcf_purposes`, `with_tcf_vendor`, `allowing_dnt`, `allowing_gpc`, `allowing_us_opt_out`, plus `pub(in crate::ec::identity) fn eid_persistence_only()` for generic pull and crate-visible getters
  - `UsOptOut { Sale, Sharing, TargetedAdvertising }`
  - `ResolveKind { Initial, Refresh }` lives here rather than in `input.rs` because `Resolves` keys on it. `input.rs` re-exports it.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;

    fn source(name: &str) -> SourceDomain {
        name.parse().expect("should parse source")
    }

    fn id(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse id")
    }

    #[test]
    fn builds_the_documented_example() {
        let descriptor = ProviderDescriptor::builder(id("exampleid"))
            .with_source(source("exampleid.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.exampleid.example.com/v1/fetch")
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl]),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(9999))
            .build()
            .expect("should build");

        assert_eq!(descriptor.id().as_str(), "exampleid", "should keep the id");
        assert_eq!(descriptor.primary_source(), &source("exampleid.example.com"), "should keep the source");
        assert_eq!(
            descriptor.endpoint().map(Url::as_str),
            Some("https://api.exampleid.example.com/v1/fetch"),
            "should parse the endpoint"
        );
        let resolves = descriptor.resolves().expect("should resolve");
        assert_eq!(
            resolves.hosts(),
            &["api.exampleid.example.com".to_owned()],
            "should allow the endpoint host"
        );
        assert_eq!(
            resolves.kinds(),
            &[ResolveKind::Initial, ResolveKind::Refresh],
            "should default to Initial and Refresh"
        );
        assert!(descriptor.captures().is_none(), "should not capture");
        assert!(!descriptor.is_expiring(&source("exampleid.example.com")), "should not expire by default");
        assert_eq!(descriptor.consent_policy().tcf_vendor(), Some(9999), "should keep the vendor");
        assert_eq!(
            descriptor.consent_policy().tcf_purposes().iter().copied().collect::<Vec<_>>(),
            vec![1, 4],
            "should require Purposes 1 and 4 under strict"
        );
    }

    #[test]
    fn build_rejects_invalid_descriptors() {
        let cases: Vec<(&str, ProviderDescriptorBuilder)> = vec![
            ("no source", ProviderDescriptor::builder(id("p")).with_resolves(Resolves::new().with_endpoint("https://a.example.com"))),
            ("no capability", ProviderDescriptor::builder(id("p")).with_source(source("example.com"))),
            ("no endpoint or host", ProviderDescriptor::builder(id("p")).with_source(source("example.com")).with_resolves(Resolves::new())),
            ("http endpoint", ProviderDescriptor::builder(id("p")).with_source(source("example.com")).with_resolves(Resolves::new().with_endpoint("http://a.example.com"))),
            ("bad host", ProviderDescriptor::builder(id("p")).with_source(source("example.com")).with_resolves(Resolves::new().with_host("a.example.com/path"))),
            (
                "provenance for undeclared source",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(&source("other.example.com"), EidProvenance::new().with_mm(2)),
            ),
            (
                "expiring undeclared source",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_expiring_source(&source("other.example.com")),
            ),
            (
                "blank provenance inserter",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(&source("example.com"), EidProvenance::new().with_inserter("  ")),
            ),
            (
                "blank provenance matcher",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(&source("example.com"), EidProvenance::new().with_matcher("")),
            ),
            (
                "oversized provenance matcher",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(
                        &source("example.com"),
                        EidProvenance::new().with_matcher(&"m".repeat(MAX_EID_FIELD_BYTES + 1)),
                    ),
            ),
            (
                "negative provenance mm",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(&source("example.com"), EidProvenance::new().with_mm(-1)),
            ),
        ];
        for (name, builder) in cases {
            let error = builder
                .build()
                .expect_err(&format!("should reject a descriptor with {name}"));
            assert!(
                matches!(error.current_context(), IdentityProviderError::Config { .. }),
                "should be a Config error: {name}"
            );
        }
    }

    #[test]
    fn expiring_and_provenance_are_per_source() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(a.clone())
            .with_source(b.clone())
            .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
            .with_expiring_source(&b)
            .with_provenance(&a, EidProvenance::new().with_inserter("example-inserter").with_mm(2))
            .build()
            .expect("should build");

        assert!(!descriptor.is_expiring(&a), "should not mark a as expiring");
        assert!(descriptor.is_expiring(&b), "should mark b as expiring");
        let provenance = descriptor.provenance(&a).expect("should have provenance for a");
        assert_eq!(provenance.inserter(), Some("example-inserter"), "should keep the inserter");
        assert_eq!(provenance.matcher(), None, "should leave the matcher unset");
        assert_eq!(provenance.mm(), Some(2), "should keep mm");
        assert!(descriptor.provenance(&b).is_none(), "should have no provenance for b");
        assert_eq!(descriptor.source_named("A.EXAMPLE.COM"), Some(&a), "should find a by any spelling");
    }

    #[test]
    fn expiring_ids_marks_every_source() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("a.example.com"))
            .with_source(source("b.example.com"))
            .with_expiring_ids()
            .with_captures(Captures::new().with_path_prefix("/integrations/p/"))
            .build()
            .expect("should build");
        assert!(descriptor.sources().iter().all(|s| descriptor.is_expiring(s)), "should mark every source");
    }

    #[test]
    fn captures_match_exact_paths_and_prefixes() {
        let captures = Captures::new()
            .with_path(Method::POST, "/integrations/p/api/token")
            .with_path_prefix("/integrations/p/stream/");
        assert!(captures.matches(&Method::POST, "/integrations/p/api/token"), "should match the exact path");
        assert!(!captures.matches(&Method::GET, "/integrations/p/api/token"), "should require the method");
        assert!(
            !captures.matches(&Method::POST, "/integrations/p/api/token/x"),
            "should not treat an exact path as a prefix"
        );
        assert!(captures.matches(&Method::GET, "/integrations/p/stream/1"), "should match a prefix for any method");
        assert!(!captures.matches(&Method::GET, "/integrations/p/settings"), "should not match others");
    }

    #[test]
    fn needs_differ_by_kind() {
        let resolves = Resolves::new()
            .with_endpoint("https://a.example.com")
            .with_needs([Input::ClientIp])
            .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]);
        assert_eq!(
            resolves.needs(ResolveKind::Initial),
            [Input::ClientIp, Input::HashedEmail].into_iter().collect(),
            "should need both inputs for Initial"
        );
        assert_eq!(
            resolves.needs(ResolveKind::Refresh),
            [Input::ClientIp].into_iter().collect(),
            "should need only the IP for Refresh"
        );
    }

    #[test]
    fn valid_provenance_builds() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("example.com"))
            .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
            .with_provenance(
                &source("example.com"),
                EidProvenance::new().with_inserter("example-inserter").with_matcher("example-matcher").with_mm(0),
            )
            .build()
            .expect("should accept non-blank provenance and mm 0");
        assert!(
            descriptor.provenance(&source("example.com")).is_some(),
            "should keep the provenance"
        );
    }
}
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::descriptor`
Expected: compile errors.

- [ ] **Step 3: Implement**

Add above the existing enums in `descriptor.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use http::Method;
use url::Url;

use super::ids::{IdentityProviderId, SourceDomain};
use super::resolution::IdentityProviderError;
use crate::ec::kv_types::MAX_EID_FIELD_BYTES;
use crate::ec::partner::normalize_partner_source_domain;

/// What a provider is, may write, needs and must respect.
///
/// Built from config with no secrets, so `ts config validate` and the running
/// coordinator read the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    id: IdentityProviderId,
    sources: Vec<SourceDomain>,
    resolves: Option<Resolves>,
    endpoint: Option<Url>,
    captures: Option<Captures>,
    consent_policy: ConsentPolicy,
    provenance: BTreeMap<SourceDomain, EidProvenance>,
    expiring: BTreeSet<SourceDomain>,
}

impl ProviderDescriptor {
    /// Starts a descriptor for `id`.
    #[must_use]
    pub fn builder(id: IdentityProviderId) -> ProviderDescriptorBuilder {
        ProviderDescriptorBuilder {
            id,
            sources: Vec::new(),
            resolves: None,
            captures: None,
            consent_policy: ConsentPolicy::strict(),
            provenance: BTreeMap::new(),
            expiring_all: false,
            expiring: BTreeSet::new(),
        }
    }

    /// The provider's ID.
    #[must_use]
    pub fn id(&self) -> &IdentityProviderId {
        &self.id
    }

    /// The sources this provider may write, in declaration order.
    #[must_use]
    pub fn sources(&self) -> &[SourceDomain] {
        &self.sources
    }

    /// The first source added. Every built descriptor has one.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        &self.sources[0]
    }

    /// Finds a declared source by a vendor's spelling of it, normalized.
    #[must_use]
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain> {
        self.sources.iter().find(|source| source.matches(name))
    }

    /// Active resolution, when declared.
    #[must_use]
    pub fn resolves(&self) -> Option<&Resolves> {
        self.resolves.as_ref()
    }

    /// The endpoint declared with [`Resolves::with_endpoint`], if any.
    #[must_use]
    pub fn endpoint(&self) -> Option<&Url> {
        self.endpoint.as_ref()
    }

    /// Passive capture, when declared.
    #[must_use]
    pub fn captures(&self) -> Option<&Captures> {
        self.captures.as_ref()
    }

    /// The consent this provider requires.
    #[must_use]
    pub fn consent_policy(&self) -> &ConsentPolicy {
        &self.consent_policy
    }

    /// Default EID-level fields for `source`, if declared.
    #[must_use]
    pub fn provenance(&self, source: &SourceDomain) -> Option<&EidProvenance> {
        self.provenance.get(source)
    }

    /// Whether IDs of `source` are declared as expiring.
    #[must_use]
    pub fn is_expiring(&self, source: &SourceDomain) -> bool {
        self.expiring.contains(source)
    }
}

/// Builds a [`ProviderDescriptor`].
#[derive(Debug, Clone)]
pub struct ProviderDescriptorBuilder {
    id: IdentityProviderId,
    sources: Vec<SourceDomain>,
    resolves: Option<Resolves>,
    captures: Option<Captures>,
    consent_policy: ConsentPolicy,
    provenance: BTreeMap<SourceDomain, EidProvenance>,
    expiring_all: bool,
    expiring: BTreeSet<SourceDomain>,
}

impl ProviderDescriptorBuilder {
    /// Adds a source this provider may write. Repeatable. A repeat is ignored.
    #[must_use]
    pub fn with_source(mut self, source: SourceDomain) -> Self {
        if !self.sources.contains(&source) {
            self.sources.push(source);
        }
        self
    }

    /// Declares active resolution.
    #[must_use]
    pub fn with_resolves(mut self, resolves: Resolves) -> Self {
        self.resolves = Some(resolves);
        self
    }

    /// Declares passive capture.
    #[must_use]
    pub fn with_captures(mut self, captures: Captures) -> Self {
        self.captures = Some(captures);
        self
    }

    /// Replaces the default policy, [`ConsentPolicy::strict`].
    #[must_use]
    pub fn with_consent_policy(mut self, policy: ConsentPolicy) -> Self {
        self.consent_policy = policy;
        self
    }

    /// Marks the IDs of every declared source as expiring (spec 6.1).
    #[must_use]
    pub fn with_expiring_ids(mut self) -> Self {
        self.expiring_all = true;
        self
    }

    /// Marks the IDs of one declared source as expiring.
    #[must_use]
    pub fn with_expiring_source(mut self, source: &SourceDomain) -> Self {
        self.expiring.insert(source.clone());
        self
    }

    /// Default EID-level fields for a declared source. At output, the
    /// coordinator applies them to a stored entry of that source, written by
    /// this provider, only when the entry sets none of `inserter`, `matcher`
    /// and `mm`. An entry that sets any of them is the vendor's complete
    /// statement, so vendor and config provenance never mix in one entry.
    /// The defaults are not stored, so a config change applies at once.
    #[must_use]
    pub fn with_provenance(mut self, source: &SourceDomain, provenance: EidProvenance) -> Self {
        self.provenance.insert(source.clone(), provenance);
        self
    }

    /// Validates and builds the descriptor.
    ///
    /// # Errors
    ///
    /// Returns `Config` when the descriptor has no source, no capability, a
    /// resolver with no endpoint or host, an endpoint that is not an `https`
    /// URL, a host that is not a bare hostname, provenance or an expiring
    /// mark for a source it does not declare, a provenance `inserter` or
    /// `matcher` that is blank or over 255 bytes, or a provenance `mm` below 0.
    pub fn build(self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let id = self.id;
        let fail = |what: &str| IdentityProviderError::config(format!("provider `{id}`: {what}"));

        if self.sources.is_empty() {
            return Err(fail("declares no source"));
        }
        if self.resolves.is_none() && self.captures.is_none() {
            return Err(fail("declares neither resolves nor captures"));
        }
        for source in self.provenance.keys().chain(self.expiring.iter()) {
            if !self.sources.contains(source) {
                return Err(fail(&format!("names undeclared source `{source}`")));
            }
        }
        for (source, provenance) in &self.provenance {
            let fields = [("inserter", provenance.inserter()), ("matcher", provenance.matcher())];
            for (name, value) in fields {
                if let Some(value) = value
                    && (value.trim().is_empty() || value.len() > MAX_EID_FIELD_BYTES)
                {
                    return Err(fail(&format!(
                        "provenance `{name}` for `{source}` is blank or over {MAX_EID_FIELD_BYTES} bytes"
                    )));
                }
            }
            if provenance.mm().is_some_and(|mm| mm < 0) {
                return Err(fail(&format!("provenance `mm` for `{source}` is below 0")));
            }
        }

        let mut endpoint = None;
        let mut resolves = self.resolves;
        if let Some(resolves) = resolves.as_mut() {
            if let Some(raw) = resolves.raw_endpoint.as_deref() {
                let url = Url::parse(raw).map_err(|_| fail("endpoint is not a URL"))?;
                if url.scheme() != "https" {
                    return Err(fail("endpoint must use https"));
                }
                let host = url.host_str().ok_or_else(|| fail("endpoint has no host"))?.to_owned();
                if !resolves.hosts.contains(&host) {
                    resolves.hosts.insert(0, host);
                }
                endpoint = Some(url);
            }
            if resolves.hosts.is_empty() {
                return Err(fail("resolves declares no endpoint or host"));
            }
            for host in &resolves.hosts {
                if normalize_partner_source_domain(host).ok().as_deref() != Some(host.as_str()) {
                    return Err(fail(&format!("host `{host}` is not a bare lowercase hostname")));
                }
            }
        }

        let expiring = if self.expiring_all {
            self.sources.iter().cloned().collect()
        } else {
            self.expiring
        };

        Ok(ProviderDescriptor {
            id,
            sources: self.sources,
            resolves,
            endpoint,
            captures: self.captures,
            consent_policy: self.consent_policy,
            provenance: self.provenance,
            expiring,
        })
    }
}

/// The kind of resolution one source needs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display,
)]
#[non_exhaustive]
pub enum ResolveKind {
    /// No stored record for the source.
    #[display("initial")]
    Initial,
    /// A stored record exists, valid or expired, and is due.
    #[display("refresh")]
    Refresh,
}

/// Active resolution: the provider calls an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolves {
    raw_endpoint: Option<String>,
    hosts: Vec<String>,
    needs: BTreeSet<Input>,
    needs_for: BTreeMap<ResolveKind, BTreeSet<Input>>,
    wants: BTreeSet<Input>,
    kinds: Vec<ResolveKind>,
    private_state: bool,
    ec_id: bool,
}

impl Default for Resolves {
    fn default() -> Self {
        Self::new()
    }
}

impl Resolves {
    /// A resolver with no inputs, handling `Initial` and `Refresh`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            raw_endpoint: None,
            hosts: Vec::new(),
            needs: BTreeSet::new(),
            needs_for: BTreeMap::new(),
            wants: BTreeSet::new(),
            kinds: vec![ResolveKind::Initial, ResolveKind::Refresh],
            private_state: false,
            ec_id: false,
        }
    }

    /// The endpoint the provider calls, as config spells it. `build()` parses
    /// it and requires `https`. Its host becomes an allowed host.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.raw_endpoint = Some(endpoint.to_owned());
        self
    }

    /// Adds another host the provider may call. Repeatable.
    #[must_use]
    pub fn with_host(mut self, host: &str) -> Self {
        if !self.hosts.iter().any(|known| known == host) {
            self.hosts.push(host.to_owned());
        }
        self
    }

    /// Inputs required for every kind.
    #[must_use]
    pub fn with_needs(mut self, inputs: impl IntoIterator<Item = Input>) -> Self {
        self.needs.extend(inputs);
        self
    }

    /// Inputs required for one kind only.
    #[must_use]
    pub fn with_needs_for(mut self, kind: ResolveKind, inputs: impl IntoIterator<Item = Input>) -> Self {
        self.needs_for.entry(kind).or_default().extend(inputs);
        self
    }

    /// Inputs passed when available, never required.
    #[must_use]
    pub fn with_wants(mut self, inputs: impl IntoIterator<Item = Input>) -> Self {
        self.wants.extend(inputs);
        self
    }

    /// The kinds this provider handles. Default: `Initial` and `Refresh`.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = ResolveKind>) -> Self {
        self.kinds = kinds.into_iter().collect();
        self
    }

    /// This provider keeps private state in KV (`ps.st`, spec 6.1). It can
    /// then read the stored state and return a state change.
    #[must_use]
    pub fn with_private_state(mut self) -> Self {
        self.private_state = true;
        self
    }

    /// Gives the provider the full EC ID. Only generic pull may use this.
    #[must_use]
    pub(in crate::ec::identity) fn with_ec_id(mut self) -> Self {
        self.ec_id = true;
        self
    }

    /// Hosts the provider may call.
    #[must_use]
    pub(crate) fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Inputs required for `kind`: the common needs plus that kind's.
    #[must_use]
    pub(crate) fn needs(&self, kind: ResolveKind) -> BTreeSet<Input> {
        let mut needs = self.needs.clone();
        if let Some(extra) = self.needs_for.get(&kind) {
            needs.extend(extra.iter().copied());
        }
        needs
    }

    /// Every input declared for any kind, needed or wanted.
    #[must_use]
    pub(crate) fn declared_inputs(&self) -> BTreeSet<Input> {
        let mut all = self.needs.clone();
        all.extend(self.wants.iter().copied());
        for extra in self.needs_for.values() {
            all.extend(extra.iter().copied());
        }
        all
    }

    /// Inputs passed when available.
    #[must_use]
    pub(crate) fn wants(&self) -> &BTreeSet<Input> {
        &self.wants
    }

    /// The kinds this provider handles.
    #[must_use]
    pub(crate) fn kinds(&self) -> &[ResolveKind] {
        &self.kinds
    }

    /// Whether the provider keeps private state.
    #[must_use]
    pub(crate) fn has_private_state(&self) -> bool {
        self.private_state
    }

    /// Whether the provider receives the full EC ID.
    #[must_use]
    pub(crate) fn gets_ec_id(&self) -> bool {
        self.ec_id
    }
}

/// Revoke-only capture from responses TS proxies for this provider: it
/// can remove IDs, opt out or clear an opt-out, and never stores a positive
/// ID (spec 4.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captures {
    paths: Vec<(Method, String)>,
    prefixes: Vec<String>,
}

impl Captures {
    /// No capture rules yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offers proxied responses to exactly this method and path. Repeatable.
    #[must_use]
    pub fn with_path(mut self, method: Method, path: &str) -> Self {
        self.paths.push((method, path.to_owned()));
        self
    }

    /// Offers proxied responses whose path starts with `prefix`, for any
    /// method. Repeatable.
    #[must_use]
    pub fn with_path_prefix(mut self, prefix: &str) -> Self {
        self.prefixes.push(prefix.to_owned());
        self
    }

    /// Whether a proxied request matches a rule.
    #[must_use]
    pub(crate) fn matches(&self, method: &Method, path: &str) -> bool {
        self.paths.iter().any(|(rule_method, rule_path)| rule_method == method && rule_path == path)
            || self.prefixes.iter().any(|prefix| path.starts_with(prefix.as_str()))
    }

    /// Exact path rules.
    #[must_use]
    pub(crate) fn paths(&self) -> &[(Method, String)] {
        &self.paths
    }

    /// Prefix rules.
    #[must_use]
    pub(crate) fn prefixes(&self) -> &[String] {
        &self.prefixes
    }
}

/// Default EID-level fields (`OpenRTB` 2.6 §3.2.27) that bidders read, such
/// as who inserted an ID and how it was matched. They apply only to an entry
/// that sets none of the three. A provider whose vendor returns these per
/// response sets them on each `ResolvedEid` instead (spec 4.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EidProvenance {
    inserter: Option<String>,
    matcher: Option<String>,
    mm: Option<i32>,
}

impl EidProvenance {
    /// No fields set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `inserter`.
    #[must_use]
    pub fn with_inserter(mut self, inserter: &str) -> Self {
        self.inserter = Some(inserter.to_owned());
        self
    }

    /// Sets `matcher`.
    #[must_use]
    pub fn with_matcher(mut self, matcher: &str) -> Self {
        self.matcher = Some(matcher.to_owned());
        self
    }

    /// Sets `mm`.
    #[must_use]
    pub fn with_mm(mut self, mm: i32) -> Self {
        self.mm = Some(mm);
        self
    }

    /// `inserter`, if set.
    #[must_use]
    pub fn inserter(&self) -> Option<&str> {
        self.inserter.as_deref()
    }

    /// `matcher`, if set.
    #[must_use]
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    /// `mm`, if set.
    #[must_use]
    pub fn mm(&self) -> Option<i32> {
        self.mm
    }
}

/// A US opt-out a policy can relax.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display,
)]
#[non_exhaustive]
pub enum UsOptOut {
    /// Sale of personal information.
    #[display("sale")]
    Sale,
    /// Sharing for cross-context behavioural advertising.
    #[display("sharing")]
    Sharing,
    /// Targeted advertising.
    #[display("targeted_advertising")]
    TargetedAdvertising,
}

/// The consent a provider requires. Evaluated by the coordinator (Task 12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPolicy {
    tcf_purposes: BTreeSet<u8>,
    tcf_vendor: Option<u16>,
    allow_dnt: bool,
    allow_gpc: bool,
    allowed_us_opt_outs: BTreeSet<UsOptOut>,
    eid_persistence_only: bool,
}

impl ConsentPolicy {
    /// The default. Blocked when any consent signal present cannot be
    /// decoded. Where TCF applies, requires Purposes 1 and 4. Blocked by
    /// every US opt-out TS decodes, by `Sec-GPC: 1` and by `DNT: 1`.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            tcf_purposes: [1, 4].into_iter().collect(),
            tcf_vendor: None,
            allow_dnt: false,
            allow_gpc: false,
            allowed_us_opt_outs: BTreeSet::new(),
            eid_persistence_only: false,
        }
    }

    /// Also requires these TCF purposes, where TCF applies.
    #[must_use]
    pub fn with_tcf_purposes(mut self, purposes: impl IntoIterator<Item = u8>) -> Self {
        self.tcf_purposes.extend(purposes);
        self
    }

    /// Requires vendor consent for this IAB Global Vendor List ID.
    #[must_use]
    pub fn with_tcf_vendor(mut self, vendor_id: u16) -> Self {
        self.tcf_vendor = Some(vendor_id);
        self
    }

    /// Does not block on `DNT: 1`. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_dnt(mut self) -> Self {
        self.allow_dnt = true;
        self
    }

    /// Does not block on GPC. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_gpc(mut self) -> Self {
        self.allow_gpc = true;
        self
    }

    /// Does not block on one US opt-out. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_us_opt_out(mut self, opt_out: UsOptOut) -> Self {
        self.allowed_us_opt_outs.insert(opt_out);
        self
    }

    /// Blocks exactly what pull sync blocks today: only the global EC and
    /// EID-persistence gates. For generic pull's parity (spec 10.4).
    #[must_use]
    pub(in crate::ec::identity) fn eid_persistence_only() -> Self {
        Self { eid_persistence_only: true, ..Self::strict() }
    }

    /// Required TCF purposes.
    #[must_use]
    pub fn tcf_purposes(&self) -> &BTreeSet<u8> {
        &self.tcf_purposes
    }

    /// Required GVL vendor, if any.
    #[must_use]
    pub fn tcf_vendor(&self) -> Option<u16> {
        self.tcf_vendor
    }

    /// Whether `DNT: 1` is allowed.
    #[must_use]
    pub fn allows_dnt(&self) -> bool {
        self.allow_dnt
    }

    /// Whether GPC is allowed.
    #[must_use]
    pub fn allows_gpc(&self) -> bool {
        self.allow_gpc
    }

    /// Whether a US opt-out is allowed.
    #[must_use]
    pub fn allows_us_opt_out(&self, opt_out: UsOptOut) -> bool {
        self.allowed_us_opt_outs.contains(&opt_out)
    }

    /// Whether this is generic pull's parity policy.
    #[must_use]
    pub(crate) fn is_eid_persistence_only(&self) -> bool {
        self.eid_persistence_only
    }
}
```

`normalize_partner_source_domain` doubles as a hostname check: it accepts exactly a bare, lowercase-normalized hostname. Comparing its output to the input also rejects uppercase.

The descriptor only holds the provenance defaults. Task 28 applies them at output to entries the provider wrote, and only to an entry that sets none of `inserter`, `matcher` and `mm` (all or nothing, spec 4.3). Nothing writes them to KV. `build()` reuses Task 6's `MAX_EID_FIELD_BYTES` (255), the same cap the coordinator applies to a `ResolvedEid`'s fields. Update `mod.rs`:

```rust
pub use descriptor::{
    Access, Captures, ConsentPolicy, EidProvenance, Input, ProviderDescriptor,
    ProviderDescriptorBuilder, ResolveKind, Resolves, UsOptOut,
};
```

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::descriptor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity
git commit --signoff -S -m "Add the identity provider descriptor and its builder"
```

### Task 9: Provider inputs

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/input.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/descriptor.rs` (`Access::EcId`), `mod.rs`, `migration_guards.rs`
- Test: `crates/trusted-server-core/src/ec/identity/input.rs`

**Interfaces:**

- Consumes: `ProviderDescriptor`, `Resolves`, `Input`, `Access`, `ResolveKind` (Task 8), `KvPartnerId`, `KvEid` and `KvUid` (Task 6), `crate::openrtb::{Eid, Uid}`, and `ConsentContext` (`consent/types.rs`)
- Produces:
  - `ProviderConsent` with `gdpr_applies`, `raw_tc_string`, `raw_gpp_string`, `gpp_section_ids`, `raw_us_privacy`, and `pub(crate) fn from_context(&ConsentContext) -> Self`
  - `Region` with `country`, `subdivision`, `is`, and `pub(crate) fn from_geo(country: &str, region: Option<&str>) -> Option<Self>`, which normalizes to upper case and strips an `XX-` prefix
  - `HashedEmail` (32 bytes) with `as_bytes`, `to_hex`, `to_base64`, a redacting `Debug`, and `pub fn from_bytes([u8; 32]) -> Self` behind `#[cfg(any(test, feature = "test-utils"))]`. It is the only constructor in v1, and Task 39's harness reuses it instead of adding a second one
  - `StoredId` with `eids`, `uid`, `atype`, `ext`, `writer`, `acquired_at`, `expires_at`, `refresh_at`, `is_expired`, and `pub(crate) fn from_record(source: &SourceDomain, record: &KvPartnerId) -> Option<Self>`, `None` for a record with an empty `eids`. `eids()` is the record's whole entry set (spec 4.4), one `Eid` per stored entry with `source` filled in; a record that browser ingest, generic pull or batch sync wrote is one entry with one UID. `uid`, `atype` and `ext` describe the first UID of the first entry, so a single-UID provider such as LiveRamp reads what it reads today
  - `ResolveSignals<'a>`: a crate-private struct holding every signal the coordinator has (IP, UA, page URL, region, HEM, consent, publisher domain, now, random seed source, EC ID, stored IDs, raw state JSON). There is no dry-run flag: phase 4 calls `request` once (spec 5.2).
  - `ResolveInput<'a>` with the spec 4.4 accessors, `pub(crate) fn new(descriptor: &'a ProviderDescriptor, kinds: BTreeMap<SourceDomain, ResolveKind>, signals: &'a ResolveSignals<'a>) -> Self`, `pub(crate) fn recorded_errors(&self) -> Vec<Access>`, `pub(crate) fn state_decode_failed(&self) -> bool`, and `pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>>`. Only the inputs of the kinds in `kinds` are available (spec 4.4): an input the descriptor declares only for a kind this call does not hold reads as `InputUnavailable`, not as an undeclared read
  - `Access::EcId`, a new variant in `descriptor.rs` (`Access` is `#[non_exhaustive]`): an EC ID read by a resolver not declared `with_ec_id()`
  - `RandomSource`: a crate-private trait `fn fill(&self, bytes: &mut [u8])`, with `ThreadRandom` for production and `pub struct SeededRandom` (an xorshift generator from a `u64` seed, `pub fn new(seed: u64)`) behind `#[cfg(any(test, feature = "test-utils"))]`. Test-support items are `pub` under that gate, as the crate's existing `test-utils` items are, so the non-test library build with `--all-features` has no dead code. Task 14's `test_support` module re-exports `SeededRandom`
  - `CaptureInput` moves to Task 10, which creates the `ProviderResponse` it holds

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::IpAddr;

    use super::*;
    use crate::ec::identity::descriptor::{ProviderDescriptor, Resolves};
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};

    fn descriptor(resolves: Resolves) -> ProviderDescriptor {
        ProviderDescriptor::builder("p".parse().expect("should parse the provider id"))
            .with_source("example.com".parse().expect("should parse the source"))
            .with_resolves(resolves)
            .build()
            .expect("should build")
    }

    fn signals() -> ResolveSignals<'static> {
        ResolveSignals {
            client_ip: Some("192.0.2.10".parse().expect("should parse ip")),
            user_agent: Some("ExampleBrowser/1.0"),
            page_url: None,
            region: Region::from_geo("us", Some("us-ca")),
            hashed_email: None,
            consent: ProviderConsent::default(),
            publisher_domain: "www.example.com",
            now: UnixTime::from_secs(1_760_000_000),
            random: &ThreadRandom,
            ec_id: Some("a".repeat(64) + ".ABC123"),
            stored: BTreeMap::new(),
            state_json: None,
        }
    }

    #[test]
    fn declared_inputs_are_available_and_wanted_missing_ones_are_unavailable() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_needs([Input::ClientIp, Input::Region])
                .with_wants([Input::PageUrl]),
        );
        let signals = signals();
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Initial), &signals);

        assert_eq!(
            input.client_ip().expect("should read the ip"),
            "192.0.2.10".parse::<IpAddr>().expect("should parse the ip"),
            "should return the client ip"
        );
        let region = input.region().expect("should read region");
        assert_eq!((region.country(), region.subdivision()), ("US", Some("CA")), "should normalize the region");
        assert!(region.is("US", "CA"), "should test country and subdivision");
        let error = input.page_url().expect_err("should be unavailable");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InputUnavailable { input: Input::PageUrl }),
            "should report a declared but missing input as InputUnavailable"
        );
        assert!(input.recorded_errors().is_empty(), "should record nothing for declared reads");
    }

    #[test]
    fn input_declared_only_for_an_absent_kind_is_unavailable() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_needs([Input::ClientIp])
                .with_needs_for(ResolveKind::Refresh, [Input::UserAgent]),
        );
        let signals = signals();

        let initial = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Initial), &signals);
        let refresh = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &signals);

        let error = initial.user_agent().expect_err("should hide a Refresh-only input on an Initial call");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InputUnavailable { input: Input::UserAgent }),
            "should report InputUnavailable, not an undeclared read"
        );
        assert!(initial.recorded_errors().is_empty(), "should record no undeclared read");
        assert_eq!(
            refresh.user_agent().expect("should read the user agent on a Refresh call"),
            "ExampleBrowser/1.0",
            "should return the user agent for the kind that declares it"
        );
    }

    #[test]
    fn undeclared_reads_are_recorded_even_when_ignored() {
        let descriptor = descriptor(Resolves::new().with_endpoint("https://a.example.com"));
        let signals = signals();
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Initial), &signals);

        let ignored = input.user_agent().ok();
        let state = input.state::<serde_json::Value>();

        assert!(ignored.is_none(), "should not return an undeclared input");
        assert!(state.is_err(), "should need with_private_state for state");
        assert_eq!(
            input.recorded_errors(),
            vec![Access::Input(Input::UserAgent), Access::PrivateState],
            "should record both undeclared reads"
        );
        input.endpoint().expect("should always return the declared endpoint");
    }

    #[test]
    fn state_decodes_and_a_bad_state_is_recorded() {
        let descriptor = descriptor(Resolves::new().with_endpoint("https://a.example.com").with_private_state());
        let mut good = signals();
        good.state_json = Some(r#"{"signature":"s"}"#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &good);
        let state: serde_json::Value = input
            .state()
            .expect("should decode the state")
            .expect("should have a state");
        assert_eq!(state["signature"], "s", "should decode the state");
        assert!(!input.state_decode_failed(), "should not record a good state as a failure");

        let mut bad = signals();
        bad.state_json = Some(r#""a bare string""#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &bad);
        let error = input.state::<TypedState>().expect_err("should not decode a bare string");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InvalidState),
            "should report InvalidState"
        );
        assert!(input.state_decode_failed(), "should record the failure");
    }

    #[test]
    fn ec_id_is_only_for_providers_declared_with_ec_id() {
        let generic = descriptor(Resolves::new().with_host("a.example.com").with_ec_id());
        let other = descriptor(Resolves::new().with_host("a.example.com"));
        let signals = signals();
        ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &signals)
            .ec_id()
            .expect("should let generic pull read the EC ID");
        let input = ResolveInput::new(&other, kinds(&other, ResolveKind::Initial), &signals);
        input.ec_id().expect_err("should refuse the EC ID to other providers");
        assert_eq!(input.recorded_errors(), vec![Access::EcId], "should record an EC ID read");

        let mut no_ec = signals();
        no_ec.ec_id = None;
        let error = ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &no_ec)
            .ec_id()
            .expect_err("should fail without an EC ID");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Config { .. }),
            "should report a missing EC ID as Config, not as a missing input"
        );
    }

    #[test]
    fn stored_ids_and_expiry() {
        let record = KvPartnerId {
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: Some(20),
            ra: Some(15),
            ru: None,
            eids: vec![KvEid {
                uids: vec![KvUid { id: "u".to_owned(), atype: Some(3), ext: None }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            }],
        };
        let source: SourceDomain = "example.com".parse().expect("should parse the source");
        let stored = StoredId::from_record(&source, &record).expect("should read a record with an entry");
        assert_eq!(stored.writer().map(IdentityProviderId::as_str), Some("p"), "should parse the writer");
        assert!(!stored.is_expired(UnixTime::from_secs(19)), "should not be expired before exp");
        assert!(stored.is_expired(UnixTime::from_secs(20)), "should be expired at exp");
        assert_eq!(stored.refresh_at(), Some(UnixTime::from_secs(15)), "should read ra");
        assert_eq!(stored.eids().len(), 1, "should expose a one-UID record as one entry");
        let eid = &stored.eids()[0];
        assert_eq!(eid.source, "example.com", "should fill in the source");
        assert_eq!(eid.uids.len(), 1, "should expose its one UID");
        assert_eq!(eid.uids[0].id, "u", "should keep the UID");
        assert_eq!(eid.uids[0].atype, Some(3), "should keep the atype");
        assert_eq!(
            (eid.inserter.as_deref(), eid.matcher.as_deref(), eid.mm, eid.ext.as_ref()),
            (None, None, None, None),
            "should expose no EID-level fields when none are stored"
        );
        assert_eq!((stored.uid(), stored.atype()), ("u", Some(3)), "should describe the first UID");

        let empty = KvPartnerId { eids: Vec::new(), ..record };
        assert!(StoredId::from_record(&source, &empty).is_none(), "should not read a record with no entry");
    }

    #[test]
    fn a_full_record_exposes_every_entry_and_its_first_uid() {
        let record = KvPartnerId {
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: None,
            ra: None,
            ru: None,
            eids: vec![
                KvEid {
                    uids: vec![
                        KvUid { id: "first".to_owned(), atype: Some(1), ext: Some(serde_json::json!({ "rtiPartner": "TDID" })) },
                        KvUid { id: "second".to_owned(), atype: Some(3), ext: None },
                    ],
                    inserter: Some("inserter.example.com".to_owned()),
                    matcher: None,
                    mm: Some(2),
                    ext: Some(serde_json::json!({ "segments": ["a"] })),
                },
                KvEid {
                    uids: vec![KvUid { id: "third".to_owned(), atype: None, ext: None }],
                    inserter: None,
                    matcher: Some("matcher.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
        };
        let source: SourceDomain = "example.com".parse().expect("should parse the source");

        let stored = StoredId::from_record(&source, &record).expect("should read the record");

        let ids: Vec<Vec<&str>> = stored
            .eids()
            .iter()
            .map(|eid| eid.uids.iter().map(|uid| uid.id.as_str()).collect())
            .collect();
        assert_eq!(ids, vec![vec!["first", "second"], vec!["third"]], "should keep every entry in stored order");
        assert!(stored.eids().iter().all(|eid| eid.source == "example.com"), "should fill in the source");
        assert_eq!(stored.eids()[0].inserter.as_deref(), Some("inserter.example.com"), "should keep the inserter");
        assert_eq!(stored.eids()[0].mm, Some(2), "should keep mm");
        assert_eq!(
            stored.eids()[0].ext,
            Some(serde_json::json!({ "segments": ["a"] })),
            "should keep the EID-level ext"
        );
        assert_eq!(stored.eids()[1].matcher.as_deref(), Some("matcher.example.com"), "should keep the matcher");
        assert_eq!(stored.uid(), "first", "should return the first UID of the first entry");
        assert_eq!(stored.atype(), Some(1), "should return that UID's atype");
        assert_eq!(
            stored.ext(),
            Some(&serde_json::json!({ "rtiPartner": "TDID" })),
            "should return that UID's ext"
        );
        assert!(!format!("{stored:?}").contains("second"), "should redact every UID in Debug");
    }

    #[test]
    fn hashed_email_debug_is_redacted() {
        let email = HashedEmail::from_bytes([0xab; 32]);
        assert_eq!(email.to_hex(), "ab".repeat(32), "should encode lowercase hex");
        assert_eq!(format!("{email:?}"), "HashedEmail([redacted])", "should redact Debug");
    }

    fn kinds(descriptor: &ProviderDescriptor, kind: ResolveKind) -> BTreeMap<SourceDomain, ResolveKind> {
        descriptor.sources().iter().map(|source| (source.clone(), kind)).collect()
    }

    #[derive(serde::Deserialize)]
    struct TypedState {
        #[allow(dead_code)]
        signature: String,
    }
}
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::input`
Expected: compile errors.

- [ ] **Step 3: Implement `input.rs`**

```rust
//! What a provider may read for one call.

use core::cell::RefCell;
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use rand::RngCore as _;
use serde::de::DeserializeOwned;
use url::Url;

use super::descriptor::{Access, Input, ProviderDescriptor, ResolveKind};
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::resolution::IdentityProviderError;
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;
use crate::openrtb::{Eid, Uid};

/// The raw consent strings a provider forwards, and `gdpr_applies`.
///
/// Providers never decide consent. The coordinator has already applied the
/// descriptor's policy.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProviderConsent {
    gdpr_applies: bool,
    tc_string: Option<String>,
    gpp_string: Option<String>,
    gpp_section_ids: Option<Vec<u16>>,
    us_privacy: Option<String>,
}

impl fmt::Debug for ProviderConsent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConsent")
            .field("gdpr_applies", &self.gdpr_applies)
            .field("tc_string", &self.tc_string.as_ref().map(|_| "[redacted]"))
            .field("gpp_string", &self.gpp_string.as_ref().map(|_| "[redacted]"))
            .field("gpp_section_ids", &self.gpp_section_ids)
            .field("us_privacy", &self.us_privacy.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl ProviderConsent {
    /// The view of a request's consent context.
    #[must_use]
    pub(crate) fn from_context(context: &ConsentContext) -> Self {
        Self {
            gdpr_applies: context.gdpr_applies,
            tc_string: context.raw_tc_string.clone(),
            gpp_string: context.raw_gpp_string.clone(),
            gpp_section_ids: context.gpp_section_ids.clone(),
            us_privacy: context.raw_us_privacy.clone(),
        }
    }

    /// Whether GDPR applies to this request.
    #[must_use]
    pub fn gdpr_applies(&self) -> bool {
        self.gdpr_applies
    }

    /// The raw TCF string.
    #[must_use]
    pub fn raw_tc_string(&self) -> Option<&str> {
        self.tc_string.as_deref()
    }

    /// The raw GPP string.
    #[must_use]
    pub fn raw_gpp_string(&self) -> Option<&str> {
        self.gpp_string.as_deref()
    }

    /// The applicable GPP section IDs.
    #[must_use]
    pub fn gpp_section_ids(&self) -> Option<&[u16]> {
        self.gpp_section_ids.as_deref()
    }

    /// The raw US Privacy string.
    #[must_use]
    pub fn raw_us_privacy(&self) -> Option<&str> {
        self.us_privacy.as_deref()
    }
}
```

Check the real field names on `ConsentContext` (`consent/types.rs:109-148`): `grep -n "pub " crates/trusted-server-core/src/consent/types.rs | sed -n 1,60p`. Use them in `from_context`. If the section IDs are stored as another type, convert here. The rest of the file:

```rust
/// Country and subdivision, upper case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    country: String,
    subdivision: Option<String>,
}

impl Region {
    /// Normalizes platform geo: upper case, and an ISO 3166-2 `US-CA` style
    /// region loses its country prefix. `None` without a two-letter country.
    #[must_use]
    pub(crate) fn from_geo(country: &str, region: Option<&str>) -> Option<Self> {
        let country = country.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return None;
        }
        let subdivision = region
            .map(|value| value.trim().to_ascii_uppercase())
            .map(|value| match value.split_once('-') {
                Some((prefix, rest)) if prefix == country => rest.to_owned(),
                _ => value,
            })
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        Some(Self { country, subdivision })
    }

    /// ISO 3166-1 alpha-2, upper case, for example `"US"`.
    #[must_use]
    pub fn country(&self) -> &str {
        &self.country
    }

    /// The ISO 3166-2 subdivision without the country prefix, for example
    /// `"CA"`. `None` when the platform does not know it.
    #[must_use]
    pub fn subdivision(&self) -> Option<&str> {
        self.subdivision.as_deref()
    }

    /// Whether this is `country` and `subdivision`.
    #[must_use]
    pub fn is(&self, country: &str, subdivision: &str) -> bool {
        self.country == country && self.subdivision.as_deref() == Some(subdivision)
    }
}

/// The 32 raw bytes of a SHA-256 of a normalized email. Personal data.
#[derive(Clone, PartialEq, Eq)]
pub struct HashedEmail([u8; 32]);

impl fmt::Debug for HashedEmail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HashedEmail([redacted])")
    }
}

impl HashedEmail {
    /// From digest bytes. Test support only: v1 has no source of hashed
    /// emails, so nothing else builds one.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, as `LiveRamp` takes it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Standard base64, as `UID2` takes it.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

/// A stored record for one of this provider's sources, valid or expired.
///
/// It holds the source's whole entry set, which is never empty.
#[derive(Clone, PartialEq)]
pub struct StoredId {
    eids: Vec<Eid>,
    uid: String,
    writer: Option<IdentityProviderId>,
    acquired_at: Option<UnixTime>,
    expires_at: Option<UnixTime>,
    refresh_at: Option<UnixTime>,
}

impl fmt::Debug for StoredId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredId")
            .field("entries", &self.eids.len())
            .field("uid", &"[redacted]")
            .field("writer", &self.writer)
            .field("acquired_at", &self.acquired_at)
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .finish_non_exhaustive()
    }
}

impl StoredId {
    /// From the KV record of `source`. A writer that does not parse is
    /// treated as none. `None` for a record with no entry, which
    /// `KvEntry::validate` never lets through.
    #[must_use]
    pub(crate) fn from_record(source: &SourceDomain, record: &KvPartnerId) -> Option<Self> {
        let uid = record.first_uid()?.to_owned();
        let eids = record
            .eids
            .iter()
            .map(|entry| Eid {
                source: source.as_str().to_owned(),
                uids: entry
                    .uids
                    .iter()
                    .map(|uid| Uid { id: uid.id.clone(), atype: uid.atype, ext: uid.ext.clone() })
                    .collect(),
                inserter: entry.inserter.clone(),
                matcher: entry.matcher.clone(),
                mm: entry.mm,
                ext: entry.ext.clone(),
            })
            .collect();
        Some(Self {
            eids,
            uid,
            writer: record.w.as_deref().and_then(|writer| writer.parse().ok()),
            acquired_at: record.at.map(UnixTime::from_secs),
            expires_at: record.exp.map(UnixTime::from_secs),
            refresh_at: record.ra.map(UnixTime::from_secs),
        })
    }

    fn first_uid(&self) -> Option<&Uid> {
        self.eids.first().and_then(|eid| eid.uids.first())
    }

    /// Every stored entry for the source, in stored order, with its UIDs and
    /// EID-level fields. A record that browser ingest, generic pull, batch
    /// sync or a version 1 row wrote is one entry with one UID.
    #[must_use]
    pub fn eids(&self) -> &[Eid] {
        &self.eids
    }

    /// The first UID of the first entry. What a single-UID refresh, such as
    /// `LiveRamp`'s, sends back.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// The `atype` of that first UID.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.first_uid().and_then(|uid| uid.atype)
    }

    /// The UID-level ext of that first UID, as bidders read it in
    /// `uids[].ext`.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.first_uid().and_then(|uid| uid.ext.as_ref())
    }

    /// The provider that wrote it. `None` for browser ingest and legacy rows.
    #[must_use]
    pub fn writer(&self) -> Option<&IdentityProviderId> {
        self.writer.as_ref()
    }

    /// When it was acquired or last refreshed.
    #[must_use]
    pub fn acquired_at(&self) -> Option<UnixTime> {
        self.acquired_at
    }

    /// When it must stop being sent.
    #[must_use]
    pub fn expires_at(&self) -> Option<UnixTime> {
        self.expires_at
    }

    /// When it is due for refresh.
    #[must_use]
    pub fn refresh_at(&self) -> Option<UnixTime> {
        self.refresh_at
    }

    /// Whether `now` is at or past its expiry.
    #[must_use]
    pub fn is_expired(&self, now: UnixTime) -> bool {
        self.expires_at.is_some_and(|expires| now >= expires)
    }
}

/// A source of random bytes.
pub(crate) trait RandomSource: Send + Sync {
    /// Fills `bytes`.
    fn fill(&self, bytes: &mut [u8]);
}

/// The platform generator EC generation uses (`rand::thread_rng`).
pub(crate) struct ThreadRandom;

impl RandomSource for ThreadRandom {
    fn fill(&self, bytes: &mut [u8]) {
        rand::thread_rng().fill_bytes(bytes);
    }
}

/// Deterministic bytes for tests: xorshift64* from a seed.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
pub struct SeededRandom(std::sync::Mutex<u64>);

#[cfg(any(test, feature = "test-utils"))]
impl SeededRandom {
    /// A generator that yields the same bytes for the same seed. A zero
    /// seed is treated as one, since xorshift never leaves zero.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(std::sync::Mutex::new(seed.max(1)))
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl RandomSource for SeededRandom {
    fn fill(&self, bytes: &mut [u8]) {
        let mut state = self.0.lock().expect("should lock the seed");
        for byte in bytes {
            *state ^= *state >> 12;
            *state ^= *state << 25;
            *state ^= *state >> 27;
            *byte = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_be_bytes()[0];
        }
    }
}

/// Every signal the coordinator holds for one request. Crate-private: a
/// provider only ever sees a [`ResolveInput`] filtered by its descriptor.
pub(crate) struct ResolveSignals<'a> {
    pub(crate) client_ip: Option<IpAddr>,
    pub(crate) user_agent: Option<&'a str>,
    pub(crate) page_url: Option<Url>,
    pub(crate) region: Option<Region>,
    pub(crate) hashed_email: Option<HashedEmail>,
    pub(crate) consent: ProviderConsent,
    pub(crate) publisher_domain: &'a str,
    pub(crate) now: UnixTime,
    pub(crate) random: &'a dyn RandomSource,
    pub(crate) ec_id: Option<String>,
    pub(crate) stored: BTreeMap<SourceDomain, StoredId>,
    pub(crate) state_json: Option<String>,
}

/// What one provider may read for one call.
pub struct ResolveInput<'a> {
    descriptor: &'a ProviderDescriptor,
    kinds: BTreeMap<SourceDomain, ResolveKind>,
    signals: &'a ResolveSignals<'a>,
    declared: BTreeSet<Input>,
    available: BTreeSet<Input>,
    undeclared: RefCell<Vec<Access>>,
    state_failed: RefCell<bool>,
}

impl fmt::Debug for ResolveInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveInput")
            .field("provider", self.descriptor.id())
            .field("kinds", &self.kinds)
            .finish_non_exhaustive()
    }
}

impl<'a> ResolveInput<'a> {
    /// The input for one call. `kinds` holds only the sources to resolve.
    /// The inputs of every kind present are available, plus the wanted ones.
    /// An input declared only for a kind this call does not hold is
    /// declared, so reading it is not an error of the provider, but it is
    /// unavailable (spec 4.4).
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        kinds: BTreeMap<SourceDomain, ResolveKind>,
        signals: &'a ResolveSignals<'a>,
    ) -> Self {
        let (declared, available) = descriptor.resolves().map_or_else(Default::default, |resolves| {
            let mut available = resolves.wants().clone();
            for kind in kinds.values().copied().collect::<BTreeSet<_>>() {
                available.extend(resolves.needs(kind));
            }
            (resolves.declared_inputs(), available)
        });
        Self {
            descriptor,
            kinds,
            signals,
            declared,
            available,
            undeclared: RefCell::new(Vec::new()),
            state_failed: RefCell::new(false),
        }
    }

    fn record(&self, access: Access) -> Report<IdentityProviderError> {
        self.undeclared.borrow_mut().push(access);
        Report::new(IdentityProviderError::UndeclaredInput { access })
    }

    fn declared<T>(&self, input: Input, value: Option<T>) -> Result<T, Report<IdentityProviderError>> {
        if !self.declared.contains(&input) {
            return Err(self.record(Access::Input(input)));
        }
        if !self.available.contains(&input) {
            return Err(Report::new(IdentityProviderError::InputUnavailable { input }));
        }
        value.ok_or_else(|| Report::new(IdentityProviderError::InputUnavailable { input }))
    }

    /// Undeclared reads recorded so far. The coordinator logs them as
    /// `Config`, and the harness fails the test.
    #[must_use]
    pub(crate) fn recorded_errors(&self) -> Vec<Access> {
        self.undeclared.borrow().clone()
    }

    /// Whether a `state()` call failed to decode.
    #[must_use]
    pub(crate) fn state_decode_failed(&self) -> bool {
        *self.state_failed.borrow()
    }

    /// The sources to resolve this time, each with its own kind.
    pub fn sources(&self) -> impl Iterator<Item = (&SourceDomain, ResolveKind)> + '_ {
        self.kinds.iter().map(|(source, kind)| (source, *kind))
    }

    /// Whether `source` is being resolved for the first time.
    #[must_use]
    pub fn is_initial(&self, source: &SourceDomain) -> bool {
        self.kinds.get(source) == Some(&ResolveKind::Initial)
    }

    /// Whether `source` is being refreshed.
    #[must_use]
    pub fn is_refresh(&self, source: &SourceDomain) -> bool {
        self.kinds.get(source) == Some(&ResolveKind::Refresh)
    }

    /// The descriptor's primary source.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        self.descriptor.primary_source()
    }

    /// The stored record for one of this provider's sources.
    #[must_use]
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId> {
        self.signals.stored.get(source)
    }

    /// This provider's private state, decoded. `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// `InvalidState` when the stored state does not decode as `T`. The
    /// failure is recorded, and the coordinator clears the state whatever
    /// the provider returns. `UndeclaredInput` without `with_private_state()`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>> {
        if !self.descriptor.resolves().is_some_and(|resolves| resolves.has_private_state()) {
            return Err(self.record(Access::PrivateState));
        }
        let Some(raw) = self.signals.state_json.as_deref() else {
            return Ok(None);
        };
        serde_json::from_str(raw).map(Some).map_err(|_| {
            *self.state_failed.borrow_mut() = true;
            Report::new(IdentityProviderError::InvalidState)
        })
    }

    /// The endpoint declared with `Resolves::with_endpoint`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: Endpoint }` when none was declared.
    pub fn endpoint(&self) -> Result<&Url, Report<IdentityProviderError>> {
        self.descriptor.endpoint().ok_or_else(|| self.record(Access::Endpoint))
    }

    /// The consent strings to forward.
    #[must_use]
    pub fn consent(&self) -> &ProviderConsent {
        &self.signals.consent
    }

    /// The publisher's domain.
    #[must_use]
    pub fn publisher_domain(&self) -> &str {
        self.signals.publisher_domain
    }

    /// The request's time.
    #[must_use]
    pub fn now(&self) -> UnixTime {
        self.signals.now
    }

    /// Random bytes from the platform generator. Fixed by the harness.
    #[must_use]
    pub fn random_bytes<const N: usize>(&self) -> [u8; N] {
        let mut bytes = [0u8; N];
        self.signals.random.fill(&mut bytes);
        bytes
    }

    /// The platform's client address.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn client_ip(&self) -> Result<IpAddr, Report<IdentityProviderError>> {
        self.declared(Input::ClientIp, self.signals.client_ip)
    }

    /// The `User-Agent`.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn user_agent(&self) -> Result<&str, Report<IdentityProviderError>> {
        self.declared(Input::UserAgent, self.signals.user_agent)
    }

    /// The page URL, without query string or fragment.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn page_url(&self) -> Result<&Url, Report<IdentityProviderError>> {
        self.declared(Input::PageUrl, self.signals.page_url.as_ref())
    }

    /// Country and subdivision.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn region(&self) -> Result<&Region, Report<IdentityProviderError>> {
        self.declared(Input::Region, self.signals.region.as_ref())
    }

    /// A hashed email. Never available in v1.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn hashed_email(&self) -> Result<&HashedEmail, Report<IdentityProviderError>> {
        self.declared(Input::HashedEmail, self.signals.hashed_email.as_ref())
    }

    /// The full EC ID. Only for a resolver declared `with_ec_id()`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: EcId }` otherwise. `Config` when the
    /// request has no EC ID, which the coordinator never allows: it does not
    /// dispatch generic pull without one.
    pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>> {
        if !self.descriptor.resolves().is_some_and(|resolves| resolves.gets_ec_id()) {
            return Err(self.record(Access::EcId));
        }
        self.signals
            .ec_id
            .as_deref()
            .ok_or_else(|| IdentityProviderError::config("generic pull was called without an EC ID"))
    }
}
```

`StoredId::uid` returns the first UID of the first entry, the value `KvPartnerId::first_uid` (Task 6) reads, so `uid()` and `eids()[0].uids[0].id` always agree. The coordinator builds the `stored` map with `StoredId::from_record(source, record)` for each of the provider's sources in the row, skipping a `None`.

`hex` is already a dependency of core (check with `grep -n '^hex' crates/trusted-server-core/Cargo.toml`). If it is only a build dependency, add `hex = { workspace = true }` to `[dependencies]`. `ResolveKind` stays in `descriptor.rs` and is re-exported from `mod.rs`. Update `mod.rs`:

```rust
pub mod input;
pub use input::{HashedEmail, ProviderConsent, Region, ResolveInput, StoredId};
```

In `descriptor.rs`, add the variant to `Access` (Task 5), after `PrivateState`:

```rust
    /// The full EC ID, without `with_ec_id()`. Only generic pull declares it.
    #[display("ec_id")]
    EcId,
```

Add `ec/identity/input.rs` to `migration_guards.rs`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::input`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add declared-input enforcement for identity providers"
```

### Task 10: Requests, responses and resolutions

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/exchange.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/resolution.rs` (add `Resolution`, `ResolvedEid`, `ResolvedUid`, `When`, `StateChange`, and the crate-private entry-set check)
- Modify: `crates/trusted-server-core/src/ec/identity/input.rs` (add `CaptureInput`, moved here from Task 9 because it holds a `ProviderResponse`)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: all three files

**Interfaces:**

- Consumes: `KvEid`, `KvUid`, `entry_set_bytes(&[KvEid]) -> usize` and the limits `MAX_UID_LENGTH`, `MAX_PARTNER_EXT_BYTES` (2,048), `MAX_EID_FIELD_BYTES` (255), `MAX_ENTRIES_PER_SOURCE` (8), `MAX_UIDS_PER_SOURCE` (32), `MAX_ENTRY_SET_BYTES` (4,096) and `MAX_PROVIDER_STATE_BYTES` (1,024) from `ec/kv_types.rs` (Task 6). `entry_set_bytes` is the one measure of a set: the coordinator and `KvEntry::validate` both call it, so a set one accepts the other never rejects
- Produces: everything listed in spec 4.5, plus:
  - `ProviderRequest::call_context_json(&self) -> Option<&str>` (crate-private)
  - `ProviderRequest::has_cookie_header(&self) -> bool` (crate-private)
  - `ProviderResponse::new(status: StatusCode, headers: HeaderMap, body: Vec<u8>, call_context: Option<String>) -> Self` (crate-private)
  - `ProviderResponse::for_test(status, body: &[u8]) -> Self` (test support)
  - `pub(crate) fn entry_sets(eids: &[ResolvedEid], now: UnixTime) -> EntrySets`: groups a result's entries by source, in order, applies every limit of spec 4.5 ("From entries to an entry set") and resolves each set's timing to the earliest its entries set
  - `pub(crate) struct EntrySet { entries: Vec<KvEid>, expires_at, refresh_at, refresh_until: Option<UnixTime> }`, `pub(crate) struct EntrySets { sets: BTreeMap<SourceDomain, EntrySet>, drops: Vec<EntryDrop>, oversized: Vec<SourceDomain> }`, `pub(crate) struct EntryDrop { source, part: DroppedPart }` and `pub(crate) enum DroppedPart`, all with `pub(crate)` fields
  - `EntrySets::all_discarded(&self) -> bool`: the result named at least one source and every set was discarded. Task 20 handles such a result as `Malformed` (spec 4.5, 8): `f += 1` with its backoff, removals still applied. A source whose set is discarded is treated as not named, so its stored set stays as it is
  - `CaptureInput<'a>` (in `input.rs`) with the spec 4.4 accessors and `pub(crate) fn new(...)`
  - `Resolution` and its private `StateOp` have a `Debug` that never prints provider state (spec 9.3)
  - `MAX_CALL_CONTEXT_BYTES = 1024`

- [ ] **Step 1: Write the failing tests**

In `exchange.rs`:

```rust
#[cfg(test)]
mod tests {
    use http::{HeaderName, HeaderValue, Method, StatusCode};
    use serde_json::json;

    use super::*;

    fn url() -> Url {
        Url::parse("https://api.example.com/v1/fetch").expect("should parse")
    }

    #[test]
    fn post_json_sets_body_and_content_type() {
        let request = ProviderRequest::post_json(&url(), &json!({ "a": 1 })).expect("should build a JSON request");
        assert_eq!(request.method(), Method::POST, "should use POST");
        assert_eq!(request.headers()["content-type"], "application/json", "should set the JSON content type");
        assert_eq!(request.body(), br#"{"a":1}"#, "should serialize the body");
    }

    #[test]
    fn query_bearer_and_headers() {
        let request = ProviderRequest::get(&url())
            .with_query("pid", "1 2")
            .with_bearer(&Redacted::new("token".to_owned()))
            .try_with_header("x-example", "v")
            .expect("should accept a valid header");
        assert_eq!(request.url().as_str(), "https://api.example.com/v1/fetch?pid=1+2", "should encode the query");
        assert_eq!(request.headers()["authorization"], "Bearer token", "should set the bearer token");
        assert_eq!(request.headers()["x-example"], "v", "should set the header");
        ProviderRequest::get(&url()).try_with_header("bad header", "v").expect_err("should reject a bad name");
        ProviderRequest::get(&url()).try_with_header("x", "bad\nvalue").expect_err("should reject a bad value");
    }

    #[test]
    fn cookie_headers_are_flagged_from_either_helper() {
        let typed = ProviderRequest::get(&url())
            .with_header(HeaderName::from_static("cookie"), HeaderValue::from_static("a=1"));
        let dynamic = ProviderRequest::get(&url())
            .try_with_header("Cookie", "a=1")
            .expect("should accept a Cookie header from strings");
        assert!(typed.has_cookie_header(), "should flag a Cookie header from the typed helper");
        assert!(dynamic.has_cookie_header(), "should flag a Cookie header from the string helper");
        assert!(!ProviderRequest::get(&url()).has_cookie_header(), "should not flag a request without one");
    }

    #[test]
    fn call_context_round_trips_and_is_capped() {
        let request = ProviderRequest::get(&url())
            .try_with_call_context(&json!({ "nonce": [1, 2] }))
            .expect("should accept a small call context");
        let response = ProviderResponse::new(
            StatusCode::OK,
            http::HeaderMap::new(),
            Vec::new(),
            request.call_context_json().map(str::to_owned),
        );
        let context: serde_json::Value = response
            .call_context()
            .expect("should decode the call context")
            .expect("should carry the call context");
        assert_eq!(context, json!({ "nonce": [1, 2] }), "should round-trip");

        let error = ProviderRequest::get(&url())
            .try_with_call_context(&"x".repeat(MAX_CALL_CONTEXT_BYTES))
            .expect_err("should reject an oversized call context");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Config { .. }),
            "should report an oversized call context as Config"
        );
    }

    #[test]
    fn success_helpers_check_status_and_encoding() {
        let ok = ProviderResponse::for_test(StatusCode::OK, br#"{"uid":"u"}"#);
        let value: serde_json::Value = ok.success_json().expect("should parse a 2xx JSON body");
        assert_eq!(value["uid"], "u", "should read the uid");

        let rejected = ProviderResponse::for_test(StatusCode::FORBIDDEN, br#"{}"#);
        let error = rejected
            .success_json::<serde_json::Value>()
            .expect_err("should reject a non-2xx status");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Rejected { status } if *status == StatusCode::FORBIDDEN),
            "should report Rejected with the status"
        );
        rejected
            .json::<serde_json::Value>()
            .expect("should parse JSON whatever the status");

        let binary = ProviderResponse::for_test(StatusCode::OK, &[0xff, 0xfe]);
        let error = binary.success_text().expect_err("should reject non-UTF-8");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Malformed),
            "should report non-UTF-8 as Malformed"
        );
    }
}
```

In `resolution.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn source(name: &str) -> SourceDomain {
        name.parse().expect("should parse the source")
    }

    fn uid(id: &str) -> ResolvedUid {
        ResolvedUid::new(id)
    }

    fn kv_uid(id: &str) -> KvUid {
        KvUid { id: id.to_owned(), atype: None, ext: None }
    }

    fn now() -> UnixTime {
        UnixTime::from_secs(1_000)
    }

    fn parts(sets: &EntrySets) -> Vec<DroppedPart> {
        sets.drops.iter().map(|drop| drop.part).collect()
    }

    #[test]
    fn empty_found_is_no_match() {
        let resolution = Resolution::found([]);
        assert!(resolution.is_no_match(), "should be no_match");
        assert!(resolution.eids().is_empty(), "should hold no entries");
    }

    #[test]
    fn a_one_uid_entry_becomes_a_one_entry_set_without_eid_level_fields() {
        let a = source("a.example.com");
        let resolution = Resolution::found([ResolvedEid::new(&a, uid("u").with_atype(3))]);

        let sets = entry_sets(resolution.eids(), now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![KvEid {
                uids: vec![KvUid { id: "u".to_owned(), atype: Some(3), ext: None }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            }],
            "should produce the one-UID shape browser ingest stores"
        );
        assert!(sets.drops.is_empty() && sets.oversized.is_empty(), "should drop nothing");
    }

    #[test]
    fn entries_group_by_source_and_keep_every_uid_and_field_in_order() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let first = ResolvedEid::new(&a, uid("a1").with_atype(1).with_ext(json!({ "rtiPartner": "TDID" })))
            .with_uid(uid("a2"))
            .with_inserter("inserter.example.com")
            .with_matcher("matcher.example.com")
            .with_mm(2)
            .with_ext(json!({ "segments": ["s"] }));
        let other = ResolvedEid::new(&b, uid("b1"));
        let second = ResolvedEid::new(&a, uid("a3"));

        let sets = entry_sets(&[first, other, second], now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![
                KvEid {
                    uids: vec![
                        KvUid { id: "a1".to_owned(), atype: Some(1), ext: Some(json!({ "rtiPartner": "TDID" })) },
                        kv_uid("a2"),
                    ],
                    inserter: Some("inserter.example.com".to_owned()),
                    matcher: Some("matcher.example.com".to_owned()),
                    mm: Some(2),
                    ext: Some(json!({ "segments": ["s"] })),
                },
                KvEid { uids: vec![kv_uid("a3")], inserter: None, matcher: None, mm: None, ext: None },
            ],
            "should keep a's entries in the order returned"
        );
        assert_eq!(sets.sets[&b].entries.len(), 1, "should group b on its own");
        assert!(sets.drops.is_empty(), "should drop nothing");
    }

    #[test]
    fn bad_fields_are_dropped_and_their_entries_kept() {
        let a = source("a.example.com");
        let entry = ResolvedEid::new(&a, uid("u").with_atype(-1).with_ext(json!(["not", "an", "object"])))
            .with_inserter("  ")
            .with_matcher(&"m".repeat(MAX_EID_FIELD_BYTES + 1))
            .with_mm(-2)
            .with_ext(json!("not an object"));

        let sets = entry_sets(&[entry], now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![KvEid { uids: vec![kv_uid("u")], inserter: None, matcher: None, mm: None, ext: None }],
            "should keep the entry without its bad fields"
        );
        assert_eq!(
            parts(&sets),
            vec![
                DroppedPart::Atype,
                DroppedPart::UidExt,
                DroppedPart::Inserter,
                DroppedPart::Matcher,
                DroppedPart::Mm,
                DroppedPart::EidExt,
            ],
            "should name each dropped field"
        );
    }

    #[test]
    fn bad_uids_are_dropped_and_an_entry_without_uids_is_discarded() {
        let a = source("a.example.com");
        let mixed = ResolvedEid::new(&a, uid(" ")).with_uid(uid("kept"));
        let empty = ResolvedEid::new(&a, uid(&"x".repeat(MAX_UID_LENGTH + 1))).with_inserter("inserter.example.com");

        let sets = entry_sets(&[mixed, empty], now());

        assert_eq!(sets.sets[&a].entries.len(), 1, "should discard the entry left with no UID");
        assert_eq!(sets.sets[&a].entries[0].uids, vec![kv_uid("kept")], "should keep the valid UID");
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::Uid, DroppedPart::Uid, DroppedPart::EmptyEntry],
            "should name the dropped UIDs and the discarded entry"
        );
    }

    #[test]
    fn a_source_with_no_kept_entry_is_not_named() {
        let a = source("a.example.com");

        let sets = entry_sets(&[ResolvedEid::new(&a, uid(""))], now());

        assert!(!sets.sets.contains_key(&a), "should leave the stored set as it is");
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::Uid, DroppedPart::EmptyEntry],
            "should record the dropped UID and the discarded entry"
        );
        assert!(sets.all_discarded(), "should report that every named set was discarded");
    }

    #[test]
    fn all_discarded_only_when_nothing_was_kept() {
        let a = source("a.example.com");
        let b = source("b.example.com");

        let mixed = entry_sets(&[ResolvedEid::new(&a, uid("")), ResolvedEid::new(&b, uid("kept"))], now());
        let empty = entry_sets(&[], now());

        assert!(!mixed.all_discarded(), "should not report a result that kept one set");
        assert!(!empty.all_discarded(), "should not report a result that named nothing");
    }

    #[test]
    fn duplicates_and_entries_and_uids_over_the_limits_are_dropped() {
        let a = source("a.example.com");
        let mut eids = vec![
            ResolvedEid::new(&a, uid("e0")),
            ResolvedEid::new(&a, uid("e0")),
        ];
        // 7 more entries of 4 UIDs: 1 + 28 = 29 UIDs, then a ninth entry.
        for entry in 1..=7 {
            let mut eid = ResolvedEid::new(&a, uid(&format!("e{entry}u0")));
            for index in 1..4 {
                eid = eid.with_uid(uid(&format!("e{entry}u{index}")));
            }
            eids.push(eid);
        }
        eids.push(ResolvedEid::new(&a, uid("ninth")));

        let sets = entry_sets(&eids, now());

        let set = &sets.sets[&a];
        assert_eq!(set.entries.len(), MAX_ENTRIES_PER_SOURCE, "should keep 8 entries");
        let uid_count: usize = set.entries.iter().map(|entry| entry.uids.len()).sum();
        assert_eq!(uid_count, 29, "should stay under the UID cap in this control");
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::DuplicateEntry, DroppedPart::ExtraEntry],
            "should drop the duplicate and the ninth entry"
        );

        let mut wide = ResolvedEid::new(&a, uid("u0"));
        for index in 1..=MAX_UIDS_PER_SOURCE {
            wide = wide.with_uid(uid(&format!("u{index}")));
        }
        let sets = entry_sets(&[wide, ResolvedEid::new(&a, uid("later"))], now());
        assert_eq!(sets.sets[&a].entries.len(), 1, "should discard the entry left with no room");
        assert_eq!(sets.sets[&a].entries[0].uids.len(), MAX_UIDS_PER_SOURCE, "should keep 32 UIDs");
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::ExtraUid, DroppedPart::ExtraUid, DroppedPart::ExtraEntry],
            "should drop the 33rd UID and the entry with no room"
        );
    }

    #[test]
    fn an_oversized_set_is_discarded_whole_and_others_are_kept() {
        let a = source("a.example.com");
        let b = source("b.example.com");

        let sets = entry_sets(&[oversized_entry(&a), ResolvedEid::new(&b, uid("small"))], now());

        assert!(!sets.sets.contains_key(&a), "should never trim an oversized set");
        assert_eq!(sets.oversized, vec![a.clone()], "should report the oversized source");
        assert!(sets.sets.contains_key(&b), "should keep the other source");
        assert!(!sets.all_discarded(), "should not report the result as all discarded");

        let only = entry_sets(&[oversized_entry(&a)], now());
        assert!(only.all_discarded(), "should report a result whose only set is oversized");
    }

    fn oversized_entry(source: &SourceDomain) -> ResolvedEid {
        let mut big = ResolvedEid::new(source, uid(&"x".repeat(500)));
        for index in 0..8 {
            big = big.with_uid(uid(&format!("{index}{}", "x".repeat(499))));
        }
        big
    }

    #[test]
    fn a_set_takes_the_earliest_times_its_entries_set() {
        let a = source("a.example.com");
        let first = ResolvedEid::new(&a, uid("one"))
            .with_expires_in(Duration::from_secs(100))
            .with_refresh_at(UnixTime::from_secs(1_080))
            .with_refresh_until(UnixTime::from_secs(5_000));
        let second = ResolvedEid::new(&a, uid("two"))
            .with_expires_at(UnixTime::from_secs(1_050))
            .with_refresh_in(Duration::from_secs(90))
            .with_refresh_until(UnixTime::from_secs(4_000));
        let third = ResolvedEid::new(&a, uid("three"));

        let set = entry_sets(&[first, second, third], now()).sets.remove(&a).expect("should keep a");

        assert_eq!(set.expires_at, Some(UnixTime::from_secs(1_050)), "should take the earliest expiry");
        assert_eq!(set.refresh_at, Some(UnixTime::from_secs(1_080)), "should take the earliest refresh");
        assert_eq!(set.refresh_until, Some(UnixTime::from_secs(4_000)), "should take the earliest refresh limit");
    }

    #[test]
    fn dropped_entries_do_not_count_toward_timing() {
        let a = source("a.example.com");
        let kept = ResolvedEid::new(&a, uid("one")).with_expires_at(UnixTime::from_secs(2_000));
        let duplicate = ResolvedEid::new(&a, uid("one")).with_expires_at(UnixTime::from_secs(1_100));
        let empty = ResolvedEid::new(&a, uid("")).with_refresh_at(UnixTime::from_secs(1_010));

        let set = entry_sets(&[kept, duplicate, empty], now()).sets.remove(&a).expect("should keep a");

        assert_eq!(
            set.expires_at,
            Some(UnixTime::from_secs(2_000)),
            "should ignore the expiry of a dropped duplicate"
        );
        assert_eq!(set.refresh_at, None, "should ignore the refresh time of a discarded entry");
    }

    #[test]
    fn invalidations_chain_and_state_changes_are_reported() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let resolution = Resolution::found([ResolvedEid::new(&a, uid("u"))])
            .also_invalidate(&b)
            .with_retry_in(Duration::from_secs(60))
            .replacing_state(&json!({ "signature": "s" }));
        assert_eq!(resolution.invalidated(), &[b], "should invalidate b");
        assert_eq!(resolution.retry_in(), Some(Duration::from_secs(60)), "should keep the retry delay");
        assert_eq!(
            resolution.state_change(),
            StateChange::Replace(r#"{"signature":"s"}"#),
            "should replace the state"
        );
        assert_eq!(
            Resolution::no_match().clearing_state().state_change(),
            StateChange::Clear,
            "should clear the state"
        );
        assert_eq!(Resolution::no_match().state_change(), StateChange::Keep, "should keep the state by default");
    }

    #[test]
    fn resolution_debug_redacts_state() {
        let resolution = Resolution::no_match().replacing_state(&json!({ "signature": "secret-state" }));

        let debug = format!("{resolution:?}");

        assert!(!debug.contains("secret-state"), "should not print provider state");
        assert!(debug.contains("Replace([redacted])"), "should show that state is replaced");
    }

    #[test]
    fn oversized_state_is_marked_invalid_not_dropped_silently() {
        let resolution = Resolution::found([ResolvedEid::new(&source("a.example.com"), uid("u"))])
            .replacing_state(&"x".repeat(2000));
        assert!(resolution.state_rejected(), "should flag the state as rejected");
    }

    #[test]
    fn oversized_ext_is_dropped_and_the_uid_and_entry_kept() {
        let pad = json!({ "pad": "x".repeat(MAX_PARTNER_EXT_BYTES) });
        let resolved = uid("u").with_ext(pad.clone());
        assert_eq!(resolved.id(), "u", "should keep the UID");
        assert!(resolved.ext().is_none(), "should drop the UID-level ext");
        let small = uid("u").with_ext(json!({ "linkType": 2 }));
        assert_eq!(small.ext(), Some(&json!({ "linkType": 2 })), "should keep a small ext");

        let eid = ResolvedEid::new(&source("a.example.com"), uid("u")).with_ext(pad);
        assert_eq!(eid.uids().len(), 1, "should keep the entry");
        assert!(eid.ext().is_none(), "should drop the EID-level ext");
    }

    #[test]
    fn debug_redacts_every_uid() {
        let eid = ResolvedEid::new(&source("a.example.com"), uid("secret-one")).with_uid(uid("secret-two"));
        let debug = format!("{eid:?} {:?}", eid.uids()[0]);
        assert!(!debug.contains("secret"), "should not print a UID");
        assert!(debug.contains("a.example.com"), "should print the source");
    }

    #[test]
    fn opted_out_is_reported() {
        assert!(Resolution::opted_out().is_opted_out(), "should report the opt-out");
        assert!(!Resolution::no_match().is_opted_out(), "should not report an opt-out for no_match");
    }
}
```

In `input.rs`, add `Captures` to the test module's `descriptor` import and `use crate::ec::identity::exchange::ProviderResponse;` beside it, then add the test that moved from Task 9:

```rust
    #[test]
    fn capture_input_exposes_path_and_never_the_request_body() {
        let descriptor = ProviderDescriptor::builder("c".parse().expect("should parse the provider id"))
            .with_source("example.com".parse().expect("should parse the source"))
            .with_captures(Captures::new().with_path(http::Method::POST, "/integrations/c/api"))
            .build()
            .expect("should build");
        let response = ProviderResponse::for_test(http::StatusCode::OK, b"{}");
        let consent = ProviderConsent::default();
        let input = CaptureInput::new(
            &descriptor,
            "/integrations/c/api",
            &response,
            &BTreeMap::new(),
            &consent,
            "www.example.com",
            UnixTime::from_secs(1),
        );
        assert_eq!(input.path(), "/integrations/c/api", "should expose the proxied path");
        assert_eq!(input.primary_source().as_str(), "example.com", "should expose the primary source");
    }
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::exchange`, `cargo test-fastly -- ec::identity::resolution` and `cargo test-fastly -- ec::identity::input`
Expected: compile errors.

- [ ] **Step 3: Implement `exchange.rs`**

```rust
//! What a provider sends and receives. The coordinator does all I/O.

use core::fmt;

use error_stack::Report;
use http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::resolution::IdentityProviderError;
use crate::redacted::Redacted;

/// Maximum JSON bytes of a call context.
pub const MAX_CALL_CONTEXT_BYTES: usize = 1024;

/// One call a provider wants made.
#[derive(Clone)]
pub struct ProviderRequest {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Vec<u8>,
    call_context: Option<String>,
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRequest")
            .field("method", &self.method)
            .field("host", &self.url.host_str())
            .field("path", &self.url.path())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl ProviderRequest {
    /// Any method, with no body.
    #[must_use]
    pub fn new(method: Method, url: &Url) -> Self {
        Self { method, url: url.clone(), headers: HeaderMap::new(), body: Vec::new(), call_context: None }
    }

    /// A `GET`.
    #[must_use]
    pub fn get(url: &Url) -> Self {
        Self::new(Method::GET, url)
    }

    /// A `POST` with a JSON body.
    ///
    /// # Errors
    ///
    /// `Config` when `body` does not serialize.
    pub fn post_json(url: &Url, body: &impl Serialize) -> Result<Self, Report<IdentityProviderError>> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| IdentityProviderError::config("request body does not serialize"))?;
        Self::new(Method::POST, url).with_body("application/json", bytes)
    }

    /// A `POST` with a `text/plain` body.
    #[must_use]
    pub fn post_text(url: &Url, body: &str) -> Self {
        let mut request = Self::new(Method::POST, url);
        request.headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        request.body = body.as_bytes().to_vec();
        request
    }

    /// Sets the body and its `Content-Type`.
    ///
    /// # Errors
    ///
    /// `Config` when `content_type` is not a valid header value.
    pub fn with_body(mut self, content_type: &str, body: Vec<u8>) -> Result<Self, Report<IdentityProviderError>> {
        let value = HeaderValue::from_str(content_type)
            .map_err(|_| IdentityProviderError::config("invalid content type"))?;
        self.headers.insert(CONTENT_TYPE, value);
        self.body = body;
        Ok(self)
    }

    /// Appends a query parameter, URL-encoded.
    #[must_use]
    pub fn with_query(mut self, name: &str, value: &str) -> Self {
        self.url.query_pairs_mut().append_pair(name, value);
        self
    }

    /// Sets a header. A `Cookie` header makes the coordinator refuse the call.
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Sets a header from strings.
    ///
    /// # Errors
    ///
    /// `Config` when the name or value is invalid.
    pub fn try_with_header(self, name: &str, value: &str) -> Result<Self, Report<IdentityProviderError>> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| IdentityProviderError::config("invalid header name"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| IdentityProviderError::config("invalid header value"))?;
        Ok(self.with_header(name, value))
    }

    /// Adds `Authorization: Bearer <token>`.
    #[must_use]
    pub fn with_bearer(mut self, token: &Redacted<String>) -> Self {
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {}", token.expose())) {
            value.set_sensitive(true);
            self.headers.insert(AUTHORIZATION, value);
        }
        self
    }

    /// Data `response` needs from this call. Kept in memory, never sent.
    ///
    /// # Errors
    ///
    /// `Config` when it does not serialize or is over 1,024 bytes of JSON.
    pub fn try_with_call_context(mut self, context: &impl Serialize) -> Result<Self, Report<IdentityProviderError>> {
        let json = serde_json::to_string(context)
            .map_err(|_| IdentityProviderError::config("call context does not serialize"))?;
        if json.len() > MAX_CALL_CONTEXT_BYTES {
            return Err(IdentityProviderError::config("call context exceeds 1024 bytes"));
        }
        self.call_context = Some(json);
        Ok(self)
    }

    /// The method.
    #[must_use]
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The URL.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The call context JSON.
    #[must_use]
    pub(crate) fn call_context_json(&self) -> Option<&str> {
        self.call_context.as_deref()
    }

    /// Whether the provider set a `Cookie` header.
    #[must_use]
    pub(crate) fn has_cookie_header(&self) -> bool {
        self.headers.contains_key(COOKIE)
    }
}

/// A buffered, decoded provider response.
#[derive(Clone)]
pub struct ProviderResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    call_context: Option<String>,
}

impl fmt::Debug for ProviderResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl ProviderResponse {
    /// Built by the coordinator after buffering and decoding.
    #[must_use]
    pub(crate) fn new(status: StatusCode, headers: HeaderMap, body: Vec<u8>, call_context: Option<String>) -> Self {
        Self { status, headers, body, call_context }
    }

    /// A response for tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn for_test(status: StatusCode, body: &[u8]) -> Self {
        Self::new(status, HeaderMap::new(), body.to_vec(), None)
    }

    /// The status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// A header as a string, if present and visible ASCII.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The decoded body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Parses the body as JSON, whatever the status.
    ///
    /// # Errors
    ///
    /// `Malformed` when it does not parse.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>> {
        serde_json::from_slice(&self.body).map_err(|_| IdentityProviderError::malformed())
    }

    /// Requires 2xx, then parses JSON.
    ///
    /// # Errors
    ///
    /// `Rejected` for a non-2xx status, `Malformed` for a bad body.
    pub fn success_json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>> {
        self.require_success()?;
        self.json()
    }

    /// Requires 2xx and a UTF-8 body.
    ///
    /// # Errors
    ///
    /// `Rejected` for a non-2xx status, `Malformed` for non-UTF-8.
    pub fn success_text(&self) -> Result<&str, Report<IdentityProviderError>> {
        self.require_success()?;
        core::str::from_utf8(&self.body).map_err(|_| IdentityProviderError::malformed())
    }

    /// The context the request carried, decoded.
    ///
    /// # Errors
    ///
    /// `Config` when it does not decode as `T`.
    pub fn call_context<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>> {
        self.call_context
            .as_deref()
            .map(|json| {
                serde_json::from_str(json)
                    .map_err(|_| IdentityProviderError::config("call context does not decode"))
            })
            .transpose()
    }

    fn require_success(&self) -> Result<(), Report<IdentityProviderError>> {
        if self.status.is_success() {
            Ok(())
        } else {
            Err(IdentityProviderError::rejected(self.status))
        }
    }
}
```

`Redacted` is `crate::redacted::Redacted` (`redacted.rs:28`).

- [ ] **Step 4: Implement the rest of `resolution.rs` and `CaptureInput`**

Merge these imports into the top import block of `resolution.rs` from Task 5, so the file keeps one block (`AGENTS.md` allows no imports after items):

```rust
use std::collections::BTreeMap;
use std::iter;
use std::time::Duration;

use serde::Serialize;

use super::ids::{SourceDomain, UnixTime};
use crate::ec::kv_types::{
    KvEid, KvUid, MAX_EID_FIELD_BYTES, MAX_ENTRIES_PER_SOURCE, MAX_ENTRY_SET_BYTES,
    MAX_PARTNER_EXT_BYTES, MAX_PROVIDER_STATE_BYTES, MAX_UID_LENGTH, MAX_UIDS_PER_SOURCE,
    entry_set_bytes,
};
```

Then append the items:

```rust
/// A time as a provider gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Relative to the request's `now`.
    In(Duration),
    /// Absolute.
    At(UnixTime),
}

impl When {
    /// Resolves against `now`.
    #[must_use]
    pub(crate) fn resolve(self, now: UnixTime) -> UnixTime {
        match self {
            Self::In(delay) => now + delay,
            Self::At(at) => at,
        }
    }
}

fn ext_fits(ext: &serde_json::Value) -> bool {
    serde_json::to_vec(ext).is_ok_and(|bytes| bytes.len() <= MAX_PARTNER_EXT_BYTES)
}

/// One UID within a [`ResolvedEid`].
#[derive(Clone, PartialEq)]
pub struct ResolvedUid {
    id: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
}

impl core::fmt::Debug for ResolvedUid {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedUid")
            .field("id", &"[redacted]")
            .field("atype", &self.atype)
            .finish_non_exhaustive()
    }
}

impl ResolvedUid {
    /// A UID with no `atype` or ext.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), atype: None, ext: None }
    }

    /// Sets `atype`.
    #[must_use]
    pub fn with_atype(mut self, atype: i32) -> Self {
        self.atype = Some(atype);
        self
    }

    /// UID-level ext. Over 2,048 bytes as compact JSON it is dropped and the
    /// UID kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = ext_fits(&ext);
        if !fits {
            log::debug!("Dropping an oversized UID-level ext");
        }
        self.ext = fits.then_some(ext);
        self
    }

    /// The UID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `atype`.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.atype
    }

    /// UID-level ext.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.ext.as_ref()
    }
}

/// One `OpenRTB` EID entry a provider returns: a source, its UIDs and its
/// EID-level fields.
#[derive(Clone, PartialEq)]
pub struct ResolvedEid {
    source: SourceDomain,
    uids: Vec<ResolvedUid>,
    inserter: Option<String>,
    matcher: Option<String>,
    mm: Option<i32>,
    ext: Option<serde_json::Value>,
    expiry: Option<When>,
    refresh: Option<When>,
    refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for ResolvedEid {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedEid")
            .field("source", &self.source)
            .field("uids", &self.uids)
            .field("mm", &self.mm)
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

impl ResolvedEid {
    /// An entry with one UID, the common case.
    #[must_use]
    pub fn new(source: &SourceDomain, uid: ResolvedUid) -> Self {
        Self {
            source: source.clone(),
            uids: vec![uid],
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
            expiry: None,
            refresh: None,
            refresh_until: None,
        }
    }

    /// Adds another UID to this entry. Repeatable.
    #[must_use]
    pub fn with_uid(mut self, uid: ResolvedUid) -> Self {
        self.uids.push(uid);
        self
    }

    /// Sets `inserter`. An entry that sets any of `inserter`, `matcher` and
    /// `mm` states its EID-level fields completely, so the descriptor's
    /// `with_provenance` default no longer applies to it (spec 4.3).
    #[must_use]
    pub fn with_inserter(mut self, inserter: &str) -> Self {
        self.inserter = Some(inserter.to_owned());
        self
    }

    /// Sets `matcher`. As [`Self::with_inserter`], it turns off the
    /// descriptor's default for this entry.
    #[must_use]
    pub fn with_matcher(mut self, matcher: &str) -> Self {
        self.matcher = Some(matcher.to_owned());
        self
    }

    /// Sets `mm`. As [`Self::with_inserter`], it turns off the descriptor's
    /// default for this entry.
    #[must_use]
    pub fn with_mm(mut self, mm: i32) -> Self {
        self.mm = Some(mm);
        self
    }

    /// EID-level ext. Over 2,048 bytes as compact JSON it is dropped and the
    /// entry kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = ext_fits(&ext);
        if !fits {
            log::debug!("Dropping an oversized EID-level ext for source {}", self.source);
        }
        self.ext = fits.then_some(ext);
        self
    }

    /// Expires after `delay`. Timing applies to the source's whole entry
    /// set, and the earliest time across its entries wins.
    #[must_use]
    pub fn with_expires_in(mut self, delay: Duration) -> Self {
        self.expiry = Some(When::In(delay));
        self
    }

    /// Expires at `at`.
    #[must_use]
    pub fn with_expires_at(mut self, at: UnixTime) -> Self {
        self.expiry = Some(When::At(at));
        self
    }

    /// Due for refresh after `delay`.
    #[must_use]
    pub fn with_refresh_in(mut self, delay: Duration) -> Self {
        self.refresh = Some(When::In(delay));
        self
    }

    /// Due for refresh at `at`.
    #[must_use]
    pub fn with_refresh_at(mut self, at: UnixTime) -> Self {
        self.refresh = Some(When::At(at));
        self
    }

    /// When this ID stops being refreshable. Only for a provider declared
    /// `with_private_state()`; the coordinator ignores it otherwise.
    #[must_use]
    pub fn with_refresh_until(mut self, at: UnixTime) -> Self {
        self.refresh_until = Some(at);
        self
    }

    /// The source.
    #[must_use]
    pub fn source(&self) -> &SourceDomain {
        &self.source
    }

    /// The UIDs, in order.
    #[must_use]
    pub fn uids(&self) -> &[ResolvedUid] {
        &self.uids
    }

    /// `inserter`, if set.
    #[must_use]
    pub fn inserter(&self) -> Option<&str> {
        self.inserter.as_deref()
    }

    /// `matcher`, if set.
    #[must_use]
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    /// `mm`, if set.
    #[must_use]
    pub fn mm(&self) -> Option<i32> {
        self.mm
    }

    /// EID-level ext.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.ext.as_ref()
    }

    /// The expiry as given.
    #[must_use]
    pub fn expiry(&self) -> Option<When> {
        self.expiry
    }

    /// The refresh hint as given.
    #[must_use]
    pub fn refresh(&self) -> Option<When> {
        self.refresh
    }

    /// The refresh limit.
    #[must_use]
    pub fn refresh_until(&self) -> Option<UnixTime> {
        self.refresh_until
    }
}

/// How a result changes the provider's private state.
#[derive(Debug, PartialEq, Eq)]
pub enum StateChange<'a> {
    /// Leave it.
    Keep,
    /// Replace it with this JSON.
    Replace(&'a str),
    /// Remove it.
    Clear,
}

#[derive(Clone, PartialEq)]
enum StateOp {
    Keep,
    Replace(String),
    Clear,
    Rejected,
}

impl core::fmt::Debug for StateOp {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Replace(_) => formatter.write_str("Replace([redacted])"),
            Self::Clear => formatter.write_str("Clear"),
            Self::Rejected => formatter.write_str("Rejected"),
        }
    }
}

/// What one provider call produced. Its `Debug` prints entries through
/// [`ResolvedEid`]'s redacting `Debug` and never prints provider state.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    eids: Vec<ResolvedEid>,
    invalidated: Vec<SourceDomain>,
    opted_out: bool,
    retry_in: Option<Duration>,
    state: StateOp,
}

impl Resolution {
    fn empty() -> Self {
        Self { eids: Vec::new(), invalidated: Vec::new(), opted_out: false, retry_in: None, state: StateOp::Keep }
    }

    /// One or more EID entries. A source may appear in several entries, in
    /// the order bidders should see them. The entries of one source form its
    /// new entry set, which replaces the stored set whole for an owned
    /// source. For an unowned source the set only fills an absent or expired
    /// record (spec 7.2). A source the result does not name, or whose set
    /// the limits discard, is left as it is. An empty set is `no_match`.
    #[must_use]
    pub fn found(eids: impl IntoIterator<Item = ResolvedEid>) -> Self {
        Self { eids: eids.into_iter().collect(), ..Self::empty() }
    }

    /// Nothing recognized, or a capture held nothing.
    #[must_use]
    pub fn no_match() -> Self {
        Self::empty()
    }

    /// The stored ID for `source` is no longer valid.
    #[must_use]
    pub fn invalidate(source: &SourceDomain) -> Self {
        Self { invalidated: vec![source.clone()], ..Self::empty() }
    }

    /// The provider reports the user opted out.
    #[must_use]
    pub fn opted_out() -> Self {
        Self { opted_out: true, ..Self::empty() }
    }

    /// Also invalidates `source`.
    #[must_use]
    pub fn also_invalidate(mut self, source: &SourceDomain) -> Self {
        if !self.invalidated.contains(source) {
            self.invalidated.push(source.clone());
        }
        self
    }

    /// The earliest the coordinator may call again.
    #[must_use]
    pub fn with_retry_in(mut self, delay: Duration) -> Self {
        self.retry_in = Some(delay);
        self
    }

    /// Replaces the provider's private state. Over 1,024 bytes of JSON, or
    /// not serializable, the positive part of the result is rejected whole.
    #[must_use]
    pub fn replacing_state(mut self, state: &impl Serialize) -> Self {
        self.state = match serde_json::to_string(state) {
            Ok(json) if json.len() <= MAX_PROVIDER_STATE_BYTES => StateOp::Replace(json),
            _ => StateOp::Rejected,
        };
        self
    }

    /// Clears the provider's private state.
    #[must_use]
    pub fn clearing_state(mut self) -> Self {
        self.state = StateOp::Clear;
        self
    }

    /// The returned entries, as given.
    #[must_use]
    pub fn eids(&self) -> &[ResolvedEid] {
        &self.eids
    }

    /// Invalidated sources.
    #[must_use]
    pub fn invalidated(&self) -> &[SourceDomain] {
        &self.invalidated
    }

    /// Whether nothing was found, invalidated or opted out.
    #[must_use]
    pub fn is_no_match(&self) -> bool {
        self.eids.is_empty() && self.invalidated.is_empty() && !self.opted_out
    }

    /// Whether the provider reported an opt-out.
    #[must_use]
    pub fn is_opted_out(&self) -> bool {
        self.opted_out
    }

    /// The retry delay.
    #[must_use]
    pub fn retry_in(&self) -> Option<Duration> {
        self.retry_in
    }

    /// The state change.
    #[must_use]
    pub fn state_change(&self) -> StateChange<'_> {
        match &self.state {
            StateOp::Keep | StateOp::Rejected => StateChange::Keep,
            StateOp::Replace(json) => StateChange::Replace(json),
            StateOp::Clear => StateChange::Clear,
        }
    }

    /// Whether `replacing_state` was given an oversized or unserializable
    /// value. The coordinator then rejects the positive part (spec 4.5, 8).
    #[must_use]
    pub(crate) fn state_rejected(&self) -> bool {
        self.state == StateOp::Rejected
    }

    /// Whether this result changes state at all.
    #[must_use]
    pub(crate) fn changes_state(&self) -> bool {
        self.state != StateOp::Keep
    }
}

/// The part of a result that a spec 4.5 limit dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum DroppedPart {
    /// A UID that is empty, blank or over `MAX_UID_LENGTH`.
    #[display("uid")]
    Uid,
    /// A negative `atype`.
    #[display("atype")]
    Atype,
    /// A UID-level ext that is not an object or is oversized.
    #[display("uid ext")]
    UidExt,
    /// A blank or oversized `inserter`.
    #[display("inserter")]
    Inserter,
    /// A blank or oversized `matcher`.
    #[display("matcher")]
    Matcher,
    /// A negative `mm`.
    #[display("mm")]
    Mm,
    /// An EID-level ext that is not an object or is oversized.
    #[display("eid ext")]
    EidExt,
    /// An entry left with no UID.
    #[display("entry without uid")]
    EmptyEntry,
    /// An entry equal to an earlier kept entry of its source.
    #[display("duplicate entry")]
    DuplicateEntry,
    /// An entry past the source's first 8, or with no room left for a UID.
    #[display("entry over the limit")]
    ExtraEntry,
    /// A UID past the source's first 32.
    #[display("uid over the limit")]
    ExtraUid,
}

/// One dropped part, for the coordinator's log line (provider, source and
/// part only, never a value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryDrop {
    pub(crate) source: SourceDomain,
    pub(crate) part: DroppedPart,
}

/// One source's new entry set, as it is stored, and its timing resolved
/// against `now`.
#[derive(Clone, PartialEq)]
pub(crate) struct EntrySet {
    pub(crate) entries: Vec<KvEid>,
    pub(crate) expires_at: Option<UnixTime>,
    pub(crate) refresh_at: Option<UnixTime>,
    pub(crate) refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for EntrySet {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("EntrySet")
            .field("entries", &self.entries.len())
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .field("refresh_until", &self.refresh_until)
            .finish()
    }
}

/// A result's entries, grouped by source and checked against spec 4.5.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EntrySets {
    /// Each source with at least one kept entry, under every limit.
    pub(crate) sets: BTreeMap<SourceDomain, EntrySet>,
    /// What the limits dropped, in order.
    pub(crate) drops: Vec<EntryDrop>,
    /// Sources whose set was over `MAX_ENTRY_SET_BYTES` and was discarded
    /// whole. A stateful provider's positive part is then rejected.
    pub(crate) oversized: Vec<SourceDomain>,
}

impl EntrySets {
    /// Whether the result named at least one source and every set was
    /// discarded. Every entry that is not kept records a drop, so a result
    /// with entries and no kept set always has a drop or an oversized set.
    /// The coordinator handles such a result as `Malformed` (spec 4.5).
    #[must_use]
    pub(crate) fn all_discarded(&self) -> bool {
        self.sets.is_empty() && (!self.drops.is_empty() || !self.oversized.is_empty())
    }
}

fn earliest(current: Option<UnixTime>, candidate: Option<UnixTime>) -> Option<UnixTime> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (current, candidate) => current.or(candidate),
    }
}

fn clean_text(value: Option<&str>, part: DroppedPart, dropped: &mut Vec<DroppedPart>) -> Option<String> {
    let value = value?;
    if value.trim().is_empty() || value.len() > MAX_EID_FIELD_BYTES {
        dropped.push(part);
        return None;
    }
    Some(value.to_owned())
}

fn clean_number(value: Option<i32>, part: DroppedPart, dropped: &mut Vec<DroppedPart>) -> Option<i32> {
    let value = value?;
    if value < 0 {
        dropped.push(part);
        return None;
    }
    Some(value)
}

fn clean_ext(
    value: Option<&serde_json::Value>,
    part: DroppedPart,
    dropped: &mut Vec<DroppedPart>,
) -> Option<serde_json::Value> {
    let value = value?;
    if !value.is_object() || !ext_fits(value) {
        dropped.push(part);
        return None;
    }
    Some(value.clone())
}

fn clean_uid(uid: &ResolvedUid, dropped: &mut Vec<DroppedPart>) -> Option<KvUid> {
    if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
        dropped.push(DroppedPart::Uid);
        return None;
    }
    Some(KvUid {
        id: uid.id.clone(),
        atype: clean_number(uid.atype, DroppedPart::Atype, dropped),
        ext: clean_ext(uid.ext.as_ref(), DroppedPart::UidExt, dropped),
    })
}

fn clean_entry(eid: &ResolvedEid, dropped: &mut Vec<DroppedPart>) -> Option<KvEid> {
    let uids: Vec<KvUid> = eid.uids.iter().filter_map(|uid| clean_uid(uid, dropped)).collect();
    if uids.is_empty() {
        return None;
    }
    Some(KvEid {
        uids,
        inserter: clean_text(eid.inserter.as_deref(), DroppedPart::Inserter, dropped),
        matcher: clean_text(eid.matcher.as_deref(), DroppedPart::Matcher, dropped),
        mm: clean_number(eid.mm, DroppedPart::Mm, dropped),
        ext: clean_ext(eid.ext.as_ref(), DroppedPart::EidExt, dropped),
    })
}

/// Groups `eids` by source, in order, and applies every limit of spec 4.5:
/// the browser parser's field and UID rules, then per-source caps the
/// browser parser does not have: duplicates, the first 8 entries and 32 UIDs
/// of a source, and `MAX_ENTRY_SET_BYTES` as [`entry_set_bytes`] measures
/// the stored set. A set over that is discarded whole, never trimmed: field
/// caps do not guarantee the set cap. Each set's `exp`, `ra` and `ru` are
/// the earliest its kept entries set; a dropped or discarded entry's times
/// do not count.
///
/// Source claims and operator narrowing are the coordinator's, which drops
/// a set for an unclaimed source before using the result.
///
/// # Performance
///
/// Serializes each set once to measure it, and compares each entry with
/// the at most 8 entries already kept.
#[must_use]
pub(crate) fn entry_sets(eids: &[ResolvedEid], now: UnixTime) -> EntrySets {
    let mut result = EntrySets::default();
    for eid in eids {
        let mut dropped = Vec::new();
        let cleaned = clean_entry(eid, &mut dropped);
        let set = result.sets.entry(eid.source.clone()).or_insert_with(|| EntrySet {
            entries: Vec::new(),
            expires_at: None,
            refresh_at: None,
            refresh_until: None,
        });
        match cleaned {
            None => dropped.push(DroppedPart::EmptyEntry),
            Some(entry) if set.entries.contains(&entry) => dropped.push(DroppedPart::DuplicateEntry),
            Some(_) if set.entries.len() >= MAX_ENTRIES_PER_SOURCE => dropped.push(DroppedPart::ExtraEntry),
            Some(mut entry) => {
                let used: usize = set.entries.iter().map(|kept| kept.uids.len()).sum();
                let room = MAX_UIDS_PER_SOURCE.saturating_sub(used);
                if entry.uids.len() > room {
                    dropped.extend(iter::repeat_n(DroppedPart::ExtraUid, entry.uids.len() - room));
                    entry.uids.truncate(room);
                }
                if entry.uids.is_empty() {
                    dropped.push(DroppedPart::ExtraEntry);
                } else {
                    set.expires_at = earliest(set.expires_at, eid.expiry.map(|when| when.resolve(now)));
                    set.refresh_at = earliest(set.refresh_at, eid.refresh.map(|when| when.resolve(now)));
                    set.refresh_until = earliest(set.refresh_until, eid.refresh_until);
                    set.entries.push(entry);
                }
            }
        }
        result
            .drops
            .extend(dropped.into_iter().map(|part| EntryDrop { source: eid.source.clone(), part }));
    }
    result.sets.retain(|source, set| {
        if set.entries.is_empty() {
            return false;
        }
        let fits = entry_set_bytes(&set.entries) <= MAX_ENTRY_SET_BYTES;
        if !fits {
            result.oversized.push(source.clone());
        }
        fits
    });
    result
}
```

`retain` borrows `result.sets` mutably while the closure pushes to `result.oversized`. The two are disjoint fields, and closures capture disjoint fields since edition 2021, so the borrows do not conflict.

`entry_sets` does not apply the expiring-source fallback (an expiring set without `exp` keeps the stored `exp` when its entries are unchanged, and is discarded otherwise). That needs the descriptor and the stored record, so it belongs to the coordinator (Task 20). The coordinator also:

- ignores `refresh_until` from a provider not declared `with_private_state()`
- rejects a stateful result's positive part when `oversized` is not empty, keeping its removals
- handles a result with `all_discarded()` as `Malformed`, keeping its removals, and otherwise leaves the stored set of a source not in `sets` as it is (spec 4.5)
- logs each `EntryDrop` and oversized source with the provider ID, never a value

An `EntrySet`'s `entries` are what Task 15's mutation stores as the record's `eids` (Task 6).

In `input.rs`, add `use super::exchange::ProviderResponse;` to the import block and append `CaptureInput` after `ResolveInput`:

```rust
/// What a capture may read from one proxied response.
pub struct CaptureInput<'a> {
    descriptor: &'a ProviderDescriptor,
    path: &'a str,
    response: &'a ProviderResponse,
    stored: &'a BTreeMap<SourceDomain, StoredId>,
    consent: &'a ProviderConsent,
    publisher_domain: &'a str,
    now: UnixTime,
}

impl fmt::Debug for CaptureInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureInput")
            .field("provider", self.descriptor.id())
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<'a> CaptureInput<'a> {
    /// The input for one capture.
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        path: &'a str,
        response: &'a ProviderResponse,
        stored: &'a BTreeMap<SourceDomain, StoredId>,
        consent: &'a ProviderConsent,
        publisher_domain: &'a str,
        now: UnixTime,
    ) -> Self {
        Self { descriptor, path, response, stored, consent, publisher_domain, now }
    }

    /// The proxied request path, without query string.
    #[must_use]
    pub fn path(&self) -> &str {
        self.path
    }

    /// The proxied response, decoded.
    #[must_use]
    pub fn response(&self) -> &ProviderResponse {
        self.response
    }

    /// The descriptor's primary source.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        self.descriptor.primary_source()
    }

    /// Finds a declared source by a vendor's spelling.
    #[must_use]
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain> {
        self.descriptor.source_named(name)
    }

    /// The stored record for a source.
    #[must_use]
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId> {
        self.stored.get(source)
    }

    /// The request's consent, from its cookies and headers. The request
    /// body is never read (spec 4.7).
    #[must_use]
    pub fn consent(&self) -> &ProviderConsent {
        self.consent
    }

    /// The publisher's domain.
    #[must_use]
    pub fn publisher_domain(&self) -> &str {
        self.publisher_domain
    }

    /// The request's time.
    #[must_use]
    pub fn now(&self) -> UnixTime {
        self.now
    }
}
```

`CaptureInput::new` takes 7 arguments, which is the maximum `AGENTS.md` allows. Do not add an eighth. Use a struct if you need one.

Add `exchange.rs` to `mod.rs` (`pub mod exchange; pub use exchange::{ProviderRequest, ProviderResponse};`), `pub use resolution::{Resolution, ResolvedEid, ResolvedUid, StateChange, When};`, and add `CaptureInput` to the `pub use input::{...}` line. Add `exchange.rs` to `migration_guards.rs`.

- [ ] **Step 5: Run and see them pass**

Run: `cargo test-fastly -- ec::identity`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add provider requests, responses and resolutions"
```

### Task 11: The `IdentityProvider` trait

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/provider.rs`
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `crates/trusted-server-core/src/ec/identity/provider.rs`

**Interfaces:**

- Consumes: Tasks 8 to 10
- Produces: `pub trait IdentityProvider: Send + Sync` with `descriptor`, `request`, `response` and `capture` exactly as spec 4.2 shows, and `pub trait IdentityProviderConfig: IntegrationConfig { fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>; }`. Also the compile-time guard `const _: Option<&dyn IdentityProvider> = None;`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use http::Method;

    use super::*;
    use crate::ec::identity::descriptor::Captures;
    use crate::ec::identity::exchange::ProviderResponse;
    use crate::ec::identity::ids::UnixTime;
    use crate::ec::identity::input::{CaptureInput, ProviderConsent};

    struct DescriptorOnly(ProviderDescriptor);

    impl IdentityProvider for DescriptorOnly {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    #[test]
    fn default_capability_methods_return_the_not_implemented_marker() {
        let provider = DescriptorOnly(
            ProviderDescriptor::builder("p".parse().expect("should parse the provider id"))
                .with_source("example.com".parse().expect("should parse the source"))
                .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                .build()
                .expect("should build"),
        );
        let boxed: Box<dyn IdentityProvider> = Box::new(provider);
        let response = ProviderResponse::for_test(http::StatusCode::OK, b"{}");
        let consent = ProviderConsent::default();
        let stored = BTreeMap::new();
        let input = CaptureInput::new(
            boxed.descriptor(),
            "/integrations/p/api",
            &response,
            &stored,
            &consent,
            "www.example.com",
            UnixTime::from_secs(1),
        );

        let error = boxed.capture(&input).expect_err("should return the not-implemented marker");
        assert!(error.current_context().is_not_implemented(), "should carry the marker");
        assert!(
            !IdentityProviderError::config("other").current_context().is_not_implemented(),
            "should not treat an ordinary Config error as the marker"
        );
    }
}
```

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- ec::identity::provider`
Expected: compile error.

- [ ] **Step 3: Implement**

Copy the trait and config trait verbatim from spec 4.2, with these imports:

```rust
//! The identity provider trait.

use error_stack::Report;

use super::descriptor::ProviderDescriptor;
use super::exchange::{ProviderRequest, ProviderResponse};
use super::input::{CaptureInput, ResolveInput};
use super::resolution::{IdentityProviderError, Resolution};
use crate::settings::IntegrationConfig;

// ... trait IdentityProvider and trait IdentityProviderConfig exactly as spec 4.2 ...

// Keeps the trait object-safe. Fails to compile if a method gains generics.
const _: Option<&dyn IdentityProvider> = None;
```

`IntegrationConfig` is `crate::settings::IntegrationConfig` (`settings.rs:235`). Write `OpenRTB`, `LiveRamp` and other mixed-case names in backticks in the copied doc comments (`clippy::doc_markdown`). Add `pub mod provider; pub use provider::{IdentityProvider, IdentityProviderConfig};` to `mod.rs`, and the file to `migration_guards.rs`.

- [ ] **Step 4: Run and see it pass**

Run: `cargo test-fastly -- ec::identity` and `cargo doc --no-deps --all-features -p trusted-server-core`
Expected: PASS, with no broken intra-doc links.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add the IdentityProvider trait"
```

### Task 12: Consent policy evaluation with the signals TS decodes today

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/policy.rs`
- Modify: `crates/trusted-server-core/src/consent/mod.rs` (make `effective_tcf` `pub(crate)`, visibility only)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `policy.rs`

**Interfaces:**

- Consumes: `ConsentPolicy` (Task 8), `ConsentContext`, the decoded forms `TcfConsent` (with `has_purpose_consent(usize)` and `has_vendor_consent(u16)`, `consent/types.rs:230-252`), `GppConsent` (with `eu_tcf` and `us_sale_opt_out: Option<bool>`) and `UsPrivacy` (with `opt_out_sale: PrivacyFlag`)
- Produces:
  - `pub(crate) struct PolicySignals { dnt: bool }`. DNT is not in `ConsentContext`, so the caller passes it.
  - `pub(crate) fn policy_allows(policy: &ConsentPolicy, consent: &ConsentContext, signals: PolicySignals) -> bool`
  - `pub(crate) fn tcf_applies(consent: &ConsentContext) -> bool`, the same test `consent_allows_server_side_auction` uses (`consent/mod.rs:298-308`)
  - a private `fn us_sale_opt_out(consent: &ConsentContext) -> bool` in `policy.rs`: the GPP US sale opt-out or the US Privacy sale flag, the same two checks `allows_ec_creation` makes (`consent/mod.rs:539-547`). No consent function changes behaviour

Vendor consent needs no new helper: `TcfConsent::has_vendor_consent` already exists (`consent/types.rs:250-252`).

Part 2 (Task 34) extends `policy_allows` with the sharing and targeted-advertising opt-outs, GPP section 6, and the GPC and known-child fields. Until then, a strict policy blocks on the signals already decoded: TCF purposes and vendor, the US sale opt-out (GPP and US Privacy), `Sec-GPC`, DNT, and any raw string present without a decoded form.

A GPP string that decodes but has no applicable section, such as the header-only `DBAA` a CMP sends when no section applies, has a decoded form and carries no opt-out, so it does not block `strict()` by itself (spec 9.1). Where TCF applies, the policy still needs TCF consent, from a TC string or from a GPP TCF section. Task 34 keeps both rules.

- [ ] **Step 1: Write the failing tests**

Build `ConsentContext` values the way existing consent tests do (`grep -n "fn .*consent_context\|ConsentContext {" crates/trusted-server-core/src/consent/mod.rs | head`). Reuse their helper. Then:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::descriptor::{ConsentPolicy, UsOptOut};

    #[test]
    fn strict_blocks_each_signal_it_decodes() {
        let strict = ConsentPolicy::strict();
        let allowed = consent_with(Signals::default());
        assert!(policy_allows(&strict, &allowed, PolicySignals { dnt: false }), "should allow a request with no signal set");

        for (name, signals, dnt) in [
            ("gdpr without purpose 4", Signals { gdpr: true, purposes: vec![1], ..Signals::default() }, false),
            ("us sale opt-out", Signals { us_sale_opt_out: true, ..Signals::default() }, false),
            ("gpc", Signals { gpc: true, ..Signals::default() }, false),
            ("dnt", Signals::default(), true),
            ("undecodable tc string", Signals { raw_tc_without_decode: true, ..Signals::default() }, false),
        ] {
            assert!(
                !policy_allows(&strict, &consent_with(signals), PolicySignals { dnt }),
                "should block under strict: {name}"
            );
        }
    }

    #[test]
    fn relaxations_and_vendor_consent() {
        let gpc = consent_with(Signals { gpc: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict().allowing_gpc(), &gpc, PolicySignals { dnt: false }),
            "should allow GPC when the policy relaxes it"
        );
        let sale = consent_with(Signals { us_sale_opt_out: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sale), &sale, PolicySignals { dnt: false }),
            "should allow a sale opt-out when the policy relaxes it"
        );
        let gdpr = consent_with(Signals { gdpr: true, purposes: vec![1, 4], vendors: vec![9999], ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict().with_tcf_vendor(9999), &gdpr, PolicySignals { dnt: false }),
            "should allow a granted vendor"
        );
        assert!(
            !policy_allows(&ConsentPolicy::strict().with_tcf_vendor(1234), &gdpr, PolicySignals { dnt: false }),
            "should block a vendor without consent"
        );
    }

    #[test]
    fn eid_persistence_only_ignores_provider_signals() {
        let consent = consent_with(Signals { gpc: true, us_sale_opt_out: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::eid_persistence_only(), &consent, PolicySignals { dnt: true }),
            "should check only the global gates for generic pull's parity policy, which the coordinator applies separately"
        );
    }

    #[test]
    fn a_header_only_gpp_string_does_not_block_strict() {
        let header_only = consent_with(Signals { gpp_header_only: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict(), &header_only, PolicySignals { dnt: false }),
            "should not block on a decoded GPP string with no applicable section"
        );

        let under_gdpr = consent_with(Signals { gdpr: true, gpp_header_only: true, ..Signals::default() });
        assert!(
            !policy_allows(&ConsentPolicy::strict(), &under_gdpr, PolicySignals { dnt: false }),
            "should still need TCF consent where TCF applies"
        );

        let gpp_tcf = consent_with(Signals { gdpr: true, purposes: vec![1, 4], tcf_in_gpp: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict(), &gpp_tcf, PolicySignals { dnt: false }),
            "should accept TCF consent from a GPP TCF section"
        );
    }
}
```

`Signals` and `consent_with` are a small test helper you write in this module. It builds a `ConsentContext` with the given TCF purposes and vendor consents decoded, the US sale opt-out decoded from a US Privacy string `1YYN`, `gpc` set, and, for `raw_tc_without_decode`, `raw_tc_string = Some(..)` with no decoded TCF. `gdpr: true` sets `gdpr_applies` and `Jurisdiction::Gdpr`; otherwise the jurisdiction is `Jurisdiction::UsState("CA".to_owned())`, because `tcf_applies` treats `Unknown` as TCF applying. `gpp_header_only` sets `raw_gpp_string = Some("DBAA")` and `gpp` to `decode_gpp_string("DBAA")`, with `gpp_section_ids = Some(vec![])`. `tcf_in_gpp` puts the decoded TCF in `gpp.eu_tcf` instead of `tcf`, with `raw_tc_string` unset. Build it on whatever constructor the consent tests use. Read `consent/types.rs` first. If `decode_gpp_string("DBAA")` fails with the pinned `iab_gpp`, stop: the fail-closed rule would then block it, and the spec 9.1 rule needs a decoder change that this task does not make. Report it instead of working around it.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::policy`
Expected: compile errors.

- [ ] **Step 3: Implement**

`policy.rs`:

```rust
//! Evaluates a provider's [`ConsentPolicy`] against a request's consent.

use super::descriptor::{ConsentPolicy, UsOptOut};
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::PrivacyFlag;
use crate::consent::{ConsentContext, effective_tcf};

/// Signals the consent context does not carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PolicySignals {
    /// `DNT: 1` was present.
    pub(crate) dnt: bool,
}

/// Where TCF applies: `gdpr_applies`, or a jurisdiction of GDPR or unknown.
#[must_use]
pub(crate) fn tcf_applies(consent: &ConsentContext) -> bool {
    consent.gdpr_applies || matches!(consent.jurisdiction, Jurisdiction::Gdpr | Jurisdiction::Unknown)
}

/// Whether the request carries a decoded US sale opt-out, from GPP or from
/// the US Privacy string.
fn us_sale_opt_out(consent: &ConsentContext) -> bool {
    consent.gpp.as_ref().and_then(|gpp| gpp.us_sale_opt_out) == Some(true)
        || consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == PrivacyFlag::Yes)
}

/// Whether `policy` allows a provider under this request's consent. The
/// global EC and EID-persistence gates are checked separately (eligibility
/// steps 1 and 2) and are not repeated here.
#[must_use]
pub(crate) fn policy_allows(policy: &ConsentPolicy, consent: &ConsentContext, signals: PolicySignals) -> bool {
    if policy.is_eid_persistence_only() {
        return true;
    }
    // Fail closed: a raw string with no decoded form blocks.
    if (consent.raw_tc_string.is_some() && consent.tcf.is_none())
        || (consent.raw_gpp_string.is_some() && consent.gpp.is_none())
        || (consent.raw_us_privacy.is_some() && consent.us_privacy.is_none())
    {
        return false;
    }
    if tcf_applies(consent) {
        // A TC string or a GPP TCF section. A header-only GPP string has
        // neither, so TCF consent is still missing.
        let Some(tcf) = effective_tcf(consent) else {
            return false;
        };
        if !policy
            .tcf_purposes()
            .iter()
            .all(|purpose| tcf.has_purpose_consent(usize::from(*purpose)))
        {
            return false;
        }
        if let Some(vendor) = policy.tcf_vendor()
            && !tcf.has_vendor_consent(vendor)
        {
            return false;
        }
    }
    if !policy.allows_us_opt_out(UsOptOut::Sale) && us_sale_opt_out(consent) {
        return false;
    }
    if !policy.allows_gpc() && consent.gpc {
        return false;
    }
    if !policy.allows_dnt() && signals.dnt {
        return false;
    }
    true
}
```

`effective_tcf` (`consent/mod.rs:283-288`) prefers `tcf` and falls back to `gpp.eu_tcf`, as the auction's consent checks do. Change only its visibility to `pub(crate)`. The fail-closed check needs no special case for `DBAA`: it decodes, so `consent.gpp` is `Some` and nothing blocks. The field names above (`raw_tc_string`, `tcf`, `raw_gpp_string`, `gpp`, `raw_us_privacy`, `us_privacy`, `gpc`, `gdpr_applies`, `jurisdiction`) are `ConsentContext`'s (`consent/types.rs:109-148`). `has_purpose_consent` takes a `usize`, so the `u8` purposes convert with `usize::from`. `us_sale_opt_out` reads the decoded `GppConsent::us_sale_opt_out` and `UsPrivacy::opt_out_sale`; it does not call the private `decode_us_sale_opt_out` (`gpp.rs:125`), which takes the parsed GPP string. Do not change any existing consent function's behaviour.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::policy` and `cargo test-fastly -- consent::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/consent/mod.rs crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Evaluate provider consent policies against decoded consent"
```

### Task 13: Revisions and hashes

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/revision.rs`
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `revision.rs`

**Interfaces:**

- Consumes: `KvPartnerId`, `KvEid`, `KvUid`, `RECORD_REVISION_HEX_LEN` (Task 6), `RandomSource` (Task 9)
- Produces:
  - `pub(crate) struct IdentityKeys`, derived once per settings load from `ec.passphrase`, with `pub(crate) fn from_settings(settings: &Settings) -> Self`. It holds one HMAC key, `visitor` (the rate-limit key), with the label `trusted-server/identity/visitor/v1`. Nothing stored in the row is keyed (spec 9.3), so this is the only key.
  - `pub(crate) struct RecordRevision(String)`: 16 lowercase hex digits, with `as_str` and `from_stored(&str)`
  - `pub(crate) enum Revision { Unset, Set(RecordRevision) }` (spec 5.4): a record's revision, `Unset` for a record without `rv`, which only a version 1 row holds
  - `pub(crate) fn new_revision(random: &dyn RandomSource) -> RecordRevision`: 8 random bytes as hex
  - `pub(crate) fn record_revision(record: &KvPartnerId) -> Revision`: `Set` of `rv` when present, else `Unset`. One `rv` covers a source's whole entry set (spec 5.4).
  - `pub(crate) struct StateRevision(String)` with `pub(crate) fn of(st: Option<&serde_json::Value>) -> Self`: the first 16 hex digits of the unkeyed SHA-256 of `st` as compact JSON, or of the empty string when there is none. A `ProviderResult`'s `expected_state` (Task 15) holds one when its call read `ps.st`.
  - `pub(crate) fn uid_hash(uid: &str) -> String`: the first 16 hex digits of the unkeyed SHA-256 of the UID (spec 6.1)
  - `pub(crate) fn set_hashes(set: &[KvEid]) -> Vec<String>`: one `uid_hash` per UID of the set, in set order, without duplicates. A removal hashes every UID of the record it removes (spec 5.4), so a one-UID record costs one hash and a 32-UID set 32. The hashes come from the live record when the removal applies (Task 15).
  - `pub(crate) fn push_hashes(list: &mut Vec<String>, hashes: Vec<String>)`: appends each hash to a `ps.iv` or `ps.ov` list, moving a hash already present to the end (spec 6.1). It does not truncate; Task 15's step 4 does.
  - `pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String`: 16 hex

The UID hash is unkeyed because the row stores every live UID in plain text, so a key would protect nothing, and an unkeyed hash survives an `ec.passphrase` rotation (spec 6.1, 9.3). It is not namespaced by provider: `ps.iv` and `ps.ov` already sit under a provider. There is no keyed legacy revision either. A version 1 record has the revision `Unset`, and an `OwnerRemove` naming it matches `Unset` until the record's next write (spec 14). The consent fingerprint is a plain bitmask that Task 20 builds, not a hash.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::test_support::tests::create_test_settings;

    fn uid(id: &str) -> KvUid {
        KvUid { id: id.to_owned(), ..KvUid::default() }
    }

    #[test]
    fn new_revisions_are_hex_and_differ() {
        let random = SeededRandom::new(7);
        let first = new_revision(&random);
        let second = new_revision(&random);
        assert_eq!(first.as_str().len(), 16, "should be 16 hex digits");
        assert!(
            first.as_str().bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "should be lowercase hex"
        );
        assert_ne!(first, second, "should give two writes of the same UID different revisions");
    }

    #[test]
    fn record_revision_is_rv_or_unset() {
        let with_rv = KvPartnerId { rv: Some("0123456789abcdef".to_owned()), ..KvPartnerId::new("u") };
        assert_eq!(
            record_revision(&with_rv),
            Revision::Set(RecordRevision::from_stored("0123456789abcdef")),
            "should use rv"
        );
        assert_eq!(
            record_revision(&KvPartnerId::browser("u", Some(10))),
            Revision::Unset,
            "should be Unset without rv, whatever the UID and at"
        );
    }

    #[test]
    fn uid_hashes_are_unkeyed_sha256_prefixes() {
        let expected = hex::encode(&Sha256::digest(b"example-uid")[..8]);
        assert_eq!(uid_hash("example-uid"), expected, "should be the first 16 hex digits of SHA-256");

        let mut other_settings = create_test_settings();
        other_settings.ec.passphrase = crate::redacted::Redacted::new("another-example-passphrase-value".to_owned());
        assert_ne!(
            visitor_key(&IdentityKeys::from_settings(&other_settings), "ec"),
            visitor_key(&IdentityKeys::from_settings(&create_test_settings()), "ec"),
            "should key only the visitor key by the passphrase"
        );
    }

    #[test]
    fn a_removal_hashes_every_uid_of_the_set_and_push_moves_repeats_to_the_end() {
        let set = vec![
            KvEid { uids: vec![uid("a"), uid("b")], ..KvEid::default() },
            KvEid { uids: vec![uid("c"), uid("a")], ..KvEid::default() },
        ];

        let hashes = set_hashes(&set);

        let expected: Vec<String> = ["a", "b", "c"].iter().map(|id| uid_hash(id)).collect();
        assert_eq!(hashes, expected, "should hash each UID once, in set order");
        assert_eq!(
            set_hashes(&KvPartnerId::new("u").eids),
            vec![uid_hash("u")],
            "should cost one hash for a one-UID record"
        );

        let mut list = vec![uid_hash("a"), uid_hash("z")];
        push_hashes(&mut list, vec![uid_hash("a"), uid_hash("y")]);
        assert_eq!(list, vec![uid_hash("z"), uid_hash("a"), uid_hash("y")], "should move a repeat to the end");
    }

    #[test]
    fn state_revisions_tell_absent_and_changed_state_apart() {
        let state = json!({ "token": "A0" });
        assert_eq!(StateRevision::of(Some(&state)), StateRevision::of(Some(&state.clone())), "should be stable");
        assert_ne!(
            StateRevision::of(Some(&state)),
            StateRevision::of(Some(&json!({ "token": "A1" }))),
            "should change with the state"
        );
        assert_ne!(StateRevision::of(None), StateRevision::of(Some(&json!(""))), "should tell absent from empty");
    }
}
```

Find the settings test helper with `grep -rn "fn create_test_settings" crates/trusted-server-core/src`, and fix the import path to match.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::revision`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Record revisions, state revisions, UID hashes and the visitor key.

use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

use super::input::RandomSource;
use crate::ec::kv_types::{KvEid, KvPartnerId};
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// The visitor key, derived from `ec.passphrase` once per settings load.
///
/// It is the only keyed value: nothing stored in the row is keyed
/// (spec 9.3).
#[derive(Clone)]
pub(crate) struct IdentityKeys {
    visitor: [u8; 32],
}

impl core::fmt::Debug for IdentityKeys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("IdentityKeys([redacted])")
    }
}

impl IdentityKeys {
    /// Derives the visitor key from `ec.passphrase`.
    #[must_use]
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let passphrase = settings.ec.passphrase.expose().as_bytes();
        let mut mac = HmacSha256::new_from_slice(passphrase).expect("should accept any HMAC key length");
        mac.update(b"trusted-server/identity/visitor/v1");
        Self { visitor: mac.finalize().into_bytes().into() }
    }
}

fn hex16(digest: &[u8]) -> String {
    hex::encode(&digest[..8])
}

/// A record's stored revision: 16 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RecordRevision(String);

impl RecordRevision {
    /// Wraps a stored `rv`. `KvEntry::validate` has checked the format.
    #[must_use]
    pub(crate) fn from_stored(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// The hex digits.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A record's revision (spec 5.4). `Unset` for a record without `rv`, which
/// only a schema version 1 row holds, until its next write gives it one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Revision {
    Unset,
    Set(RecordRevision),
}

/// A fresh random revision, drawn on every write of a record.
#[must_use]
pub(crate) fn new_revision(random: &dyn RandomSource) -> RecordRevision {
    let mut bytes = [0u8; 8];
    random.fill(&mut bytes);
    RecordRevision(hex::encode(bytes))
}

/// The revision of a stored record. One revision covers the source's whole
/// entry set.
#[must_use]
pub(crate) fn record_revision(record: &KvPartnerId) -> Revision {
    record.rv.as_deref().map_or(Revision::Unset, |rv| Revision::Set(RecordRevision::from_stored(rv)))
}

/// The hash of a provider's `ps.st` a call read, so a later apply can tell
/// whether another writer changed it since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateRevision(String);

impl StateRevision {
    /// The first 16 hex digits of the SHA-256 of `st` as compact JSON, or of
    /// the empty string when there is none. Compact JSON is never empty, so
    /// absent and present state never share a revision.
    #[must_use]
    pub(crate) fn of(st: Option<&serde_json::Value>) -> Self {
        let bytes = st.and_then(|value| serde_json::to_vec(value).ok()).unwrap_or_default();
        Self(hex16(&Sha256::digest(bytes)))
    }
}

/// The hash stored in `ps.iv` or `ps.ov` for a removed UID: the first 16 hex
/// digits of its unkeyed SHA-256 (spec 6.1).
#[must_use]
pub(crate) fn uid_hash(uid: &str) -> String {
    hex16(&Sha256::digest(uid.as_bytes()))
}

/// One hash per UID of a removed set, in set order, without duplicates.
#[must_use]
pub(crate) fn set_hashes(set: &[KvEid]) -> Vec<String> {
    let mut hashes = Vec::new();
    for uid in set.iter().flat_map(|entry| &entry.uids) {
        let hash = uid_hash(&uid.id);
        if !hashes.contains(&hash) {
            hashes.push(hash);
        }
    }
    hashes
}

/// Appends hashes to a `ps.iv` or `ps.ov` list, oldest first. A hash
/// already present moves to the end. The caller truncates.
pub(crate) fn push_hashes(list: &mut Vec<String>, hashes: Vec<String>) {
    for hash in hashes {
        list.retain(|existing| *existing != hash);
        list.push(hash);
    }
}

/// The per-visitor rate-limit key component, from the full EC ID.
#[must_use]
pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(&keys.visitor).expect("should accept a 32-byte key");
    mac.update(ec_id.as_bytes());
    hex16(&mac.finalize().into_bytes())
}
```

Check that `Settings.ec.passphrase` is `Redacted<String>` with `.expose()`. `pull_sync_marker.rs:293` uses exactly that. `sha2`, `hmac` and `hex` are already dependencies of core (check with `grep -n '^sha2\|^hmac\|^hex' crates/trusted-server-core/Cargo.toml`). Register the module in `mod.rs` (`pub(crate) mod revision;`) and in `migration_guards.rs`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::revision`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add record revisions, state revisions and UID hashes" -m "A removal hashes every UID of the record it removes with an unkeyed SHA-256, since the row already holds the UIDs in plain text. A record without rv has the revision Unset. Only the rate-limit visitor key is derived from ec.passphrase."
```

---

### Task 14: The provider catalog and the shared servability check

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/catalog.rs`
- Create: `crates/trusted-server-core/src/ec/identity/servable.rs`
- Create: `crates/trusted-server-core/src/ec/identity/test_support.rs`
- Modify: `crates/trusted-server-core/src/ec/eids.rs` (`ResolvedPartnerId`, `resolve_partner_ids`, `to_eids`, and the stale module comment at lines 1-5)
- Modify: `crates/trusted-server-core/src/openrtb.rs` (the `Eid` doc comment at about line 69 says KV-resolved entries leave the EID-level fields empty)
- Modify: callers of `resolve_partner_ids`: `auction/endpoints.rs:465`, `ec/admin.rs:485`
- Modify: `crates/trusted-server-core/src/ec/identify.rs` (`entry.ids` read at about lines 99-103)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `servable.rs`, `ec/eids.rs`, `ec/identify.rs`

**Interfaces:**

- Consumes: `KvPartnerId`, `KvEid`, `KvUid`, `first_uid()` (Task 6)
- Produces:
  - `pub(crate) struct ProviderCatalog` with `pub(crate) fn new(providers: Vec<Arc<dyn IdentityProvider>>, owners: BTreeMap<SourceDomain, IdentityProviderId>) -> Self`, `get(&IdentityProviderId) -> Option<&Arc<dyn IdentityProvider>>`, `providers() -> impl Iterator<Item = &Arc<dyn IdentityProvider>>`, `owner_of(&SourceDomain) -> Option<&IdentityProviderId>`, `is_configured(&IdentityProviderId) -> bool`, and `ProviderCatalog::empty()`. Part 1 builds it from generic pull providers only (Task 22), with no owners.
  - Narrowed sources (spec 10.1): `with_narrowing(self, narrowing: BTreeMap<IdentityProviderId, Vec<SourceDomain>>) -> Self` keeps, for each listed provider, only the listed sources its descriptor declares. `sources_of(&IdentityProviderId) -> &[SourceDomain]` returns the narrowed list, else the descriptor's. Every other accessor reads sources through it, so a provider never claims a source it was narrowed away from.
  - `claimants(&SourceDomain) -> impl Iterator<Item = &IdentityProviderId>`: every provider whose narrowed sources include the source. Task 28's `iv` and `ov` denials read the providers that claim a source through it.
  - `is_capture_only(&IdentityProviderId) -> bool` (a descriptor with `captures` and no `resolves`) and `capture_only_sources(&IdentityProviderId) -> &[SourceDomain]` (its narrowed sources, empty for any other provider). An opt-out of a capture-only provider also removes the records of these sources that no provider wrote (spec 5.4), and its sticky `ps.o` denies the browser's value for them (spec 7.1).
  - `pub(crate) struct ServeContext<'a> { now: UnixTime, consent: &'a ConsentContext, signals: PolicySignals, catalog: &'a ProviderCatalog, opted_out: &'a BTreeSet<IdentityProviderId> }`
  - `pub(crate) fn is_servable(context: &ServeContext<'_>, source: &SourceDomain, record: &KvPartnerId) -> bool`. A record is the source's whole entry set, so the set is servable or not as a whole. No check looks at one entry or one UID.
  - `pub(crate) fn resolve_partner_ids(registry: &PartnerRegistry, entry: &KvEntry, serve: &ServeContext<'_>) -> Vec<ResolvedPartnerId>`. The new third argument is required. It drops from `pub` to `pub(crate)`: its only callers are in core (`auction/endpoints.rs:465`, `ec/admin.rs:485`), and a `pub` function naming the crate-private `ServeContext` trips `private_interfaces` under `-D warnings`. `ResolvedPartnerId` keeps `source_domain`, `uid` (the first UID of the first stored entry, as before for a one-UID record) and `openrtb_atype` (the partner's), and gains `entries: Vec<KvEid>` (the record's `eids`, one entry for a one-UID record) and `writer: Option<IdentityProviderId>`.
  - `to_eids` emits one `Eid` per stored entry, in stored order. Each UID uses its stored `atype`, else `openrtb_atype`, and its stored UID-level `ext`. Each entry keeps its stored `inserter`, `matcher`, `mm` and EID-level `ext`. A one-UID record without `atype` or `ext`, which is every record on `main`, gives exactly the `Eid` it gives today. Task 28 fills the EID-level fields of a provider-written entry that sets none of them from `with_provenance`.
  - `x-ts-eids` truncation stays per entry, as today: `encode_eids_header` (`ec/eids.rs:106`) and `build_eids_header` are unchanged. The header also carries the browser's entries, whose truncation must not change, so it can hold part of a stored set; `user.eids` always holds the set whole (spec 7.1).
  - `ec/identity/test_support.rs`, declared as `#[cfg(any(test, feature = "test-utils"))] pub mod test_support;`: `test_consent_allowing_everything`, `test_consent_gdpr_without_vendor`, `example_entry_set() -> Vec<KvEid>` (spec 6.1's two entries and three UIDs: `example-a` and `example-b` with `inserter` and `mm`, then `example-c` with `matcher`) and `full_record(writer: Option<&str>, set: Vec<KvEid>) -> KvPartnerId`, all `pub`. Later tasks add to it.

The rule (spec 6.1 and 7.1), applied in this order:

1. A record with an empty `eids` is not servable. `KvEntry::validate` never lets one through, so this only guards a caller that builds a record by hand.
2. `exp` at or before `now` is not servable. The set's `exp` is the earliest any of its entries set (spec 4.5).
3. A record with a writer `w`:
   - The writer must be in the catalog. A disabled or unconfigured writer means not servable.
   - The writer must not be in `opted_out`: the providers with a `ps.o` in the row the request read. The caller computes it from KV only (spec 7.1).
   - The writer's consent policy must pass under this request's consent (`policy_allows`).
4. An owned source (the catalog has an owner):
   - The owner's policy must pass.
   - If the owner's descriptor marks the source as expiring, the record must have `w == owner` and a set, unexpired `exp`. This is the rollback rule.
5. Anything else, meaning no writer and an unowned source, is servable. The caller already applied the generic gates.

- [ ] **Step 1: Write the failing tests**

In `servable.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use super::*;
    use crate::ec::identity::descriptor::{Captures, ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::identity::test_support::{
        example_entry_set, full_record, test_consent_allowing_everything, test_consent_gdpr_without_vendor,
    };
    use crate::ec::kv_types::KvPartnerId;

    struct Fixed(ProviderDescriptor);
    impl IdentityProvider for Fixed {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    fn provider(id: &str, source: &str, policy: ConsentPolicy, expiring: bool) -> Arc<dyn IdentityProvider> {
        let mut builder = ProviderDescriptor::builder(id.parse().expect("should parse the provider ID"))
            .with_source(source.parse().expect("should parse the source"))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com"))
            .with_consent_policy(policy);
        if expiring {
            builder = builder.with_expiring_ids();
        }
        Arc::new(Fixed(builder.build().expect("should build")))
    }

    fn record(writer: Option<&str>, exp: Option<u64>) -> KvPartnerId {
        KvPartnerId { w: writer.map(str::to_owned), exp, ..KvPartnerId::new("u") }
    }

    fn check(catalog: &ProviderCatalog, opted_out: &BTreeSet<IdentityProviderId>, source: &str, record: &KvPartnerId) -> bool {
        let consent = test_consent_allowing_everything();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog,
            opted_out,
        };
        is_servable(&context, &source.parse().expect("should parse the source"), record)
    }

    #[test]
    fn expiry_writer_presence_and_opt_out() {
        let catalog = ProviderCatalog::new(
            vec![provider("p", "a.example.com", ConsentPolicy::eid_persistence_only(), false)],
            BTreeMap::new(),
        );
        let none = BTreeSet::new();
        assert!(check(&catalog, &none, "a.example.com", &record(Some("p"), Some(101))), "should serve a valid record");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("p"), Some(100))), "should treat now == exp as expired");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("gone"), None)), "should not serve an unknown writer");
        let opted: BTreeSet<_> = ["p".parse().expect("should parse the provider ID")].into_iter().collect();
        assert!(!check(&catalog, &opted, "a.example.com", &record(Some("p"), None)), "should not serve an opted-out writer");
        // `an_opted_out_owner_stops_every_record_of_its_source`: with `owner`
        // owning a.example.com and opted out, a browser-seeded record of
        // a.example.com is not served; with no opt-out it is.
        assert!(check(&catalog, &none, "b.example.com", &record(None, None)), "should serve an unowned browser record");
    }

    #[test]
    fn a_full_record_is_servable_or_not_as_a_whole() {
        let catalog = ProviderCatalog::new(
            vec![provider("p", "a.example.com", ConsentPolicy::eid_persistence_only(), false)],
            BTreeMap::new(),
        );
        let none = BTreeSet::new();
        let valid = KvPartnerId { exp: Some(101), ..full_record(Some("p"), example_entry_set()) };
        let expired = KvPartnerId { exp: Some(100), ..full_record(Some("p"), example_entry_set()) };
        assert!(check(&catalog, &none, "a.example.com", &valid), "should serve the whole valid set");
        assert!(!check(&catalog, &none, "a.example.com", &expired), "should never serve an expired set");
    }

    #[test]
    fn rollback_rule_for_expiring_owned_sources() {
        let owner: IdentityProviderId = "owner".parse().expect("should parse the provider ID");
        let catalog = ProviderCatalog::new(
            vec![provider("owner", "a.example.com", ConsentPolicy::eid_persistence_only(), true)],
            [("a.example.com".parse().expect("should parse the source"), owner)].into_iter().collect(),
        );
        let none = BTreeSet::new();
        assert!(check(&catalog, &none, "a.example.com", &record(Some("owner"), Some(200))), "should serve the owner's value");
        assert!(!check(&catalog, &none, "a.example.com", &record(None, None)), "should never serve a browser-seeded value from the server");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("owner"), None)), "should not serve a value with a stripped exp");
    }

    #[test]
    fn narrowing_limits_claims_and_capture_only_sources() {
        let capture_only: Arc<dyn IdentityProvider> = Arc::new(Fixed(
            ProviderDescriptor::builder("c".parse().expect("should parse the provider ID"))
                .with_source("a.example.com".parse().expect("should parse the source"))
                .with_source("b.example.com".parse().expect("should parse the source"))
                .with_captures(Captures::new().with_path_prefix("/integrations/c/"))
                .build()
                .expect("should build"),
        ));
        let resolver = provider("p", "a.example.com", ConsentPolicy::eid_persistence_only(), false);
        let a: SourceDomain = "a.example.com".parse().expect("should parse the source");
        let b: SourceDomain = "b.example.com".parse().expect("should parse the source");
        let c: IdentityProviderId = "c".parse().expect("should parse the provider ID");
        let p: IdentityProviderId = "p".parse().expect("should parse the provider ID");

        let catalog = ProviderCatalog::new(vec![capture_only, resolver], BTreeMap::new())
            .with_narrowing([(c.clone(), vec![b.clone()])].into_iter().collect());

        assert_eq!(catalog.sources_of(&c), &[b.clone()], "should narrow the capture-only provider");
        assert!(catalog.is_capture_only(&c) && !catalog.is_capture_only(&p), "should tell capture-only apart");
        assert_eq!(catalog.capture_only_sources(&c), &[b.clone()], "should list its narrowed sources");
        assert!(catalog.capture_only_sources(&p).is_empty(), "should list none for a resolver");
        assert_eq!(catalog.claimants(&a).collect::<Vec<_>>(), vec![&p], "should drop a narrowed-away claim");
        assert_eq!(catalog.claimants(&b).collect::<Vec<_>>(), vec![&c], "should keep the narrowed claim");
    }

    #[test]
    fn writer_policy_applies_to_unowned_sources() {
        let strict = ConsentPolicy::strict().with_tcf_vendor(9999);
        let catalog = ProviderCatalog::new(vec![provider("p", "a.example.com", strict, false)], BTreeMap::new());
        let consent = test_consent_gdpr_without_vendor(9999);
        let none = BTreeSet::new();
        let context = ServeContext {
            now: UnixTime::from_secs(100),
            consent: &consent,
            signals: PolicySignals::default(),
            catalog: &catalog,
            opted_out: &none,
        };
        let source = "a.example.com".parse().expect("should parse the source");
        assert!(!is_servable(&context, &source, &record(Some("p"), None)), "should not serve when the writer's vendor is denied");
        assert!(is_servable(&context, &source, &record(None, None)), "should leave a browser value unaffected");
    }
}
```

`test_consent_allowing_everything` and `test_consent_gdpr_without_vendor` reuse the helper you wrote in Task 12. Move it to `ec/identity/test_support.rs` now so every identity test can share it. Declare the module in `mod.rs` as `#[cfg(any(test, feature = "test-utils"))] pub mod test_support;`, and keep it public from the start, as the crate's other `test-utils` items are (`IntegrationRegistry::from_request_filters`): adapter tests reach it in Part 1 (Task 30). A helper whose signature names only public types (`ConsentContext`, `KvEid`, `KvPartnerId`, `IdentityProvider`) is `pub` with a doc comment. A helper that takes or returns a crate-private type (`ProviderCatalog`, `IdentityKeys`) stays `pub(crate)`, because a `pub` function naming a `pub(crate)` type trips `private_interfaces` under `-D warnings`. Start the file with `#![cfg_attr(not(test), allow(dead_code, reason = "the test-utils library build compiles the crate-private helpers without their unit-test callers"))]`, so `cargo clippy-fastly`, which enables `test-utils` for the adapter's dev-dependency, does not flag them. Add the entry-set helpers next to it:

```rust
/// Spec 6.1's example set: two entries, three UIDs, with UID-level and
/// EID-level fields.
#[must_use]
pub fn example_entry_set() -> Vec<KvEid> {
    vec![
        KvEid {
            uids: vec![
                KvUid { id: "example-a".to_owned(), atype: Some(1), ext: None },
                KvUid { id: "example-b".to_owned(), atype: Some(3), ext: Some(json!({ "stype": "ppuid" })) },
            ],
            inserter: Some("example.com".to_owned()),
            mm: Some(3),
            ..KvEid::default()
        },
        KvEid {
            uids: vec![KvUid { id: "example-c".to_owned(), atype: Some(1), ext: None }],
            matcher: Some("partner.example.com".to_owned()),
            ..KvEid::default()
        },
    ]
}

/// A record holding `set`, written by `writer`.
#[must_use]
pub fn full_record(writer: Option<&str>, set: Vec<KvEid>) -> KvPartnerId {
    KvPartnerId { w: writer.map(str::to_owned), eids: set, ..KvPartnerId::default() }
}
```

In `ec/eids.rs` tests, extend the existing `resolve_partner_ids` test (line 195) to pass a `ServeContext` built from `ProviderCatalog::empty()`, and add:

```rust
    #[test]
    fn resolve_partner_ids_skips_expired_records_and_uses_stored_atype() {
        let registry = registry_with(&["a.example.com", "b.example.com"]);
        let mut entry = KvEntry::minimal("a.example.com", "ua", 1);
        entry.ids.insert(
            "b.example.com".to_owned(),
            KvPartnerId { exp: Some(50), ..KvPartnerId::new("ub") },
        );
        entry.ids.get_mut("a.example.com").expect("should hold source a").eids[0].uids[0].atype = Some(3);

        let resolved = resolve_partner_ids(&registry, &entry, &serve_context_at(100));

        assert_eq!(resolved.len(), 1, "should drop the expired b");
        assert_eq!(resolved[0].source_domain, "a.example.com", "should keep a");
        assert_eq!(to_eids(&resolved)[0].uids[0].atype, Some(3), "should prefer the stored atype");
    }

    #[test]
    fn a_one_uid_record_goes_out_as_on_main() {
        let registry = registry_with(&["a.example.com"]);
        let entry = KvEntry::minimal("a.example.com", "ua", 1);

        let eids = to_eids(&resolve_partner_ids(&registry, &entry, &serve_context_at(100)));

        assert_eq!(
            eids,
            vec![Eid::new(
                "a.example.com".to_owned(),
                vec![Uid { id: "ua".to_owned(), atype: Some(99), ext: None }],
            )],
            "should be the EID main sends"
        );
    }

    #[test]
    fn a_full_record_goes_out_as_one_eid_per_stored_entry() {
        let registry = registry_with(&["a.example.com"]);
        let mut set = example_entry_set();
        set[1].uids[0].atype = None;
        let mut entry = KvEntry::minimal("b.example.com", "unregistered", 1);
        entry.ids.insert("a.example.com".to_owned(), full_record(None, set));

        let eids = to_eids(&resolve_partner_ids(&registry, &entry, &serve_context_at(100)));

        assert_eq!(
            eids,
            vec![
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![
                        Uid { id: "example-a".to_owned(), atype: Some(1), ext: None },
                        Uid { id: "example-b".to_owned(), atype: Some(3), ext: Some(json!({ "stype": "ppuid" })) },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                Eid {
                    source: "a.example.com".to_owned(),
                    uids: vec![Uid { id: "example-c".to_owned(), atype: Some(99), ext: None }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
            "should send every entry in stored order, a UID without atype taking the partner's"
        );
    }
```

Write `registry_with` and `serve_context_at` as local helpers. `registry_with` follows the existing test's registry setup and gives every partner `openrtb_atype = 99`, so a fallback is visible. `serve_context_at` uses `ProviderCatalog::empty()` with permissive consent. The full record above has no writer, because an empty catalog holds no writer for it to pass.

In `ec/identify.rs` tests, add two. Copy the setup from the nearest existing identify test (`grep -n "fn .*identify" crates/trusted-server-core/src/ec/identify.rs`):

- A row with an expired record and a valid one: the identify response lists only the valid one.
- A row whose partner source holds `full_record(None, example_entry_set())`: the response's `uid` is `example-a`, and its `eid` is `Eid::new(source, vec![Uid { id: "example-a", atype: Some(partner.openrtb_atype), ext: None }])`, the shape a one-UID record gives today. `/identify` never returns a full set (spec 7.1).

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::servable`, `cargo test-fastly -- ec::eids`, `cargo test-fastly -- ec::identify`
Expected: compile errors, then the identify test fails because it returns the expired ID.

- [ ] **Step 3: Implement `catalog.rs`**

```rust
//! The enabled identity providers and source ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::ids::{IdentityProviderId, SourceDomain};
use super::provider::IdentityProvider;

/// Enabled providers, their narrowed sources and source owners. Built once
/// per settings load.
#[derive(Clone)]
pub(crate) struct ProviderCatalog {
    providers: BTreeMap<IdentityProviderId, Arc<dyn IdentityProvider>>,
    sources: BTreeMap<IdentityProviderId, Vec<SourceDomain>>,
    owners: BTreeMap<SourceDomain, IdentityProviderId>,
}

impl ProviderCatalog {
    /// A catalog with no providers.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::new(Vec::new(), BTreeMap::new())
    }

    /// Builds the catalog. Validation (ownership, duplicates) happens in
    /// config validation before this is called.
    #[must_use]
    pub(crate) fn new(
        providers: Vec<Arc<dyn IdentityProvider>>,
        owners: BTreeMap<SourceDomain, IdentityProviderId>,
    ) -> Self {
        let providers: BTreeMap<_, _> = providers
            .into_iter()
            .map(|provider| (provider.descriptor().id().clone(), provider))
            .collect();
        let sources = providers
            .iter()
            .map(|(id, provider)| (id.clone(), provider.descriptor().sources().to_vec()))
            .collect();
        Self { providers, sources, owners }
    }

    /// Narrows providers to the listed sources their descriptors declare
    /// (spec 10.1). Config validation has rejected a source a descriptor
    /// does not declare and a narrowing to nothing.
    #[must_use]
    pub(crate) fn with_narrowing(mut self, narrowing: BTreeMap<IdentityProviderId, Vec<SourceDomain>>) -> Self {
        for (id, listed) in narrowing {
            if let Some(sources) = self.sources.get_mut(&id) {
                sources.retain(|source| listed.contains(source));
            }
        }
        self
    }

    /// A provider by ID.
    #[must_use]
    pub(crate) fn get(&self, id: &IdentityProviderId) -> Option<&Arc<dyn IdentityProvider>> {
        self.providers.get(id)
    }

    /// Every provider, ordered by ID.
    pub(crate) fn providers(&self) -> impl Iterator<Item = &Arc<dyn IdentityProvider>> {
        self.providers.values()
    }

    /// Whether `id` is an enabled provider.
    #[must_use]
    pub(crate) fn is_configured(&self, id: &IdentityProviderId) -> bool {
        self.providers.contains_key(id)
    }

    /// The owner of `source`, if any.
    #[must_use]
    pub(crate) fn owner_of(&self, source: &SourceDomain) -> Option<&IdentityProviderId> {
        self.owners.get(source)
    }

    /// A provider's sources after narrowing. Empty for an unknown provider.
    #[must_use]
    pub(crate) fn sources_of(&self, id: &IdentityProviderId) -> &[SourceDomain] {
        self.sources.get(id).map_or(&[], Vec::as_slice)
    }

    /// Every provider that claims `source` after narrowing, ordered by ID.
    pub(crate) fn claimants<'a>(&'a self, source: &'a SourceDomain) -> impl Iterator<Item = &'a IdentityProviderId> {
        self.sources.iter().filter(move |(_, sources)| sources.contains(source)).map(|(id, _)| id)
    }

    /// Whether `id` captures and never resolves.
    #[must_use]
    pub(crate) fn is_capture_only(&self, id: &IdentityProviderId) -> bool {
        self.get(id).is_some_and(|provider| {
            let descriptor = provider.descriptor();
            descriptor.captures().is_some() && descriptor.resolves().is_none()
        })
    }

    /// The narrowed sources of a capture-only provider. Empty for any other.
    #[must_use]
    pub(crate) fn capture_only_sources(&self, id: &IdentityProviderId) -> &[SourceDomain] {
        if self.is_capture_only(id) { self.sources_of(id) } else { &[] }
    }
}
```

- [ ] **Step 4: Implement `servable.rs`**

```rust
//! The one servability check shared by the bidstream, the admin view and
//! `/_ts/api/v1/identify` (spec 7.1).

use std::collections::BTreeSet;

use super::catalog::ProviderCatalog;
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::policy::{PolicySignals, policy_allows};
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;

/// What a servability decision needs from the request.
pub(crate) struct ServeContext<'a> {
    pub(crate) now: UnixTime,
    pub(crate) consent: &'a ConsentContext,
    pub(crate) signals: PolicySignals,
    pub(crate) catalog: &'a ProviderCatalog,
    /// The providers with a `ps.o` in the row the request read.
    pub(crate) opted_out: &'a BTreeSet<IdentityProviderId>,
}

/// Whether a stored record, the source's whole entry set, may be sent from
/// the server.
#[must_use]
pub(crate) fn is_servable(context: &ServeContext<'_>, source: &SourceDomain, record: &KvPartnerId) -> bool {
    if record.eids.is_empty() {
        return false;
    }
    if record.exp.is_some_and(|exp| context.now.as_secs() >= exp) {
        return false;
    }
    let writer = record.w.as_deref().and_then(|writer| writer.parse::<IdentityProviderId>().ok());
    if record.w.is_some() {
        let Some(writer) = writer.as_ref() else {
            return false;
        };
        let Some(provider) = context.catalog.get(writer) else {
            return false;
        };
        if context.opted_out.contains(writer)
            || !policy_allows(provider.descriptor().consent_policy(), context.consent, context.signals)
        {
            return false;
        }
    }
    if let Some(owner) = context.catalog.owner_of(source) {
        let Some(owner_provider) = context.catalog.get(owner) else {
            return false;
        };
        let descriptor = owner_provider.descriptor();
        if !policy_allows(descriptor.consent_policy(), context.consent, context.signals) {
            return false;
        }
        if descriptor.is_expiring(source) && (writer.as_ref() != Some(owner) || record.exp.is_none()) {
            return false;
        }
    }
    true
}
```

- [ ] **Step 5: Switch the readers**

In `ec/eids.rs`, replace the module comment at lines 1-5 with one that says KV IDs are filtered by the shared servability check in `ec::identity::servable`, and that a stored record goes out as one EID per stored entry. Add `serve: &ServeContext<'_>` to `resolve_partner_ids`, and inside the loop add `if !is_servable(serve, &source, partner_uid) { continue; }` after parsing `source_domain` into a `SourceDomain`. Skip any key that does not parse. Then change `ResolvedPartnerId` and `to_eids`:

```rust
/// A source-domain keyed record resolved from a KV entry against the partner
/// registry and the servability check.
///
/// Only includes partners with `bidstream_enabled = true`, a non-empty UID and
/// a servable record.
pub struct ResolvedPartnerId {
    /// The partner's identity source domain and EC KV `ids` key.
    pub source_domain: String,
    /// The first UID of the first stored entry.
    pub uid: String,
    /// The partner's `OpenRTB` agent type, used for a UID without a stored one.
    pub openrtb_atype: i32,
    /// The source's stored entry set, in order. One entry for a one-UID record.
    pub entries: Vec<KvEid>,
    /// The provider that wrote the record. `None` for browser ingest and version 1 rows.
    pub writer: Option<IdentityProviderId>,
}
```

In the loop, fill `uid` from `partner_uid.first_uid()`, `entries` with `partner_uid.eids.clone()` and `writer` with the parsed `w`.

```rust
/// Converts resolved partner IDs to `OpenRTB` `Eid` entries: one per stored
/// entry, in stored order.
#[must_use]
pub fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .flat_map(|item| item.entries.iter().map(move |entry| stored_entry_to_eid(item, entry)))
        .collect()
}

fn stored_entry_to_eid(item: &ResolvedPartnerId, entry: &KvEid) -> Eid {
    let uids = entry
        .uids
        .iter()
        .map(|uid| Uid {
            id: uid.id.clone(),
            atype: Some(uid.atype.unwrap_or(item.openrtb_atype)),
            ext: uid.ext.clone(),
        })
        .collect();
    Eid {
        source: item.source_domain.clone(),
        uids,
        inserter: entry.inserter.clone(),
        matcher: entry.matcher.clone(),
        mm: entry.mm,
        ext: entry.ext.clone(),
    }
}
```

Leave `encode_eids_header` and `build_eids_header` as they are. They truncate per `Eid` entry, so a full set can be cut part-way in `x-ts-eids` and reach the 4 KB cap sooner; `x-ts-eids-truncated` is set as today (spec 7.1). `user.eids` is built from `to_eids` without that cap, so bidders always get the set whole. Update the `Eid` doc comment in `openrtb.rs`: KV-resolved entries carry the EID-level fields a provider stored, and a record from browser ingest or a version 1 row has none.

At each caller, build a `ServeContext` with the KV `ps.o` keys of the row as `opted_out`. Until Task 23 puts the request's `IdentityRuntime` on `EcContext`, pass `ProviderCatalog::empty()`. Task 23 replaces it with the runtime's catalog, and adds `generic_pull_record_with_writer_is_served_after_flush`: from Task 29 on, generic pull records carry `w = pull-…`, which an empty catalog would never serve. `now` comes from `checked_current_timestamp()`. If that fails, resolve no KV IDs, because a zero time would make every expiry check pass (spec 4.4). `consent` is the request's `ConsentContext`. DNT is read from the request header there (`auction/routing.rs:336` shows how). In `ec/admin.rs`, `build_auction_view` reports a record the check drops in `skipped` with the reason `not_servable`, so the view still explains every stored ID it leaves out.

In `ec/identify.rs`, keep the lookup of this partner's own source and gate it with `is_servable`, using the same context built from the request's consent, never the row's stored `consent` (spec 7.1). Do not route it through `resolve_partner_ids`, which also drops partners with `bidstream_enabled = false`; `/identify` returns those today. The response keeps its shape: `uid` is `record.first_uid()`, the first UID of the first entry, and `eid` is still built from it with `partner.openrtb_atype` and no `ext` (`ec/identify.rs:127-135`).

A provider-written record (`w` set) passes only with browser context (spec 7.1, 9.5): `classify_origin` allowed the `Origin` and `Sec-Fetch-Site` is `same-origin` or `same-site` (`is_same_site_fetch`; a missing header fails closed). A browser-seeded record keeps today's behaviour. Tests: `identify_returns_a_provider_written_id_only_with_browser_context` (same-site and same-origin return it; no `Origin`, no `Sec-Fetch-Site`, `cross-site` and `none` withhold it) and `identify_returns_a_browser_seeded_id_without_browser_context`. The Fastly adapter's `identify_request` test helper sends both headers, as a browser does.

- [ ] **Step 6: Run and see them pass**

Run: `cargo test-fastly -- ec::` and `cargo test-fastly -- auction::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Filter served KV IDs through one servability check" -m "Expired IDs, IDs from disabled or opted-out writers, and IDs whose writer or owner policy fails are no longer sent to bidders, the admin view or /identify. /identify uses the request's consent, never the row's stored snapshot." -m "A stored record goes out as one EID per stored entry. A one-UID record goes out exactly as before, x-ts-eids keeps its per-entry truncation, and /identify keeps returning one UID."
```

### Task 15: `IdentityMutation` and its merge into a live row

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/mutation.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs`, `mod.rs`, `migration_guards.rs`
- Test: `mutation.rs`

**Interfaces:**

- Consumes: `KvEntry`, `KvPartnerId`, `KvProviderState`, `KvOptOut`, `MAX_INVALIDATED_UIDS` (128), `MAX_PROVIDER_STATES` (Task 6), `Revision`, `StateRevision`, `new_revision`, `record_revision`, `set_hashes`, `push_hashes` (Task 13), `ProviderCatalog` with `claimants`, `is_capture_only` and `capture_only_sources` (Task 14), `RandomSource` (Task 9)
- Produces:
  - `pub(crate) enum PartnerIdChange` (spec 5.4):
    - `FillMissing { source, record: KvPartnerId }`: from browser ingest (`record.w` is `None`) or an unowned provider (`record.w` names it)
    - `OwnerWrite { source, record: KvPartnerId, expected: Option<Revision> }`: from the owning provider, whose ID `record.w` holds
    - `OwnerRemove { source, provider: IdentityProviderId, expected: Revision }`: from `Resolution::invalidate` or `also_invalidate`. `provider` is the provider that invalidated, whose `ps.iv` receives the hashes. Spec 5.4's enum leaves it out, but step 3 reads the `ps.iv` of every provider that claims a source, owned or unowned, so the change must say whose list it feeds.
    - `WriterRemove { source, writer: IdentityProviderId }`: from `Resolution::opted_out`. Removes the source's record if `writer` wrote it, at any revision, and for a capture-only `writer` also a record no provider wrote. Its hashes go to the writer's `ps.ov`.
  - There is no `staged_at` and no `invalidate` flag: nothing is staged in a cookie, so every change comes from this request's own KV read (spec 5.4).
  - Each `record` is the source's whole record: its `eids`, `w`, the timing fields and `rv`. Every change writes or removes a source's set whole. No change edits one entry or one UID, so one `rv` per source covers every entry in it.
  - `pub(crate) enum StateUpdate { Keep, Replace(serde_json::Value), Clear }`, defaulting to `Keep`
  - `pub(crate) struct ProviderStateChange { n: Option<u64>, f: Option<u32>, opt_out: Option<KvOptOut>, opt_out_clear: Option<u64>, st: StateUpdate }`, deriving `Default`. It has no `iv` or `ov` field: those hashes come only from applied removals.
  - `pub(crate) struct ProviderResult { provider, ids: Vec<PartnerIdChange>, state: ProviderStateChange, expected_state: Option<StateRevision> }`. `None` means the call read no state, so the state check always passes.
  - `pub(crate) struct IdentityMutation`, deriving `Default`, with `add_change(PartnerIdChange)`, `add_result(ProviderResult)`, `merge_state(IdentityProviderId, ProviderStateChange)`, `set_cluster_size(u32)`, `is_empty()`, `has_removals()` (any `OwnerRemove`, `WriterRemove`, opt-out or clear), and `removals(&self) -> IdentityMutation` (a copy holding only the removals, opt-outs and clears, which Task 18's removal write and Task 29's removal-only retry apply). Task 18 adds `removal_labels()`.
  - `pub(crate) struct ApplyContext<'a> { now: u64, random: &'a dyn RandomSource, catalog: &'a ProviderCatalog }`
  - `pub(crate) struct Applied { entry: KvEntry, changed: bool, removals_only: bool }`
  - `pub(crate) fn apply(mutation: &IdentityMutation, live: &KvEntry, context: &ApplyContext<'_>) -> Option<Applied>`. `None` means drop everything: the live row is a tombstone, or even the removals-only result failed validation.
  - `pub(crate) fn apply_removals_only(mutation: &IdentityMutation, live: &KvEntry, context: &ApplyContext<'_>) -> Option<Applied>`: spec 5.4's steps 1, 2, 4 (only `o`, `oc`, `iv` and `ov`), 5 and 6, for the capture removal write and the removal-only retry. It ignores every positive change, `n`, `f`, `st` and `cluster_size` in `mutation`.

Apply order (spec 5.4). Copy it into the doc comment of `apply`:

1. A tombstone (`consent.ok == false`) gives `None`.
2. Removals, never skipped:
   - Every `OwnerRemove` whose `expected` equals `record_revision(live_record)`. A version 1 record matches `Revision::Unset`. It appends `set_hashes(&live_record.eids)` to `ps[provider].iv`.
   - Every `WriterRemove` whose live record has `w == writer`, or, when `writer` is capture-only and the source is one of its `capture_only_sources`, has no `w`. It appends the removed record's hashes to `ps[writer].ov`.
   - Each new opt-out applies when `at > live.o.at` (or there is no live `o`) and `at >= live.oc` (or there is no live `oc`). Applying it stores `o` and removes every live record that provider wrote, at any revision, and for a capture-only provider every record of its sources that no provider wrote, each adding its hashes to `ps.ov`.
   - Clears apply before opt-outs (revised after the round 14 review). A clear (`opt_out_clear`) carries a watermark, the `at` of the opt-out the clearing request read. It applies when the live `o.at` is at or before the watermark and the live `oc` is before it, stores the watermark as `oc` and empties `ps.ov`. One whose watermark is before the live `o.at`, or that finds no live opt-out, is dropped and writes nothing. `ps.iv` is never emptied.
   - Hashes always come from the record KV held. A removal that no longer applies adds none.
3. Positive changes:
   - A provider is opted out when its live `o` is set and its `oc` is not strictly later than `o.at`. Every change whose `record.w` is opted out is skipped, `OwnerWrite` and `FillMissing` alike.
   - `owner_denies`: for an owned source, every positive change is skipped while the owner is opted out, whoever wrote it, and an `OwnerWrite` or `FillMissing` any of whose UIDs hashes into the owner's `ps.iv` is skipped. Both read the entry after step 2. Tests: an invalidation and a browser refill of the same UID in one mutation leave it removed; an opted-out owner blocks a browser fill and another writer's fill of its source (control: an unowned source still fills); an owner opt-out in the same mutation blocks a browser refill; an owner write of an invalidated UID is skipped (control: another UID is stored).
   - A `FillMissing` applies when the source is absent, or its record is expired, or its record's writer is not configured. It is skipped when the hash of any UID of its set is in the `ps.iv` or `ps.ov` of any provider in `catalog.claimants(source)`. A browser `FillMissing` (`record.w` is `None`) is also skipped when the source belongs to an opted-out capture-only provider's `capture_only_sources`.
   - An `OwnerWrite` applies when the live revision equals `expected`, or when `expected` is `None` and the source has no valid record with `w == record.w`. A browser-seeded value never blocks the owner.
   - A written record replaces the live record whole, never merges into it, so no entry of the old set survives. It gets a fresh `rv` and `at = record.at.or(now)`, and keeps the change's `w`.
   - Each `ProviderResult` applies only when every one of its ID changes would apply on its own and, when `expected_state` is `Some`, `StateRevision::of(live ps.st)` equals it. Its state change, `n` and `f` then apply with its IDs. Otherwise the whole result is dropped.
4. `ps` changes: `n` takes the later value, `f` is replaced, `st` is replaced or cleared. Then `cluster_size`. Then truncate each `ps.iv` and `ps.ov` from the front to `MAX_INVALIDATED_UIDS`. Then prune the `ps` entries of providers the catalog does not hold: each keeps only `o`, `oc` and `ov`, so a retired provider's opt-out survives its removal from config and holds again if it comes back, and an entry with none of them is dropped. While `ps` is over `MAX_PROVIDER_STATES`, drop retained entries first, oldest `o.at` first. Then drop expired records, except refresh anchors: the source's owner is configured and either holds a `st` while the record's `ru` has not passed, or wrote the record and declares `Resolves::with_refresh_grace` while `exp` plus the grace has not passed (added after the round 14 review).
5. `validate()`. On failure, rebuild from the live entry through `apply_removals_only`, and if that also fails, give `None` and log. `validate()` checks the entry-set limits of spec 4.5, so a set that breaks one, such as a set over `MAX_ENTRY_SET_BYTES`, is dropped whole here and never trimmed.
6. `changed` is whether the result differs from `live`. A caller skips the write when it is `false`.

- [ ] **Step 1: Write the failing tests**

Add the fixtures to `ec/identity/test_support.rs`:

- `pub fn fixed_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider>`: a descriptor-only resolver with a dummy `https://api.example.com` endpoint and the `eid_persistence_only` policy
- `pub fn fixed_capture_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider>`: a descriptor-only provider with `Captures::new().with_path_prefix("/integrations/<id>/")` and no `resolves`

Then the tests in `mutation.rs`:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::revision::{RecordRevision, StateRevision, set_hashes, uid_hash};
    use crate::ec::identity::test_support::{example_entry_set, fixed_capture_provider, fixed_provider, full_record};
    use crate::ec::kv_types::{
        KvEntry, KvOptOut, KvPartnerId, KvProviderState, KvUid, MAX_INVALIDATED_UIDS, MAX_PROVIDER_STATES,
    };

    const NOW: u64 = 1_000;

    fn src(name: &str) -> SourceDomain {
        name.parse().expect("should parse the source")
    }

    fn pid(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse the provider ID")
    }

    fn rev(value: &str) -> Revision {
        Revision::Set(RecordRevision::from_stored(value))
    }

    /// `owner` owns a.example.com, `other` claims b.example.com unowned, and
    /// the capture-only `cap` is narrowed to c.example.com.
    fn catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![
                fixed_provider("owner", &["a.example.com"]),
                fixed_provider("other", &["b.example.com"]),
                fixed_capture_provider("cap", &["c.example.com"]),
            ],
            [(src("a.example.com"), pid("owner"))].into_iter().collect(),
        )
    }

    fn run(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply(mutation, live, &ApplyContext { now: NOW, random: &random, catalog: &catalog })
    }

    fn run_removals(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply_removals_only(mutation, live, &ApplyContext { now: NOW, random: &random, catalog: &catalog })
    }

    fn written(writer: &str, uid: &str, rv: &str) -> KvPartnerId {
        KvPartnerId { w: Some(writer.to_owned()), rv: Some(rv.to_owned()), at: Some(900), ..KvPartnerId::new(uid) }
    }

    fn opt_out(at: u64) -> ProviderStateChange {
        ProviderStateChange { opt_out: Some(KvOptOut { at, fp: "1".to_owned() }), ..ProviderStateChange::default() }
    }

    fn fill(source: &str, record: KvPartnerId) -> PartnerIdChange {
        PartnerIdChange::FillMissing { source: src(source), record }
    }

    #[test]
    fn tombstone_drops_everything() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("b.example.com", KvPartnerId::browser("u", Some(NOW))));
        assert!(run(&mutation, &KvEntry::tombstone(1)).is_none(), "should drop on a tombstone");
        assert!(run_removals(&mutation, &KvEntry::tombstone(1)).is_none(), "should drop removals on a tombstone");
    }

    #[test]
    fn fill_missing_fills_absent_expired_and_unconfigured_only() {
        let mut live = KvEntry::minimal("b.example.com", "existing", 1);
        live.ids.insert("d.example.com".to_owned(), KvPartnerId { exp: Some(NOW), ..KvPartnerId::new("old") });
        live.ids.insert("f.example.com".to_owned(), written("retired", "stale", "1111111111111111"));
        let mut mutation = IdentityMutation::default();
        for source in ["b.example.com", "d.example.com", "e.example.com", "f.example.com"] {
            mutation.add_change(fill(source, KvPartnerId::browser("new", Some(NOW))));
        }

        let applied = run(&mutation, &live).expect("should apply");

        let uid = |source: &str| applied.entry.ids[source].first_uid().map(str::to_owned);
        assert_eq!(uid("b.example.com").as_deref(), Some("existing"), "should not replace a live value");
        assert_eq!(uid("d.example.com").as_deref(), Some("new"), "should replace an expired value");
        assert_eq!(uid("e.example.com").as_deref(), Some("new"), "should fill an absent source");
        assert_eq!(uid("f.example.com").as_deref(), Some("new"), "should treat an unconfigured writer as absent");
        let record = &applied.entry.ids["e.example.com"];
        assert_eq!((record.at, record.w.as_deref()), (Some(NOW), None), "should be a browser record");
        assert!(record.rv.is_some(), "should give every record it writes an rv");
    }

    #[test]
    fn owner_write_checks_revision_and_gets_a_fresh_rv() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), written("owner", "v1", "1111111111111111"));
        let write = |expected| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId { w: Some("owner".to_owned()), ..KvPartnerId::new("v2") },
                expected,
            });
            run(&mutation, &live).expect("should apply")
        };

        assert!(!write(Some(rev("2222222222222222"))).changed, "should lose a stale revision to the newer write");
        let applied = write(Some(rev("1111111111111111")));
        let record = &applied.entry.ids["a.example.com"];
        assert_eq!(record.first_uid(), Some("v2"), "should write");
        assert_eq!(record.w.as_deref(), Some("owner"), "should keep the writer");
        assert_ne!(record.rv.as_deref(), Some("1111111111111111"), "should draw a new rv");
        assert_eq!(record.at, Some(NOW), "should stamp at");
    }

    #[test]
    fn owner_write_replaces_the_whole_set_and_leaves_other_sources() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId { rv: Some("1111111111111111".to_owned()), ..full_record(Some("owner"), example_entry_set()) },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId { w: Some("owner".to_owned()), ..KvPartnerId::new("only") },
            expected: Some(rev("1111111111111111")),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(applied.entry.ids["a.example.com"], KvPartnerId {
            rv: applied.entry.ids["a.example.com"].rv.clone(),
            at: Some(NOW),
            w: Some("owner".to_owned()),
            ..KvPartnerId::new("only")
        }, "should keep no entry of the old set");
        assert_eq!(applied.entry.ids["b.example.com"].first_uid(), Some("x"), "should leave other sources alone");

        let mut full = IdentityMutation::default();
        full.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: full_record(Some("owner"), example_entry_set()),
            expected: None,
        });
        let applied = run(&full, &KvEntry::minimal("b.example.com", "x", 1)).expect("should apply");
        assert_eq!(applied.entry.ids["a.example.com"].eids, example_entry_set(), "should write every entry, in order");
    }

    #[test]
    fn browser_seeded_and_version_1_values_never_block_the_owner() {
        let live = KvEntry::minimal("a.example.com", "seeded", 1);
        let mut write = IdentityMutation::default();
        write.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId { w: Some("owner".to_owned()), ..KvPartnerId::new("owned") },
            expected: None,
        });
        assert_eq!(
            run(&write, &live).expect("should apply").entry.ids["a.example.com"].first_uid(),
            Some("owned"),
            "should let the owner take over a seeded value"
        );

        let mut remove = IdentityMutation::default();
        remove.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: Revision::Unset,
        });
        let applied = run(&remove, &live).expect("should apply");
        assert!(!applied.entry.ids.contains_key("a.example.com"), "should match a record without rv as Unset");
        assert_eq!(applied.entry.ps["owner"].iv, vec![uid_hash("seeded")], "should hash the removed UID into iv");
    }

    #[test]
    fn an_applied_invalidation_hashes_every_uid_of_the_live_set() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId { rv: Some("1111111111111111".to_owned()), ..full_record(Some("owner"), example_entry_set()) },
        );
        let invalidate = |expected: &str| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerRemove {
                source: src("a.example.com"),
                provider: pid("owner"),
                expected: rev(expected),
            });
            run(&mutation, &live).expect("should apply")
        };

        let applied = invalidate("1111111111111111");
        let stale = invalidate("2222222222222222");

        assert!(!applied.entry.ids.contains_key("a.example.com"), "should remove the set");
        assert_eq!(applied.entry.ps["owner"].iv, set_hashes(&example_entry_set()), "should hash every UID it removed");
        assert!(stale.entry.ids.contains_key("a.example.com"), "should keep a set at another revision");
        assert!(stale.entry.ps.get("owner").is_none_or(|state| state.iv.is_empty()), "should add no hash");
    }

    #[test]
    fn iv_keeps_the_128_most_recent_hashes() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        let old: Vec<String> = (0..MAX_INVALIDATED_UIDS).map(|index| format!("{index:016x}")).collect();
        live.ps.insert("owner".to_owned(), KvProviderState { iv: old.clone(), ..KvProviderState::default() });
        live.ids.insert("a.example.com".to_owned(), written("owner", "u", "1111111111111111"));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run(&mutation, &live).expect("should apply");

        let iv = &applied.entry.ps["owner"].iv;
        assert_eq!(iv.len(), MAX_INVALIDATED_UIDS, "should cap at 128");
        assert_eq!(iv[..127], old[1..], "should drop the oldest hash");
        assert_eq!(iv[127], uid_hash("u"), "should append the new hash");
    }

    #[test]
    fn opt_out_removes_what_the_writer_wrote_fills_ov_and_skips_its_changes() {
        let mut live = KvEntry::minimal("e.example.com", "browser", 1);
        live.ids.insert("a.example.com".to_owned(), written("owner", "v1", "1111111111111111"));
        live.ids.insert("b.example.com".to_owned(), written("owner", "unowned-by-owner", "2222222222222222"));
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(pid("owner"), opt_out(NOW));
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId { w: Some("owner".to_owned()), ..KvPartnerId::new("v2") },
            expected: None,
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert!(!applied.entry.ids.contains_key("a.example.com"), "should remove the owned record");
        assert!(!applied.entry.ids.contains_key("b.example.com"), "should remove the unowned record it wrote");
        assert_eq!(applied.entry.ids["e.example.com"].first_uid(), Some("browser"), "should leave other records");
        let state = &applied.entry.ps["owner"];
        assert_eq!(state.o.as_ref().map(|o| o.at), Some(NOW), "should store the opt-out");
        assert_eq!(state.ov, vec![uid_hash("v1"), uid_hash("unowned-by-owner")], "should hash what it removed into ov");
        assert!(state.iv.is_empty(), "should leave iv for invalidations");
    }

    #[test]
    fn a_capture_only_opt_out_removes_browser_records_of_its_sources_and_blocks_their_fill() {
        let mut live = KvEntry::minimal("c.example.com", "browser-c", 1);
        live.ids.insert("e.example.com".to_owned(), KvPartnerId::new("browser-e"));
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(pid("cap"), opt_out(NOW));

        let applied = run(&mutation, &live).expect("should apply");

        assert!(!applied.entry.ids.contains_key("c.example.com"), "should remove a browser record of its source");
        assert!(applied.entry.ids.contains_key("e.example.com"), "should leave a source it is not narrowed to");

        let mut refill = IdentityMutation::default();
        refill.add_change(fill("c.example.com", KvPartnerId::browser("another", Some(NOW))));
        let applied = run(&refill, &applied.entry).expect("should apply");
        assert!(!applied.entry.ids.contains_key("c.example.com"), "should skip a browser fill while it is opted out");
    }

    #[test]
    fn opt_out_and_clear_ordering() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let mut same_second = IdentityMutation::default();
        same_second.merge_state(pid("owner"), ProviderStateChange { opt_out_clear: Some(NOW), ..opt_out(NOW) });
        let applied = run(&same_second, &live).expect("should apply");
        assert!(applied.entry.ps["owner"].o.is_some(), "should stay opted out");
        assert_eq!(applied.entry.ps["owner"].oc, None, "should drop a clear that is not later");

        let mut no_opt_out = IdentityMutation::default();
        no_opt_out.merge_state(pid("owner"), ProviderStateChange { opt_out_clear: Some(NOW), ..ProviderStateChange::default() });
        assert!(!run(&no_opt_out, &live).expect("should apply").changed, "should drop a clear with nothing to clear");

        let mut opted = live.clone();
        opted.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                o: Some(KvOptOut { at: 900, fp: "1".to_owned() }),
                iv: vec![uid_hash("invalidated")],
                ov: vec![uid_hash("opted-out")],
                ..KvProviderState::default()
            },
        );
        let mut clear = IdentityMutation::default();
        clear.merge_state(pid("owner"), ProviderStateChange { opt_out_clear: Some(NOW), ..ProviderStateChange::default() });
        let state = &run(&clear, &opted).expect("should apply").entry.ps["owner"];
        assert_eq!(state.oc, Some(NOW), "should store a later clear");
        assert!(state.ov.is_empty(), "should empty ov");
        assert_eq!(state.iv, vec![uid_hash("invalidated")], "should never empty iv");

        let mut older = IdentityMutation::default();
        older.merge_state(pid("owner"), opt_out(899));
        assert!(!run(&older, &opted).expect("should apply").changed, "should drop an opt-out not after the live one");
    }

    #[test]
    fn fill_missing_is_skipped_when_any_uid_is_in_a_claimants_iv_or_ov() {
        let mut live = KvEntry::minimal("e.example.com", "x", 1);
        live.ps.insert("owner".to_owned(), KvProviderState { iv: vec![uid_hash("example-b")], ..KvProviderState::default() });
        live.ps.insert("other".to_owned(), KvProviderState { ov: vec![uid_hash("removed")], ..KvProviderState::default() });
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("a.example.com", full_record(None, example_entry_set())));
        mutation.add_change(fill("b.example.com", KvPartnerId::browser("removed", Some(NOW))));
        mutation.add_change(fill("d.example.com", KvPartnerId::browser("removed", Some(NOW))));

        let applied = run(&mutation, &live).expect("should apply");

        assert!(!applied.entry.ids.contains_key("a.example.com"), "should block an owned set on its second UID");
        assert!(!applied.entry.ids.contains_key("b.example.com"), "should block an unowned source by its claimant's ov");
        assert!(applied.entry.ids.contains_key("d.example.com"), "should not block a source no denying provider claims");
    }

    #[test]
    fn provider_result_applies_atomically() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), written("owner", "newer", "3333333333333333"));
        live.ps.insert("owner".to_owned(), KvProviderState { st: Some(json!({ "token": "B" })), ..KvProviderState::default() });
        let result = |expected, expected_state| ProviderResult {
            provider: pid("owner"),
            ids: vec![PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId { w: Some("owner".to_owned()), ..KvPartnerId::new("mine") },
                expected: Some(expected),
            }],
            state: ProviderStateChange { st: StateUpdate::Replace(json!({ "token": "A" })), f: Some(0), ..ProviderStateChange::default() },
            expected_state,
        };

        let mut lost_id = IdentityMutation::default();
        lost_id.add_result(result(rev("1111111111111111"), Some(StateRevision::of(Some(&json!({ "token": "B" }))))));
        let mut lost_state = IdentityMutation::default();
        lost_state.add_result(result(rev("3333333333333333"), Some(StateRevision::of(Some(&json!({ "token": "A0" }))))));
        let mut wins = IdentityMutation::default();
        wins.add_result(result(rev("3333333333333333"), Some(StateRevision::of(Some(&json!({ "token": "B" }))))));

        for (name, mutation) in [("an ID", lost_id), ("the state", lost_state)] {
            assert!(!run(&mutation, &live).expect("should apply").changed, "should drop the whole result when {name} lost");
        }
        let applied = run(&wins, &live).expect("should apply");
        assert_eq!(applied.entry.ids["a.example.com"].first_uid(), Some("mine"), "should write the ID");
        assert_eq!(applied.entry.ps["owner"].st, Some(json!({ "token": "A" })), "should write the state with it");
    }

    #[test]
    fn refresh_anchor_survives_until_ru_or_state_loss() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId { exp: Some(NOW - 1), ru: Some(NOW + 100), ..written("owner", "expired", "1111111111111111") },
        );
        live.ps.insert("owner".to_owned(), KvProviderState { st: Some(json!({ "r": 1 })), ..KvProviderState::default() });
        let mut unrelated = IdentityMutation::default();
        unrelated.set_cluster_size(1);

        assert!(run(&unrelated, &live).expect("should apply").entry.ids.contains_key("a.example.com"), "should keep the anchor");
        live.ps.get_mut("owner").expect("should hold the owner's state").st = None;
        assert!(!run(&unrelated, &live).expect("should apply").entry.ids.contains_key("a.example.com"), "should drop it");
    }

    #[test]
    fn unconfigured_provider_state_keeps_o_oc_and_ov() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        let kept = KvProviderState {
            o: Some(KvOptOut { at: 7, fp: "1".to_owned() }),
            oc: Some(6),
            ov: vec![uid_hash("removed")],
            ..KvProviderState::default()
        };
        live.ps.insert(
            "retired".to_owned(),
            KvProviderState { n: Some(5), iv: vec![uid_hash("invalidated")], st: Some(json!(1)), ..kept.clone() },
        );
        live.ps.insert("gone".to_owned(), KvProviderState { n: Some(5), ..KvProviderState::default() });
        live.ps.insert("other".to_owned(), KvProviderState { n: Some(5), ..KvProviderState::default() });
        let mut mutation = IdentityMutation::default();
        mutation.set_cluster_size(1);

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(applied.entry.ps["retired"], kept, "should keep only o, oc and ov");
        assert!(!applied.entry.ps.contains_key("gone"), "should drop an entry with none of them");
        assert_eq!(applied.entry.ps["other"].n, Some(5), "should leave a configured provider untouched");
    }

    #[test]
    fn retained_opt_outs_are_pruned_first_when_over_the_state_cap() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert("other".to_owned(), KvProviderState { n: Some(5), ..KvProviderState::default() });
        for (at, index) in (100_u64..).zip(0..MAX_PROVIDER_STATES) {
            live.ps.insert(
                format!("retired-{index:02}"),
                KvProviderState { o: Some(KvOptOut { at, fp: "1".to_owned() }), ..KvProviderState::default() },
            );
        }
        let mut mutation = IdentityMutation::default();
        mutation.set_cluster_size(1);

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(applied.entry.ps.len(), MAX_PROVIDER_STATES, "should fit the cap");
        assert!(applied.entry.ps.contains_key("other"), "should keep the configured provider");
        assert!(!applied.entry.ps.contains_key("retired-00"), "should drop the oldest retained opt-out");
        assert!(applied.entry.ps.contains_key("retired-01"), "should keep the newer ones");
    }

    #[test]
    fn an_invalid_result_falls_back_to_removals_only() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), written("owner", "v1", "1111111111111111"));
        let mut oversized = example_entry_set();
        oversized[0].ext = Some(json!({ "segments": "x".repeat(2_000) }));
        oversized[1].ext = Some(json!({ "segments": "y".repeat(2_000) }));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("d.example.com", full_record(Some("other"), oversized)));
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run(&mutation, &live).expect("should apply removals");

        assert!(applied.removals_only, "should fail validation for a set over MAX_ENTRY_SET_BYTES");
        assert!(!applied.entry.ids.contains_key("d.example.com"), "should drop the set whole, never trim it");
        assert!(!applied.entry.ids.contains_key("a.example.com"), "should still apply the removal");
    }

    #[test]
    fn apply_removals_only_ignores_everything_but_removals() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), written("owner", "v1", "1111111111111111"));
        live.ids.insert("d.example.com".to_owned(), KvPartnerId { exp: Some(NOW - 1), ..KvPartnerId::new("expired") });
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("e.example.com", KvPartnerId::browser("new", Some(NOW))));
        mutation.merge_state(pid("other"), ProviderStateChange { n: Some(5), f: Some(2), ..opt_out(NOW) });
        mutation.set_cluster_size(3);
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run_removals(&mutation, &live).expect("should apply");

        assert!(!applied.entry.ids.contains_key("a.example.com"), "should apply the removal");
        assert!(!applied.entry.ids.contains_key("e.example.com"), "should ignore a positive change");
        assert!(applied.entry.ids.contains_key("d.example.com"), "should not prune expired records");
        let state = &applied.entry.ps["other"];
        assert_eq!((state.n, state.f), (None, 0), "should never touch n or f");
        assert!(state.o.is_some(), "should store the opt-out");
        assert_eq!(applied.entry.network, live.network, "should not touch cluster_size");

        let nothing = IdentityMutation::default();
        assert!(!run_removals(&nothing, &live).expect("should apply").changed, "should report no change");
    }
}
```

The ordering tests build `ProviderStateChange` with `..opt_out(..)` and `..ProviderStateChange::default()` because they test one field at a time. They are not exhaustiveness tests. Check the field name of `KvEntry`'s network block (`network` above) against `kv_types.rs` and use the real one.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::mutation`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `mutation.rs` with the types listed under Interfaces, then implement `apply` and `apply_removals_only` from private steps over a cloned `KvEntry`. Use one function per step so each rule can be read on its own:

```rust
pub(crate) fn apply(mutation: &IdentityMutation, live: &KvEntry, context: &ApplyContext<'_>) -> Option<Applied> {
    if !live.consent.ok {
        return None;
    }

    let mut entry = live.clone();
    apply_removals(&mut entry, mutation, context);
    apply_positive(&mut entry, mutation, context);
    apply_state(&mut entry, mutation);
    finish_removal_fields(&mut entry, context);
    drop_expired_records(&mut entry, context);

    if let Err(message) = entry.validate() {
        log::warn!("Identity mutation produced an invalid row, keeping removals only: {message}");
        return apply_removals_only(mutation, live, context).map(|applied| Applied { removals_only: true, ..applied });
    }
    let changed = &entry != live;
    Some(Applied { entry, changed, removals_only: false })
}

pub(crate) fn apply_removals_only(
    mutation: &IdentityMutation,
    live: &KvEntry,
    context: &ApplyContext<'_>,
) -> Option<Applied> {
    if !live.consent.ok {
        return None;
    }

    let mut entry = live.clone();
    apply_removals(&mut entry, mutation, context);
    finish_removal_fields(&mut entry, context);

    if let Err(message) = entry.validate() {
        log::warn!("Identity removals produced an invalid row, dropping them: {message}");
        return None;
    }
    let changed = &entry != live;
    Some(Applied { entry, changed, removals_only: true })
}
```

- `apply_removals`: collect every `OwnerRemove` and `WriterRemove` from `partner_ids`. The coordinator always puts removals there, never in `ProviderResult.ids` (spec 5.4's comment on `ProviderResult`). Then the opt-outs and clears from `provider_state` and from each result's `state`, in that order: opt-outs first, then clears, so an opt-out and a clear in the same second leave the provider opted out. Compare revisions with `record_revision(live_record)`. Hash a record with `set_hashes(&live_record.eids)` before removing it, and add the hashes with `push_hashes` to the provider's `iv` (an `OwnerRemove`) or `ov` (a `WriterRemove` or an applied opt-out).
- `apply_positive`: for each plain change, call `fn change_applies(entry, change, context) -> bool` and, when true, `fn write_change(entry, change, context)`. `change_applies` checks the opted-out rule, the claimant `iv`/`ov` rule and the capture-only rule of step 3 before the change's own rule. `write_change` inserts the change's record in place of the live one, whole, with a fresh `rv` from `new_revision(context.random)` and `at = record.at.or(Some(now))`, and never copies a field from the live record. For each `ProviderResult`, first check that every ID change applies and that `expected_state` is `None` or equals `StateRevision::of(entry.ps[provider].st.as_ref())`. Then apply all of them and the result's `state` together.
- `apply_state`: merge `provider_state` (`n` = max, `f` replaced when `Some`, `st` per `StateUpdate`) and set the network block's `cluster_size` when given (match the existing `KvNetwork` field).
- `finish_removal_fields`: truncate each `iv` and `ov` from the front to `MAX_INVALIDATED_UIDS`. For each `ps` entry whose provider the catalog does not hold, keep only `o`, `oc` and `ov`, and drop the entry when all three are empty. While `entry.ps.len() > MAX_PROVIDER_STATES`, remove the retained entry with the oldest `o.at` (an entry without `o` sorts first). Configured providers' entries are never pruned here; if they alone exceed the cap, `validate()` reports it.
- `drop_expired_records`: drop records with `exp <= now` unless they are anchors: the catalog's owner of that source is configured, and either `entry.ps[owner].st.is_some()` and `record.ru.is_some_and(|ru| now < ru)`, or `record.w` is the owner and `now < exp + grace` for the owner's `Resolves::refresh_grace()`. `Resolves::with_refresh_grace(Duration)` (descriptor.rs) is rejected by `build()` without `ResolveKind::Refresh` or with a zero grace. Tests: `stateless_refresh_anchor_survives_until_exp_plus_grace` and the two `build_rejects_invalid_descriptors` cases (round 14 review).

`IdentityMutation::merge_state(provider, change)` merges into any existing change for the same provider: later `n`, last `f`, the later `opt_out`, the later `opt_out_clear`, and the last non-`Keep` `st`. `IdentityMutation::removals()` copies the `OwnerRemove` and `WriterRemove` changes and, from every `provider_state` entry and every result's `state`, only `opt_out` and `opt_out_clear`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::mutation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add IdentityMutation and its merge into a live identity row" -m "Removals apply first and are never skipped. An opt-out removes every record its provider wrote, and for a capture-only provider the browser records of its sources, hashing each removed UID into ps.ov; a later clear empties ps.ov. An applied invalidation hashes every UID of the live set into ps.iv, and a fill is skipped when any of its UIDs is in the iv or ov of a provider that claims the source." -m "Stateful results apply atomically, refresh anchors survive while their owner holds state, an unconfigured provider keeps its opt-out and ov, and an invalid row falls back to removals only. apply_removals_only applies a removal-only mutation for the capture removal write and the removal-only retry."
```

### Task 17: The `ts-eids-s` mirror payload and its size cap

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/mirror.rs`
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `mirror.rs`

**Interfaces:**

- Consumes: `crate::openrtb::Eid`
- Produces:
  - Constants `MIRROR_MAX_VALUE_BYTES = 2048` (the cookie value, after encoding) and `MIRROR_MAX_AGE_SEC = 86_400` (the lifetime of `ts-eids`)
  - `pub(crate) struct MirrorSource { source: String, provider: Option<IdentityProviderId>, priority: i32, expires_at: Option<u64>, eids: Vec<Eid> }`, with `pub(crate)` fields: one source's servable server entries exactly as bidders receive them (spec 6.2), the provider whose `cookie_priority` ranks it, and the source's earliest `exp`
  - `pub(crate) enum MirrorRead<'a> { NotRead, Cleared, Servable(&'a [MirrorSource]) }`: what the request's KV read gave. `Cleared` covers consent that denies EID persistence, a tombstone, `consent.ok = false` and a missing row.
  - `pub(crate) enum MirrorPayload { Value { value: String, max_age: u64 }, Clear }` with `pub(crate) fn to_json(&self) -> serde_json::Value`: `{"value": ..., "max_age": ...}` or `{"clear": true}`
  - `pub(crate) fn mirror_payload(read: MirrorRead<'_>, now: u64) -> Option<MirrorPayload>`: `None` for `NotRead`, so the response carries no member and the shim leaves the cookie as it is

The rules (spec 6.2):

1. `NotRead` gives `None`. `Cleared` gives `Clear`.
2. `Servable` keeps every source, in the order given. While the encoded value is over `MIRROR_MAX_VALUE_BYTES`, it leaves out one whole source: the lowest `priority` first, then the largest (the most bytes of compact JSON), then the later in input order. It logs each omission at info with the provider ID and source, never a value. A source is mirrored whole or not at all.
3. When no source is left, the payload is `Clear`, so the cookie never holds an empty or stale set.
4. `value` is the unpadded base64url of the compact JSON `{"v":1,"eids":[...]}`, the kept sources' entries in order.
5. `max_age` is the smaller of `MIRROR_MAX_AGE_SEC` and the time from `now` to the earliest `expires_at` among the kept sources. The servability check has already left out every record with `exp <= now`, so it is at least 1.

The mirror never holds `ps`, a revision, a writer or a timing field. `MirrorSource.eids` is built from the merge's server set after `gate_eids_by_consent` (Task 26), so this module does no consent or servability check of its own.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    use super::*;
    use crate::openrtb::{Eid, Uid};

    const NOW: u64 = 1_760_000_000;

    fn source(name: &str, priority: i32, expires_at: Option<u64>, ids: &[&str]) -> MirrorSource {
        MirrorSource {
            source: name.to_owned(),
            provider: Some("p".parse().expect("should parse the provider ID")),
            priority,
            expires_at,
            eids: vec![Eid::new(
                name.to_owned(),
                ids.iter().map(|id| Uid { id: (*id).to_owned(), atype: Some(1), ext: None }).collect(),
            )],
        }
    }

    fn decode(payload: &MirrorPayload) -> (serde_json::Value, u64) {
        let MirrorPayload::Value { value, max_age } = payload else {
            panic!("should hold a value");
        };
        let bytes = URL_SAFE_NO_PAD.decode(value).expect("should be unpadded base64url");
        (serde_json::from_slice(&bytes).expect("should be JSON"), *max_age)
    }

    #[test]
    fn no_read_is_absent_and_a_denial_or_empty_set_is_clear() {
        assert_eq!(mirror_payload(MirrorRead::NotRead, NOW), None, "should leave the cookie as it is");
        assert_eq!(mirror_payload(MirrorRead::Cleared, NOW), Some(MirrorPayload::Clear), "should clear on a denial");
        assert_eq!(mirror_payload(MirrorRead::Servable(&[]), NOW), Some(MirrorPayload::Clear), "should clear an empty set");
        assert_eq!(MirrorPayload::Clear.to_json(), json!({ "clear": true }), "should serialize the clear shape");
    }

    #[test]
    fn value_is_the_v1_entries_in_unpadded_base64url() {
        let sources = [source("example.com", 0, None, &["example-a"])];

        let payload = mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload");

        let (decoded, max_age) = decode(&payload);
        assert_eq!(
            decoded,
            json!({ "v": 1, "eids": [{ "source": "example.com", "uids": [{ "id": "example-a", "atype": 1 }] }] }),
            "should hold the entries exactly as bidders receive them"
        );
        assert_eq!(max_age, MIRROR_MAX_AGE_SEC, "should live a day without an exp");
        let MirrorPayload::Value { value, .. } = &payload else { panic!("should hold a value") };
        assert_eq!(payload.to_json(), json!({ "value": value, "max_age": MIRROR_MAX_AGE_SEC }), "should serialize");
    }

    #[test]
    fn max_age_never_outlives_the_earliest_exp() {
        let sources = [
            source("a.example.com", 0, Some(NOW + 3_600), &["a"]),
            source("b.example.com", 0, Some(NOW + 600), &["b"]),
            source("c.example.com", 0, Some(NOW + 999_999), &["c"]),
        ];

        let (_, max_age) = decode(&mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload"));

        assert_eq!(max_age, 600, "should take the earliest exp");
    }

    #[test]
    fn an_oversized_set_drops_whole_sources_lowest_priority_then_largest() {
        let big = |name: &str, priority, count| {
            let ids: Vec<String> = (0..count).map(|index| format!("{name}-{index:040}")).collect();
            source(name, priority, None, &ids.iter().map(String::as_str).collect::<Vec<_>>())
        };
        let sources = [
            big("keep-high", 10, 6),
            big("drop-low-large", 0, 12),
            big("keep-low-small", 0, 6),
            big("keep-mid", 5, 6),
        ];

        let payload = mirror_payload(MirrorRead::Servable(&sources), NOW).expect("should return a payload");

        let MirrorPayload::Value { value, .. } = &payload else { panic!("should hold a value") };
        assert!(value.len() <= MIRROR_MAX_VALUE_BYTES, "should fit the cap");
        let (decoded, _) = decode(&payload);
        let kept: Vec<&str> =
            decoded["eids"].as_array().expect("should be an array").iter().map(|eid| eid["source"].as_str().expect("should name the source")).collect();
        assert_eq!(kept, vec!["keep-high", "keep-low-small", "keep-mid"], "should drop the largest lowest-priority source whole");
    }

    #[test]
    fn a_set_where_nothing_fits_is_clear() {
        let ids: Vec<String> = (0..80).map(|index| format!("only-{index:040}")).collect();
        let sources = [source("only.example.com", 0, None, &ids.iter().map(String::as_str).collect::<Vec<_>>())];

        assert_eq!(
            mirror_payload(MirrorRead::Servable(&sources), NOW),
            Some(MirrorPayload::Clear),
            "should clear rather than keep an empty or partial set"
        );
    }
}
```

Check the sizes once before Step 3: in `an_oversized_set_drops_whole_sources_lowest_priority_then_largest`, the four sources together must encode to more than 2,048 bytes and the three kept ones to at most 2,048. Adjust the UID counts if the encoding differs, keeping the order of the rule the test pins.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::mirror`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The `ts-eids-s` mirror payload: the servable server set for page scripts,
//! capped to one cookie (spec 6.2). The server never reads the cookie back.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use super::ids::IdentityProviderId;
use crate::openrtb::Eid;

/// Maximum bytes of the encoded cookie value.
pub(crate) const MIRROR_MAX_VALUE_BYTES: usize = 2048;
/// Maximum cookie lifetime: that of `ts-eids`.
pub(crate) const MIRROR_MAX_AGE_SEC: u64 = 86_400;

/// One source's servable server entries, as bidders receive them.
pub(crate) struct MirrorSource {
    pub(crate) source: String,
    /// The provider whose `cookie_priority` ranks the source, for the log.
    pub(crate) provider: Option<IdentityProviderId>,
    pub(crate) priority: i32,
    /// The source's earliest `exp`, in Unix seconds.
    pub(crate) expires_at: Option<u64>,
    pub(crate) eids: Vec<Eid>,
}

/// What the request's KV read gave the mirror.
pub(crate) enum MirrorRead<'a> {
    /// No KV read: the response carries no mirror member.
    NotRead,
    /// Consent denies EID persistence, or the row is a tombstone, withdrawn
    /// or missing.
    Cleared,
    /// The servable server set, in output order.
    Servable(&'a [MirrorSource]),
}

/// The mirror member of an `/auction` or `/_ts/page-bids` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirrorPayload {
    Value { value: String, max_age: u64 },
    Clear,
}

impl MirrorPayload {
    /// The JSON the shim reads.
    #[must_use]
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Value { value, max_age } => json!({ "value": value, "max_age": max_age }),
            Self::Clear => json!({ "clear": true }),
        }
    }
}

fn encode(sources: &[&MirrorSource]) -> String {
    let eids: Vec<&Eid> = sources.iter().flat_map(|source| &source.eids).collect();
    let bytes = serde_json::to_vec(&json!({ "v": 1, "eids": eids })).expect("should serialize EIDs");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn source_bytes(source: &MirrorSource) -> usize {
    serde_json::to_vec(&source.eids).map_or(usize::MAX, |bytes| bytes.len())
}

/// The mirror payload for a request (spec 6.2).
///
/// # Performance
///
/// Encodes the set once per omission. A set holds at most one entry set per
/// configured source, so this stays small.
#[must_use]
pub(crate) fn mirror_payload(read: MirrorRead<'_>, now: u64) -> Option<MirrorPayload> {
    let sources = match read {
        MirrorRead::NotRead => return None,
        MirrorRead::Cleared => return Some(MirrorPayload::Clear),
        MirrorRead::Servable(sources) => sources,
    };
    let mut kept: Vec<&MirrorSource> = sources.iter().collect();
    let mut value = encode(&kept);
    while value.len() > MIRROR_MAX_VALUE_BYTES && !kept.is_empty() {
        let index = (0..kept.len())
            .min_by_key(|&index| (kept[index].priority, core::cmp::Reverse(source_bytes(kept[index])), core::cmp::Reverse(index)))
            .expect("should hold a source");
        let left_out = kept.remove(index);
        log::info!(
            "Leaving source '{}' of provider '{}' out of the ts-eids-s mirror, which exceeds {MIRROR_MAX_VALUE_BYTES} bytes",
            left_out.source,
            left_out.provider.as_ref().map_or("none", IdentityProviderId::as_str),
        );
        value = encode(&kept);
    }
    if kept.is_empty() {
        return Some(MirrorPayload::Clear);
    }
    let max_age = kept
        .iter()
        .filter_map(|source| source.expires_at)
        .map(|exp| exp.saturating_sub(now).max(1))
        .fold(MIRROR_MAX_AGE_SEC, u64::min);
    Some(MirrorPayload::Value { value, max_age })
}
```

The `Reverse(index)` term makes "the later in input order" the tie-break, so the result never depends on sort stability. Register the module in `mod.rs` (`pub(crate) mod mirror;`) and in `migration_guards.rs`. `base64` is already a dependency of core: Task 9's `HashedEmail::to_base64` uses it.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::mirror`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add the ts-eids-s mirror payload and its size cap" -m "The payload holds the servable server entries as bidders receive them, base64url-encoded, or tells the shim to clear the cookie. A set over 2,048 bytes leaves out whole sources, lowest cookie_priority first, then largest, and max_age never outlives the earliest exp."
```

---

### Task 18: The shared removal write

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/removal.rs`
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (a crate-visible generation-checked write)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `removal.rs`

**Interfaces:**

- Consumes: `IdentityMutation`, `removals()`, `removal_labels()`, `apply_removals_only`, `ApplyContext` (Task 15), `KvIdentityGraph::load_snapshot`, `EcKvSnapshot`, `MAX_CAS_RETRIES` (`ec/kv.rs:28`)
- Produces:
  - `KvIdentityGraph::write_if_generation(&self, ec_id: &str, entry: &KvEntry, generation: u64) -> Result<EcKvWriteOutcome, Report<TrustedServerError>>`, `pub(crate)`: serializes with `serialize_entry` (so `validate` runs) and writes with `ENTRY_TTL` and `EcKvWriteMode::IfGenerationMatch(generation)`. Phase 4's flush (Task 29) uses it too, so every identity write goes through one path.
  - `pub(crate) const REMOVAL_WRITE_ATTEMPTS: u32 = MAX_CAS_RETRIES;` (5)
  - `pub enum RemovalWriteOutcome { Written, NothingToWrite, Failed }`. `NothingToWrite` covers a tombstone, `consent.ok = false`, a missing row and removals that are already applied. `Failed` covers a failed read, a store error other than the per-key limit, a per-key limit before send, and five generation mismatches or per-key limits in a row. It is `pub`, not `pub(crate)`, because the public `FlushOutcome::WriteFailed` (Task 29) carries it, and `private_interfaces` would fail the clippy gate otherwise.
  - `pub(crate) fn write_removals(graph: &KvIdentityGraph, ec_id: &str, removals: &IdentityMutation, context: &ApplyContext<'_>) -> RemovalWriteOutcome`
  - `pub(crate) fn log_removal_lost(ec_id: &str, removals: &IdentityMutation)`: logs at error with `log_id(ec_id)` and the provider IDs and sources from `removal_labels()`, never a UID. The caller increments `removal_lost` (Task 23 adds the counter to `RequestIdentity`).

This is the CAS loop the header's G4 note names: the capture removal write (spec 5.3, Task 38) and the removal-only retry of a failed flush (spec 5.2, phase 4, step 5; Task 29) both call `write_removals`. The withdrawal tombstone keeps its own loop in `ec/kv.rs` (`tombstone_existing_from_snapshot`, `kv.rs:1349`). Task 27 gives it its after-send attempt.

The loop (spec 5.3):

1. `graph.load_snapshot(ec_id)`. `Missing` gives `NothingToWrite`. `Failed` gives `Failed`. `Present` without a generation is treated as `Failed`, as `upsert_partner_ids_from_snapshot` does (`kv.rs:775-781`).
2. `apply_removals_only(removals, &entry, context)`. `None` (a tombstone, `consent.ok = false`, or a removals-only row that fails validation) gives `NothingToWrite`. `changed == false` gives `NothingToWrite`: the removals already applied.
3. `graph.write_if_generation(ec_id, &applied.entry, generation)`. `Written` gives `Written`. `PreconditionFailed` re-reads and goes back to step 1, up to `REMOVAL_WRITE_ATTEMPTS` writes in total. With `RemovalTiming::AfterSend`, an `Err` carrying `EcKvRateLimited` calls `graph.pause(PER_KEY_WRITE_WINDOW)` and is retried the same way. Any other `Err` gives `Failed` at once.

   `write_removals` takes a `RemovalTiming`: `BeforeSend` for the capture write before send and the admin clear, `AfterSend` for the phase 4 removal-only retry and the capture's after-send attempt. `AfterSend` first calls `graph.pause(PER_KEY_WRITE_WINDOW)` (1 second), because the write it follows failed moments ago on the same key. `EcKvStore::pause` defaults to a no-op; the Fastly adapter sleeps, and maps `KVStoreError::TooManyRequests` to an error with an `EcKvRateLimited` attachment. Tests: before send never waits and stops at the limit; after send waits once before writing, waits again and retries a limit, gives up after five, and does not retry other errors; a failed flush waits one window before its removal write, and a written flush never waits.

4. Five `PreconditionFailed` in a row give `Failed`.

`write_removals` runs whatever the request's consent: a revocation can arrive when consent is unusable. It does not log a lost removal itself, because a capture tries once more after send (Task 38) before the removal counts as lost. `removals` must hold only removals, opt-outs and clears: callers pass `mutation.removals()`. `apply_removals_only` ignores anything else anyway.

First add to Task 15's `IdentityMutation`: `pub(crate) fn removal_labels(&self) -> Vec<(IdentityProviderId, Option<SourceDomain>)>`, one pair per `OwnerRemove` (its provider and source), `WriterRemove` (its writer and source), opt-out and clear (the provider, no source), sorted and de-duplicated. Add a unit test for it to `mutation.rs`.

- [ ] **Step 1: Write the failing tests**

The tests drive `write_removals` against the in-memory store with generation tracking, `InMemoryEcKv` (`ec/kv_backend.rs:316`), and a local wrapper modelled on `ConflictInjectingEcKv` (`ec/kv.rs:1877`) that fails the first `n` generation-checked writes with `PreconditionFailed`, or every write with an error. Count writes the way `RecordingEcKv` (`ec/kv.rs:3449`) does.

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::mutation::{PartnerIdChange, ProviderStateChange};
    use crate::ec::identity::revision::{RecordRevision, Revision};
    use crate::ec::identity::test_support::fixed_provider;
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId};

    const EC_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000.ABC123";

    /// Fails the first `conflicts` generation-checked writes with
    /// `PreconditionFailed`, or every write with an error when `error` is set.
    struct ScriptedEcKv { /* inner: InMemoryEcKv, conflicts, error, writes */ }

    /// Seeds `entry` with `create`, then wraps the store. Outline only:
    /// write the body after `ConflictInjectingEcKv` (`ec/kv.rs:1877`).
    fn graph_with(entry: Option<&KvEntry>, conflicts: u32, error: bool) -> (KvIdentityGraph, Arc<ScriptedEcKv>);

    fn removal() -> IdentityMutation {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: "a.example.com".parse().expect("should parse the source"),
            provider: "owner".parse().expect("should parse the provider ID"),
            expected: Revision::Set(RecordRevision::from_stored("1111111111111111")),
        });
        mutation
    }

    fn live() -> KvEntry {
        let mut entry = KvEntry::minimal("b.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId { w: Some("owner".to_owned()), rv: Some("1111111111111111".to_owned()), ..KvPartnerId::new("u") },
        );
        entry
    }

    fn run(graph: &KvIdentityGraph, removals: &IdentityMutation) -> RemovalWriteOutcome {
        let random = SeededRandom::new(1);
        let catalog = ProviderCatalog::new(
            vec![fixed_provider("owner", &["a.example.com"])],
            [("a.example.com".parse().expect("should parse the source"), "owner".parse().expect("should parse the provider ID"))]
                .into_iter()
                .collect(),
        );
        write_removals(graph, EC_ID, removals, &ApplyContext { now: 1_000, random: &random, catalog: &catalog })
    }

    #[test]
    fn writes_the_removal_once_with_a_generation_check() {
        let (graph, store) = graph_with(Some(&live()), 0, false);

        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::Written, "should write");

        assert_eq!(store.generation_checked_writes(), 1, "should write once, with IfGenerationMatch");
        let (entry, _) = graph.get(EC_ID).expect("should read").expect("should hold the row");
        assert!(!entry.ids.contains_key("a.example.com"), "should remove the record");
        assert_eq!(entry.ps["owner"].iv.len(), 1, "should hash the removed UID into iv");
    }

    #[test]
    fn retries_only_a_generation_mismatch_up_to_five_writes() {
        let (graph, store) = graph_with(Some(&live()), 2, false);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::Written, "should win on the third write");
        assert_eq!(store.generation_checked_writes(), 3, "should re-read and retry each conflict");

        let (graph, store) = graph_with(Some(&live()), 5, false);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::Failed, "should give up after five conflicts");
        assert_eq!(store.generation_checked_writes(), 5, "should stop at five writes");

        let (graph, store) = graph_with(Some(&live()), 0, true);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::Failed, "should stop at a non-CAS error");
        assert_eq!(store.generation_checked_writes(), 1, "should not retry a non-CAS error");
    }

    #[test]
    fn writes_nothing_for_a_tombstone_a_missing_row_or_an_applied_removal() {
        let (graph, store) = graph_with(Some(&KvEntry::tombstone(1)), 0, false);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::NothingToWrite, "should skip a tombstone");

        let (graph, _) = graph_with(None, 0, false);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::NothingToWrite, "should skip a missing row");

        let (graph, _) = graph_with(Some(&KvEntry::minimal("b.example.com", "x", 1)), 0, false);
        assert_eq!(run(&graph, &removal()), RemovalWriteOutcome::NothingToWrite, "should skip an applied removal");
        assert_eq!(store.generation_checked_writes(), 0, "should not write a tombstone");
    }

    #[test]
    fn an_opt_out_and_its_clear_are_written_without_positive_changes() {
        let (graph, _) = graph_with(Some(&live()), 0, false);
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            "owner".parse().expect("should parse the provider ID"),
            ProviderStateChange { opt_out: Some(KvOptOut { at: 1_000, fp: "1".to_owned() }), ..ProviderStateChange::default() },
        );

        assert_eq!(run(&graph, &mutation.removals()), RemovalWriteOutcome::Written, "should write the opt-out");

        let (entry, _) = graph.get(EC_ID).expect("should read").expect("should hold the row");
        assert!(entry.ps["owner"].o.is_some(), "should store o");
        assert!(!entry.ids.contains_key("a.example.com"), "should remove what the provider wrote");
    }
}
```

The `ScriptedEcKv` and `graph_with` bodies above are outlines: write them in full, after `ConflictInjectingEcKv`. Keep the `ProviderStateChange` struct update syntax: these tests set one field.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::removal`
Expected: compile errors.

- [ ] **Step 3: Implement**

In `ec/kv.rs`, next to `upsert_partner_ids_from_snapshot`:

```rust
    /// Writes `entry` only if the row is still at `generation`.
    ///
    /// The identity removal write and the phase 4 flush share this path, so
    /// every identity write is validated and generation-checked.
    ///
    /// # Errors
    ///
    /// Returns an error when `entry` does not serialize or validate, or the
    /// store write fails for a reason other than a generation mismatch.
    pub(crate) fn write_if_generation(
        &self,
        ec_id: &str,
        entry: &KvEntry,
        generation: u64,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let (body, metadata) = Self::serialize_entry(entry, self.store_name())?;
        self.write_entry(ec_id, &body, &metadata, ENTRY_TTL, EcKvWriteMode::IfGenerationMatch(generation))
    }
```

`removal.rs`:

```rust
//! The shared removal write: a removal-only mutation written with the
//! withdrawal path's CAS loop (spec 5.2 phase 4, 5.3).

use super::ids::SourceDomain;
use super::mutation::{ApplyContext, IdentityMutation, apply_removals_only};
use crate::ec::kv::{EcKvSnapshot, KvIdentityGraph, MAX_CAS_RETRIES};
use crate::ec::kv_backend::EcKvWriteOutcome;
use crate::ec::log_id;

/// Writes the removal write makes at most, as the withdrawal path does.
pub(crate) const REMOVAL_WRITE_ATTEMPTS: u32 = MAX_CAS_RETRIES;

/// How a removal write ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalWriteOutcome {
    /// The removals are in KV.
    Written,
    /// Nothing to write: a tombstone, a missing row, or already applied.
    NothingToWrite,
    /// Not written. The caller may try once more after send.
    Failed,
}

/// Reads the row, applies only `removals` and writes them with a CAS,
/// retrying only a generation mismatch.
#[must_use]
pub(crate) fn write_removals(
    graph: &KvIdentityGraph,
    ec_id: &str,
    removals: &IdentityMutation,
    context: &ApplyContext<'_>,
) -> RemovalWriteOutcome {
    for _attempt in 0..REMOVAL_WRITE_ATTEMPTS {
        let (entry, generation) = match graph.load_snapshot(ec_id) {
            EcKvSnapshot::Present { entry, generation: Some(generation), .. } => (entry, generation),
            EcKvSnapshot::Missing { .. } => return RemovalWriteOutcome::NothingToWrite,
            _ => return RemovalWriteOutcome::Failed,
        };
        let Some(applied) = apply_removals_only(removals, &entry, context).filter(|applied| applied.changed) else {
            return RemovalWriteOutcome::NothingToWrite;
        };
        match graph.write_if_generation(ec_id, &applied.entry, generation) {
            Ok(EcKvWriteOutcome::Written) => return RemovalWriteOutcome::Written,
            Ok(EcKvWriteOutcome::PreconditionFailed) => {}
            Err(error) => {
                log::warn!("Identity removal write failed for '{}': {error:?}", log_id(ec_id));
                return RemovalWriteOutcome::Failed;
            }
        }
    }
    log::warn!(
        "Identity removal write for '{}': CAS conflict after {REMOVAL_WRITE_ATTEMPTS} attempts",
        log_id(ec_id)
    );
    RemovalWriteOutcome::Failed
}

/// Logs a removal that every attempt failed to write. Names providers and
/// sources only, never a UID.
pub(crate) fn log_removal_lost(ec_id: &str, removals: &IdentityMutation) {
    for (provider, source) in removals.removal_labels() {
        log::error!(
            "Identity removal lost for '{}': provider '{provider}', source '{}'",
            log_id(ec_id),
            source.as_ref().map_or("*", SourceDomain::as_str),
        );
    }
}
```

Check the real import paths of `EcKvSnapshot`, `EcKvWriteOutcome`, `MAX_CAS_RETRIES` and `log_id` against `ec/kv.rs`, and make `MAX_CAS_RETRIES` `pub(crate)` if it is private. Register the module in `mod.rs` (`pub(crate) mod removal;`) and in `migration_guards.rs`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::removal` and `cargo test-fastly -- ec::identity::mutation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec
git commit --signoff -S -m "Add the shared identity removal write" -m "A removal-only mutation is read, applied and written with the withdrawal path's CAS loop: up to five attempts, retrying only a generation mismatch. It writes nothing for a tombstone, a missing row or a removal that already applied. The capture removal write and the removal-only retry of a failed flush both use it."
```

---

### Task 19: `[ec.identity]` settings

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (`IdentitySettings`, `ProviderSettings`, `ProviderMode` and `RevokeScope` added next to `Ec` at about line 542; `Ec.identity`; `Ec.pull_sync_concurrency` removed)
- Modify: `crates/trusted-server-core/src/config.rs` (`validate_identity_settings` and `validate_removed_keys`, called from both validators)
- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs` (reads `[ec.identity].max_concurrency` until Task 29 removes it), `crates/trusted-server-core/src/config_payload.rs` (the round-trip test at about line 294)
- Modify: `trusted-server.example.toml` (line 83), `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml` (line 15), the Viceroy configs generated from it (`scripts/generate-integration-viceroy-configs.sh`), and `docs/guide/configuration.md` (the `pull_sync_concurrency` row at about line 646)
- Test: `settings.rs`, `config.rs`, and the CLI overlay test `crates/trusted-server-cli/tests/config_env_overlay.rs`, which reads the integration fixture

**Interfaces:**

- Produces, all in `settings.rs` unless noted:
  - `IdentitySettings { max_concurrency: usize, hold_budget_ms: u64, hold_retry_sec: u64, max_response_bytes: usize, providers: BTreeMap<String, ProviderSettings> }`, deriving `Debug, Clone, Deserialize, Serialize` (the settings envelope serializes `Ec`), with spec 10.1's defaults: 4, 100, 300 and 65,536
  - A hand-written `impl Default for IdentitySettings` that calls the same `default_*` functions the serde attributes name. A derived `Default` would give `hold_budget_ms = 0` and the other zeros, which disagree with the serde defaults and fail validation.
  - `ProviderSettings { mode: ProviderMode, timeout_ms: Option<u64>, after_send_timeout_ms: u64, sources: Option<Vec<String>>, refresh_after_sec: u64, max_calls_per_minute: u32, breaker_failures_per_minute: u32, cookie_priority: i32, revoke_scope: Option<RevokeScope> }`, with the same hand-written `impl Default`. Defaults: `AfterSend`, none, 1,000, none, 86,400, 1, 50, 0, none.
  - `enum ProviderMode { Hold, AfterSend }`, serde `snake_case`, default `AfterSend`
  - `pub enum RevokeScope { Provider, Global }`, serde `snake_case`, no `Default` derive: an unset value is `None`, and `ProviderSettings::revoke_scope(&self) -> RevokeScope` supplies `Provider`. Nothing in Part 1 reads it. Task 38 applies it.
  - `ProviderSettings::hold_timeout_ms(&self, identity: &IdentitySettings) -> u64`: `timeout_ms`, else `hold_budget_ms`. The coordinator caps it at the remaining hold budget (Task 37). Any dispatch after send uses `after_send_timeout_ms`, including a hold-mode provider on a navigation that runs no auction (spec 10.1).
  - `IdentitySettings::provider(&self, id: &IdentityProviderId) -> ProviderSettings`, which returns the defaults when there is no table. Task 22 adds `provider_with_base` beside it without changing this signature.
  - `Ec.identity: IdentitySettings` with `#[serde(default)]`
  - `Ec.pull_sync_concurrency` is removed. In its place, `#[serde(default, rename = "pull_sync_concurrency", skip_serializing)] pub removed_pull_sync_concurrency: Option<serde_json::Value>` catches the key, so `deny_unknown_fields` does not reject it with a generic message, and validation names its replacement instead.
  - In `config.rs`: `pub(crate) fn validate_identity_settings(settings: &IdentitySettings) -> Result<(), Report<TrustedServerError>>`, covering the numeric rules of spec 10.2: `hold_budget_ms <= 1000`, `max_concurrency != 0`, `refresh_after_sec > 0`, `after_send_timeout_ms` in `1..=10_000`, and `timeout_ms <= hold_budget_ms` for a hold-mode provider. A `mode` or `revoke_scope` that is not one of its values fails to parse, so serde enforces those rules. Rules that need descriptors (an unknown provider ID, `revoke_scope` for a provider without `captures`) live in Task 22's `validate_identity_catalog`, which Task 33 extends.
  - In `config.rs`: `pub(crate) fn validate_removed_keys(settings: &Settings) -> Result<(), Report<TrustedServerError>>`. In this task it rejects `ec.pull_sync_concurrency` with an error naming `[ec.identity].max_concurrency`. Task 22 adds `pull_sync_ttl_sec` and `pull_sync_rate_limit` on `[[ec.partners]]`.

Spec 10.2 removes the key outright instead of deprecating it, so there is no fallback and no warning: `effective_max_concurrency` and its legacy argument do not exist. Every config that sets the key fails validation until it moves the value, and the error says where.

- [ ] **Step 1: Write the failing tests**

In the `settings.rs` test module (each name starts with `identity_settings_`, so one filter runs them all):

```rust
    #[test]
    fn identity_settings_defaults_without_any_table() {
        let settings: IdentitySettings = toml::from_str("").expect("should parse an empty table");
        assert_eq!(settings.max_concurrency, 4, "should default the concurrency to 4");
        assert_eq!(settings.hold_budget_ms, 100, "should default the hold budget to 100 ms");
        assert_eq!(settings.hold_retry_sec, 300, "should default the hold retry to 300 s");
        assert_eq!(settings.max_response_bytes, 65_536, "should default the response cap to 64 KiB");
        let provider = settings.provider(&"anything".parse().expect("should parse the provider id"));
        assert_eq!(provider.mode, ProviderMode::AfterSend, "should default to after-send mode");
        assert_eq!(provider.after_send_timeout_ms, 1000, "should default the after-send timeout to 1 s");
        assert_eq!(provider.hold_timeout_ms(&settings), 100, "should default the hold timeout to the budget");
        assert_eq!(
            (provider.refresh_after_sec, provider.max_calls_per_minute, provider.breaker_failures_per_minute, provider.cookie_priority),
            (86_400, 1, 50, 0),
            "should use the spec 10.1 provider defaults"
        );
        assert_eq!(provider.revoke_scope(), RevokeScope::Provider, "should scope revokes to the provider by default");
        validate_identity_settings(&settings).expect("should accept the defaults");
    }

    #[test]
    fn identity_settings_default_matches_an_empty_table() {
        let parsed: IdentitySettings = toml::from_str("").expect("should parse an empty table");
        assert_eq!(
            serde_json::to_value(IdentitySettings::default()).expect("should serialize the default"),
            serde_json::to_value(parsed).expect("should serialize the parsed value"),
            "should give Default the serde defaults, not zeros"
        );
        let provider: ProviderSettings = toml::from_str("").expect("should parse an empty provider table");
        assert_eq!(
            serde_json::to_value(ProviderSettings::default()).expect("should serialize the default"),
            serde_json::to_value(provider).expect("should serialize the parsed value"),
            "should give the provider Default the serde defaults"
        );
    }

    #[test]
    fn identity_settings_timeouts_and_bounds_are_enforced() {
        let settings: IdentitySettings = toml::from_str(
            "hold_budget_ms = 120\n[providers.id5]\nmode = \"hold\"\nafter_send_timeout_ms = 2500\n",
        )
        .expect("should parse a hold-mode table");
        let provider = settings.provider(&"id5".parse().expect("should parse the provider id"));
        assert_eq!(provider.hold_timeout_ms(&settings), 120, "should use the budget as the hold timeout");
        assert_eq!(provider.after_send_timeout_ms, 2500, "should read the after-send timeout");
        validate_identity_settings(&settings).expect("should accept a 2.5 s after-send timeout");

        for (name, toml_text) in [
            ("budget over 1000", "hold_budget_ms = 1001"),
            ("zero concurrency", "max_concurrency = 0"),
            ("zero refresh", "[providers.p]\nrefresh_after_sec = 0"),
            ("zero after-send timeout", "[providers.p]\nafter_send_timeout_ms = 0"),
            ("after-send timeout over 10 s", "[providers.p]\nafter_send_timeout_ms = 10001"),
            ("hold timeout over budget", "[providers.p]\nmode = \"hold\"\ntimeout_ms = 101"),
        ] {
            let settings: IdentitySettings = toml::from_str(toml_text).expect("should parse before validation");
            validate_identity_settings(&settings).expect_err(&format!("should reject {name}"));
        }
        let edge: IdentitySettings = toml::from_str("[providers.p]\nafter_send_timeout_ms = 10000")
            .expect("should parse the upper bound");
        validate_identity_settings(&edge).expect("control: should accept exactly 10 s");
        toml::from_str::<IdentitySettings>("[providers.p]\nmode = \"sometimes\"").expect_err("should reject an unknown mode");
        toml::from_str::<IdentitySettings>("[providers.p]\nrevoke_scope = \"everyone\"").expect_err("should reject an unknown revoke scope");
    }

    #[test]
    fn identity_settings_revoke_scope_parses_and_keeps_its_default_when_unset() {
        let settings: IdentitySettings =
            toml::from_str("[providers.lockr]\nrevoke_scope = \"global\"\n").expect("should parse the revoke scope");
        let lockr = settings.provider(&"lockr".parse().expect("should parse the provider id"));
        assert_eq!(lockr.revoke_scope(), RevokeScope::Global, "should read a global revoke scope");
        let other = settings.provider(&"other".parse().expect("should parse the provider id"));
        assert_eq!(other.revoke_scope(), RevokeScope::Provider, "control: another provider should keep the provider scope");
        toml::from_str::<IdentitySettings>("[providers.lockr]\ncapture_enabled = true\n")
            .expect("should still load a key this build does not know");
    }
```

Core already has `toml` as a dependency (`crates/trusted-server-core/Cargo.toml`), so the tests parse with `toml::from_str`. The last assertion pins the lenient tables: a provider table holding a key this build does not know still loads, so a rollback from a later Part keeps loading its config. `capture_enabled` was a key of earlier spec revisions and is the example.

Also in `settings.rs` tests, add `identity_settings_ec_without_identity_table_parses`: a full settings TOML with no `[ec.identity]`, loaded the way the existing `Ec` tests do, gets the defaults.

In `config.rs` tests, add `removed_pull_sync_concurrency_is_rejected_with_its_replacement`: settings with `ec.pull_sync_concurrency = 3` parse, then fail both validators with an error whose text contains `[ec.identity].max_concurrency`. The control, the same settings with `[ec.identity] max_concurrency = 3` instead, passes.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- identity_settings` and `cargo test-fastly -- config::tests::removed_pull_sync_concurrency`
Expected: compile errors.

- [ ] **Step 3: Implement**

Add `IdentitySettings`, `ProviderSettings`, `ProviderMode` and `RevokeScope` in `settings.rs` next to `Ec`, with each field under `#[serde(default = "...")]` and the hand-written `Default` impls. Do not move these types to another file.

`IdentitySettings` and `ProviderSettings` stay lenient: no `#[serde(deny_unknown_fields)]`, unlike the rest of `settings.rs`, so a rollback to an earlier Part still loads a config written for a later one (spec 14). Add `Ec.identity`:

```rust
    /// Server-side identity provider policy (`[ec.identity]`).
    #[serde(default)]
    pub identity: IdentitySettings,
```

Replace `Ec.pull_sync_concurrency` and `Ec::default_pull_sync_concurrency` with the catch field from Interfaces, documented as removed and rejected by validation. In `ec/pull_sync.rs`, read `settings.ec.identity.max_concurrency.max(1)` where it read `settings.ec.pull_sync_concurrency.max(1)` (`pull_sync.rs:184`), and move its tests' `settings.ec.pull_sync_concurrency = N` to `settings.ec.identity.max_concurrency = N`. Task 29 removes the file. Fix `config_payload.rs:294` to compare `ec.identity.max_concurrency`.

With the new defaults, the serialized `Ec` holds every `[ec.identity]` default, so whole-config snapshot, envelope-hash and CLI snapshot tests change. Update their expected values in this task and name them in the commit body.

Call `validate_identity_settings(&settings.ec.identity)` and `validate_removed_keys(settings)` from both `validate_settings_for_deploy` (`config.rs:249`) and `validate_settings_for_runtime` (`config.rs:270`), next to `PartnerRegistry::...`.

Every config that sets the removed key must move it in this task:

- `trusted-server.example.toml` (line 83): replace `pull_sync_concurrency = 3` with an `[ec.identity]` table holding `max_concurrency = 3`. Add spec 10.1's other `[ec.identity]` keys and one commented `[ec.identity.providers.<id>]` table (with `after_send_timeout_ms`, `cookie_priority` and `revoke_scope`) as a commented block, with `example.com` partner names.
- `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml` (line 15), which the parity tests, `src/bin/generate-viceroy-config.rs` and the CLI overlay test read: the same change. Then regenerate the Viceroy configs with `scripts/generate-integration-viceroy-configs.sh`.
- `docs/guide/configuration.md`: replace the `pull_sync_concurrency` row with an `[ec.identity]` section that lists the keys and defaults of spec 10.1. Task 31 adds the rest of the docs.

The example-config and CLI template tests must keep passing (`grep -rn "trusted-server.example.toml" crates/ --include=*.rs`); run `./scripts/test-cli.sh` for the CLI ones.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- identity_settings`, `cargo test-fastly -- settings`, `cargo test-fastly -- config::`, `cargo test-fastly -- ec::pull_sync`, `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` and `./scripts/test-cli.sh`
Expected: PASS. A config with no `[ec.identity]` still loads with the defaults. A config with `ec.pull_sync_concurrency` is rejected with an error that names its replacement.

- [ ] **Step 5: Commit**

```bash
git add -A crates/ trusted-server.example.toml docs/guide/configuration.md
git commit --signoff -S -m "Add [ec.identity] coordinator settings" -m "Adds coordinator policy and per-provider tables, including after_send_timeout_ms and revoke_scope. ec.pull_sync_concurrency is removed: a config that sets it fails validation with an error naming [ec.identity].max_concurrency, and the example config and the integration fixture move their value there."
```

---

### Task 20: Coordinator eligibility and outcomes

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (pure logic only in this task)
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs`, `mod.rs`, `migration_guards.rs`
- Test: `coordinator.rs`

**Interfaces:**

- Consumes: catalog, servable, mutation, revision, settings, policy (Tasks 12 to 19), and `entry_sets`, `EntrySets` and `EntrySet` (Task 10)
- Produces:
  - `pub(crate) struct EligibilityView`, the view of spec 4.7, built by `EligibilityView::from_kv(entry: &KvEntry) -> Self` from the row the request read, and nothing else. It holds the per-source records (whole `KvPartnerId`s) and, per provider, `n`, `f`, `o`, `oc`, `iv`, `ov` and the `st` the call reads. It never reads `ts-eids-s`: every removal is already in KV (spec 5.3).
  - `EligibilityView::is_opted_out(&self, provider: &IdentityProviderId) -> bool`: `o` is set and `oc` is not strictly later than `o.at`
  - `pub(crate) struct RequestFacts { is_real_browser, ec_persistence_allowed, policy_signals: PolicySignals, writer: bool, new_visitor: bool, runs_auction: bool, now: UnixTime, available_inputs: BTreeSet<Input> }`
  - `pub(crate) enum Phase { Hold, AfterSend }`
  - `pub(crate) enum Eligibility { Eligible { kinds: BTreeMap<SourceDomain, ResolveKind> }, Skip(SkipReason) }`, with a `SkipReason` enum naming each of steps 1 to 7 (step 6, the breaker, is Task 35's; step 8, the burst cap, is checked at dispatch)
  - `pub(crate) struct EligibilityInput<'a> { provider: &'a dyn IdentityProvider, settings: &'a ProviderSettings, identity: &'a IdentitySettings, catalog: &'a ProviderCatalog, consent: &'a ConsentContext }`, so `eligibility` stays within 7 arguments
  - `pub(crate) fn eligibility(input: &EligibilityInput<'_>, view: &EligibilityView, facts: &RequestFacts, phase: Phase) -> Eligibility`
  - `pub(crate) fn fingerprint_bits(policy: &ConsentPolicy, consent: &ConsentContext, signals: PolicySignals) -> String`: the consent an opt-out's `fp` records, as a plain lowercase hex bitmask of at most 16 digits (`format!("{bits:x}")` of a `u64`), never a hash (spec 8, 9.3). Part 1 covers the signals TS decodes today, one bit each in this order: each TCF purpose the policy reads (1 to 10), the vendor bit when the policy names one, the US sale opt-out (from `us_privacy` or GPP), GPC and DNT. A signal the policy relaxes stays 0. Task 34 appends the sharing and targeted-advertising opt-outs after these and keeps these bits where they are.
  - `pub(crate) enum CallOutcome { Resolved(Resolution), Skipped, InputUnavailable, InvalidState(Option<Resolution>), HoldCutOff, InputFailure, TransportFailure, BreakerOpen, BurstCapped, NotDispatched }`. `BurstCapped` is the burst cap's skip; `NotDispatched` is a limiter error's
  - `pub(crate) fn outcome_to_changes(provider: &dyn IdentityProvider, outcome: CallOutcome, view: &EligibilityView, context: &OutcomeContext<'_>) -> OutcomeChanges`, where `OutcomeChanges { changes: Vec<PartnerIdChange>, result: Option<ProviderResult>, state: ProviderStateChange }`. This is the section 8 table, row by row. `changes` holds every removal, and the positive changes of a provider without private state. `OutcomeContext` holds `now`, `catalog`, provider settings, `identity` settings, `expected_state: Option<StateRevision>` and the consent fingerprint bits from `fingerprint_bits`.

Eligibility steps, in order (spec 4.7):

1. `facts.is_real_browser`
2. `facts.ec_persistence_allowed`
3. `policy_allows(descriptor.consent_policy(), consent, facts.policy_signals)`
4. Per-source kinds, over `catalog.sources_of(provider)`:
   - **Owned by this provider.** `Initial` when there is no record. `Refresh` when there is one and it is due: expired, written by someone else, `ra` passed, or no `ra` and `at + refresh_after_sec` passed or `at` missing. A browser-seeded record any of whose UIDs hashes into the provider's `iv` is never due (spec 7.2). An expired refresh anchor (spec 5.4) is a stored record, so its kind stays `Refresh`.
   - **Unowned.** `Initial` only, and only when KV has no unexpired ID for the source. An expired record, or one whose writer is disabled or no longer configured, counts as absent. This is not the servability check: a record hidden only by this request's consent or by an opt-out still counts as present, because `FillMissing` would not replace it and the call would be wasted.
   - A source whose kind the provider does not list, or whose needs (`resolves.needs(kind)`) are not in `facts.available_inputs`, is left out. No sources left means skip, logged at debug.
5. `n` has passed, and `view.is_opted_out(provider)` is false.
6. (Breaker: Task 35. Part 1 has no breaker.)
7. Writer and mode:
   - No provider runs when `facts.new_visitor` or when `!facts.writer`: stagers run no resolving provider in either mode (spec 5.1).
   - `Phase::Hold` needs `settings.mode == Hold` and `facts.runs_auction`.
   - `Phase::AfterSend` needs `settings.mode == AfterSend`, or `settings.mode == Hold` and `!facts.runs_auction`: a hold-mode provider on a writer navigation that runs no auction runs after send instead (spec 4.7 step 7), with `after_send_timeout_ms`.

How `Resolved(resolution)` becomes changes (spec 4.5, "From entries to an entry set", and spec 8):

1. Drop entries for sources the provider does not claim after narrowing (`catalog.sources_of`). Log each with the provider ID and source only.
2. Call `entry_sets(&kept, context.now)` (Task 10). It groups the rest by source in order and applies every limit of spec 4.5. Log each `EntryDrop` and each `oversized` source with the provider ID, source and field only, never a value.
3. A source listed in `oversized` has a **discarded set**. A source whose every entry `entry_sets` dropped is absent from `sets`, so the result does not name it and its stored set is left as it is. A set of an expiring source that carries no expiry keeps the stored `exp` when its entries equal the stored `eids`, and is a discarded set otherwise.
4. For a provider with private state, any discarded set rejects the result's positive part whole, exactly as an oversized `replacing_state` does: no `ProviderResult`, `st` unchanged, `f` and `n` as for `Malformed`. The result's removals still apply. For a provider without private state, a discarded set leaves that source's stored record as it is, as if the result did not name the source, and every other set is kept.
5. A result that named at least one source and all of whose sets are discarded is handled as `Malformed` (spec 4.5, section 8): `f += 1` with its backoff, and its removals still apply. Timing comes only from kept entries (Task 10).
6. `EntrySet.refresh_until` from a provider without private state is ignored with a warning.
7. Each kept set becomes `KvPartnerId { w: Some(provider), at: Some(now), exp, ra, ru, eids: set.entries, .. }`, with `exp`, `ra` and `ru` from the `EntrySet`. A one-UID result writes the same one-entry shape browser ingest writes. Descriptor provenance is not stored: Task 28 fills it in at output.
   7a. A `Resolution::found` that steps 4, 5 and 12 do not turn into `Malformed` sets `f = 0`, even when `entry_sets` dropped every entry and nothing is stored. It sets `f = 0` in the state it writes, in the `ProviderResult` for a provider with private state and in `state` otherwise.
8. Each record becomes `OwnerWrite` with `expected = Some(record_revision(stored))` (or `None` when no record) when this provider owns the source, else `FillMissing`. Either change replaces the source's set whole. A source the result does not name gets no change.
9. `invalidated` becomes `OwnerRemove { source, provider, expected: record_revision(stored) }` for each named source that has a record in the view, plus `n = now`. It carries no hash: `mutation::apply` (Task 15) hashes every UID of the live record it removes into the provider's `ps.iv`. A named source with no record makes no change.
10. `opted_out` becomes `WriterRemove { source, writer: provider }` for every source in the view whose record this provider wrote, and, for a capture-only provider, for every one of its `capture_only_sources` whose record has no writer. Plus `opt_out = KvOptOut { at: now, fp: fingerprint_bits }` and `st = Clear`. The mutation hashes each removed record into `ps.ov`.
11. When the provider has private state, the positive changes plus the state change plus `n` and `f` go into one `ProviderResult` with `expected_state`. Otherwise they go into `changes`, and `n` and `f` into `state`.
12. `state_rejected()` drops the positive part and applies the `Malformed` backoff.
13. `with_retry_in` on any resolution, not only `no_match`: after steps 1 to 12 set `n`, raise it to `max(n, now + retry_in)`, with `retry_in` capped at 7 d. A `found`, `invalidate` or `opted_out` with no `n` of its own gets `n = now + retry_in`.

The other rows of section 8: `Skipped` and `InputUnavailable` set `n = now + refresh_after_sec`. `BurstCapped` sets `n = max(n, now + 60)` and never counts a failure (spec 4.7 step 8). `InvalidState` clears `st`, keeps `f`, and sets `n = now + refresh_after_sec` unless its resolution sets `n`. `HoldCutOff` sets `n = now + hold_retry_sec` and never counts a failure; only a hold dispatch before the auction can be cut off. An after-send timeout is a `TransportFailure`. `InputFailure` is `f += 1`, `n = now + min(1 h × 2^(f-1), 24 h)`. `TransportFailure` is `f += 1`, `n = now + min(5 min × 2^(f-1), 1 h)`. `BreakerOpen` and `NotDispatched` change nothing. Backoff arithmetic uses `saturating_pow` and `min`, so a large `f` cannot overflow.

- [ ] **Step 1: Write the failing tests**

Write one test per eligibility step and one per section 8 row, using `fixed_provider` and `ProviderSettings::default()`. Name them after the rule, for example `skips_when_not_a_real_browser`, `skips_new_visitor_in_every_mode`, `stager_runs_no_provider_in_either_phase`, `owned_record_written_by_someone_else_is_due_for_refresh`, `browser_seeded_uid_in_iv_is_never_due`, `expired_refresh_anchor_stays_refresh`, `unowned_source_with_unexpired_record_is_not_initial_even_when_consent_hides_it`, `expired_unowned_record_counts_as_absent`, `record_of_an_unconfigured_writer_counts_as_absent`, `hold_provider_runs_before_the_auction_and_not_after_send`, `hold_provider_runs_after_send_when_the_navigation_runs_no_auction`, `after_send_provider_never_runs_in_hold`, `narrowed_away_source_is_never_a_kind`, `future_n_and_sticky_opt_out_block`, `a_later_clear_lifts_the_opt_out`, `no_match_waits_retry_in_capped_at_seven_days`, `input_failure_backs_off_exponentially_to_a_day`, `transport_failure_backs_off_to_an_hour`, `hold_cut_off_waits_hold_retry_without_counting_a_failure`, `skip_waits_refresh_after`, `invalid_state_clears_state_and_keeps_f`, `oversized_state_rejects_positive_part_but_keeps_removals`, `opted_out_removes_what_it_wrote_and_clears_state`, `capture_only_opt_out_also_removes_browser_records_of_its_sources`, `opted_out_records_the_fingerprint_of_the_current_consent`, `invalidate_removes_the_set_and_sets_n_now` and `invalidating_a_source_without_a_record_makes_no_change`. Add `fingerprint_bits_is_a_plain_hex_bitmask_and_changes_with_each_signal` (one case per signal, each against a control that differs only in it, and an assertion that the value parses with `u64::from_str_radix(.., 16)`).

Then the entry-set rules (spec 13, "Complete provider EIDs (G13)"): `found_with_several_entries_becomes_one_whole_set_per_source`, `one_uid_result_without_eid_fields_becomes_a_one_entry_record`, `unclaimed_source_entries_are_discarded`, `oversized_set_from_a_stateless_provider_leaves_only_that_source_unchanged`, `oversized_set_from_a_stateful_provider_rejects_the_positive_part_and_keeps_removals`, `result_whose_every_set_is_discarded_is_malformed`, `invalidating_a_set_carries_no_hash_until_it_applies`, `expiring_set_without_expiry_keeps_stored_exp_only_when_unchanged`, `result_without_a_source_leaves_its_set_alone`, and `record_carries_the_earliest_times_of_its_set`. Task 10 tests the limits themselves. These tests check only what the coordinator does with `entry_sets`' output.

Here are seven written out in full. Write the rest in the same shape:

```rust
    #[test]
    fn skips_new_visitor_in_every_mode() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new());
        let settings = ProviderSettings::default();
        let identity = IdentitySettings::default();
        let consent = allowing_consent();
        let input = EligibilityInput { provider: provider.as_ref(), settings: &settings, identity: &identity, catalog: &catalog, consent: &consent };
        for phase in [Phase::Hold, Phase::AfterSend] {
            let facts = RequestFacts { new_visitor: true, ..facts_for_writer() };
            assert!(
                matches!(eligibility(&input, &view, &facts, phase), Eligibility::Skip(SkipReason::NewVisitor)),
                "should run no provider on a new visitor's first navigation ({phase:?})"
            );
        }
        let returning = facts_for_writer();
        assert!(
            matches!(eligibility(&input, &view, &returning, Phase::AfterSend), Eligibility::Eligible { .. }),
            "control: a returning writer should be eligible"
        );
    }

    #[test]
    fn hold_provider_runs_after_send_when_the_navigation_runs_no_auction() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new());
        let settings = ProviderSettings { mode: ProviderMode::Hold, ..ProviderSettings::default() };
        let identity = IdentitySettings::default();
        let consent = allowing_consent();
        let input = EligibilityInput { provider: provider.as_ref(), settings: &settings, identity: &identity, catalog: &catalog, consent: &consent };
        let eligible = |facts: &RequestFacts, phase| matches!(eligibility(&input, &view, facts, phase), Eligibility::Eligible { .. });

        let no_auction = RequestFacts { runs_auction: false, ..facts_for_writer() };
        let auction = RequestFacts { runs_auction: true, ..facts_for_writer() };
        let stager = RequestFacts { writer: false, runs_auction: true, ..facts_for_writer() };

        assert!(eligible(&no_auction, Phase::AfterSend), "should move after send without an auction");
        assert!(!eligible(&no_auction, Phase::Hold), "should not hold without an auction");
        assert!(eligible(&auction, Phase::Hold), "control: should hold before an auction");
        assert!(!eligible(&auction, Phase::AfterSend), "should not also run after send");
        assert!(!eligible(&stager, Phase::Hold), "should never run on a stager");
    }

    #[test]
    fn no_match_waits_retry_in_capped_at_seven_days() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let fixture = OutcomeFixture::new();
        let context = fixture.context_at(1_000);
        let default = outcome_to_changes(provider.as_ref(), CallOutcome::Resolved(Resolution::no_match()), &EligibilityView::empty(), &context);
        assert_eq!(default.state.n, Some(1_000 + 86_400), "should wait the 24 h default");
        let long = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::no_match().with_retry_in(Duration::from_secs(30 * 86_400))),
            &EligibilityView::empty(),
            &context,
        );
        assert_eq!(long.state.n, Some(1_000 + 7 * 86_400), "should cap the wait at 7 days");
    }

    #[test]
    fn found_resets_failures_and_retry_in_applies_to_every_resolution() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a: SourceDomain = "a.example.com".parse().expect("should parse the source");
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 3);
        view.set_record_for_test(&a, record_for_test("example-old", Some("p"), 900));
        let fixture = OutcomeFixture::new();
        let context = fixture.context_at(1_000);

        let found = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::found([ResolvedEid::new(&a, ResolvedUid::new("u"))]).with_retry_in(Duration::from_secs(600))),
            &view,
            &context,
        );
        let invalidated = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::invalidate(&a).with_retry_in(Duration::from_secs(600))),
            &view,
            &context,
        );

        assert_eq!(found.state.f, Some(0), "should reset the failure count on found");
        assert_eq!(found.state.n, Some(1_000 + 600), "should wait at least retry_in after found");
        assert_eq!(invalidated.changes.len(), 1, "control: should remove the stored record");
        assert_eq!(invalidated.state.n, Some(1_000 + 600), "should raise invalidate's n = now to retry_in");
    }

    #[test]
    fn transport_failure_backs_off_to_an_hour() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 5);
        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(provider.as_ref(), CallOutcome::TransportFailure, &view, &fixture.context_at(1_000));
        assert_eq!(changes.state.f, Some(6), "should count the failure");
        assert_eq!(changes.state.n, Some(1_000 + 3_600), "should back off min(5 min x 2^5, 1 h) = 1 h");
    }

    #[test]
    fn found_with_several_entries_becomes_one_whole_set_per_source() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let resolution = Resolution::found([
            ResolvedEid::new(&a, ResolvedUid::new("example-a").with_atype(1))
                .with_uid(ResolvedUid::new("example-b").with_atype(3).with_ext(json!({ "stype": "ppuid" })))
                .with_inserter("example.com")
                .with_mm(3),
            ResolvedEid::new(&a, ResolvedUid::new("example-c")).with_matcher("partner.example.com"),
        ]);

        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(provider.as_ref(), CallOutcome::Resolved(resolution), &EligibilityView::empty(), &fixture.context_at(1_000));

        assert_eq!(changes.changes.len(), 1, "should make one change for the one source");
        let record = positive_record(&changes.changes[0]).expect("should be a positive change");
        assert_eq!(record.w.as_deref(), Some("p"), "should name the writer");
        assert_eq!(
            record.eids,
            vec![
                KvEid {
                    uids: vec![
                        KvUid { id: "example-a".to_owned(), atype: Some(1), ext: None },
                        KvUid { id: "example-b".to_owned(), atype: Some(3), ext: Some(json!({ "stype": "ppuid" })) },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                KvEid {
                    uids: vec![KvUid { id: "example-c".to_owned(), atype: None, ext: None }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
            "should store the whole set, in the order the provider returned it"
        );
    }

    #[test]
    fn oversized_set_from_a_stateful_provider_rejects_the_positive_part_and_keeps_removals() {
        let provider = stateful_fixed_provider("p", &["a.example.com", "b.example.com"]);
        let a = source("a.example.com");
        let b = source("b.example.com");
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&b, record_for_test("example-old", Some("p"), 900));
        // Three entries of about 2 KB each: the set is over MAX_ENTRY_SET_BYTES.
        let large_ext = json!({ "segments": "x".repeat(2_000) });
        let entries = (0..3).map(|index| ResolvedEid::new(&a, ResolvedUid::new(format!("example-{index}"))).with_ext(large_ext.clone()));
        let resolution = Resolution::found(entries)
            .also_invalidate(&b)
            .replacing_state(&json!({ "token": "example-token" }));

        let fixture = OutcomeFixture::new();
        let changes = outcome_to_changes(provider.as_ref(), CallOutcome::Resolved(resolution), &view, &fixture.context_at(1_000));

        assert!(changes.result.is_none(), "should store no ID and no state change");
        assert!(
            changes.changes.iter().any(|change| matches!(change, PartnerIdChange::OwnerRemove { source, .. } if *source == b)),
            "should still apply the invalidation"
        );
        assert!(matches!(changes.state.st, StateUpdate::Keep), "should keep the old state");
        assert_eq!(changes.state.f, Some(1), "should handle it as Malformed");
        assert_eq!(changes.state.n, Some(1_000 + 3_600), "should back off min(1 h x 2^0, 24 h)");
    }

    #[test]
    fn invalidating_a_set_carries_no_hash_until_it_applies() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let a = source("a.example.com");
        let fixture = OutcomeFixture::new();
        let mut record = record_for_test("example-0-0", Some("p"), 900);
        record.eids = (0..4)
            .map(|chunk| KvEid {
                uids: (0..8).map(|index| KvUid { id: format!("example-{chunk}-{index}"), atype: None, ext: None }).collect(),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            })
            .collect();
        let expected = record_revision(&record);
        let mut view = EligibilityView::empty();
        view.set_record_for_test(&a, record);

        let changes = outcome_to_changes(provider.as_ref(), CallOutcome::Resolved(Resolution::invalidate(&a)), &view, &fixture.context_at(1_000));

        assert_eq!(changes.changes.len(), 1, "should make one removal for the 32-UID set");
        assert!(
            matches!(
                &changes.changes[0],
                PartnerIdChange::OwnerRemove { source, provider, expected: revision }
                    if *source == a && provider.as_str() == "p" && *revision == expected
            ),
            "should remove the whole set at the revision it read"
        );
        assert_eq!(changes.state.n, Some(1_000), "should set n = now");
    }
```

Add these to `test_support.rs`:

- `facts_for_writer()`, which returns a real browser, persistence allowed, a writer, a returning visitor, no auction, and every input available
- `allowing_consent()`
- `OutcomeFixture`, an owned fixture holding the catalog, the provider and identity settings and the fingerprint bits, with `OutcomeFixture::new()` and `fn context_at(&self, now: u64) -> OutcomeContext<'_>`. A helper cannot return an `OutcomeContext` that borrows values it creates, so tests keep the fixture in a local and borrow from it.
- `source(domain) -> SourceDomain` and `pid(id) -> IdentityProviderId`, if Task 14 did not already add them
- `stateful_fixed_provider(id, sources)`, a `fixed_provider` declared `with_private_state()`
- `record_for_test(uid, writer: Option<&str>, at) -> KvPartnerId`, a one-UID record with a fresh `rv`
- `EligibilityView::empty()`, `set_record_for_test` and `set_failures_for_test`, `pub` under the gate

Every item in `test_support.rs` is `pub` under the module's `#![cfg(any(test, feature = "test-utils"))]` (Global Constraints), except one that names a crate-private type, which stays `pub(crate)` (Task 14).

Add `positive_record(&PartnerIdChange) -> Option<&KvPartnerId>` to the test module. It matches `FillMissing` and `OwnerWrite`, and returns `None` for the two removals.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::coordinator`
Expected: compile errors.

- [ ] **Step 3: Implement**

Implement `EligibilityView::from_kv`, `eligibility`, `fingerprint_bits` and `outcome_to_changes` as plain functions. Follow the numbered lists above. Give the entry-set steps 1 to 5 their own private helpers (`fn drop_unclaimed`, `fn discarded_sets`, `fn reject_positive_part`, `fn all_sets_discarded`), so a reviewer can match each to its spec bullet. The `iv` check of step 4 hashes each UID of the browser-seeded record with `uid_hash` (Task 13) and looks it up in the provider's `iv`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::coordinator`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add coordinator eligibility and outcome rules" -m "Eligibility reads the KV row alone. Only a returning writer runs providers; a hold-mode provider moves after send when the navigation runs no auction. Provider results become whole entry sets per source, an invalidation carries no hash until it applies, a discarded set rejects a stateful provider's positive part, and a result whose every set is discarded is Malformed. An opt-out's fingerprint is a plain consent bitmask."
```

### Task 21: Coordinator dispatch

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (fake HTTP client)
- Test: `coordinator.rs`

**Interfaces:**

- Consumes: `RuntimeServices::http_client().send_async`, `services.backend().ensure(&PlatformBackendSpec { .. })` exactly as `dispatch_pull_sync` uses them (`ec/pull_sync.rs`, the body of `dispatch_pull_sync`), the bounded decoders in `streaming_processor.rs`, and the rate limiter's `exceeded_per_minute`
- Produces:
  - `pub(crate) struct IdentityDeps<'a> { settings: &'a Settings, catalog: &'a ProviderCatalog, keys: &'a IdentityKeys, random: &'a dyn RandomSource, services: &'a RuntimeServices, rate_limiter: Option<&'a dyn RateLimiter> }`. This one struct keeps `handle_auction` and `handle_publisher_request` within 7 arguments. `keys` holds only the visitor key (Task 13). `random` draws record revisions when a mutation applies. Task 23 builds `IdentityDeps` from `IdentityRuntime`.
  - `pub(crate) struct Call { provider: IdentityProviderId, kinds: BTreeMap<SourceDomain, ResolveKind> }`
  - `pub(crate) async fn dispatch(deps: &IdentityDeps<'_>, calls: Vec<Call>, signals: &ResolveSignals<'_>, budget: Option<Duration>) -> Vec<(IdentityProviderId, CallOutcome)>`. `budget` is `Some(remaining hold budget)` for a hold dispatch before the auction and `None` after send. It awaits `send_async` and each pending response instead of calling `futures::executor::block_on`: the hold dispatch runs inside the publisher handler's executor, where a nested `block_on` panics. Only phase 4 (`run_after_send`, Task 29), which runs after send outside any executor, blocks on it.
  - `pub(crate) fn check_request(request: &ProviderRequest, descriptor: &ProviderDescriptor) -> Result<(), Report<IdentityProviderError>>`: `https` only, the host in `resolves.hosts()`, no `Cookie` header
  - `pub(crate) fn decode_body(encoding: Option<&str>, body: Vec<u8>, cap: usize) -> Option<Vec<u8>>`: identity, `gzip` or `br`, the cap applied to the decoded size, `None` when over the cap or for an unknown encoding

Dispatch per call:

1. Build the `ResolveInput` and call `request`.
   - `Ok(None)` is `Skipped`.
   - `Err` with `InputUnavailable` is `InputUnavailable`.
   - `Err` with `UndeclaredInput` or any other `Config` is `InputFailure`, logged as `Config` with `current_context()` only.
   - Any recorded undeclared read is also logged as `Config` and turns the outcome into `InputFailure`.
2. `check_request`. On failure: log `Config`, `InputFailure`.
3. Order: calls are dispatched in a stable order. Generic pull providers keep today's fairness rule (spec 10.4, `pull_sync.rs:143-156`): sorted by source domain, then rotated left by `(now / 3600) % len`, so the starting partner changes each hour when `max_concurrency` limits a batch. Other providers follow, by provider ID.
4. The burst cap: `rate_limiter.exceeded_per_minute(&format!("identity:{provider}:{}", visitor_key(keys, ec_id)), settings.max_calls_per_minute)`. Exceeded means not dispatched: the call's outcome is `BurstCapped`, which sets `n = max(n, now + 60)` (Task 20), never a failure. A limiter error is logged and the call's outcome is `NotDispatched`: not dispatched, `ps` unchanged (Task 35).
5. `ensure` a backend with first-byte timeout `after_send_timeout_ms` after send, or for a hold dispatch `min(hold_timeout_ms, remaining budget)`, rounded up to the orchestrator's buckets (`auction/orchestrator.rs`, search for the bucket rounding). A hold dispatch also sets `connect_timeout: Some(timeout.min(1 s))`; after send it is `None` (revised after the round 14 review). `PlatformBackendSpec::connect_timeout: Option<Duration>` is new: `None` keeps the Fastly adapter's 1-second default and the existing backend name, and Fastly names a set value as `_ct<ms>` and folds it into the digest. Every other spec literal sets `None`. Tests: `hold_dispatch_caps_the_connect_timeout_at_the_call_timeout`, `fastly_prediction_names_a_connect_timeout_only_when_set`. Do not follow redirects. Send with `send_async`, keeping at most `[ec.identity].max_concurrency` in flight.
6. Collect responses: check `Content-Length` against `max_response_bytes`, buffer, `decode_body`. Over the cap is `TransportFailure`. Build the `ProviderResponse` with the request's call context. Call `response`.
   - `Ok(resolution)` is `Resolved`, or `InvalidState(Some(resolution))` when the input recorded a state decode failure.
   - `Rejected` with 429 or 5xx is `TransportFailure`.
   - Other `Rejected`, `Malformed` and `Config` are `InputFailure`.
   - `InvalidState` is `InvalidState(None)`.
7. A hold-mode call still pending when the budget runs out is dropped and becomes `HoldCutOff`. After send there is no budget: a call that misses `after_send_timeout_ms` is an ordinary timeout, `TransportFailure` (spec 4.7 step 7).

- [ ] **Step 1: Write the failing tests**

Add a `FakeHttpClient` to `test_support.rs`. It implements the platform HTTP client trait that `RuntimeServices` holds (`grep -n "trait .*HttpClient" crates/trusted-server-core/src/platform/*.rs`) and returns canned responses per host, with an optional delay counter for hold-budget tests. Check first whether `auction/test_support.rs` already has one, and reuse it if so. Then test:

```rust
    #[test]
    fn check_request_enforces_https_host_and_no_cookie() {
        let descriptor = fixed_provider("p", &["a.example.com"]).descriptor().clone();
        let good = ProviderRequest::get(&"https://api.example.com/x".parse().expect("should parse the URL"));
        check_request(&good, &descriptor).expect("should allow an https request to a declared host (control)");
        for (name, request) in [
            ("http", ProviderRequest::get(&"http://api.example.com/x".parse().expect("should parse the URL"))),
            ("host", ProviderRequest::get(&"https://other.example.net/x".parse().expect("should parse the URL"))),
            ("cookie", good.clone().try_with_header("cookie", "a=1").expect("should add the header")),
        ] {
            check_request(&request, &descriptor).expect_err(&format!("should reject the {name} case"));
        }
    }

    #[test]
    fn decode_body_caps_decoded_size() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b'a'; 10_000]).expect("should write to the encoder");
        let gzipped = encoder.finish().expect("should finish the gzip stream");
        assert_eq!(decode_body(Some("gzip"), gzipped.clone(), 10_000).map(|b| b.len()), Some(10_000), "should decode a body that fits");
        assert!(decode_body(Some("gzip"), gzipped, 9_999).is_none(), "should reject a body over the cap after decoding");
        assert!(decode_body(Some("zstd"), vec![1], 10).is_none(), "should reject an unknown encoding");
        assert_eq!(decode_body(None, b"ok".to_vec(), 10).as_deref(), Some(&b"ok"[..]), "should pass an identity body through");
    }
```

The test module imports `std::io::Write as _` at its top for `write_all`.

Also write `dispatch_maps_status_codes_to_outcomes` (200 is `Resolved`, 503 is `TransportFailure`, 400 is `InputFailure`, a body over the cap is `TransportFailure`), `after_send_timeout_is_a_transport_failure_not_a_cut_off`, `burst_cap_skips_without_counting_a_failure` and `undeclared_read_is_an_input_failure_even_when_ignored`, using a provider that reads `user_agent().ok()` without declaring it, against the fake client. Port today's rotation test as `generic_pull_dispatch_order_rotates_hourly`: three generic pull providers, dispatch at `now` and at `now + 3600` with `max_concurrency = 1`, and a different first host each hour, in the order `pull_sync.rs` gives today.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::coordinator`
Expected: compile errors.

- [ ] **Step 3: Implement**

Follow the transport code in `dispatch_pull_sync` closely. It already handles backend resolution, `send_async`, draining the in-flight batch (`drain_pull_batch`), the `Content-Length` check (`response_content_length_exceeds_limit`) and buffering (`extract_pull_uid`). Move those three helpers out of `pull_sync.rs` into `coordinator.rs`, generalized to `max_response_bytes`, and make `pull_sync.rs` call the moved versions until Task 29 deletes its dispatcher. For decoding, reuse the bounded decoders in `streaming_processor.rs` if a buffered entry point exists (`grep -n "pub(crate) fn\|pub fn" crates/trusted-server-core/src/streaming_processor.rs`). Otherwise use `flate2::read::MultiGzDecoder` and `brotli::Decompressor` with `Read::take(cap as u64 + 1)` and reject when more than `cap` bytes come out.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::coordinator` and `cargo test-fastly -- ec::pull_sync`
Expected: PASS. Pull sync behaviour is unchanged so far.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Add coordinator dispatch with request checks and byte caps"
```

### Task 22: `GenericPullProvider`, the write-counting store and the basic harness

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/generic_pull.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (`ProviderHarness` basics, `WriteCountingEcKv`)
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (`ProviderCatalog::from_settings`, `validate_identity_catalog`)
- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs` (`validated_pull_sync_url` takes the URL fields instead of a `PartnerConfig`, and becomes `pub(crate)` with `build_pull_request_url`)
- Modify: `crates/trusted-server-core/src/settings.rs` (`EcPartner.pull_sync_ttl_sec` and `pull_sync_rate_limit` removed, with catch fields), and every `EcPartner` literal that sets them (`ec/finalize.rs:628`, `auction/endpoints.rs:617`, `publisher.rs:7905`, `settings.rs:4047` and `:5859`, and the `PartnerConfig` fixtures in `ec/pull_sync.rs` tests)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`KvIdentityGraph::write_counting` constructor, test support)
- Modify: `crates/trusted-server-core/src/config.rs` (call `validate_identity_catalog` from both validators; extend `validate_removed_keys`)
- Modify: `crates/trusted-server-core/src/ec/registry.rs` (`PartnerConfig`, if it copies the two removed fields)
- Modify: `trusted-server.example.toml`, the integration fixture and `docs/guide/configuration.md`, wherever they set the two removed partner keys
- Test: `generic_pull.rs`, `catalog.rs`

**Interfaces:**

- Consumes: `EcPartner` (`settings.rs:385`, the settings type: `settings.ec.partners` is `Vec<EcPartner>`, `settings.rs:572`), `build_pull_request_url` (`ec/pull_sync.rs:368`), `ResolvedEid` and `ResolvedUid` (Task 10)
- Produces:
  - `pub(crate) fn validated_pull_sync_url(pull_sync_url: Option<&str>, allowed_domains: &[String]) -> Option<Url>`. Today's caller passes a `PartnerConfig`'s two fields. The rules do not change.
  - `pub(crate) struct GenericPullProvider` with `pub(crate) fn from_partner(partner: &EcPartner) -> Result<Self, Report<IdentityProviderError>>`. The descriptor reads only `source_domain`, `pull_sync_url` and `pull_sync_allowed_domains`, which are not secret, so it builds from unresolved deploy-time settings too. The provider keeps `ts_pull_token` and reads it only in `request`. At runtime the settings are resolved, so it holds the token there.
  - `pub(crate) fn generic_pull_id(source_domain: &str) -> IdentityProviderId`: `pull-` plus the domain with `.` replaced by `-`. A domain that already contains `-` gets `-` and the first 8 hex digits of SHA-256 of the full domain appended, so `a-b.example.com` and `a.b.example.com` (both valid under `normalize_partner_source_domain`) get different IDs. Over 63 characters, the readable part is cut to 54 characters before the `-` and the hash.
  - `pub(crate) fn validate_identity_catalog(descriptors: &[ProviderDescriptor], partners: &[EcPartner], identity: &IdentitySettings) -> Result<(), Report<TrustedServerError>>`: the one validator for the catalog rules of spec 10.2. In this task it rejects a duplicate provider ID, more than 32 providers, an `[ec.identity.providers.<id>]` table naming an unknown provider, and `revoke_scope` set for a provider whose descriptor declares no `captures`. No generic pull provider declares `captures`, so in Part 1 `revoke_scope` on any table is an error. Task 33 adds its rules to this same function. Nothing else repeats them.
  - `validate_removed_keys` (Task 19) also rejects `pull_sync_ttl_sec` and `pull_sync_rate_limit` on any `[[ec.partners]]` entry, with an error that names the partner and the replacement: `refresh_after_sec`, and `max_calls_per_minute = ceil(N / 60)`, in `[ec.identity.providers.<generic_pull_id>]` (spec 10.2, 10.4). `EcPartner` keeps a catch field for each, as Task 19 does for `pull_sync_concurrency`.
  - `pub(crate) fn generic_pull_descriptors(partners: &[EcPartner]) -> Result<Vec<ProviderDescriptor>, Report<TrustedServerError>>`, for the pull-enabled partners
  - `ProviderCatalog::from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>>`. It reads `settings.ec.partners` directly and never builds a `PartnerRegistry`, so no secret is checked. In Part 1 it holds generic pull providers only, with no owners, and it calls `validate_identity_catalog` on their descriptors before building. Task 32 adds the integration registry's providers.
  - Test support, all `pub` in the `pub mod test_support` module behind `#[cfg(any(test, feature = "test-utils"))]`, so Task 30's adapter tests can reach them (the adapter's dev-dependency already enables `trusted-server-core/test-utils`, `adapter-fastly/Cargo.toml:45`):
    - `WriteCountingEcKv` (an in-memory store that counts `Add`, `IfGenerationMatch` and `Overwrite` writes and lookups) with `pub fn writes(&self) -> WriteCounts`
    - `KvIdentityGraph::write_counting(name) -> (KvIdentityGraph, Arc<WriteCountingEcKv>)`, `pub` under the same gate
    - `FakeHttpClient` (Task 21), made `pub` here if Task 21 left it crate-private
    - `ProviderHarness` with `new`, the `with_*` setters, `request`, `request_json`, `respond`, `respond_raw` and `resolve`. Conformance checks come in Task 39.

Generic pull's descriptor:

```rust
let id = generic_pull_id(&partner.source_domain);
let mut resolves = Resolves::new().with_ec_id();
for host in &partner.pull_sync_allowed_domains {
    resolves = resolves.with_host(host);
}
ProviderDescriptor::builder(id)
    .with_source(partner.source_domain.parse()?)
    .with_resolves(resolves)
    .with_consent_policy(ConsentPolicy::eid_persistence_only())
    .build()
```

Its `request` is a `GET` of `validated_pull_sync_url(partner.pull_sync_url.as_deref(), &partner.pull_sync_allowed_domains)` with `?ec_id=<input.ec_id()?>` (via `build_pull_request_url`) and `with_bearer(ts_pull_token)`. Skip (`Ok(None)`) when there is no valid URL or token. Its `response` keeps today's no-op answers (`pull_sync.rs:55-57`, `:447-453`):

- A 404 is `Resolution::no_match()`, checked before `success_json`, as today's "treating as no-op".
- The body is `success_json::<{ uid: Option<String> }>()`. `None`, an empty UID or one over `MAX_UID_LENGTH` (`is_valid_eid_uid`) is `no_match`, never `Malformed`, so a partner's "no user" answer waits the `no_match` 24 h instead of counting a failure.
- Any other UID is `found([ResolvedEid::new(&source, ResolvedUid::new(uid))])`: one entry, one UID, no `atype`, no `ext` and no EID-level fields, so the coordinator stores the one-entry record pull sync stores today (G13).

A generic pull provider's settings are `settings.ec.identity.provider(&generic_pull_id(&partner.source_domain))` like any other provider's: `ProviderSettings::default()` unless the operator writes an `[ec.identity.providers.<pull-id>]` table, where `<pull-id>` is `generic_pull_id(source_domain)` (below). The unknown-provider validation error lists every configured provider ID, so an operator who writes the plain form for a hyphenated domain sees the real ID. Pacing moves there (spec 10.4): an operator who had `pull_sync_rate_limit = N` per hour sets `max_calls_per_minute = ceil(N / 60)`, and one who had `pull_sync_ttl_sec` sets `refresh_after_sec`. The changelog (Task 31) gives both conversions.

Until Task 29 deletes `dispatch_pull_sync`, `ec/pull_sync.rs` reads the same table: its rate check becomes `rate_limiter.exceeded_per_minute(&rate_key, provider_settings.max_calls_per_minute)` (`pull_sync.rs:200`), and its marker TTL uses `provider_settings.refresh_after_sec`. Its tests set those through `settings.ec.identity.providers` instead of the removed partner fields.

- [ ] **Step 1: Port today's pull sync tests**

List them with `grep -n "fn " crates/trusted-server-core/src/ec/pull_sync.rs | sed -n '/mod tests/,$p'`. For each test of protocol behaviour (URL building, bearer, response parsing, 64 KiB cap, allowlist, empty or oversized UID), write the equivalent against `GenericPullProvider` through `ProviderHarness`. For example:

```rust
    #[test]
    fn calls_the_partner_url_with_the_ec_id_and_bearer() {
        let partner = ec_partner("ssp.example.com", "https://sync.ssp.example.com/pull", "example-token-value-0123456789abcdef");
        let provider = GenericPullProvider::from_partner(&partner).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

        let request = harness.request().expect("should build").expect("should not skip");

        assert_eq!(request.url().host_str(), Some("sync.ssp.example.com"), "should call the partner host");
        assert_eq!(
            request.url().query_pairs().find(|(name, _)| name == "ec_id").map(|(_, value)| value.into_owned()),
            Some(TEST_EC_ID.to_owned()),
            "should pass the EC ID"
        );
        assert_eq!(request.headers()["authorization"], "Bearer example-token-value-0123456789abcdef", "should send the bearer token");

        let resolution = harness.respond(StatusCode::OK, &json!({ "uid": "partner-uid" })).expect("should parse the answer");
        assert_eq!(resolution.eids().len(), 1, "should return one entry");
        let eid = &resolution.eids()[0];
        assert_eq!(eid.uids().len(), 1, "should return one UID");
        assert_eq!(eid.uids()[0].id(), "partner-uid", "should return the partner's UID");
        assert_eq!(
            (eid.uids()[0].atype(), eid.uids()[0].ext(), eid.inserter(), eid.matcher(), eid.mm(), eid.ext()),
            (None, None, None, None, None, None),
            "should set nothing beyond the UID, so it is stored as one entry with one UID"
        );
    }

    #[test]
    fn not_found_and_null_uid_are_no_match_as_today() {
        let partner = ec_partner("ssp.example.com", "https://sync.ssp.example.com/pull", "example-token-value-0123456789abcdef");
        let provider = GenericPullProvider::from_partner(&partner).expect("should build");
        for (name, status, body) in [
            ("404", StatusCode::NOT_FOUND, json!({})),
            ("null uid", StatusCode::OK, json!({ "uid": null })),
            ("empty uid", StatusCode::OK, json!({ "uid": "" })),
        ] {
            let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);
            harness.request().expect("should build").expect("should not skip");
            let resolution = harness.respond(status, &body).expect("should not be an error");
            assert!(resolution.is_no_match(), "should be no_match for {name}");
        }
    }

    #[test]
    fn provider_ids_are_derived_and_bounded() {
        assert_eq!(generic_pull_id("ssp.example.com").as_str(), "pull-ssp-example-com", "should keep a dot-only domain readable");
        let long = format!("{}.example.com", "a".repeat(70));
        let id = generic_pull_id(&long);
        assert!(id.as_str().len() <= 63, "should bound the ID to 63 characters");
        assert_ne!(id, generic_pull_id(&format!("{}.example.com", "a".repeat(71))), "should keep long IDs unique by hash");
        assert_ne!(
            generic_pull_id("a-b.example.com"),
            generic_pull_id("a.b.example.com"),
            "should not collide when a domain already contains a hyphen"
        );
    }

```

`ProviderHarness::with_ec_id` is crate-private (only generic pull reads the EC ID). Add it next to the public setters with `pub(crate)`.

`ec_partner(source, url, token)` builds an `EcPartner` with pull sync enabled and `url`'s host allowed. Add it to `test_support.rs`.

Also add `generic_pull_pacing_comes_from_its_provider_table`: a partner with no table gets `refresh_after_sec = 86_400` and `max_calls_per_minute = 1`, and one with `[ec.identity.providers.pull-ssp-example-com]` holding `max_calls_per_minute = 2` gets 2. In `config.rs` tests, add `removed_partner_pull_keys_are_rejected_with_their_replacements`: a partner with `pull_sync_ttl_sec` fails both validators with an error naming `refresh_after_sec`, one with `pull_sync_rate_limit` with an error naming `max_calls_per_minute`, and the control without either passes.

In `catalog.rs`, add `validate_identity_catalog_rejects_unknown_provider_tables`, `validate_identity_catalog_rejects_revoke_scope_without_captures`, `validate_identity_catalog_rejects_duplicate_ids_and_more_than_32_providers` and `from_settings_builds_from_unresolved_settings`. Each rejection pairs a rejected config with a control that differs only in the key under test: a table for the partner's real `pull-` ID passes, and the same table with `revoke_scope = "provider"` does not. The last one builds the catalog from settings whose `api_token` holds a short secret-store key name, which `PartnerRegistry::from_config` would reject (`ec/registry.rs:279`).

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::generic_pull` and `cargo test-fastly -- ec::identity::catalog`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `generic_pull.rs`, `ProviderCatalog::from_settings`, the harness basics and the counting store. The harness builds `ResolveSignals` from its setters (`SeededRandom` from `with_random_seed`, default seed `1`, default `now` 1,760,000,000). It computes kinds as spec 13 says: a source with a `with_stored` record is `Refresh`, any other is `Initial`, and sources whose kind's needs are missing are dropped, with `Ok(None)` and no provider call when none are left. It keeps the built request's call context for `respond`. `respond` builds a `ProviderResponse` from `serde_json::to_vec(body)`.

Call `validate_identity_catalog(&generic_pull_descriptors(&settings.ec.partners)?, &settings.ec.partners, &settings.ec.identity)` from `validate_settings_for_deploy` and `validate_settings_for_runtime`, after the partner check. Deploy validation calls only `PartnerRegistry::validate_config_for_deploy` today (`config.rs:259`) and must never build a `PartnerRegistry`: `from_config` checks API token lengths, and at deploy time `api_token` holds a key name. Building generic pull descriptors reads no secret (spec 10.2), so this works on unresolved settings.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity`, `cargo test-fastly -- ec::pull_sync`, `cargo test-fastly -- config::`, `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` and `./scripts/test-cli.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Model pull sync partners as GenericPullProvider" -m "Adds the provider harness basics and a KV store that counts writes by mode. Generic pull results stay one UID per source, a 404 or null UID stays a no-op, and one catalog validator rejects provider tables it cannot apply." -m "pull_sync_ttl_sec and pull_sync_rate_limit are removed from [[ec.partners]]: pacing comes from each pull partner's [ec.identity.providers.<pull-id>] table, and a config that sets either key fails validation with an error naming its replacement."
```

---

### Task 23: The writer decision and request-scoped identity state on `EcContext`

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/runtime.rs` (`IdentityRuntime`)
- Create: `crates/trusted-server-core/src/ec/identity/request.rs` (`RequestIdentity`)
- Modify: `crates/trusted-server-core/src/http_util.rs` (add `is_document_navigation`, move `is_prefetch_request` here)
- Modify: `crates/trusted-server-core/src/publisher.rs` (`is_prefetch_request` at about line 3200 moves out; the navigation and page-bids merge build their `ServeContext` from the runtime)
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs` (its call at line 274 switches to `crate::http_util::is_prefetch_request`)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`EcContext` fields and accessors)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`, `crates/trusted-server-core/src/ec/identify.rs` and `crates/trusted-server-core/src/ec/admin.rs` (the servability callers Task 14 left on `ProviderCatalog::empty()`)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`AppState`, `build_state_from_settings`, `build_ec_request_state`, the publisher fallback, the integration-proxy EC generation path, the admin lookup call, and the `AppState` test literal at about line 1579)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (the settings-reload branch that runs without an `AppState`, about line 526)
- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (integration-proxy EC generation, about line 1046, gains the real-browser check)
- Test: `http_util.rs`, `ec/mod.rs`, `ec/identity/runtime.rs`, `auction/endpoints.rs`, `adapter-fastly/src/app.rs`

**Interfaces:**

- Consumes: `ProviderCatalog::from_settings` (Task 22), `IdentityKeys::from_settings` (Task 13), `IdentityMutation` (Task 15), `ServeContext` (Task 14)
- Produces:
  - `pub struct IdentityRuntime`, built once per settings load. Its fields are private: the `ProviderCatalog`, the `IdentityKeys` and a clone of `IdentitySettings` (`identity()`). It is the one identity type the adapter names, so the crate-private catalog and keys never cross the crate boundary or appear in a `pub` signature (`private_interfaces` is an error under `-D warnings`).
    - `pub fn from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>>`: `ProviderCatalog::from_settings(settings)?` narrowed with each provider table's `sources` (`with_narrowing`, Task 14), and `IdentityKeys::from_settings(settings)`. Task 32 adds a second parameter, the integration registry, for registered providers. Keep the body a short sequence of builder calls so that parameter slots in.
    - `pub(crate) fn catalog(&self) -> &ProviderCatalog`, `keys(&self) -> &IdentityKeys`, `identity(&self) -> &IdentitySettings`
    - `pub(crate) fn deps<'a>(&'a self, settings: &'a Settings, services: &'a RuntimeServices, random: &'a dyn RandomSource, rate_limiter: Option<&'a dyn RateLimiter>) -> IdentityDeps<'a>` (Task 21)
    - A hand-written `Debug` that prints the provider count, never key material.
  - `EcContext::set_identity_runtime(&mut self, runtime: Arc<IdentityRuntime>)` (`pub`), which the adapter calls where it builds `EcRequestState`, and `pub(crate) fn identity_runtime(&self) -> Option<&IdentityRuntime>`. Without `ec.ec_store` there is no KV and no record to serve, so no runtime is needed there.
  - `pub(crate) fn opted_out(entry: &KvEntry) -> BTreeSet<IdentityProviderId>` (in `request.rs`): the providers with a sticky opt-out in KV `ps.o`, unless a later `oc` clears it (spec 7.1). KV is the only source: no cookie carries an opt-out.
  - `pub fn is_document_navigation(req: &Request<EdgeBody>) -> bool`: `Sec-Fetch-Dest: document` only, with no `Accept` fallback
  - `pub fn is_prefetch_request(req: &Request<EdgeBody>) -> bool`, moved unchanged and made public
  - `pub(crate) struct RequestIdentity` stored on `EcContext`. It derives `Default` and `Clone` and has a hand-written `Debug` that prints only counts and flags (the number of mutation changes, `writer`, `is_real_browser`, `removal_lost`), because the derived `Debug` of `IdentityMutation` and `KvPartnerId` would print UIDs whenever `EcContext` is debug-printed (spec 9.3). Accessors:
    - `writer: bool`
    - `is_real_browser: bool`, which Task 38 reads for the real-browser gate on capture
    - `user_agent: Option<String>`, `dnt: bool`, `page_url: Option<Url>` (captured in phase 1)
    - `runtime: Option<Arc<IdentityRuntime>>`
    - `mutation: IdentityMutation`: the writer's one flush (Task 25 fills it, Task 29 writes it)
    - `pending_removals: Option<IdentityMutation>`: a removal write that failed before send, to try once more after send (Task 38)
    - `removal_lost: u32` with `record_removal_lost(&mut self)`: removals every attempt failed to write (spec 5.2 phase 4, 5.3). Task 31 reports it.
    - `capture_unpersisted: BTreeMap<(IdentityProviderId, SourceDomain), u32>` with `record_unpersisted(provider, source)`: positive capture IDs counted, never stored (spec 4.7). Task 38 fills it and Task 31 reports it.
  - `pub(crate) fn EcContext::identity(&self) -> &RequestIdentity` and `pub(crate) fn identity_mut(&mut self) -> &mut RequestIdentity`. They are crate-private because `RequestIdentity` is. The adapter never calls them.
  - `pub fn EcContext::decide_writer(&mut self, req: &Request<EdgeBody>, organic_route: bool, is_real_browser: bool)`, called from the adapter. It sets `writer = organic_route && is_document_navigation(req) && !is_prefetch_request(req) && is_real_browser`, records `is_real_browser`, and captures the user agent, DNT and the page URL (the request URL without query or fragment).
  - `pub fn EcContext::set_real_browser(&mut self, is_real_browser: bool)`, called from the adapter

The writer decision is made in the adapter where routing already decides EC generation (spec 5.1). That is after pre-route filters allow dispatch:

- In the publisher fallback branch of `app.rs`, next to `let is_navigation = is_navigation_request(&req);`, call `ec.ec_context.decide_writer(&req, true, ec.is_real_browser)`.
- In the integration-proxy branch, call `decide_writer(&req, true, is_real_browser)` before `handle_proxy`, and make proxy EC generation require `is_real_browser` explicitly, not just a present KV graph (`registry.rs:1048`). `ProxyDispatchInput` gains `is_real_browser: bool` for this. EC generation is its only reader: capture reads the flag from `EcContext`. The Fastly adapter passes integration proxy dispatch the identity graph for every client (`proxy_kv_graph`), so a non-browser's captured revocations reach KV.
- Every other route leaves `writer = false` and still records `is_real_browser` through `EcContext::set_real_browser(bool)`, called where the adapter builds `EcRequestState`, so a stager's capture knows whether its clear passes the real-browser gate (Task 38).

The adapter holds the runtime (spec 10.3: built once per settings load):

- `AppState` gains `identity: Arc<IdentityRuntime>`. Add `fn build_identity_runtime(settings: &Settings) -> Result<Arc<IdentityRuntime>, Report<TrustedServerError>>` in `app.rs`, called by `build_state_from_settings` and by the settings-reload branch in `main.rs` that runs when `app_state` is `None` (`load_settings_from_config_store`, about line 526), so both paths have one. Update the `AppState` test literal (about `app.rs:1579`).
- `build_ec_request_state` calls `ec_context.set_identity_runtime(Arc::clone(&state.identity))`.

The servability callers get the real catalog from here on (spec 7.1). Task 14 passed `ProviderCatalog::empty()`. From Task 29 on, generic pull results are written with `w = pull-…`, and `is_servable` rule 3 drops any record whose writer is not in the catalog, so an empty catalog would silently stop every pull ID reaching bidders (G13). Replace it at each caller:

- `/auction` (`auction/endpoints.rs`, where it calls `resolve_partner_ids`), the publisher navigation and page-bids merge (`publisher.rs`) and `/identify` (`ec/identify.rs`, which already takes `ec_context`): `catalog = ec_context.identity_runtime().map(IdentityRuntime::catalog)`, falling back to `ProviderCatalog::empty()` only when there is no runtime, and `opted_out = request::opted_out(&entry)`.
- The admin view (`handle_admin_ec_lookup`, `ec/admin.rs:276`) gains `runtime: Option<&IdentityRuntime>`, and the Fastly adapter passes `Some(&state.identity)`. Its `opted_out` is the same KV `ps.o` set. The other adapters return the "not supported" response before reaching it.

`resolve_partner_ids` becomes `pub(crate)`. Its only callers are `auction/endpoints.rs` and `ec/admin.rs`, and it now takes the crate-private `ServeContext`.

- [ ] **Step 1: Write the failing tests**

In `http_util.rs`:

```rust
    #[test]
    fn document_navigation_requires_sec_fetch_dest_document() {
        let with_dest = Request::builder()
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build the request");
        let accept_only = Request::builder()
            .header(header::ACCEPT, "text/html")
            .body(EdgeBody::empty())
            .expect("should build the request");
        let iframe = Request::builder()
            .header("sec-fetch-dest", "iframe")
            .body(EdgeBody::empty())
            .expect("should build the request");
        assert!(is_document_navigation(&with_dest), "should count a document destination");
        assert!(!is_document_navigation(&accept_only), "should not count the Accept fallback");
        assert!(is_navigation_request(&accept_only), "control: the broad check should still accept it");
        assert!(!is_document_navigation(&iframe), "should not count an iframe");
    }
```

In `ec/mod.rs` tests:

```rust
    #[test]
    fn decide_writer_needs_every_condition() {
        let navigation = || {
            Request::builder()
                .uri("https://www.example.com/a?b=c#d")
                .header("sec-fetch-dest", "document")
                .header(header::USER_AGENT, "ExampleBrowser/1.0")
                .header("dnt", "1")
                .body(EdgeBody::empty())
                .expect("should build the request")
        };
        let mut context = EcContext::default();
        context.decide_writer(&navigation(), true, true);
        assert!(context.identity().writer(), "should make an organic real-browser document navigation the writer");
        assert!(context.identity().is_real_browser(), "should record the real browser");
        assert_eq!(context.identity().page_url().map(Url::as_str), Some("https://www.example.com/a"), "should drop the query and fragment");
        assert_eq!(context.identity().user_agent(), Some("ExampleBrowser/1.0"), "should capture the user agent");
        assert!(context.identity().dnt(), "should capture DNT");

        for (name, organic, browser, request) in [
            ("named route", false, true, navigation()),
            ("bot", true, false, navigation()),
            (
                "prefetch",
                true,
                true,
                Request::builder()
                    .header("sec-fetch-dest", "document")
                    .header("sec-purpose", "prefetch")
                    .body(EdgeBody::empty())
                    .expect("should build the request"),
            ),
        ] {
            let mut context = EcContext::default();
            context.decide_writer(&request, organic, browser);
            assert!(!context.identity().writer(), "should not be a writer: {name}");
            assert_eq!(context.identity().is_real_browser(), browser, "should keep the real-browser flag: {name}");
        }
    }
```

Use the prefetch headers `is_prefetch_request` actually checks. Read it before writing the test.

Also write:

- `request_identity_debug_prints_no_uid` (a `RequestIdentity` whose mutation holds a `FillMissing` and an `OwnerRemove`: `format!("{:?}", identity)` contains no UID)
- `opted_out_reads_ps_o_and_honours_a_later_clear`: a row with `ps.a.o` set, `ps.b.o` set and `ps.b.oc` strictly later, and `ps.c` with only `oc`. Only `a` is opted out.
- in `runtime.rs`, `runtime_debug_prints_no_key_material` and `runtime_narrows_providers_by_their_sources_table`
- in `auction/endpoints.rs`, `generic_pull_record_with_writer_is_served_after_flush`: a row whose record for `ssp.example.com` has `w = pull-ssp-example-com`, and a context whose runtime's catalog holds that generic pull provider. `/auction` sends the UID. The control, with a runtime whose catalog lacks the provider, does not.
- in `ec/admin.rs`, `admin_view_serves_generic_pull_records_with_the_runtime_catalog`

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- http_util::tests::document_navigation`, `cargo test-fastly -- ec::tests::decide_writer`, `cargo test-fastly -- ec::identity::request`, `cargo test-fastly -- ec::identity::runtime`, `cargo test-fastly -- generic_pull_record_with_writer_is_served_after_flush` and `cargo test-fastly -- admin_view_serves_generic_pull_records`
Expected: compile errors.

- [ ] **Step 3: Implement**

Add `IdentityRuntime` (`pub mod runtime;` in `ec/identity/mod.rs`, re-exported as `ec::identity::IdentityRuntime`) and `RequestIdentity` (`pub(crate) mod request;`, file `ec/identity/request.rs`), both registered in `migration_guards.rs`. Give `RequestIdentity` `Default`, `Clone`, the redacting `Debug`, the accessors above, and setters used by later tasks. Add the field `identity: RequestIdentity` to `EcContext` (it already derives `Debug, Default, Clone`, `ec/mod.rs:249`, so this needs `RequestIdentity: Debug + Default + Clone`). `IdentityMutation` needs `Clone`, so add `#[derive(Clone)]` to it and its parts where it is missing. `EcFinalizeState` must stay `Clone + Send + Sync`. `IdentityRuntime` is shared through `Arc` and holds `Arc<dyn IdentityProvider>`s, which are `Send + Sync` (Task 11), so it does.

Move `is_prefetch_request` into `http_util.rs` as `pub`. In `publisher.rs`, delete the old definition and import it with `use crate::http_util::is_prefetch_request;` at the top. Switch `gpt_diagnostics.rs:274` from `crate::publisher::is_prefetch_request` to `crate::http_util::is_prefetch_request`; a private `use` in `publisher.rs` would not be visible on that path.

Wire `decide_writer`, `set_real_browser` and `set_identity_runtime` in `app.rs` as described above, and the runtime into `AppState` and the reload branch. Add `is_real_browser` to `ProxyDispatchInput` and set it from `EcRequestState`. In `IntegrationRegistry::handle_proxy`, require it before proxy EC generation. Switch the servability callers to the runtime's catalog and the request's opt-outs.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly` (full: the adapter and core)
Expected: PASS. Behaviour is unchanged so far, apart from integration-proxy EC generation now needing a real browser. If an existing proxy test minted an EC for a non-browser request, update it and note the change in the commit body.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Decide the writer request once routing allows dispatch" -m "Only an organic real-browser top-level document navigation that is not a prefetch is the writer. Integration-proxy EC generation now requires a real browser explicitly." -m "The adapter builds one IdentityRuntime per settings load, and the auction, publisher, identify and admin readers check servability against its catalog and the row's opt-outs."
```

### Task 24: `/identify` stops writing; cluster size rides on the writer

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identify.rs` (the `evaluate_cluster` call at about line 107)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`evaluate_cluster` at about line 1484 gains a non-writing variant)
- Test: `ec/identify.rs`, `ec/kv.rs`

**Interfaces:**

- Produces:
  - `KvIdentityGraph::compute_cluster_size(&self, ec_id: &str) -> Result<u32, Report<TrustedServerError>>`: the prefix list and count only, no write
  - `/identify` uses a stored `cluster_size` when present and `compute_cluster_size` otherwise, and never writes. Phase 4 (Task 29) adds a missing `cluster_size` to its one write with `compute_cluster_size` and `IdentityMutation::set_cluster_size`.

`/identify`'s response shape does not change (spec 7.1). Task 14 gates the partner's own record with `is_servable`, not `resolve_partner_ids`. Its `uid` is the stored record's `first_uid()`, the first UID of the first entry, and its `eid` is built from that UID with the partner's `openrtb_atype`, exactly as `ec/identify.rs:127-135` builds it today. A multi-entry record's other UIDs and EID-level fields are never returned there.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn identify_computes_cluster_size_without_writing() {
        let (graph, store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_row_without_cluster_size(&graph);
        let response = identify_for_test(&graph, &ec_id);

        assert_eq!(response.status(), StatusCode::OK, "should answer");
        assert!(response_body_json(response)["cluster_size"].is_number(), "should report a size");
        assert_eq!(store.writes().total(), 0, "should not write");
    }

    #[test]
    fn identify_returns_the_first_uid_of_a_multi_entry_record_with_the_partner_atype() {
        let (graph, _store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_row_with_entry_set(&graph, "partner.example.com", &[&["example-a", "example-b"], &["example-c"]]);

        let body = response_body_json(identify_for_test(&graph, &ec_id));

        assert_eq!(body["uid"], json!("example-a"), "should return the first UID of the first entry");
        assert_eq!(
            body["eid"],
            json!({ "source": "partner.example.com", "uids": [{ "id": "example-a", "atype": 3 }] }),
            "should return one UID with the partner's openrtb_atype, as for a one-UID record"
        );
    }
```

`seed_row_without_cluster_size`, `identify_for_test` and `response_body_json` are thin wrappers around what the existing identify tests already do. Write them in the test module from that setup. `seed_row_with_entry_set` seeds a row whose record for the source holds a set with each inner slice one entry, stored `atype: 1` on every UID and `inserter: "example.com"` on the first entry, so the test also shows that stored fields do not leak into `/identify`. Register the partner with `openrtb_atype = 3` and `bidstream_enabled = true`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identify::tests::identify_`
Expected: the cluster test fails with one write. The multi-entry test may already pass, since Task 14 reads `first_uid()`. Keep it: it pins the shape.

- [ ] **Step 3: Implement**

Split `evaluate_cluster` into `compute_cluster_size` (list and count) and the existing write. `/identify` calls only `compute_cluster_size`. Keep `evaluate_cluster` for now, because other callers may still use it. Task 29 adds the size to the mutation with `compute_cluster_size` and `IdentityMutation::set_cluster_size`, and removes `evaluate_cluster` in its Step 3 once nothing calls it.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identify` and `cargo test-fastly -- ec::kv`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src/ec
git commit --signoff -S -m "Stop writing cluster size from /identify" -m "Pins the response shape for multi-entry records: the first UID with the partner's atype, as for a one-UID record."
```

### Task 25: Finalize builds the mutation, and stagers write nothing

This is the core behaviour change of Part 1. `ec_finalize_response` stops writing KV for returning visitors. On the writer request it adds browser ingest to the mutation that phase 4 (Task 29) flushes. On stagers it ingests nothing and writes nothing. No identity cookie is set on any path (spec 5.2 phase 3, 9.4).

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (`ec_finalize_response`, `confirm_then_recover_orphaned_ec`, `recover_orphaned_ec`, and the new `browser_seed`)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`generate_if_needed` takes the browser ingest; its three callers in `ec/mod.rs` tests)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`create_if_absent` callers pass the seeded entry)
- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (`ProxyDispatchInput` gains the seed, and `handle_proxy`'s `generate_if_needed` call at about line 1047 passes it)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` and `crates/trusted-server-core/src/publisher.rs` (their test callers of `ec_finalize_response`, whose expectations change)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (build the seed for `generate_if_needed` and `ProxyDispatchInput`)
- Test: `ec/finalize.rs`

**Interfaces:**

- Consumes: `RequestIdentity` (Task 23), `IdentityMutation` and `PartnerIdChange::FillMissing` (Task 15), `KvPartnerId::browser` (Task 6), `collect_eid_updates` with `now` (Task 7)
- Produces:
  - Writer, returning EC, live row: adds one `FillMissing { source, record: KvPartnerId::browser(uid, Some(now)) }` per consent-gated browser-ingest update from `ts-eids` and sharedId to `ec_context.identity_mut().mutation`. It covers owned sources too, which seeds them for their owner (spec 7.2). It writes nothing.
  - Stager, returning EC: no ingest and no write. Today's returning-visitor `sync_eid_cookie_updates` call (`ec/finalize.rs:110-112`) is gone for `/auction`, `/_ts/page-bids` and every other stager (spec 5.3).
  - `ec_finalize_response` keeps its signature. It sets no `Set-Cookie` beyond today's `ts-ec` and the pull-sync marker that Task 29 removes.
  - `pub struct BrowserSeed`, an opaque wrapper over `Vec<PartnerIdUpdate>`, built by `pub fn browser_seed(settings: &Settings, registry: &PartnerRegistry, consent: &ConsentContext, eids_cookie: Option<&str>, sharedid_cookie: Option<&str>, now: u64) -> BrowserSeed` in `ec/finalize.rs`. It calls `collect_eid_updates` and the private `gate_eid_updates_by_consent` (`ec/finalize.rs:268`), so it must live in that file. `BrowserSeed::empty()` is for callers with nothing to seed. `PartnerIdUpdate` (`ec/kv.rs:75`) stays `pub(crate)`.
  - `pub fn generate_if_needed(settings, kv, seed: &BrowserSeed)`: the new-visitor create carries the browser ingest, so finalize no longer writes for a new EC (spec 5.2, phase 1). `ProxyDispatchInput` gains `seed: &'a BrowserSeed`, which the adapter builds with the same `browser_seed` call, so an EC minted on an integration-proxy navigation carries its ingest in the same one write.
  - Orphan rotation creates the new row carrying this request's browser ingest, as today (`ec/finalize.rs:323`). It is the page load's one write: rotation empties the mutation, and phase 4 skips (spec 5.2 phase 1). If the confirm read finds the row after all, no rotation happens, that read becomes the phase 1 snapshot, and the ingest joins the mutation instead of today's immediate write (`ec/finalize.rs:383-386`).

Browser ingest keeps writing one entry with one UID per registered source (G13). `collect_eid_updates` already picks one UID per registered source and drops unregistered sources: for a repeated source, the first valid UID of its last entry, with the sharedId cookie applied last (`ec/prebid_eids.rs:146-151`). Nothing in this task changes that rule or widens what is stored. A browser entry's extra UIDs and EID-level fields still reach bidders through the `/auction` body and the merge, as today, and never KV. The ingest reads `ts-eids` and sharedId only: the `/auction` body is no longer an ingest input (spec G10), and Task 26 removes that plumbing.

What each path does (spec 5.1, 5.2, 5.5):

| Path                               | KV                                                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| New EC (writer or not)             | the create before routing, carrying browser ingest (`generate_if_needed`). `ts-ec` set as today.                    |
| Returning EC, writer, live row     | none. Ingest joins the mutation, and phase 4 flushes it.                                                            |
| Returning EC, writer, missing row  | today's orphan recovery: confirm, then rotate with ingest in the create. An integration-proxy writer never rotates. |
| Returning EC, stager               | none                                                                                                                |
| Unusable consent, not withdrawn    | none                                                                                                                |
| Consent withdrawn                  | today's tombstone path, unchanged here (Task 27 adds the after-send attempt)                                        |
| Any KV read that found a tombstone | none, and the mutation stays empty                                                                                  |

- [ ] **Step 1: Write the failing tests**

Add a `mod identity_flow` test module nested inside `finalize.rs`'s existing `mod tests`, so the filter `ec::finalize::tests::identity_flow` matches it. Build requests and contexts the way the existing finalize tests do (`grep -n "fn finalize_" crates/trusted-server-core/src/ec/finalize.rs | head -40`). Use `KvIdentityGraph::write_counting` (Task 22). Write these tests. The first two are shown in full:

```rust
    #[test]
    fn returning_writer_navigation_adds_ingest_to_the_mutation_and_writes_nothing() {
        let (graph, store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_live_row(&graph, KvEntry::minimal("other.example.com", "x", 1));
        let mut context = returning_context(&ec_id, Writer::Yes);
        context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let mut response = html_response();

        ec_finalize_response(
            &settings_with_partner("sharedid.org"),
            &mut context,
            Some(&graph),
            &registry_with_partner("sharedid.org"),
            None,
            Some("example-shared-id"),
            &mut response,
        );

        assert_eq!(store.writes().total(), 0, "should not write in finalize for a returning writer");
        let records = queued_records(context.identity().mutation());
        assert_eq!(records.len(), 1, "should queue the sharedId fill");
        assert_eq!(records[0].1.first_uid(), Some("example-shared-id"), "should queue the cookie's UID");
        assert!(set_cookie_names(&response).is_empty(), "should set no cookie on a returning HTML response");
    }

    #[test]
    fn golden_browser_ingest_stays_one_uid_whatever_the_browser_sent() {
        let (graph, store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_live_row(&graph, KvEntry::minimal("other.example.com", "x", 1));
        let mut context = returning_context(&ec_id, Writer::Yes);
        context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let eids_cookie = ts_eids_cookie(&json!([
            {
                "source": "partner.example.com",
                "uids": [
                    { "id": "example-a", "atype": 1, "ext": { "stype": "ppuid" } },
                    { "id": "example-b", "atype": 3 }
                ],
                "inserter": "example.com",
                "mm": 3
            },
            { "source": "unregistered.example.com", "uids": [{ "id": "example-c", "atype": 1 }] }
        ]));
        let mut response = html_response();

        ec_finalize_response(
            &settings_with_partner("partner.example.com"),
            &mut context,
            Some(&graph),
            &registry_with_partner("partner.example.com"),
            Some(&eids_cookie),
            None,
            &mut response,
        );

        assert_eq!(store.writes().total(), 0, "should not write in finalize");
        let records = queued_records(context.identity().mutation());
        assert_eq!(records.len(), 1, "should queue only the registered source, as today");
        let (source, record) = &records[0];
        assert_eq!(source.as_str(), "partner.example.com", "should queue the registered source");
        assert_eq!(
            serde_json::to_value(record).expect("should serialize the record"),
            json!({ "at": record.at, "eids": [{ "uids": [{ "id": "example-a" }] }] }),
            "should store the UID collect_eid_updates picks today, with no atype, ext or EID-level field"
        );
    }
```

The others:

- `stagers_ingest_nothing_and_write_nothing`: the same row and cookies on `/auction` (`EidSyncSource::Auction`, with body EIDs), `/_ts/page-bids` and a non-navigation proxy call. Zero writes, an empty mutation, no `Set-Cookie`. This replaces today's tests that assert a returning-visitor ingest write on those routes.
- `unusable_consent_writes_nothing_and_queues_nothing`
- `withdrawal_still_writes_its_tombstone_on_any_request`: the existing withdrawal tests keep passing, on a writer and on a stager.
- `tombstone_read_leaves_the_mutation_empty`
- `orphan_rotation_carries_browser_ingest_in_one_create`: a missing row on a writer navigation. One `Add` write, holding the sharedId as a one-UID record, and an empty mutation afterwards.
- `confirm_read_that_finds_the_row_queues_ingest_instead_of_writing`
- `integration_proxy_writer_never_rotates_on_missing_row` (an integration-proxy writer navigation and a missing row: no create, as today, spec 5.1)
- `new_visitor_create_carries_browser_ingest_in_one_write` (one `Add` write, and the row holds the sharedId as a one-UID record)
- `integration_proxy_new_ec_carries_browser_ingest` (`handle_proxy` minting an EC with a seed: one `Add` write holding the seeded UID)

`ts_eids_cookie`, `queued_records`, `set_cookie_names`, `seed_live_row`, `returning_context` and the response builders go in the test module. Build them on `ec/identity/test_support.rs`. `queued_records` lists the `(source, record)` of every positive change in the mutation, in order.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::finalize::tests::identity_flow`
Expected: compile errors, then assertion failures (finalize still writes).

- [ ] **Step 3: Implement**

Restructure the returning-EC branch of `ec_finalize_response` (`ec/finalize.rs:80-121`):

1. Collect browser ingest from `ts-eids` and sharedId as today, with `now` (Task 7), then consent-gate it.
2. **Writer** (`ec_context.identity().writer()`): when the snapshot is a live row, add each update to the mutation as `FillMissing`. When it is `NotRead` (a failed or skipped phase 1 read), still add them: phase 4 re-reads before it writes (spec 5.2 phase 4 step 2). A missing row keeps today's orphan recovery; pass the updates into `confirm_then_recover_orphaned_ec` so the create carries them, and clear the mutation when the create is `Written`. A confirm read that finds the row sets the snapshot and queues the updates.
3. **Stager:** do nothing. Remove the `sync_eid_cookie_updates` call and the snapshot refresh around it for these paths. The `EidCookieSyncOutcome` measurement line (`EID sync measurement: ...`) goes with it for returning visitors, and Task 31's per-request counters replace it. Keep `record_eid_sync_terminal` for the new-EC create. Name the removed line and its replacement in this commit's body and in Task 31's CHANGELOG entry, because operator dashboards may read it.
4. New EC: `generate_if_needed` already wrote the seeded row. Finalize sets `ts-ec` as today and stops writing ingest for it (`ec/finalize.rs:144`).
5. Call `reconcile_pull_sync_marker` as today. Task 29 removes it.

In `ec/mod.rs`, give `generate_if_needed` a `seed: &BrowserSeed` parameter and apply its updates to the `KvEntry::new(..)` it creates, via `apply_partner_id_updates`, before `create_if_absent`. In `app.rs`, build the seed with `browser_seed(settings, registry, consent, ec.eids_cookie, ec.sharedid_cookie, now)` before calling `generate_if_needed`, and pass the same seed in `ProxyDispatchInput` for `handle_proxy`'s call (`registry.rs:1047`). `ec/mod.rs`'s own test callers pass `&BrowserSeed::empty()`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::finalize`, `cargo test-fastly -- golden_browser_ingest` and then the full `cargo test-fastly`
Expected: PASS. Existing finalize, auction and publisher tests that asserted a KV write on a returning navigation, `/auction` or `/_ts/page-bids` must now assert no write, plus a queued mutation on the writer. Update each one and list them in the commit body.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Queue browser ingest in the writer's mutation and stop stager writes" -m "Returning-visitor finalize no longer writes KV. The writer navigation adds ingest from ts-eids and sharedId to its single mutation; /auction, /_ts/page-bids and other stagers ingest nothing. A new visitor's create, from the publisher fallback or an integration proxy, and an orphan rotation carry browser ingest, so each stays one write." -m "Browser ingest still writes one UID per registered source. The EID sync measurement line no longer runs for returning visitors; the identity counters of the Part 1 metrics task replace it."
```

### Task 26: `/auction` and `/_ts/page-bids` stop ingesting and return the mirror payload

> Revised after the round 14 review (spec 15, question 5): the mirror ships gated off. `IdentitySettings::mirror_enabled` (`[ec.identity] mirror_enabled`, `#[serde(default)]`, `false`) is read by `mirror_enabled(ec_context)` in `auction/endpoints.rs`. `AuctionIdentity::mirror_member` returns `None` while it is off, and `/_ts/page-bids` sends its consent-denial `clear` only while it is on. Tests: `auction_omits_the_mirror_while_it_is_disabled` and `page_bids_omits_the_mirror_while_it_is_disabled`. Tests that exercise the mirror turn it on with `EcContext::enable_mirror_for_test`, and the adapter's identity scenarios set `settings.ec.identity.mirror_enabled = true`.

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (the client EID plumbing at about lines 290-302, and the response build)
- Modify: `crates/trusted-server-core/src/openrtb.rs` (`ResponseExt` at line 232 gains `ts_eids_s`)
- Modify: `crates/trusted-server-core/src/publisher.rs` (`handle_page_bids` at about line 6904, and its response body at about line 7232)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`EcContext.client_eids` loses its ingest role)
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs` (the injected `window.__tsjs_prebid` config at about line 1485 gains `registeredSources`)
- Create: `crates/trusted-server-js/lib/src/core/eids_mirror.ts` (writes `__Host-ts-eids-s` from a payload)
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/eids.ts` (the shim writes only registered sources into `ts-eids`, merged per source) and `prebid/index.ts` (reads the `/auction` payload)
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` (`PageBidsResponse` and its handler read the page-bids payload)
- Test: `auction/endpoints.rs`, `publisher.rs`, `integrations/prebid.rs`, and the Vitest files next to each changed TS file

**Interfaces:**

- Consumes: `mirror_payload` and `MirrorRead` (Task 17), the servable records (Task 14) and `to_eids`, `gate_eids_by_consent`, `ServeContext` from the runtime (Task 23)
- Produces:
  - `pub(crate) fn mirror_read<'a>(context: &EcContext, serve: &ServeContext<'_>, sources: &'a mut Vec<MirrorSource>) -> MirrorRead<'a>`: `NotRead` when the request read no KV; `Cleared` when consent denies EID persistence, or the read found a tombstone, `consent.ok = false` or a missing row; else `Servable` over the servable records with `bidstream_enabled = true`, after `gate_eids_by_consent`, one `MirrorSource` per source with its writer's `cookie_priority` (0 for a browser-seeded record) and the earliest `exp` of its set (spec 6.2). The EIDs are the server entries the merge would send, before browser precedence, provenance defaults included once Task 28 adds them.
  - `ResponseExt { orchestrator, ts_eids_s: Option<serde_json::Value> }`, with `#[serde(skip_serializing_if = "Option::is_none")]`, holding `MirrorPayload::to_json()`. The page-bids body gains a `ts_eids_s` member under the same rule.
  - `/auction`'s consent-denied early return still answers with `ts_eids_s = {"clear": true}`, so a withdrawal clears the mirror (spec 5.6).
  - TS: `writeEidsMirror(payload: unknown): void` in `core/eids_mirror.ts`. On `{value, max_age}` it writes `__Host-ts-eids-s=<value>; Path=/; Secure; SameSite=Lax; Max-Age=<max_age>`, skipping the write when the cookie already holds that value. On `{clear: true}` it expires the cookie. On anything else, absent included, it does nothing. It never reads the cookie for any other purpose and never copies it anywhere (spec 6.2, "No feedback").
  - TS: the shim keeps only entries whose source is in `registeredSources` and writes no `ts-eids` when no list is injected. It decodes the current cookie (`decodeEidCookie`), replaces its entries per source with the request's registered entries, keeps sources the request lacks, and then runs the EID fidelity trim on the merged set. A request with no EIDs leaves the cookie as it is; a TCF block still clears it (spec 5.3).

What stops (spec 5.3, G10):

- `/auction` no longer ingests its body EIDs or `ts-eids` into KV. The body EIDs still feed the auction merge exactly as today. `ec_context.set_client_eids` stays only if the merge or finalize logging still reads it; otherwise remove it with `client_eids()` and their callers.
- `/_ts/page-bids` no longer ingests `ts-eids` into KV.
- `EidSyncSource::Auction` and `EidSyncSource::PageBids` no longer select an ingest path in finalize. Keep the enum only if Task 31's counters label requests with it.

- [ ] **Step 1: Write the failing tests**

In `auction/endpoints.rs`, next to the existing `/auction` + KV tests:

- `auction_body_eids_are_not_written_to_kv`: a registered source in the body and not in KV. Zero writes, and the body's UID still reaches the bidder request.
- `auction_returns_the_mirror_of_its_servable_set`: a row with one servable record. `ext.ts_eids_s.value` decodes (base64url, no padding) to `{"v":1,"eids":[...]}` holding exactly that record's EID, and `max_age` is 86,400.
- `auction_mirror_max_age_follows_the_earliest_exp`
- `auction_mirror_leaves_out_bidstream_disabled_and_consent_gated_sources`
- `auction_returns_clear_on_consent_denial_tombstone_and_missing_row` (one case each, plus a control with a live row)
- `auction_returns_clear_when_the_servable_set_is_empty`
- `auction_without_a_kv_read_omits_the_mirror` (no `ts_eids_s` key at all in `ext`)

In `publisher.rs`:

- `page_bids_never_writes_kv`: with a `ts-eids` cookie holding a registered source missing from KV, zero writes.
- `page_bids_returns_the_mirror_payload` and `page_bids_without_an_auction_omits_the_mirror`

In `integrations/prebid.rs`:

- `injected_config_lists_registered_sources`: the `window.__tsjs_prebid` config holds `registeredSources` with every `[[ec.partners]]` source domain, sorted, and nothing else from the partner table.

In Vitest:

- `eids_mirror.test.ts`: writes the value with the given `Max-Age` and the `__Host-` attributes; skips an unchanged value; expires on `clear`; ignores an absent or malformed payload; never touches `ts-eids`.
- `eids.test.ts`: `keeps_registered_sources_before_unregistered_ones` (a set over 3,072 bytes whose registered source comes last: the cookie keeps it and drops unregistered ones first), and a control showing the order within each group is unchanged.
- In `prebid/index.test.ts` and `gpt/index.test.ts`: a response carrying `ts_eids_s` calls `writeEidsMirror` with it, and one without it does not.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- auction::endpoints::tests::auction_`, `cargo test-fastly -- publisher::tests::page_bids_`, `cargo test-fastly -- injected_config_lists_registered_sources`, and `cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run`
Expected: failures (today `/auction` and page-bids write, and there is no mirror).

- [ ] **Step 3: Implement**

In `auction/endpoints.rs`, remove the ingest of body and cookie EIDs; keep the merge input. Build the payload with `mirror_read` and `mirror_payload` once the KV read is done, and put it in `ResponseExt`. In `publisher.rs`, remove the page-bids path that leads to a KV write (with Task 25 in place finalize no longer writes for a returning EC, so check that page-bids has no other direct write: `grep -n "upsert\|sync_eid\|create" crates/trusted-server-core/src/publisher.rs`) and add `ts_eids_s` to the body. In `prebid.rs`, add `registeredSources` to the injected config. In TS, add `core/eids_mirror.ts`, call it from the `/auction` response handler in `prebid/index.ts` and the page-bids handler in `gpt/index.ts`, and pass the injected `registeredSources` (an empty set when absent, which keeps today's trim) to `fitEidCookie`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- auction::`, `cargo test-fastly -- publisher::`, `cargo test-fastly -- integrations::prebid`, and in `crates/trusted-server-js/lib`: `mise exec node@24.12.0 -- npx vitest run`, `mise exec node@24.12.0 -- npm run lint` and `mise exec node@24.12.0 -- npm run format`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src crates/trusted-server-js/lib/src
git commit --signoff -S -m "Return the ts-eids-s mirror from /auction and stop stager ingest" -m "/auction and /_ts/page-bids no longer write browser EIDs to KV. Both return a ts_eids_s payload built from the servable server set, which the shim writes as the client-only __Host-ts-eids-s cookie, or a clear on denied consent, a tombstone or a missing row." -m "The shim writes only registered sources into ts-eids, merged per source, so the writer navigation's ingest still sees every registered source /auction used to ingest."
```

### Task 27: The withdrawal tombstone's after-send attempt

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (the withdrawal path at about lines 470-505, `apply_withdrawal_tombstones` at about line 527)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`EcContext` records the failed IDs)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (the after-send hook at about lines 517 and 536)
- Test: `ec/finalize.rs`, `adapter-fastly/src/main.rs`

**Interfaces:**

- Produces:
  - `EcContext::failed_withdrawals(&self) -> &[String]` (`pub(crate)`), the withdrawn EC IDs whose before-send tombstone returned `EcKvSnapshot::Failed` after its five CAS attempts (`ec/kv.rs:1349`). The withdrawal closure records each one where it logs the failure today (`ec/finalize.rs:495`).
  - `pub fn retry_failed_withdrawals(context: &mut EcContext, kv: &KvIdentityGraph)`: after send, calls `tombstone_existing_from_snapshot(ec_id, EcKvSnapshot::NotRead)` once for each failed ID, so it re-reads first (spec 5.1). It is one more run of the same path, with its own five CAS attempts and completion marker, never a second retry. A second failure is logged at `warn` with the EC ID through `log_id` only and counted (Task 31). A success updates the context's snapshot when the ID is the active one, as the before-send path does.

This is the withdrawal's existing exception to G4, extended by one after-send run (spec 5.1). Nothing else rides on it in Part 1. Task 38's captured global opt-out reuses it.

- [ ] **Step 1: Write the failing tests**

In `ec/finalize.rs`:

- `failed_withdrawal_is_recorded_for_after_send`: a store that fails every tombstone write. After finalize, `failed_withdrawals()` holds the active ID.
- `successful_withdrawal_records_nothing`
- `after_send_retry_writes_the_tombstone_once`: a store that fails five times and then succeeds. Finalize records the ID; `retry_failed_withdrawals` writes the tombstone and the completion marker; the row reads as a tombstone.
- `after_send_retry_is_the_last_attempt`: a store that always fails. The retry makes one run of at most five CAS attempts, logs, and leaves `failed_withdrawals()` unchanged for the counters.
- `withdrawal_of_two_ids_retries_only_the_failed_one`: the cookie ID's tombstone fails and the active ID's succeeds. The retry touches only the cookie ID.

In the adapter:

- `after_send_runs_the_withdrawal_retry_before_identity_work`: with a failed withdrawal recorded, the after-send hook calls the retry and runs no provider dispatch, since the row is now a tombstone or still unwritten.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::finalize::tests::failed_withdrawal`, `cargo test-fastly -- ec::finalize::tests::after_send_retry` and `cargo test-fastly -- after_send_runs_the_withdrawal_retry`
Expected: compile errors.

- [ ] **Step 3: Implement**

Record each `Failed` outcome in the withdrawal closure. Add `retry_failed_withdrawals` next to `apply_withdrawal_tombstones`. In `main.rs`, call it first in the after-send hook, before `run_edgezero_pull_sync_after_send` (which Task 29 replaces with phase 4), whenever the request has a graph and a failed withdrawal. A withdrawal request never runs providers or flushes a mutation, so the order matters only for logs.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::finalize` and `cargo test-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Retry a failed withdrawal tombstone once after send" -m "A tombstone whose five CAS attempts fail before send runs the same path once more after send, by which time the per-key write limit has usually passed. A second failure is logged and counted."
```

### Task 28: The server set and provider denials in the auction merge

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (`merge_auction_eids`, about line 505)
- Modify: `crates/trusted-server-core/src/publisher.rs` (navigation and page-bids merge, about lines 5547-5568)
- Modify: `crates/trusted-server-core/src/ec/eids.rs` (`to_eids` fills provenance defaults)
- Create: `crates/trusted-server-core/src/ec/identity/denials.rs`
- Test: `auction/endpoints.rs`, `publisher.rs`, `ec/eids.rs`, `denials.rs`

**Interfaces:**

- Consumes: `ProviderDescriptor::provenance(source)` and the `with_provenance` default (Task 8), `uid_hash` (Task 13), `ProviderCatalog::claimants`, `is_capture_only` and `sources_of` (Task 14), the servability check, `ResolvedPartnerId { entries, writer, .. }` and `to_eids`, which already emits one `Eid` per stored entry (Task 14), and `opted_out` (Task 23)
- Produces:
  - `pub(crate) struct ServerSet<'a>`, built by `pub(crate) fn server_set<'a>(entry: Option<&'a KvEntry>, serve: &ServeContext<'_>) -> ServerSet<'a>`: per source, the servable record of the KV row the request read, with its writer. It never holds anything from `ts-eids-s` (spec 7.1). No KV read, or a row that is not live and consenting, gives an empty set.
  - `pub(crate) struct Denials`, built by `pub(crate) fn denials(entry: &KvEntry, catalog: &ProviderCatalog) -> Denials` in `denials.rs`, read from KV only (spec 7.1, "Provider denials apply to browser candidates"):
    - per source, the union of the `ps.iv` and `ps.ov` hashes of every provider in `catalog.claimants(source)`
    - the set of sources whose browser value is dropped whole: every source the owner of which has a sticky `ps.o`, and every source of a capture-only provider (`is_capture_only`) with a sticky `ps.o`, owned or not (`opted_out`, Task 23, decides "sticky")
  - `pub(crate) fn filter_browser_candidates(client: &[Eid], denials: &Denials) -> Vec<Eid>`: drops each entry of a dropped-whole source; otherwise drops each UID whose `uid_hash` is in that source's hashes, then each entry left with no UID. It runs before the precedence table and before `gate_eids_by_consent`. Without a KV read there are no denials (a stager without a read has no browser candidates to check against, and the writer always reads, Task 29).
  - `merge_auction_eids(client: &[Eid], server: &ServerSet<'_>, denials: &Denials, registry: &PartnerRegistry) -> Vec<Eid>`
  - `pub(crate) fn fill_provenance(eids: &mut [Eid], writer: Option<&IdentityProviderId>, source: &SourceDomain, catalog: &ProviderCatalog)`, called on a provider-written record's output

Part 1's catalog holds only generic pull providers, which claim one source each, own nothing and capture nothing, so in production the denials hold only `iv` and `ov` hashes, and only once a provider writes them. The tests build test catalogs with owners and capture-only providers (`test_support`), so every rule lands and is tested here, and Task 36 adds only the owned precedence row.

Precedence (spec 7.1):

| Source  | Browser sent it                                                         | Browser did not send it                                                            |
| ------- | ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| Owned   | the owner's valid value (`w == owner`, not expired), else the browser's | the owner's valid value, else a browser-seeded value if the source does not expire |
| Unowned | the browser's (today)                                                   | the server value (today)                                                           |

This task implements the unowned row. Task 36 adds the owned row.

How a server value goes out (spec 7.1):

- The source's whole entry set, one `Eid` per stored entry, in stored order, after the browser entries. Task 14's `to_eids` already does this, with each UID's stored `atype`, else the partner's `openrtb_atype`, and its stored UID-level `ext`, and each entry's stored EID-level fields. A one-UID record is one entry, exactly as today (`ec/eids.rs:64-76`).
- For a record a provider wrote (`w` names a configured provider), the provider's `with_provenance` default for the source (`descriptor.provenance(source)`) applies only to an entry that sets none of `inserter`, `matcher` and `mm` (spec 4.3). Then each field the default sets is filled. An entry that sets any of the three is the vendor's complete statement and is left as stored, so vendor and config provenance never mix in one entry. A browser-seeded or version 1 record (no `w`) gets no default.
- An unowned server set goes out only when the browser did not send the source (after the denials). It is never mixed with browser entries.

Sources with `bidstream_enabled = false` are left out. The merged set goes through `gate_eids_by_consent` and into `x-ts-eids` exactly as today, with the same 4 KB cap and `x-ts-eids-truncated` when an entry is cut. `encode_eids_header` and `build_eids_header` are unchanged and still truncate per entry (Task 14), so the header can carry part of a stored set; `user.eids` always carries it whole (spec 7.1).

Browser entries that pass the denials keep today's path untouched: they come first, in the browser's order, with the browser's own EID-level fields, and the merge never edits one beyond dropping a denied UID.

Task 26's mirror payload reads the same `ServerSet` (before browser precedence), so it carries the provenance defaults too.

- [ ] **Step 1: Write the failing tests**

Every test name in this task starts with `merge_`, `denial_` or `golden_merge_`, so the filters below run them all:

- `merge_browser_value_wins_for_unowned_source` (control: today's rule)
- `merge_server_value_goes_out_when_the_browser_did_not_send_the_source`
- `merge_never_reads_the_mirror_cookie`: the same request with and without a `__Host-ts-eids-s` cookie gives the same `Vec<Eid>`.
- `merge_expired_entry_is_not_sent`
- `merge_stored_atype_and_ext_are_emitted`
- `merge_x_ts_eids_matches_what_was_sent`
- `merge_entry_set_goes_out_one_eid_per_entry_in_stored_order_after_browser_entries`
- `merge_provenance_applies_only_to_an_entry_with_no_eid_level_fields` (three entries of a provider-written set: one with no EID-level field gets the whole default; one with `inserter` set keeps its `inserter` and gets no `matcher` or `mm`; and a browser-seeded record gets none)
- `merge_header_still_truncates_per_entry` (a stored set whose entries pass 4 KB: `x-ts-eids` holds the entries that fit, `x-ts-eids-truncated` is set, and `user.eids` holds the whole set)
- `denial_drops_a_browser_uid_in_a_claimants_iv_for_an_unowned_source`
- `denial_drops_a_browser_uid_in_a_claimants_ov`
- `denial_filters_uid_by_uid_and_drops_an_emptied_entry`: a browser entry with UIDs A and B, where only A is in `iv`, goes out with B alone; a second entry holding only A is dropped.
- `denial_ignores_hashes_of_a_provider_that_does_not_claim_the_source` (control)
- `denial_drops_every_owned_source_while_the_owner_is_opted_out`
- `denial_drops_every_source_of_an_opted_out_capture_only_provider`: owned and unowned sources alike. Control: after a later `oc`, the browser values go out again.
- `denial_mirror_id_fed_back_through_the_body_is_filtered`: a UID that only the mirror once held, now in `ps.iv`, sent back in the `/auction` body, is not sent.
- `golden_merge_one_uid_row_goes_out_as_on_main` (G13): the merge of an empty client set with a row of one-UID records gives the exact `Vec<Eid>` today's `merge_auction_eids(None, Some(to_eids(..)))` gives. Write the expected value from `main`'s code, and do not edit it afterwards.
- `golden_merge_browser_entries_are_forwarded_unchanged` (G13): a client set with repeated sources, several UIDs and EID-level fields, no denials and an empty server set: the merge returns the client set as today's `merge_auction_eids` does, minus entries without UIDs.

Put the `publisher.rs` cases (the navigation and page-bids merge) under the same names with a `merge_publisher_` prefix. Compare whole `Vec<Eid>` values with every field written out, with no `..Default::default()`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- merge_`, `cargo test-fastly -- denial_`, `cargo test-fastly -- golden_merge_`, `cargo test-fastly -- ec::eids` and `cargo test-fastly -- auction::endpoints`
Expected: FAIL, then PASS. The existing `merge_auction_eids_*` tests and the PBS and APS goldens (`pbs_request_serialization_goldens_cover_disabled_and_deterministic_enabled_signing` at `integrations/prebid.rs:9981`, and `aps_request_forwards_eid_provenance_fields_and_repeated_sources` at `integrations/aps.rs:2312`, the closest APS test; there is no APS golden) still pass with their expected values unchanged. Port the existing merge tests to the new signature without changing what they assert.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Merge the server identity set into auction EIDs and apply provider denials" -m "A server value goes out as its whole entry set, one EID per stored entry, from KV only. A provider's provenance default applies only to an entry that sets no EID-level field. Browser candidates lose each UID a claiming provider invalidated or removed on opt-out, and every source of an opted-out owner or capture-only provider. Browser entries and one-UID records otherwise go out exactly as before."
```

### Task 29: Phase 4: the single flush after send, replacing pull sync

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (the post-route part of `edgezero_main` at about lines 506-557 becomes `finalize_send_and_flush`; `apply_edgezero_ec_finalize`, `run_edgezero_pull_sync_after_send` and `prepare_pull_sync_after_send` at about lines 595-666)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`AppState` gains `identity_graph: Option<KvIdentityGraph>`, used by `build_ec_request_state` (about line 450) and the admin lookup (about line 585) in place of per-request `maybe_identity_graph` calls; `app_state_for_settings` at about line 1462 gains a graph-taking twin for tests)
- Create: `crates/trusted-server-core/src/ec/identity/flush.rs` (`run_after_send`)
- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs` (delete `dispatch_pull_sync`, `build_pull_sync_context` and `PullSyncContext`; keep the helpers generic pull uses)
- Delete: `crates/trusted-server-core/src/ec/pull_sync_marker.rs`, with `COOKIE_TS_EC_PULL_COMPLETE` (`constants.rs:5`), the `EcContext` marker field and its accessors (`ec/mod.rs:187-212`, `:283`, `:366`, `:454`, `:581-616`), `validate_pull_sync_marker` and `reconcile_pull_sync_marker` calls (`ec/finalize.rs`, `publisher.rs:4495`), and the marker probe tests (`publisher.rs` at about line 8014)
- Modify: `crates/trusted-server-core/src/publisher.rs` (the preload decision at about lines 4819-4831 drops `marker_valid`)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` and `ec/identify.rs` (delete `evaluate_cluster` once no caller is left, Step 3)
- Test: `flush.rs`, `adapter-fastly/src/main.rs`, `adapter-fastly/src/app.rs`, `publisher.rs`

**Interfaces:**

- Consumes: `eligibility` with `Phase::AfterSend` and `outcome_to_changes` (Task 20), `dispatch` (Task 21), `mutation::apply` (Task 15), `write_removals` and `log_removal_lost` (Task 18), `compute_cluster_size` (Task 24)
- Produces:
  - `pub fn run_after_send(input: AfterSendInput<'_>) -> FlushOutcome`. `pub struct AfterSendInput<'a> { runtime: &'a IdentityRuntime, settings: &'a Settings, services: &'a RuntimeServices, graph: &'a KvIdentityGraph, context: &'a mut EcContext, status: StatusCode, rate_limiter: &'a dyn RateLimiter }`. The catalog and keys come from the runtime (Task 23), so no crate-private type is in the signature (P1). `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FlushOutcome { Skipped, NoChange, Written, Dropped(&'static str), WriteFailed { removals: RemovalWriteOutcome } }`.
  - In the adapter's `main.rs`: `fn finalize_send_and_flush(input: PostRouteInput<'_>, response: HttpResponse, send: &mut dyn FnMut(HttpResponse)) -> Option<FlushOutcome>` with `struct PostRouteInput<'a> { runtime: &'a IdentityRuntime, settings: &'a Settings, ec_state: &'a mut EcFinalizeState, graph: &'a dyn Fn() -> Option<KvIdentityGraph>, rate_limiter: &'a dyn RateLimiter }`. It takes the runtime and settings rather than `&AppState`, because the reload branch has no `AppState`. It runs finalize (Task 25) with the graph from `graph`, captures the status, hands the finalized response to `send`, runs the withdrawal retry (Task 27), then `run_after_send` with `ec_state.services`, and returns its outcome (`None` when there is no EC state). `edgezero_main` calls it with `send_edgezero_response` as `send`, `&state.identity` and `|| state.identity_graph.clone()` as `graph` (or `maybe_identity_graph(&settings)` on the reload branch, with that branch's runtime from Task 23's `build_identity_runtime`) and `FastlyRateLimiter::new(RATE_COUNTER_NAME)`. `send` is a callback, not a return value, so phase 4 still runs after the response is sent.
  - In `main.rs` tests: `fn drive_request_for_test(state: &Arc<AppState>, request: HttpRequest) -> (HttpResponse, Option<FlushOutcome>)`. It routes the request with the router the existing `app.rs` tests use (`block_on(router.oneshot(request))`), then calls `finalize_send_and_flush` with a `send` that keeps the response, `|| state.identity_graph.clone()` and an allow-all rate limiter. Tests build `state` with `app_state_with_graph_for_test(settings, graph)` in `app.rs`, passing the graph from `KvIdentityGraph::write_counting` (Task 22), so routing, finalize and phase 4 all count against one store. Tasks 30 and 39b drive every request-level test through it.
- `AppState` already holds the runtime (Task 23). This task adds only `identity_graph`.

**Phase 1 reads on every returning writer navigation** (spec 5.2 phase 1, 6.3 step 1). The publisher preload (`publisher.rs`, `auction_needs_row` and the preload at about lines 3110-3117 and 4819-4831) reads the row whenever `ec_context.identity().writer()` and the EC is returning, in addition to today's auction rule, and without the pull-complete marker's skip. That one read finds orphans, answers eligibility and filters browser candidates (Task 36), so nothing needs a cookie copy of the row. `/auction` and `/_ts/page-bids` keep their reads unchanged.

Phase 4 (`run_after_send`, spec 5.2 steps 1 to 5):

1. If the request is not a returning-EC writer: `Skipped`. If `status` is not 2xx or 304 (spec 5.1), no after-send provider is due, and the mutation is cut down with `IdentityMutation::retain_for_error_status()` (added here): it keeps every removal, opt-out and clear, and the `ps.n` and `ps.f` of every outcome other than `found`, and drops browser ingest and every positive result with its state and scheduling. In Part 1 the mutation holds only browser ingest, so this empties it; Task 37's hold-mode outcomes are what it keeps.
2. Collect the after-send providers: each provider `eligibility(.., Phase::AfterSend)` passes on the phase 1 view (`EligibilityView::from_kv`, Task 20). Without a phase 1 view (a failed read), use an empty view: step 3 re-reads before anything dispatches. If the mutation is empty and none is due: `Skipped`. No read.
3. If phase 1 read the row (`Present` with a generation) and no after-send provider is due, use that snapshot. Otherwise call `graph.get(ec_id)`. A tombstone, `consent.ok = false`, a missing row or a failed read: `Dropped(reason)`, and stop. No provider is called before this check, so no EC ID is disclosed before consent is confirmed (as `ec/pull_sync.rs:100-110` does today).
4. Recompute eligibility steps 4 and 5 for each after-send provider from the live row, rebuild its request from it (the live `ps.st` for a stateful provider), dispatch with `coordinator::dispatch(.., budget: None)`, and turn the outcomes into changes added to the mutation.
5. If the live row has no `cluster_size`, add it with `compute_cluster_size` and `set_cluster_size`.
6. `mutation::apply` against the live entry. If `changed`, write once with `IfGenerationMatch(generation)`: `Written`, or `NoChange` when nothing changed. Never retry the whole flush.
7. On a failed write (a precondition failure, Fastly's per-key rate limit or any other error), log at `warn` with `log_id`. When `mutation.removals()` (Task 15) is not empty, run `write_removals(graph, ec_id, &removals, &context)` (Task 18): a re-read, the removal rules of section 5.4, and up to five attempts. `Failed` there calls `log_removal_lost` and `RequestIdentity::record_removal_lost` (Task 23) once per lost removal. Return `FlushOutcome::WriteFailed { removals }` with that `RemovalWriteOutcome`, using `RemovalWriteOutcome::NothingToWrite` when the mutation held no removal.

`mutation::apply` writes each source's record whole (Task 15), so a multi-entry record the mutation does not touch keeps its `eids` byte for byte, and a version 1 row upgraded on write serializes as Task 6's golden expects.

Remove `run_edgezero_pull_sync_after_send` and `prepare_pull_sync_after_send`. Their callers call `finalize_send_and_flush`. Build the KV graph after send exactly as today (`require_identity_graph`).

- [ ] **Step 1: Write the failing tests**

In `flush.rs`:

- `writer_flushes_mutation_once_with_generation_match` (1 `IfGenerationMatch` write, no read after phase 1)
- `writer_with_nothing_to_do_reads_and_writes_nothing` (0 reads, 0 writes)
- `non_2xx_or_304_status_drops_ingest_and_dispatches_nothing` (and `retain_for_error_status_keeps_removals_and_backoff` in `mutation.rs`: a mutation with a `FillMissing`, an `OwnerWrite` from a `found`, an `OwnerRemove` and a `no_match` backoff keeps only the last two)
- `tombstone_on_reread_drops_mutation_and_calls_no_provider` (the fake HTTP client sees zero calls)
- `failed_phase_one_read_rereads_before_writing`
- `due_generic_pull_provider_dispatches_after_reread_and_its_uid_lands_in_the_same_write` (and the record it lands is a one-entry record with `w = pull-…`)
- `hold_provider_on_a_navigation_without_an_auction_runs_in_phase_four`
- `cas_conflict_is_not_retried_as_a_whole` (1 flush attempt, `WriteFailed`)
- `failed_flush_writes_its_removals_alone`: a mutation holding a `FillMissing`, an `OwnerRemove` and an opt-out, and a store that refuses the flush once. One flush attempt, then one removal write holding only the removal and the opt-out.
- `failed_removal_retry_counts_removal_lost`: a store that refuses every write. Five removal attempts, then `removal_lost` counts each removal.
- `burst_capped_provider_gets_sixty_second_backoff_in_the_write`
- `missing_cluster_size_is_added_to_the_one_write`
- `untouched_entry_set_is_preserved_by_the_flush` (a live row holding a multi-entry record and a sharedId fill in the mutation: the written row's record equals the live one, field for field)

In `publisher.rs`: `returning_writer_navigation_reads_kv_without_an_auction` (one read, and a control: a stager navigation without an auction reads nothing), replacing the marker probe tests.

In the adapter (`main.rs` tests): replace `pull_sync_noop_states_skip_post_send_graph_factory` with `after_send_noop_states_skip_post_send_graph_factory`, which checks that no graph is built when there is nothing to flush. Add `finalize_send_and_flush_sends_before_it_flushes`: a `send` callback that records the store's write count when it runs sees 0, and the outcome is `Written` with 1 write. Add `drive_request_for_test_counts_routing_finalize_and_flush_against_one_store`: a returning writer navigation with one change gives one read and one `IfGenerationMatch` write on the injected store. Add `no_response_sets_or_reads_the_pull_complete_cookie`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- flush`, `cargo test-fastly -- returning_writer_navigation_reads_kv` and `cargo test-fastly`
Expected: FAIL, then PASS.

- [ ] **Step 3: Delete pull sync's dispatcher and the marker**

Delete `dispatch_pull_sync`, `build_pull_sync_context`, `PullSyncContext`, `InFlightPull`, `pull_rate_limit_key` and `AllowAllRateLimiter` (its tests move with them; `drive_request_for_test` uses core's `test_support` allow-all limiter, or adds one there) from `ec/pull_sync.rs`, and `pull_sync_marker.rs` with every caller listed above. Then delete `KvIdentityGraph::evaluate_cluster` (`ec/kv.rs:1484`) and its two tests: Task 24 moved `/identify` to `compute_cluster_size`, and phase 4 writes the size through the mutation. `grep -rn "evaluate_cluster\|pull_sync_marker\|PULL_COMPLETE" crates/` must come back empty. Port every remaining pull sync test whose behaviour is unchanged to `generic_pull.rs` or `flush.rs` before deleting it. For each behaviour change in spec 10.4, write a test named after it:

- `generic_pull_runs_only_on_returning_writer`
- `new_visitor_reaches_pull_partners_from_second_page_load`
- `generic_pull_results_join_the_single_flush`
- `failing_pull_partner_backs_off_through_ps`
- `rate_limit_key_is_per_visitor_not_per_ip`

- [ ] **Step 4: Run everything**

Run: `cargo test-fastly && cargo test-fastly-reuse && cargo clippy-fastly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Flush identity changes once after send and run pull partners there" -m "Phase 4 replaces the pull sync after-send hook. It reuses the phase 1 snapshot when no after-send provider is due, otherwise re-reads the row, dispatches the due providers, and applies the whole mutation in one IfGenerationMatch write. A failed flush is not retried, but its removals are written alone with the shared CAS loop, and any that still fail are counted as lost." -m "Every returning writer navigation now reads KV in phase 1, so the pull-complete marker cookie is removed. Pull partners run only on a returning visitor's writer navigation and back off through ps. evaluate_cluster is removed."
```

### Task 30: Prove the one-write invariant, reads, G10 and G13

**Files:**

- Modify (tests only): `crates/trusted-server-adapter-fastly/src/main.rs`, a `#[cfg(test)] mod identity_writes` nested in its `mod tests`. The adapter is a binary crate with no `src/lib.rs`, so a `tests/` file could not import its items; every test here drives a request through Task 29's `drive_request_for_test`.
- Modify (tests only): `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` or `eids.test.ts`, for the `ortb2` half of G10, the registered-only merge and the `ts-eids` golden
- Test only

**Interfaces:**

- Consumes: `KvIdentityGraph::write_counting` and `WriteCountingEcKv` (Task 22), the fake HTTP client (Task 21), and Task 29's `drive_request_for_test` and `app_state_with_graph_for_test`. Core's `test_support` items are `pub` behind `test-utils`, which the adapter's dev-dependency on core enables.

- [ ] **Step 1: Write the scenario table as tests**

One test per row of spec 13's "One-write invariant" table that Part 1 can exercise. Each drives a full request through `drive_request_for_test` (routing, finalize, phase 4) and asserts the exact write counts by mode. The table's capture rows need Task 38, so they land in Task 39b as `captured_revocation_writes_with_cas`, `global_revoke_from_real_browser_writes_as_withdrawal` and `non_browser_capture_writes_nothing`. Part 1 has no hold mode, so the 3xx, 4xx and 5xx row is 0 here, and Task 39b adds its hold-mode half:

| Test                                                        | Writes                                                        |
| ----------------------------------------------------------- | ------------------------------------------------------------- |
| `new_visitor_navigation_writes_once`                        | 1 `Add`                                                       |
| `new_visitor_collision_remints_and_writes_once`             | 1 `Add` that lands (a precondition failure first)             |
| `returning_visitor_with_changes_writes_once`                | 1 `IfGenerationMatch`                                         |
| `returning_visitor_without_changes_writes_nothing`          | 0                                                             |
| `revalidation_304_with_changes_writes_once`                 | 1                                                             |
| `returning_navigation_answered_3xx_4xx_5xx_writes_nothing`  | 0 each                                                        |
| `new_visitor_prefetch_writes_once`                          | 1 `Add`                                                       |
| `returning_prefetch_writes_nothing`                         | 0                                                             |
| `new_visitor_without_sec_fetch_dest_writes_once`            | 1 `Add`                                                       |
| `returning_iframe_or_no_sec_fetch_dest_writes_nothing`      | 0                                                             |
| `integration_proxy_navigation_minting_ec_writes_once`       | 1 `Add`                                                       |
| `auction_page_bids_identify_proxy_and_assets_write_nothing` | 0 each                                                        |
| `consent_withdrawal_writes_tombstone_and_marker`            | today's count, per ID, plus one after-send run when it failed |
| `cas_conflict_on_flush_is_one_attempt`                      | 1 failed flush, then its removals alone (up to 5), or none    |

Also write:

- **Reads:** `returning_writer_navigation_reads_once`, `due_after_send_provider_adds_one_reread`, and `auction_and_page_bids_keep_one_read`. Each counts reads with the same store.
- **No server identity cookie:** `no_response_sets_an_identity_cookie_other_than_ts_ec`: across every scenario above, no `Set-Cookie` names `ts-eids`, `__Host-ts-eids-s`, any `ts-eids-r` or `ts-ec-pull-complete`.
- **The server never reads the mirror:** `forged_mirror_cookie_changes_nothing`: the same navigation and `/auction`, once without `__Host-ts-eids-s` and once with a forged, stale or malformed one. The bidder requests, the `/identify` answer and every KV write are identical.
- **G10:** `registered_source_beyond_todays_trim_reaches_kv_on_next_navigation` (the shim's `ts-eids` from Task 26's trim, carrying a registered source today's trim would drop: the next navigation's single write holds it) and `ortb2_only_eid_reaches_kv` (below).
- **Failed flush:** `refused_flush_is_rederived_by_the_next_navigation` (a refused write, then a second navigation with the same cookies: the second write holds the ingest) and `refused_flush_writes_its_removals_alone`.
- **Orphans:** `deleted_row_is_found_by_the_next_writer_and_rotated` and `stager_that_sees_missing_row_returns_clear_and_next_writer_recovers`.
- **Withdrawal:** `withdrawal_on_auction_after_navigation_send_makes_phase_four_drop_and_call_nothing`, and `withdrawal_returns_clear_for_the_mirror`.
- **Servability (P2):** `generic_pull_uid_written_after_send_is_served_on_the_next_auction`: a returning writer navigation whose phase 4 lands a generic pull UID (`w = pull-…`), then an `/auction`: `user.ext.eids` carries the UID, and `ext.ts_eids_s` mirrors it. This is Task 23's `generic_pull_record_with_writer_is_served_after_flush` driven end to end.

**R8: an EID present only in Prebid's `ortb2` config reaches `ts-eids` and KV (spec 13, G10).** Lockr writes its ID5 there. Two halves:

1. In the shim's Prebid tests, add `ortb2_config_only_eid_reaches_ts_eids_and_the_auction_body` if the existing tests do not already cover it (`grep -n "ortb2" crates/trusted-server-js/lib/test/integrations/prebid/*.ts`). A bidder request whose `ortb2.user.ext.eids` holds an `id5-sync.com` entry that `getUserIdsAsEids()` does not return: `buildRequests` writes it into `ts-eids` and the `/auction` body. Assert the exact cookie value, and copy it into the Rust test as a fixture.
2. `ortb2_only_eid_reaches_kv`: a returning writer navigation carrying that exact `ts-eids` value, with `id5-sync.com` registered and absent from KV. After the one write, KV holds the UID.

**Complete provider EIDs (G13, spec 13).** Part 1 writes no multi-entry record, but reads, preserves and serves one (spec 14, step 1), so these seed version 2 records directly:

- `entry_set_round_trips_from_kv_to_bidders`: a row holding a two-entry, three-UID record with every UID-level and EID-level field, for a registered unowned source the browser does not send. One `/auction`: `user.ext.eids` and `x-ts-eids` carry both entries after the browser entries, in stored order, every field intact, and the mirror payload holds the same entries.
- `entry_set_survives_an_unrelated_flush`: the same row, then a navigation that flushes a sharedId fill. The written row's record is unchanged.
- `expired_entry_set_is_never_served_or_mirrored`.
- `version_1_row_is_served_and_upgraded_by_the_next_write`: a version 1 row with `uid` records. `/auction` serves them as today; the next writer navigation with a change writes version 2.

**Behaviour that predates this spec (G13, spec 13).** Golden tests pin each path G13 must not change. Write each expected value from the code on `main`: run the test body in a worktree of `main` (`git worktree add`) and copy what it prints into the assertion, then never edit it. Where the row shape differs only by the version 2 encoding (Task 6), the assertion compares the decoded records:

- `golden_browser_ingest_writes_the_first_uid`: a navigation whose `ts-eids` holds a registered source with two UIDs, `inserter`, `mm` and EID-level `ext`, and an unregistered source. The created or flushed row holds the same UID for the same sources as `main`'s row for the same input, apart from the record metadata Part 1 adds (`at`, `rv`).
- `golden_one_uid_row_goes_out_unchanged`: a row of one-UID records, `/auction`, browser sends nothing. `user.ext.eids` and `x-ts-eids` equal `main`'s, byte for byte.
- `golden_auction_body_forwarding_is_unchanged`: a body with repeated sources and EID-level fields and an empty KV row. `user.ext.eids` equals `main`'s.
- The `ts-eids` cookie's content and EID-level `ext` removal belong to the shim, and Part 1 changes only its trim order. Check that the shim's Prebid tests pin both (`grep -n "MAX_EID_COOKIE_BYTES\|ts-eids" crates/trusted-server-js/lib/test/integrations/prebid/*.ts`), and add `ts_eids_cookie_golden` with a pinned value of a set with no unregistered source, which Task 26's trim leaves exactly as today.
- The PBS and APS golden requests named in Task 28 pass unchanged.

- [ ] **Step 2: Run them**

Run: `cargo test-fastly -- identity_writes`, `cargo test-fastly -- golden_`, then `cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run`
Expected: PASS. A failure here is a real bug in Tasks 23 to 29. Fix it in the task that owns the behaviour, and do not loosen the test.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-adapter-fastly crates/trusted-server-js/lib/test
git commit --signoff -S -m "Test the one-write-per-page-load invariant end to end" -m "Also pins the paths complete provider EIDs must not change, the ortb2-only EID reaching KV, entry sets read and served whole, version 1 rows upgraded on write, and a server that never reads the ts-eids-s mirror."
```

### Task 31: Metrics, docs and the Part 1 PR

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs` or `ec/identity/request.rs` (metrics)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (emit the line after phase 4)
- Modify: `docs/guide/edge-cookies.md`, `docs/guide/configuration.md`, `docs/guide/ec-setup-guide.md`, `docs/guide/api-reference.md`, `CHANGELOG.md`, `trusted-server.example.toml`

- [ ] **Step 1: Metrics**

Find how the repo emits request metrics or counters today (`grep -rn "record_eid_sync_terminal\|metric\|counter" crates/trusted-server-core/src/ec/finalize.rs | head`). Today that is a structured `log::info!` line (`EID sync measurement: ...`), which Task 25 removed for returning visitors (the new-EC `record_eid_sync_terminal` stays). These counters replace it. Emit them the same way, as one `log::info!("identity request measurement: ...")` line per request after phase 4, with no IDs and no cookie values:

- the inbound `Cookie` header size in bytes (spec 14 step 1)
- KV reads and writes per request, labelled by route kind (writer, stager, new EC)
- the phase 4 outcome (`FlushOutcome` as a label)
- `removal_lost` (Task 23), and whether a withdrawal tombstone needed its after-send run and whether that run failed (Task 27)

Task 38 adds `capture_unpersisted` to the same line.

Add `identity_measurement_records_counts_for_a_writer_navigation` (a writer navigation with one change: the measurement holds the `Cookie` header size, one read, one write and `Written`), and a control `identity_measurement_holds_no_identifier` that checks the formatted line contains neither the EC ID nor any UID or cookie value.

- [ ] **Step 2: Docs**

- `edge-cookies.md`: add a section for `__Host-ts-eids-s`: a client-only mirror of the server IDs that the shim writes from `/auction` and `/_ts/page-bids` responses for page scripts, with `Path=/; Secure; SameSite=Lax`, not `HttpOnly`, a `Max-Age` of at most one day and never past the earliest ID expiry, and a 2,048-byte cap that leaves out whole sources by `cookie_priority`. The server never reads it, and it is stripped from origin and Prebid Server requests (Tasks 1 to 4). Note that `ts-ec-pull-complete` is gone. Describe the writer-request rule in plain words: only a full page navigation from a returning visitor writes KV, once, after the page is sent. Say that the shim now writes only registered sources into `ts-eids` and merges each auction's entries per source.
- `configuration.md`: the `[ec.identity]` table, with every key, its default and its meaning, from spec 10.1. Name `revoke_scope` as applying from Part 2. Name the removed keys and their replacements: `ec.pull_sync_concurrency` becomes `[ec.identity].max_concurrency`; `pull_sync_ttl_sec` becomes `refresh_after_sec` and `pull_sync_rate_limit = N` becomes `max_calls_per_minute = ceil(N / 60)`, both in `[ec.identity.providers.<pull-id>]` (Tasks 19 and 22). Give the ID rule with examples: `ssp.example.com` is `pull-ssp-example-com`, and a domain that contains `-`, or an ID over 63 characters, also gets `-` and 8 hex digits of the domain's SHA-256 (spec 10.4). Validation rejects the old keys. The capture provider checklist lands with capture, in Task 41.
- `ec-setup-guide.md`: pull partners now run from a visitor's second page load, back off on errors, and are paced by their provider table.
- `api-reference.md`: `/_ts/api/v1/identify`, `/auction` and `/_ts/page-bids` no longer write KV, and `/identify` omits expired IDs. `/identify`'s response shape is unchanged: one UID per source, even for a record that holds several. Document `ext.ts_eids_s` in the `/auction` response and `ts_eids_s` in the page-bids body, with the `value`, `clear` and absent cases (spec 6.2).
- `configuration.md`, a "Rolling back" subsection after the `[ec.identity]` table (spec 14). This release moves KV to schema version 2, and a release before it cannot read a version 2 row, so a rollback past it is unsupported once it has written. For a later vendor release rolled back to this one: remove `owner` and every vendor `[ec.identity.providers.*]` table from the config first, because this release rejects an owner or a provider table it does not know; entry sets stay readable but are not served while their writer is not configured; and a write prunes an unconfigured provider's `ps` to `o`, `oc` and `ov`, so opt-outs survive while `ps.iv` and `ps.st` do not.
- `CHANGELOG.md` `### Changed`: one entry each for:
  - fewer KV writes, with the writer-request rule
  - `/auction`, `/_ts/page-bids` and `/identify` no longer writing
  - pull partners from the second page load, paced by their provider table
  - the removed config keys and their replacements, and that validation now rejects the old ones
  - the `ts-eids-s` mirror payload and the shim's registered-only `ts-eids` merge
  - the removed `ts-ec-pull-complete` cookie
  - KV schema version 2, and that rolling back past this release is unsupported
  - the removed `EID sync measurement: ...` log line for returning visitors and its replacement, the `identity request measurement: ...` line, because operator dashboards may read it

  Link the "Rolling back" subsection.

Update `trusted-server.example.toml` with a commented `[ec.identity]` table and one commented `[ec.identity.providers.pull-ssp-example-com]` table.

Run `cd docs && npm run format` and the root markdown prettier command from `AGENTS.md` gate 8.

- [ ] **Step 3: Commit**

```bash
git add -A crates/ docs/ CHANGELOG.md trusted-server.example.toml
git commit --signoff -S -m "Document the single identity write and the ts-eids-s mirror"
```

- [ ] **Step 4: Part 1 gate and PR**

Run `/check-ci`. Every gate must pass, including `scripts/lint-wasm-clock.sh` (new core code must not call `std::time::{Instant,SystemTime}::now`. Use `checked_current_timestamp()` and `web_time` where you need a clock), the parity crate, `./scripts/test-cli.sh`, and the shim's `npm run lint`, which CI's `format-typescript` job runs beside the format check. Open the implementation PR against the spec branch with `pr-creator`. Title: "Write identity KV once per page load". The description lists the behaviour changes from spec 14 step 1, the removed config keys, the `test-utils` feature reuse, and the rollback limit.

# Part 2: Providers (spec 14, step 2)

Part 2 lets integrations register providers, and adds ownership, hold mode, revoke-only passive capture (the capture removal write, its `capture_unpersisted` and `removal_lost` counters, the real-browser gate, and `revoke_scope` defaulting to `"provider"`), provider state and sticky opt-outs, the circuit breaker, and the consent decoding that strict policies need. It also brings the first full-set writes (G13): Part 1 already reads, preserves, serves and validates complete entry sets, and here registered providers start returning several entries or UIDs per source, or EID-level fields. Every path that predates the spec (browser ingest, `ts-eids`, batch sync, generic pull and the `/identify` response shape) keeps its Part 1 golden tests (the `golden_` tests) unchanged. No vendor ships here: ID5, LiveRamp, Lockr and UID2 each get their own spec and plan (spec 2.2). This part proves the framework with the fictional `exampleid` and capture providers from spec 4.6, registered only in tests.

Before you start: `git switch feat/1246-identity-single-write && git switch -c feat/1246-identity-providers`.

### Task 32: Registering identity providers

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (`IntegrationRegistration`, the builder around line 641, `with_plan` around line 842, and a new `from_registrations`)
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (`from_settings` adds registered providers)
- Modify: `crates/trusted-server-core/src/ec/identity/runtime.rs` (`IdentityRuntime::from_settings` takes the integration registry)
- Modify: `crates/trusted-server-core/src/config.rs` (`identity_descriptors_for_deploy`)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`build_identity_runtime` passes the registry `build_state_from_settings` already builds, at about line 226)
- Create: `crates/trusted-server-core/src/ec/identity/examples.rs` (`#[cfg(test)] pub(crate) mod examples;` in `mod.rs`)
- Test: `integrations/registry.rs`, `ec/identity/catalog.rs`

**Interfaces:**

- Consumes: `IdentityRuntime` (Task 23), `ProviderCatalog::from_settings` and `validate_identity_catalog` (Task 22)
- Produces:
  - `IntegrationRegistrationBuilder::with_identity_provider(self, provider: Arc<dyn IdentityProvider>) -> Self`
  - `IntegrationRegistrationBuilder::try_with_identity_provider<P: IdentityProvider + 'static>(self, provider: Result<P, Report<IdentityProviderError>>) -> Result<Self, Report<TrustedServerError>>`. A failure becomes `TrustedServerError::Integration { integration: <id>, message: <current_context()> }`.
  - `IntegrationRegistry::identity_providers(&self) -> Vec<Arc<dyn IdentityProvider>>`
  - `pub(crate) fn IntegrationRegistry::from_registrations(registrations: Vec<IntegrationRegistration>) -> Result<Self, Report<TrustedServerError>>`. `with_plan` gathers its registrations as today (`prebid::register_for_plan`, `aps::register_for_plan` and `crate::integrations::builders()`, `integrations/mod.rs:292`) and then calls it. The merge and validation loop moves into it, so tests can pass a test integration that no settings can add.
  - In `from_registrations`: every capture path and prefix must lie under one of the registering integration's `IntegrationProxy::routes()`. Otherwise it is a `Configuration` error naming the integration.
  - `ProviderCatalog::from_settings(settings, registered: &[Arc<dyn IdentityProvider>])` adds the registered providers after generic pull and runs Task 22's `validate_identity_catalog` over every descriptor, so a duplicate ID is reported by the one validator. `IdentityRuntime::from_settings(settings, integrations: &IntegrationRegistry)` passes `integrations.identity_providers()`. The adapter's `build_identity_runtime` passes `&registry`, both from `build_state_from_settings` and from the settings-reload branch.
  - `pub(crate) fn identity_descriptors_for_deploy(settings: &Settings) -> Result<Vec<ProviderDescriptor>, Report<TrustedServerError>>`: the generic pull descriptors built from `settings.ec.partners` (Task 22, secret-free) plus one line per identity integration. No integration has a line in this PR. Task 33 adds the mechanism for one and the test line.
  - `examples.rs` (`#[cfg(test)]`): `ExampleIdConfig`, `ExampleIdState`, `ExampleIdProvider` and the capture-only `ExampleCaptureProvider`, copied verbatim from spec 4.6, plus `example_config()`. Tasks 33, 38 and 39 import from here.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn try_with_identity_provider_converts_a_constructor_error_once() {
        let failing: Result<ExampleIdProvider, Report<IdentityProviderError>> =
            Err(IdentityProviderError::config("bad partner id"));
        let error = IntegrationRegistration::builder("exampleid")
            .try_with_identity_provider(failing)
            .expect_err("should fail on a constructor error");
        assert!(
            matches!(error.current_context(), TrustedServerError::Integration { integration, .. } if integration == "exampleid"),
            "should name the integration"
        );

        let built = IntegrationRegistration::builder("exampleid")
            .try_with_identity_provider(ExampleIdProvider::new(&example_config()))
            .expect("should register a valid provider")
            .build();
        assert_eq!(built.identity_providers.len(), 1, "should hold one registered provider");
    }

    #[test]
    fn capture_rule_outside_the_integrations_routes_is_rejected_at_startup() {
        let error = IntegrationRegistry::from_registrations(vec![capture_registration("/integrations/other/api")])
            .expect_err("should reject a capture rule outside the integration's routes");
        assert!(format!("{error}").contains("examplecapture"), "should name the integration");

        IntegrationRegistry::from_registrations(vec![capture_registration("/integrations/examplecapture/api/token")])
            .expect("should accept a capture rule under the integration's own routes");
    }
```

`capture_registration(path)` is a test helper in this module. It builds an `IntegrationRegistration` for `examplecapture` with an `IntegrationProxy` whose routes sit under `/integrations/examplecapture/`, and an `ExampleCaptureProvider` whose capture path is `path`. Follow how existing registry tests build a stub integration (`grep -n "from_request_filters\|fn stub_" crates/trusted-server-core/src/integrations/registry.rs | head`).

In `catalog.rs`: `registered_provider_with_a_generic_pull_id_is_rejected_by_the_shared_validator`. A registered provider whose ID equals a generic pull ID fails with Task 22's duplicate-ID error text. The control registers `exampleid` next to one pull partner and builds.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- integrations::registry::tests` and `cargo test-fastly -- ec::identity::catalog`
Expected: FAIL, then PASS. Add `identity_providers: Vec<Arc<dyn IdentityProvider>>` to `IntegrationRegistration` and merge it in `from_registrations`, as proxies are merged. A route matches a capture path when the path equals the route's path or, for a wildcard route, starts with its prefix. Mirror how `handle_proxy` matches routes today. Existing `with_plan` tests must pass unchanged.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Let integrations register identity providers"
```

### Task 33: Ownership and deploy-time validation

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (`EcPartner` gains `owner`)
- Modify: `crates/trusted-server-core/src/ec/registry.rs` (`PartnerConfig.owner`)
- Modify: `crates/trusted-server-core/src/config.rs` (`validate_identity_integration`, and `identity_descriptors_for_deploy` gains its test line)
- Modify: the module that holds Task 22's `validate_identity_catalog` (the ownership rules)
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (owners from `[[ec.partners]].owner`; Task 14's `sources` narrowing already applies)
- Test: `config.rs`, and the `validate_identity_catalog` test module

**Interfaces:**

- Consumes: Task 22's `validate_identity_catalog(descriptors, partners, identity)`, which already rejects a duplicate provider ID, more than 32 providers (generic pull included), an `[ec.identity.providers.<id>]` table naming an unknown provider, and `revoke_scope` set for a provider whose descriptor declares no `captures`. This task adds rules to that one function. It does not add a second validator.
- Produces:
  - `EcPartner.owner: Option<String>` and `PartnerConfig.owner: Option<String>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`
  - `fn validate_identity_integration<T: IdentityProviderConfig + DeserializeOwned>(settings: &Settings, id: &str) -> Result<Option<ProviderDescriptor>, Report<TrustedServerError>>`. It parses `[integrations.<id>]` as `T` and, when enabled, calls `build_descriptor`. It never builds a provider and never reads secrets.
  - New rules in `validate_identity_catalog`, every remaining error of spec 10.2:
    - `owner` names a provider that is not enabled
    - `owner` names a provider whose (narrowed) descriptor lacks that source
    - an enabled provider claims a source with no `[[ec.partners]]` entry
    - two enabled providers claim a source one of them owns (`sources` narrowing applied first)
    - `sources` names an undeclared source, or narrows to nothing
    - a capture-only provider (one that declares `captures` and no `resolves`) has no `[ec.identity.providers.<id>].sources` list: it must name the sources it is the delivery path for (spec 10.2)
    - `owner` names a capture-only provider: a capture-only provider cannot own a source, because it stores no positive ID (spec 4.7)
  - The deploy path (`validate_settings_for_deploy`, next to `PartnerRegistry::validate_config_for_deploy`) calls `validate_identity_catalog` on `identity_descriptors_for_deploy(settings)`. It never builds a `PartnerRegistry`, because `PartnerRegistry::from_config` validates API tokens that are still key names at deploy time. The runtime path validates through `IdentityRuntime::from_settings` (Task 32). `identity_descriptors_for_deploy` has one `validate_identity_integration::<T>` line per identity integration. No production integration has one in this PR. Under `#[cfg(test)]` it has one for `examples::ExampleIdConfig` (Task 32), so the list's shape is exercised.

- [ ] **Step 1: Write the failing tests**

One test per new rule above, each paired with a control config that passes. Also:

- `config_without_ec_identity_is_accepted`
- `build_descriptor_runs_in_deploy_validation_without_secrets`: a config whose secret field holds a key name, not a value, still validates
- `capture_settings_on_a_provider_without_captures_are_rejected_at_deploy`: through `validate_settings_for_deploy`, `revoke_scope = "global"` on the resolve-only `exampleid` fails, naming the provider. The rule is Task 22's; this test proves the deploy path reaches it with an integration descriptor. The control is the same key on `ExampleCaptureProvider`'s descriptor, with its `sources` listed, which validates.
- `capture_only_provider_without_sources_is_rejected` and `capture_only_provider_cannot_own_a_source`, each with a control.
- `capture_enabled_has_no_effect`: a provider table holding `capture_enabled = true` loads (Task 19 keeps provider tables lenient) and changes nothing: a capture still stores no positive ID.

For example:

```rust
    #[test]
    fn owner_must_name_an_enabled_provider_that_declares_the_source() {
        let descriptors = vec![descriptor("exampleid", &["exampleid.example.com"])];
        let ok = vec![partner("exampleid.example.com", Some("exampleid"))];
        validate_identity_catalog(&descriptors, &ok, &IdentitySettings::default())
            .expect("should accept an owner that declares the source");

        let unknown = vec![partner("exampleid.example.com", Some("missing"))];
        let error = validate_identity_catalog(&descriptors, &unknown, &IdentitySettings::default())
            .expect_err("should reject an owner that is not enabled");
        assert!(format!("{error}").contains("missing"), "should name the owner");

        let wrong_source = vec![
            partner("exampleid.example.com", None),
            partner("other.example.com", Some("exampleid")),
        ];
        validate_identity_catalog(&descriptors, &wrong_source, &IdentitySettings::default())
            .expect_err("should reject an owner whose descriptor lacks the source");
    }
```

`descriptor` and `partner` are test helpers in that module. `partner` builds the partner type Task 22's signature takes.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- validate_identity_catalog`, `cargo test-fastly -- owner`, and `cargo test-fastly -- config::`
Expected: FAIL, then PASS.

- [ ] **Step 3: CLI check**

Run: `./scripts/test-cli.sh`
Expected: PASS. `ts config validate` and `ts config push` call the deploy validation, so the CLI tests exercise the new pass. If a CLI fixture config now fails, it is a real config error by the new rules. Fix the fixture and say so in the commit.

- [ ] **Step 4: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Add source ownership and validate identity provider config at deploy"
```

### Task 34: Consent decoding for strict policies, and the consent fingerprint

**Files:**

- Modify: `crates/trusted-server-core/src/consent/gpp.rs` (around `decode_us_sale_opt_out` at line 125, and the section-6 skip at line 112)
- Modify: `crates/trusted-server-core/src/consent/types.rs` (decoded fields, and a redacting `Debug` for `ConsentContext`)
- Modify: `crates/trusted-server-core/src/ec/identity/policy.rs` (new blocks, and the fingerprint bits)
- Test: `consent/gpp.rs`, `consent/types.rs`, `policy.rs`

**Interfaces:**

- Consumes: Task 20's `fingerprint_bits` (TCF purposes, the vendor bit, the US sale opt-out, GPC and DNT, as a plain hex bitmask), which this task extends
- Produces:
  - Decoded on the GPP form: `us_sharing_opt_out: Option<bool>`, `us_targeted_advertising_opt_out: Option<bool>`, `us_gpc: Option<bool>` and `us_known_child: Option<bool>` (US National section), and the section 6 US Privacy string read through the existing US Privacy decoder. Use the `iab_gpp` fields the spec names: `sharing_opt_out` (US National, California) and `targeted_advertising_opt_out` (US National and the state sections that define it). Check the `iab_gpp` version in `Cargo.lock` and read its section structs before writing the extraction.
  - `policy_allows` additionally blocks a strict policy on the sharing opt-out, the targeted-advertising opt-out, GPP's GPC field, and a known child without consent (a known child blocks every provider, whatever the relaxations).
  - A GPP string that decodes but has no applicable section, such as the header-only `DBAA` (spec 9.1), keeps its decoded form with every new field `None`. It carries no opt-out, so it does not block `strict()` by itself. Task 12 owns the fail-closed rule (a raw string with no decoded form blocks). The new decoders must not turn an empty section list into a missing decoded form. Where TCF applies, the policy still needs TCF consent from a TC string or a GPP TCF section.
  - `fingerprint_bits(policy, consent, signals)` appends two bits after Task 20's: the sharing and the targeted-advertising opt-out, each 0 when the policy relaxes it (spec 8). GPP's GPC field sets the existing GPC bit. Each signal keeps its own named bit position (`const FP_US_SALE: u32`, and so on, in `policy.rs`), so Task 36 can test the US opt-out bits by mask. Never the raw consent string.
  - Pub(crate) consts `FP_US_OPT_OUT_MASK` (sale, sharing, targeted advertising, GPC) for Task 36.
  - A hand-written `impl fmt::Debug for ConsentContext` that prints `[redacted]` for every raw string

- [ ] **Step 1: Write the failing tests**

- In `gpp.rs`: one decode test per new field, per section that defines it, using GPP strings built the way existing tests build them. Each asserts the decoded value, next to a control string where the field is not set.
- In `policy.rs`: `strict_blocks_sharing_targeted_gpc_field_and_known_child`, and `targeted_advertising_opt_out_without_sale_opt_out_blocks_strict`.
- In `policy.rs`: `header_only_gpp_does_not_block_strict_where_tcf_does_not_apply`. A request outside TCF jurisdiction with only the GPP string `DBAA` passes `strict()`. Two controls: the same string with `gdpr_applies` and no TC string fails for lack of TCF consent, and an undecodable GPP string fails closed, as Task 12 has it.
- In `gpp.rs`: `header_only_gpp_decodes_with_no_section_fields`. `DBAA` decodes, `section_ids` is empty, and every new field is `None`.
- `consent_context_debug_redacts_raw_strings`: `format!("{context:?}")` does not contain the TC string.
- `fingerprint_is_stable_across_cmp_resaves_and_changes_with_a_purpose`: two TC strings with identical decisions but different `LastUpdated` give the same bits, and dropping purpose 4 changes them.
- `fingerprint_bits_name_each_opt_out_separately`: setting only the sharing opt-out and setting only the targeted-advertising opt-out give two different bitmasks, both different from neither, and each sets only its own bit.
- `fingerprint_keeps_part_1_bit_positions`: a consent that sets only Part 1 signals gives the same hex value as Task 20's function did (pinned).

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- consent::` and `cargo test-fastly -- ec::identity::policy`
Expected: FAIL, then PASS. Do not change any existing auction consent decision. Run `cargo test-fastly -- auction::` and `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` to confirm.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src/consent crates/trusted-server-core/src/ec/identity
git commit --signoff -S -m "Decode the GPP opt-outs strict identity policies need" -m "Adds the US sharing and targeted-advertising opt-outs, GPP section 6, and the US National GPC and known-child fields. A header-only GPP string with no applicable section still does not block a strict policy. ConsentContext's Debug output no longer prints raw consent strings."
```

### Task 35: Rate-limiter `record` and `count`, and the circuit breaker

**Files:**

- Modify: `crates/trusted-server-core/src/ec/rate_limiter.rs` (trait)
- Modify: `crates/trusted-server-core/src/ec/batch_sync.rs` (`MockRateLimiter`, about line 328, implements the new methods)
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (`FakeRateLimiter`)
- Modify: `crates/trusted-server-adapter-fastly/src/rate_limiter.rs` (`FastlyRateLimiter`)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (breaker in dispatch, eligibility step 6)
- Test: `rate_limiter.rs` (adapter), `coordinator.rs`

**Interfaces:**

- Produces:
  - `RateLimiter::record(&self, key: &str) -> Result<(), Report<TrustedServerError>>` and `RateLimiter::count(&self, key: &str) -> Result<u32, Report<TrustedServerError>>`. Both are trait methods without defaults, so every implementation must decide: `FastlyRateLimiter`, `MockRateLimiter` in `ec/batch_sync.rs`, and `FakeRateLimiter`. (`AllowAllRateLimiter` in `ec/pull_sync.rs` is gone since Task 29.) Fastly uses `counter.increment(key, 1)` and `counter.lookup_count(key, CounterDuration::SixtySecs)`, and maps errors the way `exceeded` does.
  - `pub struct FakeRateLimiter` in `test_support.rs` (public under `test-utils`, as the module's other items), storing counts in a `Mutex<HashMap<String, u32>>`
  - Breaker: before dispatch, `count("identity-fail:<provider>") >= breaker_failures_per_minute` gives `CallOutcome::BreakerOpen` (not dispatched, `ps` unchanged). After dispatch, only `TransportFailure` outcomes (transport errors, timeouts, 429 and 5xx) call `record("identity-fail:<provider>")`. `HoldCutOff` is a budget cut, not a provider failure, and never records (spec 4.7).
  - Limiter errors. If `count` fails, log it and treat the breaker as closed. If the burst-cap check fails, log it and do not dispatch that provider on this request. If `record` fails, log it and keep the outcome.

- [ ] **Step 1: Write the failing tests**

In core, with `FakeRateLimiter`, which can also be set to fail each method:

- `breaker_opens_at_threshold_and_skips_every_visitor`
- `four_xx_and_malformed_never_open_the_breaker`: 1,000 visitors with 400 responses leave `count` at 0
- `hold_cut_off_does_not_record_a_failure`
- `burst_cap_is_checked_after_every_other_eligibility_step`: an ineligible provider never increments the burst counter
- `breaker_count_error_treats_the_breaker_as_closed`
- `burst_cap_error_skips_the_dispatch`

In the adapter: the existing `mod tests` in `rate_limiter.rs` tests only pure helpers and never calls the counter, because Viceroy 0.17's Edge Rate Limiting host calls cannot be relied on to count (this plan has not confirmed that they do). Keep that pattern. Put the two counter calls behind a private `trait CounterOps { fn increment(&self, key: &str) -> ...; fn lookup_count(&self, key: &str) -> ...; }`, implemented for `RateCounter`, and test `record` and `count` against a fake `CounterOps`: `record_increments_once`, `count_reads_the_sixty_second_window`, and `counter_errors_map_to_kv_store_errors`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- rate_limiter`, `cargo test-fastly -- breaker`, `cargo test-fastly -- burst_cap` and `cargo test-fastly -- ec::batch_sync`
Expected: FAIL, then PASS.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Add a per-provider circuit breaker on transport failures"
```

### Task 36: Owner precedence, opt-out re-grants and the admin clear

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (opt-out clearing on re-grant)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` and `publisher.rs` (the owned precedence row)
- Modify: `crates/trusted-server-core/src/ec/admin.rs` (an operator clear of `ps.o`)
- Modify: `crates/trusted-server-core/src/settings.rs` (`Settings::ADMIN_ENDPOINTS`, about line 3346)
- Modify: the admin route tables of all four adapters: `crates/trusted-server-adapter-fastly/src/app.rs` (routes at about line 1149, the module doc table at lines 25-26), `crates/trusted-server-adapter-axum/src/app.rs` (about line 365), `crates/trusted-server-adapter-cloudflare/src/app.rs` (about line 640) and `crates/trusted-server-adapter-spin/src/app.rs` (`named_fallback_paths`, about line 224, whose array length grows by one)
- Test: `coordinator.rs`, `auction/endpoints.rs`, `publisher.rs`, `ec/admin.rs`, `settings.rs`, adapter `app.rs` tests

**Interfaces:**

- Consumes: Task 28's `ServerSet`, `Denials` and `filter_browser_candidates`, which already drop denied browser UIDs and every source of an opted-out owner or capture-only provider; Task 34's `fingerprint_bits` and `FP_US_OPT_OUT_MASK`; Task 15's `opt_out_clear`, which also empties `ps.ov`
- Produces:
  - `pub(crate) fn regrant_clears_opt_out(descriptor: &ProviderDescriptor, opt_out: &KvOptOut, consent: &ConsentContext, signals: PolicySignals) -> bool` (spec 8):
    - **GVL vendor:** `o.fp` has the TCF bit (`FP_TCF`, bit 16) and lacks the vendor bit or a required purpose bit, and the current TC string grants the vendor and every required purpose. No `LastUpdated` comparison (revised after the round 14 review; test `tcf_regrant_clears_only_a_denial_the_opt_out_recorded`).
    - **Watermark:** `opt_out_clears` returns `(provider, o.at)` for the opt-out the request read, and the clear stores that `at` as `oc`, not `now`. `apply_clear` lifts the opt-out when `o.at <= watermark` and `oc < watermark`; `is_opted_out` holds while `oc` is absent or `< o.at`; an opt-out applies when `at > oc`. Clears apply before opt-outs in `apply_state`. Tests: `opt_out_clears_carry_the_opt_out_they_saw`, `a_clear_lifts_only_the_opt_out_it_saw`, `a_clear_watermark_lifts_the_opt_out_it_saw`.
    - **Any provider, under GPP:** parse `o.fp` as a `u64` (Task 6 validates it as hex). Clear when `fp & FP_US_OPT_OUT_MASK` is not 0 and the current `fingerprint_bits` has none of the US opt-out bits `fp` recorded set: `current & fp & FP_US_OPT_OUT_MASK == 0`. Other bits may differ; the spec clears "whatever else changed" (spec 13).
    - A jurisdiction change, a newly present GPP string, or a relaxed signal never clears. A `fp` that does not parse never clears.
  - When it clears, the writer adds `opt_out_clear = Some(now)` to that provider's `ProviderStateChange`. Clears are writer-only on the resolve path, so they ride the one flush; Task 38 adds the capture-only clear through the capture removal write.
  - The owned row of the precedence table becomes active (spec 7.1). The owner's valid value (`w == owner`, not expired) is its whole entry set, and it replaces every browser entry for the source, all of them when the browser repeated the source. The two are never mixed. Task 28's output rules then send it, one `Eid` per stored entry in stored order, after the browser entries. Without an owner's valid value, the browser's value goes out after Task 28's denials. When the browser did not send the source, a browser-seeded value is used only if the source does not expire. A browser-seeded record never beats the browser's current value.
  - Admin: `DELETE /_ts/admin/ec/{id}/providers/{provider}/opt-out`, which clears `ps.o` by writing `oc = now` through the existing admin write path, behind the same Basic-auth coverage. The clear empties `ps.ov` (Task 15). Add the path to `Settings::ADMIN_ENDPOINTS` so its sync test (about `settings.rs:8191`) covers it. Add the route to all four adapters. The Fastly adapter calls the core handler; Axum, Cloudflare and Spin return the same "not available on this adapter" response they give the EC lookup. Keep the Fastly reserved-path test's `/_ts/admin/ec/{ec_id}/extra` case (app.rs, about line 2078) as a path that must not route.

- [ ] **Step 1: Write the failing tests**

- `tcf_regrant_after_opt_out_clears_it` and `jurisdiction_change_never_clears_it`
- `gpp_regrant_clears_when_none_of_the_recorded_opt_outs_remain`: the opt-out was stored with the sale bit set, and the current section shows no opt-out. It clears even with purpose 4 also changed. Two controls: a current section that still shows the sale opt-out does not clear, and a stored `fp` with no US opt-out bit never clears by GPP.
- `regrant_clear_empties_ov`: after the clear flushes, `ps.ov` is empty and the browser UID it held goes out again.
- `owner_valid_value_beats_browser_value`
- `owner_valid_set_replaces_every_repeated_browser_entry`: the browser sends the owned source twice, and the owner's stored set has two entries with three UIDs and EID-level fields. Exactly the owner's two entries go out, in stored order, with every field written out.
- `targeted_opt_out_blocks_server_ids_flush_and_mirror` (spec 13): a targeted-advertising opt-out without a sale opt-out gives no server-side IDs in `user.eids`, no flush of that provider's entries, and none in the `ts-eids-s` mirror payload
- `browser_seeded_value_never_beats_browser_current_value`
- `expiring_owned_source_never_serves_browser_seeded_value`
- `unowned_source_keeps_the_browser_first_rule` (control)
- `admin_clears_provider_opt_out`, `admin_opt_out_route_is_in_admin_endpoints`, and, per adapter, a test that the `DELETE` path routes (Fastly) or returns the not-available response (the others)

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- regrant`, `cargo test-fastly -- owner`, `cargo test-fastly -- targeted_opt_out`, `cargo test-fastly -- browser_`, `cargo test-fastly -- ec::admin`, `cargo test-fastly -- admin`, `cargo test-axum`, `cargo test-cloudflare` and `cargo test-spin`
Expected: FAIL, then PASS. The merge tests compare whole `Vec<Eid>` values with every field written out, with no `..Default::default()`. Task 28's `merge_` and `denial_` tests and the Part 1 `golden_` tests for browser entries must pass unchanged.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Apply owner precedence and clear provider opt-outs on re-grant"
```

### Task 37: Hold mode on the writer navigation

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (before the navigation auction dispatch, about lines 4886-4889)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (`run_hold`, `HoldResults`)
- Modify: `crates/trusted-server-core/src/ec/identity/mutation.rs` (tag hold outcomes for `retain_for_error_status`, Task 29)
- Test: `coordinator.rs`, `publisher.rs`, `mutation.rs`

**Interfaces:**

- Consumes: `IdentityDeps` (Task 21), `eligibility` with `Phase::Hold` and `outcome_to_changes` (Task 20), `dispatch` with a budget (Task 21), the `ServerSet` (Task 28), and `IdentityMutation::retain_for_error_status` (Task 29)
- Produces:
  - `pub(crate) struct HoldResults { additions: BTreeMap<SourceDomain, Vec<KvEid>>, writer: BTreeMap<SourceDomain, IdentityProviderId> }`: the answers this auction's server set adds, per source, with the provider that wrote each, so Task 28's output rules can apply that provider's provenance default. `ServerSet::with_hold_results(&mut self, results: HoldResults)` overlays them, replacing the stored value for each source they name.
  - `pub(crate) async fn run_hold(deps: &IdentityDeps<'_>, context: &mut EcContext) -> HoldResults`, awaited by `hold_before_auction` in `handle_publisher_request` (see `dispatch`, Task 21, for why it must not block). On a returning writer navigation that runs an auction, it dispatches every provider `eligibility(.., Phase::Hold)` passes on the phase 1 view, within `hold_budget_ms` measured from the first dispatch, each waiting `min(hold_timeout_ms, remaining budget)` (spec 4.7). The phase 1 read already confirmed a live, consenting row, so no EC ID is disclosed before consent is confirmed.
    - Each outcome becomes changes through `outcome_to_changes` and joins the writer's mutation, stateful providers included. No answer goes into a cookie (spec 4.7, "Results").
    - Each `found` set also feeds this auction through `HoldResults`: one `Eid` per entry, in the order the provider returned it, after the browser entries, with the provider's provenance default applied (Task 28).
    - A cut-off call is `HoldCutOff`: `n = now + hold_retry_sec`, and no failure is counted.
  - The mutation tags each change and state change with whether it came from a `found` outcome, so `retain_for_error_status` (Task 29) keeps a hold call's removals, opt-outs and the `ps.n`/`ps.f` of every outcome other than `found`, and drops its positive answers, when the origin answers with a status other than 2xx or 304 (spec 5.1).
- Nothing else runs hold mode. `/auction`, `/_ts/page-bids` and every other stager run no resolving provider (spec 5.3). A hold-mode provider on a writer navigation without an auction runs after send in phase 4 (Task 20's `Phase::AfterSend` rule, Task 29), with `after_send_timeout_ms`.
- The navigation takes `IdentityDeps` inside its existing deps or context struct, so it passes no more than 7 arguments.

- [ ] **Step 1: Write the failing tests**

With the fake HTTP client's latency control. Every test name contains `hold`:

- `auction_starts_at_the_hold_budget_when_a_provider_is_slow` (the elapsed fake time equals the budget, and the auction ran)
- `hold_answer_feeds_this_auction_and_joins_the_mutation`
- `full_set_hold_answer_is_sent_in_order_and_flushed_whole`: two entries, three UIDs, no EID-level fields on the second entry and `inserter` on the first. `user.ext.eids` carries both entries in order. The first entry goes out exactly as returned. The second, which sets none of `inserter`, `matcher` or `mm`, gets the provider's provenance default. The one flush stores the set as returned, without the default.
- `hold_sets_no_cookie`: no `Set-Cookie` beyond today's on the navigation response.
- `stateful_hold_answer_flushes_its_state_with_its_ids`
- `cut_off_hold_call_sets_hold_retry_without_counting_a_failure`
- `no_hold_on_a_stager`: `/auction` and `/_ts/page-bids` for the same visitor, with the hold provider due, make no provider call.
- `hold_provider_moves_after_send_on_a_navigation_without_an_auction`: dispatched in phase 4 with `after_send_timeout_ms`, and a miss is a transport failure, not a cut-off.
- `writer_navigation_with_non_2xx_status_flushes_hold_removals_and_backoff_only`: a 404 navigation whose hold calls returned one `found`, one `no_match` and one `invalidate`. The one write holds the `no_match` `ps.n` and the removal, and no ID from the `found`.
- `hold_no_match_on_a_writer_stops_recalls_from_the_spa_session`: after the writer's flush, a run of `/auction` and `/_ts/page-bids` calls never calls the provider before `ps.n`.
- `hold_kv_opt_out_blocks_dispatch`

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- hold`
Expected: FAIL, then PASS. Use the platform's time source the auction orchestrator uses for its own timeouts. Do not use `std::time::Instant::now` (`scripts/lint-wasm-clock.sh` rejects it in shared crates).

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Run hold-mode identity providers before the writer navigation's auction" -m "Answers feed that auction and join the writer's single flush. Stagers run no provider, and a navigation without an auction runs hold-mode providers after send instead."
```

### Task 38: Revoke-only passive capture

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (`handle_proxy`, about lines 1031-1065)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (`run_capture`)
- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (`ec_finalize_response`: a requested global revoke takes the withdrawal branch)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (`finalize_send_and_flush` runs the pending removal retry after send)
- Modify: the identity measurement line of Task 31 (`capture_unpersisted`)
- Modify: `crates/trusted-server-core/src/integrations/lockr.rs` (log only the upstream path, about line 204)
- Test: `integrations/registry.rs`, `coordinator.rs`, `ec/finalize.rs`, `integrations/lockr.rs`, `adapter-fastly/src/main.rs`

**Interfaces:**

- Consumes: `ProxyDispatchInput.is_real_browser` (Task 23), `decode_body` (Task 21), `outcome_to_changes` (Task 20), `PartnerIdChange` and `ProviderStateChange` (Task 15), `write_removals`, `RemovalWriteOutcome` and `log_removal_lost` (Task 18), `RequestIdentity::pending_removals`, `record_removal_lost` and `record_unpersisted` (Task 23), `ProviderSettings.revoke_scope` (Task 19), `regrant_clears_opt_out` (Task 36), and `ExampleCaptureProvider` and `capture_registration` (Task 32)
- Produces:
  - `pub(crate) struct CaptureCall<'a> { method: &'a Method, path: &'a str, status: StatusCode, headers: &'a HeaderMap, body: &'a [u8] }`. One struct, so `run_capture` stays under 7 arguments. There is no request-body consent: the request body is never read (spec 4.7).
  - `pub(crate) fn run_capture(deps: &IdentityDeps<'_>, graph: Option<&KvIdentityGraph>, context: &mut EcContext, call: &CaptureCall<'_>)`
  - `EcContext::request_global_revoke(&mut self)` and `EcContext::global_revoke_requested(&self) -> bool`. `run_capture` sets it from any client. `global_revoke_requested` is `pub`: the Fastly adapter gives finalize the identity graph when it is set, whatever the client, as it does for a consent withdrawal.
  - `pub fn retry_pending_removals(context: &mut EcContext, graph: &KvIdentityGraph)`: after send, one more `write_removals` for a capture removal write that failed before send.

`handle_proxy` per request, only when some provider's `Captures::matches(method, path)`:

1. **After `handle`,** on 2xx only: buffer the response up to `max_response_bytes`. A larger body is passed through uncaptured. Decode a copy with `decode_body`, call `capture` per matching provider, and re-wrap the original bytes with `EdgeBody::from(bytes)` so the browser receives the response unchanged. The request body is never read or buffered.
2. **The real-browser gate covers clears only** (spec 4.7). Removals, opt-outs and global revokes apply whatever the client, as consent withdrawals do. A clear passes `passes_consent_steps`, which requires the request's `is_real_browser`, so a client that fails `looks_like_browser()` never clears an opt-out.
3. **Build the removal mutation** from any client. From each capture result, through `outcome_to_changes` on the request's KV view, keep only:
   - its removals: `OwnerRemove` for `invalidate` and `also_invalidate`, and the opt-out's `WriterRemove`s, including a capture-only provider's browser records of its sources (Task 20). The hashes are derived when the mutation applies (Task 15).
   - its opt-out: `ps.o = {at: now, fp}` and `st = Clear`
   - a clear: when the result has positive IDs, eligibility steps 1 to 3 pass under the request's consent, and the provider has a sticky `ps.o` that only a capture can clear (a capture-only provider without a GVL ID, spec 8): `opt_out_clear = Some(now)`
   - nothing else. Capture never touches `n`, `f` or the breaker. A state change in a result is ignored with a warning. Each positive source is counted in `capture_unpersisted` (`record_unpersisted(provider, source)`) and never stored.
4. **The capture removal write** (spec 5.3), before the proxied response is sent: `write_removals(graph, ec_id, &mutation, &context)` (Task 18). It re-reads, applies the removal rules of section 5.4, and writes with a CAS, up to five attempts, retrying only on a generation mismatch. `NothingToWrite` covers a tombstone, a missing row and a removal already applied. On `Failed`, the mutation is kept in `RequestIdentity::pending_removals`, and `finalize_send_and_flush` calls `retry_pending_removals` once after send. When that fails too, `log_removal_lost` logs at error with the provider ID and source only, and `record_removal_lost` counts each removal. Without a graph, there is nothing to write.
5. **On an integration-proxy writer navigation** the capture still goes through the capture removal write, never into the writer's mutation (spec 5.2 phase 3). The writer's own phase 4 flush then fails its CAS if the capture write landed first, which is the existing rule (spec 5.2 phase 4 step 2), and its removals are retried alone.
6. **Revoke scope** (spec 8). A captured `opted_out` with `revoke_scope = "provider"` (the default) is the provider opt-out above. With `"global"`, from any client, `run_capture` calls `context.request_global_revoke()` and writes nothing else for that result. `ec_finalize_response` treats a requested global revoke exactly like `ec_consent_withdrawn`: the row is tombstoned through `tombstone_existing_from_snapshot`, the EC cookie is expired, the request's mutation is dropped, and a failed tombstone gets Task 27's after-send run. This is the withdrawal's existing page-load exception, not a new one.
7. **Lockr proxy logging** (spec 9.3). In `integrations/lockr.rs`, log only the upstream path: `log::info!("Forwarding to Lockr API path: {target_path}")`. The query string can carry identifiers and never reaches a log.

Add `capture_unpersisted` (per provider and source, counts only) to Task 31's measurement line.

- [ ] **Step 1: Write the failing tests**

Using `capture_registration` and `ExampleCaptureProvider` (Task 32), and `KvIdentityGraph::write_counting` (Task 22). Every test name contains `capture`:

- `capture_runs_only_for_matching_rules`
- `capture_decodes_gzip_and_brotli_responses_and_forwards_them_unchanged` (byte-equal body to the browser)
- `capture_never_reads_the_request_body` (a request body with an email field: the body reaches the upstream unchanged, the provider sees none, and a log capture shows no email)
- `capture_stores_no_positive_id_and_counts_it`: a capture returning two sources stores nothing for them, and `capture_unpersisted` holds one count for each, with no value.
- `capture_removal_is_written_before_send`: an `also_invalidate` from a real browser gives one CAS write before the response is sent, and the row's `ps.iv` holds the record's hashes.
- `capture_removal_applies_whatever_the_consent`: the same capture under denied consent writes the same removal.
- `capture_removal_write_retries_only_on_generation_mismatch` (a store that fails the CAS twice then succeeds: 3 attempts; a non-CAS error: 1 attempt)
- `failed_capture_removal_write_runs_once_more_after_send`, and `capture_removal_lost_after_both_attempts_is_counted_and_logged_without_a_value`
- `capture_removal_write_skips_a_tombstone_a_missing_row_and_an_applied_removal` (0 writes each)
- `capture_opt_out_removes_every_record_the_provider_wrote`
- `capture_only_opt_out_also_removes_browser_records_of_its_sources`
- `capture_only_provider_clears_its_opt_out_on_a_consenting_positive_result`: a later 2xx capture with IDs writes `ps.oc` set to the `at` of the opt-out it read (its watermark) and no ID. Two controls: the same capture under denied consent writes no clear, and a provider with a GVL ID never clears by capture.
- `capture_global_revoke_from_a_real_browser_tombstones_the_row_and_clears_the_ec_cookie`: `revoke_scope = "global"`. Exactly one tombstone write and the EC cookie's expiry header. The control with `"provider"` removes only that provider's records and keeps the row live.
- `capture_from_a_non_browser_applies_its_removals_and_opt_out`: a client that fails the check has its invalidation and opt-out written, and a `"global"` revoke requests the tombstone.
- `capture_from_a_non_browser_never_clears_an_opt_out`: a positive result from such a client writes nothing and still counts the ID in `capture_unpersisted`.
- `capture_with_replayed_ec_cookie_changes_no_kv_row`: a non-browser request with a copied `ts-ec` and a revoke-style response. The counting store records zero writes.
- `capture_on_a_proxy_writer_navigation_uses_the_removal_write_not_the_mutation`
- `capture_never_touches_n_f_or_the_breaker`
- `capture_state_change_is_ignored`
- `capture_skips_an_oversized_proxied_body`
- In `integrations/lockr.rs`: `lockr_proxy_log_omits_query`, using a log capture, with a request whose query holds an identifier

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- capture`, `cargo test-fastly -- lockr_proxy_log` and `cargo test-fastly -- ec::finalize`
Expected: FAIL, then PASS. Today's withdrawal tests in `ec/finalize.rs` must pass unchanged.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Capture identity revocations from proxied provider responses" -m "Capture is revoke-only: a real browser's captured removals, opt-outs and clears are written to KV on the proxied request with a CAS loop, retried once after send, and counted as lost if both fail. Positive IDs are counted, never stored. Removals and opt-outs apply from any client; only clears need a real browser. revoke_scope = \"global\" runs the consent-withdrawal tombstone from any client." -m "The Lockr proxy now logs only the upstream path, not its query."
```

### Task 39: The full provider harness and conformance checks

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/examples.rs` (created in Task 32): the harness tests
- Test: `examples.rs`

**Interfaces:**

- Produces everything in spec 13's harness listing: `ProviderHarness::{with_setup, with_client_ip, with_user_agent, with_page_url, with_region, with_hashed_email, with_consent, with_stored, with_state, with_now, with_random_seed, request, request_json, respond, respond_raw, capture, resolve, state_of, output_eids, check_conformance, check_conformance_with_state, try_check_conformance, try_check_conformance_with_state}`, `HarnessSetup`, `TestConsent`, `CaptureFixture`, `ConformanceCases`, `ConformanceFailure`, `StoredId::{for_test, with_eids, written_by, expiring_at}`, `HashedEmail::for_test`, and `check_descriptor_ignores_secrets`. The module and every item are `pub` under `#[cfg(any(test, feature = "test-utils"))]` (it has been since Task 14), and reachable from outside the crate as `trusted_server_core::ec::identity::test_support::*`.
  - `StoredId::for_test` builds a one-entry, one-UID `KvPartnerId` with `KvPartnerId::new` and goes through `StoredId::from_record(source, &record)` (Task 9), so `eids()` holds one entry and `uid()` is its first UID.
  - `StoredId::with_eids(self, eids: Vec<Eid>) -> Self` replaces the set with a full one, and sets `uid()` to its first UID (spec 13), so a test can give a refreshing provider a stored full set.
  - `HashedEmail::for_test(bytes: [u8; 32]) -> HashedEmail` calls Task 9's gated `HashedEmail::from_bytes`. There is one constructor, so the two cannot drift.
  - `ProviderHarness::output_eids(&self, resolution: &Resolution) -> Vec<Eid>` applies `entry_sets` (Task 10) and the descriptor's provenance default (Task 28) as the bidstream would. Provenance is applied only here; `Resolution`'s getters show what the provider set.
  - `#[derive(Debug, derive_more::Display)] #[non_exhaustive] pub enum ConformanceFailure`, one variant per conformance bullet below (`UndeclaredInput`, `UndeclaredHost`, `CookieHeader`, `UndeclaredSource`, `LimitHit`, `NotImplemented`, `StatelessState`, `CaptureStateChange`, `MissingExpiry`, `BareStringStateAccepted`, and the rest), each naming what failed and never a value. It implements `core::error::Error`.
  - `try_check_conformance(&self, cases: &ConformanceCases) -> Result<(), Report<ConformanceFailure>>` and `try_check_conformance_with_state::<S>`. `check_conformance` and `check_conformance_with_state` call them and `expect("should conform: ...")`, for provider authors. `cargo test-fastly` runs on `wasm32-wasip1`, where a panic aborts the whole test binary and `catch_unwind` never returns `Err`, so the harness's own negative tests use the `try_` forms. No `panic!` anywhere in this code: the workspace denies `clippy::panic`.

`try_check_conformance` checks every bullet of spec 13's conformance list:

- inputs hold only what the descriptor declares
- a resolver returns `Some` for at least one listed setup, and its `response` never returns the `not_implemented` marker
- a capturer returns a non-`no_match` result for at least one fixture, and never the marker
- no undeclared input or state read is recorded
- no state is returned without `with_private_state()`
- requests go only to declared hosts, over HTTPS, with no `Cookie` header and with valid headers
- entries are only for declared sources, and no fixture's result hits a limit of spec 4.5: no dropped UID or field, no discarded entry, and no set over `MAX_ENTRY_SET_BYTES`. The harness runs `entry_sets` (Task 10) on each result and fails when `drops` or `oversized` is not empty.
- `ResolvedEid::expiry()` and `refresh()` report what was set, and a fixture set of an expiring source carries an expiry
- `capture` never returns a state change
- a bare-JSON-string stored state is never treated as valid

`try_check_conformance` returns an error with a pointer to `try_check_conformance_with_state` for a stateful provider. `try_check_conformance_with_state::<S>` also round-trips `S` through `replacing_state` and `input.state()`.

- [ ] **Step 1: Write the tests (the spec's examples are the first harness cases)**

In `examples.rs`, next to the Task 32 copies of spec 4.6:

```rust
    #[test]
    fn example_provider_round_trip() {
        let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0");

        let request = harness
            .request()
            .expect("should build a request")
            .expect("should not skip the request");
        assert_eq!(request.url().host_str(), Some("api.exampleid.example.com"), "should call the declared host");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "uid": "example-uid", "signature": "example-signature" }))
            .expect("should parse the response");
        assert_eq!(
            resolution.eids()[0].uids()[0].id(),
            "example-uid",
            "should return the uid"
        );
        let state: ExampleIdState = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(state.signature, "example-signature", "should keep the signature");
    }

    #[test]
    fn example_provider_conforms() {
        let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
        ProviderHarness::new(&provider).check_conformance_with_state::<ExampleIdState>(
            &ConformanceCases::default().with_resolve_case(
                HarnessSetup::default().with_client_ip("192.0.2.10").with_user_agent("ExampleBrowser/1.0"),
                StatusCode::OK,
                &json!({ "uid": "example-uid", "signature": "s" }),
            ),
        );
    }

    #[test]
    fn conformance_catches_an_undeclared_read() {
        let provider = UndeclaredUserAgentReader::new();
        let error = ProviderHarness::new(&provider)
            .try_check_conformance(
                &ConformanceCases::default().with_resolve_case(HarnessSetup::default(), StatusCode::OK, &json!({})),
            )
            .expect_err("should fail a provider that reads an undeclared input through .ok()");
        assert!(
            matches!(error.current_context(), ConformanceFailure::UndeclaredInput { .. }),
            "should report the undeclared input"
        );
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&example_config(), &example_config());
    }

    #[test]
    fn output_eids_applies_the_provenance_default_only_to_bare_entries() {
        let provider = ProvenanceTestProvider::new();
        let harness = ProviderHarness::new(&provider);
        let resolution = harness
            .resolve(&HarnessSetup::default())
            .expect("should resolve")
            .expect("should not skip the request");
        let eids = harness.output_eids(&resolution);
        assert_eq!(eids[0].inserter.as_deref(), Some("vendor-inserter"), "should keep the entry's own fields");
        assert_eq!(eids[0].matcher, None, "should not mix the default into an entry that sets a field");
        assert_eq!(eids[1].inserter.as_deref(), Some("default-inserter"), "should apply the default to a bare entry");
    }
```

Add negative conformance tests the same way, each with a tiny test provider in the test module and each asserting `try_check_conformance(...)` returns the matching `ConformanceFailure` variant: a `Cookie` header, a request to an undeclared host, an entry for an undeclared source, a fixture result that hits a spec 4.5 limit (a 33rd UID for one source, and a blank `inserter`), a capability declared but not overridden (the `not_implemented` marker), a stateless provider returning state, a capture returning a state change, a stateful provider that accepts a bare-JSON-string stored state, and an expiring source without an expiry.

Several entries and UIDs for one source are valid (G13), so add a positive control too: `conformance_accepts_a_complete_set_within_the_limits`. A test provider returns two entries for one source, three UIDs in all, with `inserter`, `matcher`, `mm`, an EID-level `ext` and a UID-level `ext`, and `try_check_conformance` returns `Ok`. Add `stored_full_set_reaches_a_refreshing_provider`: `with_stored(StoredId::for_test(..).with_eids(..))` gives the provider `stored(source).eids()` equal to the set.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- ec::identity::examples`
Expected: FAIL, then PASS.

- [ ] **Step 3: Check the feature gate from outside the crate**

Run: `cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1 --tests` (the Fastly adapter's dev-dependency already enables `trusted-server-core/test-utils`, `crates/trusted-server-adapter-fastly/Cargo.toml:45`), then `cargo clippy-fastly`
Expected: both pass. This proves a provider crate can use the harness, and that the `pub` gated items raise no `dead_code` warning.

- [ ] **Step 4: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Add the provider harness and conformance checks"
```

### Task 39b: Complete provider EIDs from a registered provider, end to end

Part 1 built and tested multi-entry records at unit level and in the invariant tests (Tasks 6, 10, 15, 17, 28 and 30). This task proves that a provider registered through `try_with_identity_provider` gets a complete set from its response to KV, the mirror payload and the bidstream, and that nothing that predates the spec moved.

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (`MultiEntryTestProvider`, `pub` under `test-utils`)
- Test: the Fastly adapter's request-level test module from Task 30 (`crates/trusted-server-adapter-fastly/src/main.rs` tests), using `drive_request_for_test` (Task 29)

**Interfaces:**

- Consumes: Tasks 32, 37, 38, the counting store (Task 22), and Task 29's `drive_request_for_test` and Task 30's request-level helpers
- Produces: no production code. `MultiEntryTestProvider`, a test-only resolver for `exampleid.example.com` that returns two entries: the first with UIDs `uid-a` (`atype: 1`, a UID-level `ext`) and `uid-b`, plus `inserter`, `matcher` and `mm`; the second with `uid-c` and an EID-level `ext`, and no `inserter`, `matcher` or `mm`. Its descriptor declares `with_provenance` with an `inserter` and `matcher` default for the source. `MultiEntryTestProvider::stateful()` is the same provider declared `with_private_state()`, for the stateful case. It lives in core `test_support` because the adapter tests cannot see core's `#[cfg(test)]` items.

- [ ] **Step 1: Write the failing tests**

Each test registers `MultiEntryTestProvider` through `IntegrationRegistration::builder("exampleid").try_with_identity_provider(...)`, owning the source, and drives requests through `drive_request_for_test`:

- `registered_provider_full_set_reaches_kv_mirror_and_bidstream_in_order`: a writer navigation in hold mode. The flush writes the record with `eids` holding both entries as returned, `w = exampleid` and a fresh `rv`. `user.ext.eids` and `x-ts-eids` carry both entries in the order returned, after the browser entries. The first entry keeps exactly its own `inserter`, `matcher` and `mm`. The second, which sets none of them, gets the provenance default. The next `/auction`'s `ext.ts_eids_s` decodes to the same two entries, defaults included.
- `one_entry_one_uid_result_is_written_as_one_entry`: the same provider configured to return only `uid-a` with no EID-level fields. The KV record is `{w, rv, at, eids: [{uids: [{id: "uid-a", atype: 1, ext}]}]}`, the one shape of spec 6.1.
- `later_result_without_an_entry_removes_it`: a refresh that returns only the first entry leaves exactly that entry in KV and in the bidstream. A source the result does not name keeps its record.
- `stateful_result_with_an_oversized_set_keeps_only_its_removals`: a set over `MAX_ENTRY_SET_BYTES` from `MultiEntryTestProvider::stateful()`. The positive part, its state included, is rejected, and its `also_invalidate` still applies.
- `identify_returns_the_first_uid_of_a_full_record`: `/_ts/api/v1/identify` returns `uid: "uid-a"` and an `eid` with the partner's `openrtb_atype`, the same shape as for a one-UID record.
- `rollback_to_part_1_config_keeps_the_owners_opt_out`: a row with the provider's `ps.o`, `ps.ov` and a multi-entry record, flushed once by a build whose catalog no longer has the provider. The provider's `ps` keeps `o`, `oc` and `ov` and loses `n`, `f`, `iv` and `st`. The record stays readable but is not served (spec 14). There is no test of a rollback past Part 1: a release before it cannot read schema version 2, and that rollback is unsupported.
- `captured_revocation_writes_with_cas`, `global_revoke_from_real_browser_writes_as_withdrawal` and `non_browser_capture_writes_nothing`: the capture rows of spec 13's one-write table that Task 30 leaves to this task, driven through a capture registration (Task 32). The first gives one CAS write before send (up to five attempts), and one more after send only when it failed.
- `returning_navigation_answered_404_flushes_only_hold_removals_and_backoff`: the hold-mode half of the 3xx, 4xx and 5xx row.

Then rerun Part 1's `golden_` tests for paths that predate the spec with the multi-entry provider registered on another source: browser ingest writes one-UID records, `ts-eids` content is unchanged, `/auction` body forwarding is unchanged, and a one-UID row goes out as on `main`. Do not edit those tests. If one fails, the change is wrong, not the test.

Compare whole `Vec<Eid>` values and whole `KvPartnerId` values with every field written out, with no `..Default::default()`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- registered_provider`, `cargo test-fastly -- one_entry_one_uid`, `cargo test-fastly -- later_result`, `cargo test-fastly -- stateful_result`, `cargo test-fastly -- identify_returns`, `cargo test-fastly -- rollback_to_part_1`, `cargo test-fastly -- captured_revocation`, `cargo test-fastly -- global_revoke_from_real_browser`, `cargo test-fastly -- returning_navigation_answered_404`, `cargo test-fastly -- non_browser_capture` and `cargo test-fastly -- golden_`
Expected: the new tests FAIL until `MultiEntryTestProvider` exists, then PASS. A failure after that points at a gap in Tasks 32 to 38, not here. Fix it in the task that owns the code, and note the fix in this task's commit message.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Prove complete provider EIDs end to end through a registered provider"
```

### Task 40: Batch sync never replaces an owned value

**Files:**

- Modify: `crates/trusted-server-core/src/ec/batch_sync.rs` (`handle_batch_sync` at about line 114, the upsert around line 247)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`run_batch_sync`, about line 737, passes `&state.identity`)
- Test: `ec/batch_sync.rs`

**Interfaces:**

- Produces: `pub fn handle_batch_sync(kv: &KvIdentityGraph, registry: &PartnerRegistry, rate_limiter: &dyn RateLimiter, identity: &IdentityRuntime, req: Request<EdgeBody>) -> Result<Response<EdgeBody>, Report<TrustedServerError>>`. `IdentityRuntime` is `pub` (Task 23), so the adapter can pass it. The handler reads the owned sources from its catalog.

- [ ] **Step 1: Write the failing test**

`batch_sync_update_to_owned_source_applies_only_when_absent`: with the source owned by a provider, a batch update applies when the source is absent, and is `Unchanged` (no write) when a value exists. A control update to an unowned source keeps today's behaviour.

`batch_sync_never_touches_an_owners_entry_set`: the owner's record holds two entries. A batch update to the source is `Unchanged`, and the row's `eids` are byte-equal before and after. A batch write that does apply (an absent owned source, or an unowned source) writes a one-entry, one-UID record without `w` or `rv`, as Task 6's `KvPartnerId::new` builds it.

- [ ] **Step 2: Run, see it fail, implement, see it pass**

Run: `cargo test-fastly -- ec::batch_sync` and `cargo test-fastly -- batch_sync`
Expected: FAIL, then PASS. Existing batch sync tests pass an `IdentityRuntime` with an empty catalog and are otherwise unchanged.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Keep batch sync from replacing an owned source's value"
```

### Task 41: Docs, the provider author guide, and final verification

**Files:**

- Create: `docs/guide/identity-providers.md` (provider author guide)
- Modify: `docs/guide/configuration.md` (`[[ec.partners]].owner`, per-provider tables, `revoke_scope`), `docs/guide/edge-cookies.md` (the capture provider checklist, complete provider EIDs, rollback), `docs/.vitepress` sidebar config (find it with `grep -rn "edge-cookies" docs/.vitepress`), `CHANGELOG.md`, `trusted-server.example.toml`

- [ ] **Step 1: Write the author guide**

Base it on spec 4.2 to 4.6 and 13, using only the fictional `exampleid` provider:

- what a provider is
- the descriptor
- declaring inputs and reading them with `?` or `.ok()`
- building requests
- parsing responses
- returning a `Resolution` of `ResolvedEid` entries, each with one or more `ResolvedUid`s: several entries and UIDs per source, UID-level `atype` and `ext`, and EID-level `inserter`, `matcher`, `mm` and `ext`
- the limits of spec 4.5: the browser parser's field rules, plus per-source caps the browser parser does not have (8 entries and 32 UIDs per source, 4,096 bytes per set). Each `ext` may hold 2,048 bytes and each `inserter` or `matcher` 255 bytes, but the field caps do not guarantee the set cap: a set whose legal fields exceed 4,096 bytes is discarded whole, never trimmed. A discarded set leaves the stored set as it is, and a result all of whose sets are discarded counts as malformed.
- that a result replaces an owned source's whole set, while for an unowned source it only fills an absent or expired record
- `with_provenance`: a default applied at output only to an entry that sets none of `inserter`, `matcher` and `mm`. An entry that sets any of them is the vendor's complete statement. A blank provenance `inserter` or `matcher` fails `build()`.
- what vendor `ext` data means for privacy: it is stored only in KV, never in `ts-eids`, and reaches bidders, `x-ts-eids` and the `ts-eids-s` mirror under the same consent gates as the UIDs (spec 9.3). The mirror is readable by every page script. A vendor spec that stores such data says what it holds.
- private state
- capture: revoke-only, so a capture's removals, opt-out and clear are written to KV and its positive IDs are only counted in `capture_unpersisted`; `revoke_scope`; the real-browser gate on capture clears; and that a capture-only provider must list its `sources` and cannot own one
- registration with `try_with_identity_provider`
- testing with `ProviderHarness`, `check_conformance` and `output_eids`
- what the coordinator does for you (consent, transport, backoff, KV)

Link to the spec for the protocol.

- [ ] **Step 2: Config and changelog**

Document `owner`, `mode`, `timeout_ms`, `after_send_timeout_ms`, `sources` narrowing, `refresh_after_sec`, `max_calls_per_minute`, `breaker_failures_per_minute`, `cookie_priority` and `revoke_scope`, each with its default and a validation error example. Add the commented `[ec.identity.providers.<id>]` capture keys to `trusted-server.example.toml`, as spec 10.1 shows them, with `revoke_scope = "provider"`.

In `docs/guide/edge-cookies.md`:

- Add the capture provider checklist from spec 14 as the steps an operator follows before enabling a capture provider:
  - each captured source has an `[[ec.partners]]` entry with `bidstream_enabled = true` and the right `openrtb_atype`
  - a source moving to an owner comes off pull sync first
  - the provider's `sources` list holds only the sources it is the delivery path for
  - the live acceptance check through `/_ts/admin/eids` and `/_ts/admin/ec`, including that `removal_lost` stays at zero
- Say what `revoke_scope = "global"` does (a captured opt-out from a real browser runs the consent-withdrawal tombstone), and that a non-browser client's capture never changes the KV row.
- Add a short section on complete provider EIDs: what a multi-entry record looks like in the admin lookup, and that `/identify` still returns one UID per source.
- Add the rollback steps of spec 14. Before rolling back to Part 1, remove every `owner` and vendor `[ec.identity.providers.*]` table, because Part 1's validation rejects them. After the rollback, multi-entry records stay readable but are not served while their writer is unconfigured, and browser ingest may replace them. Each provider's `ps.o`, `ps.oc` and `ps.ov` are kept, and the rest of its `ps` is pruned. Rolling back past Part 1 is unsupported, because a release before it cannot read schema version 2.

In `CHANGELOG.md` under `### Added`, add the provider framework, ownership, hold mode, revoke-only passive capture (with `revoke_scope` and the capture removal write), complete provider EIDs, the `capture_unpersisted` and `removal_lost` counters and the breaker. Under `### Changed`, add stricter consent decoding for identity providers, and that the Lockr proxy logs only the upstream path.

Run the docs and markdown format commands.

- [ ] **Step 3: Commit**

```bash
git add docs/ CHANGELOG.md trusted-server.example.toml
git commit --signoff -S -m "Document the identity provider framework for operators and authors"
```

- [ ] **Step 4: Full verification**

Run `/check-ci`, then `cargo test-fastly-reuse`, `./scripts/test-cli.sh`, the parity crate, `scripts/lint-wasm-clock.sh`, and `cargo doc --no-deps --all-features -p trusted-server-core`. Every one must pass. Then use the `verify-app` agent for an independent pass.

- [ ] **Step 5: PR**

Open the PR against `feat/1246-identity-single-write` with `pr-creator`. Title: "Add server-side identity providers". The description lists spec 14 step 2's contents, says no vendor ships in it, says that capture is revoke-only and that each capture provider is enabled only after its checklist has passed on a live service, and names the follow-up vendor specs (ID5, LiveRamp, Lockr, UID2).

---

## Spec coverage

| Spec section                                                                            | Tasks                                                                                          |
| --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| 2.1 G1, G2, G11 (trait, coordinator, ergonomics)                                        | 8 to 11, 20, 21, 39                                                                            |
| 2.1 G3 (hold or after-send)                                                             | 19, 20, 29, 37                                                                                 |
| 2.1 G4 (one write; withdrawal, removal-retry and capture-removal exceptions)            | 18, 25, 26, 27, 29, 30, 38, 39b                                                                |
| 2.1 G5 (KV, mirrored in `ts-eids-s`)                                                    | 17, 26, 28                                                                                     |
| 2.1 G6 (ownership)                                                                      | 33, 36                                                                                         |
| 2.1 G7 (expiry, refresh)                                                                | 14, 15, 20, 36                                                                                 |
| 2.1 G8 (pull sync on the trait)                                                         | 21 (hourly rotation), 22, 29                                                                   |
| 2.1 G9 (HEM slot)                                                                       | 9 (always empty in v1)                                                                         |
| 2.1 G10 (no lost EIDs; registered-only merge)                                           | 25, 26, 30 (including an `ortb2`-only EID)                                                     |
| 2.1 G12 (removals win)                                                                  | 15, 18, 27, 28 (denials), 29 (removal-only retry), 36, 38                                      |
| 2.1 G13 (complete provider EIDs, nothing that predates the spec changes)                | 6, 7, 8, 9, 10, 13, 14, 15, 17, 22, 24, 25, 26, 28, 30, 36, 37, 39, 39b, 40                    |
| 3.4 platform facts (status capture, real-browser gate, clock failure)                   | 14, 23, 29, 38                                                                                 |
| 4.1 file layout                                                                         | 5 and each task's Files list                                                                   |
| 4.2 trait and additivity rules                                                          | 11, 39                                                                                         |
| 4.3 descriptor, provenance default                                                      | 8, 28, 32 (capture-path check), 39 (`output_eids`)                                             |
| 4.4 inputs, per-kind availability                                                       | 9, 10 (`CaptureInput`), 23 (phase-1 captures)                                                  |
| 4.5 requests, responses, results, entry-set limits, discarded sets                      | 10, 20, 39                                                                                     |
| 4.6 example provider and registration                                                   | 32, 39, 39b                                                                                    |
| 4.7 eligibility, dispatch, hold timing, breaker, revoke-only capture, real-browser gate | 20, 21, 35, 37, 38                                                                             |
| 5.1 writer and stagers, status rule, withdrawal exception and its after-send run        | 23, 25, 27, 29, 38, 39b                                                                        |
| 5.2 phases 1 to 4, phase 1 read on every writer, removal-only retry                     | 23, 25, 29                                                                                     |
| 5.3 stagers, the capture removal write, the mirror payload, registered-only merge       | 18, 25, 26, 38                                                                                 |
| 5.4 `IdentityMutation`, whole-set writes, `iv` and `ov` at apply, `ps` pruning          | 15                                                                                             |
| 5.5 orphans, collisions, deletion                                                       | 25, 30 (the deletion framework itself is deferred; its tombstone rule is a note for that work) |
| 5.6 rules that keep removals winning                                                    | 15, 18, 26, 29, 38                                                                             |
| 6.1 KV schema version 2, version 1 read, `ps`, admin redaction                          | 6, 13, 14, 39b                                                                                 |
| 6.2 the `ts-eids-s` mirror, its payload, cap and strips                                 | 1 to 4, 17, 26                                                                                 |
| 6.3 read order                                                                          | 26, 29                                                                                         |
| 7.1 merge, entry-set output, provider denials, servability, `/identify` shape           | 14, 24, 28, 36                                                                                 |
| 7.2 ownership semantics, whole-set replacement, batch sync                              | 33, 36, 40                                                                                     |
| 8 errors and outcomes, sticky opt-outs, re-grants, revoke scope, plain fingerprint      | 15, 20, 34, 36, 38                                                                             |
| 9.1 consent, header-only GPP                                                            | 12, 34                                                                                         |
| 9.2 outbound requests                                                                   | 1 to 3, 21                                                                                     |
| 9.3 logging, redaction, vendor `ext`, Lockr path-only log                               | 9, 10 (`Resolution` `Debug`), 23 (`RequestIdentity` `Debug`), 34, 38, 41, every task's logging |
| 9.4 caching                                                                             | 4, 25 (no identity cookie on HTML responses)                                                   |
| 10.1 to 10.3 configuration, removed keys, validation, registration                      | 4, 19, 22 (catalog validator, removed partner keys), 32, 33                                    |
| 10.4 pull sync on the trait, pull-complete marker removed                               | 21 (hourly rotation), 22, 29                                                                   |
| 11 adapters                                                                             | 3 (strips on every adapter), 36 (admin route on every adapter); parity gate in 22 and 34       |
| 12 vendor examples                                                                      | Out of scope (spec 2.2). The fictional examples in 32, 39 and 39b prove the API                |
| 13 testing                                                                              | each task, 30, 39, 39b                                                                         |
| 14 rollout, rollback, capture checklist                                                 | Parts 1 and 2; 31 (metrics, rollback note), 39b, 41 (checklist, rollback)                      |
| 15 open questions                                                                       | Not implemented. Track as issues after Part 1 ships                                            |
