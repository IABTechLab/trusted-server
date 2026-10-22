# Server-side Identity Providers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Trusted Server call identity providers (ID5, LiveRamp ATS, Lockr, UID2 and generic pull partners) from the edge through one `IdentityProvider` trait and one coordinator. Every page load makes at most one browser-driven KV write. Provider IDs are cached first in a sealed first-party cookie (`__Host-ts-eids-s`) and then in KV, and removals always win.

**Architecture:** New code lives in `crates/trusted-server-core/src/ec/identity/`. Providers are pure. They describe themselves with a `ProviderDescriptor`, build `ProviderRequest`s and parse buffered `ProviderResponse`s into `Resolution`s. The `IdentityCoordinator` owns eligibility, consent, transport limits, backoff and turning results into changes. Each request is either the **writer** (a real-browser top-level document navigation) or a **stager**. The writer adds every KV change to one request-scoped `IdentityMutation` and applies it with one conditional write after the response is sent. Stagers never write KV. They save positive changes in `__Host-ts-eids-s` and removals in per-batch `__Host-ts-eids-r-<id>` cookies, and the next writer flushes them. Today's pull sync becomes `GenericPullProvider`.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core`, `trusted-server-adapter-fastly`, `error-stack`, `derive_more`, `serde`/`serde_json`, `hmac`/`sha2`, `base64`, `rand`, `flate2`/`brotli` (already dependencies), and a new `aes-gcm = "0.10"`. Fastly Compute (Viceroy for tests).

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md` (revision 11, branch `spec/1246-server-side-identity-providers`). Read it before starting. "Spec 5.4" means section 5.4 of that file. Where this plan and the spec disagree, the spec wins. Stop and report the disagreement instead of choosing one. The spec cites `file:line` at commit `c84a73da7`. Line numbers have drifted slightly since then, so find code by the function names given here, not by line number.

## How this plan is split

The spec's rollout (spec 14) has three steps, and each step can ship on its own. This plan has one **Part** per step, and each Part is one pull request. Each PR's branch is stacked on the previous one:

| Part | Spec step                | Branch                             | Base                                       | Tasks    |
| ---- | ------------------------ | ---------------------------------- | ------------------------------------------ | -------- |
| 0    | 0. Strips first          | `feat/1246-identity-cookie-strips` | `spec/1246-server-side-identity-providers` | 1 to 4   |
| 1    | 1. One write, no vendors | `feat/1246-identity-single-write`  | `feat/1246-identity-cookie-strips`         | 5 to 31  |
| 2    | 2. Providers             | `feat/1246-identity-providers`     | `feat/1246-identity-single-write`          | 32 to 41 |

Part 0 is the rollback floor (spec 14). Do not start Part 1 until Part 0's PR is open. Do not start Part 2 until Part 1's one-write invariant tests (Task 30) pass.

## Global Constraints

Copied from the spec and `AGENTS.md`. Every task implicitly includes these.

- Follow `AGENTS.md` for commands, style, error handling and CI gates. Bare `cargo test`, `cargo build` and `cargo check` fail at the workspace root by design. Use the aliases (`cargo test-fastly`, `cargo clippy-fastly`, and the others).
- Errors use `error-stack` `Report<E>` with `derive_more::Display` enums and `impl core::error::Error`. No `anyhow` and no `thiserror`. Use `expect("should ...")`, never `unwrap()`, in production and in tests.
- No local imports inside functions. No wildcard imports except `use super::*` in test modules. Comments go on their own line, above the code.
- Every public item has a doc comment, and every fallible public function has `# Errors`. Every builder method and getter is `#[must_use]`. Spec 4.2, "Conventions for the listings", gives the derive list for each type. Copy it exactly.
- Never log UIDs, envelopes, HEMs, provider state, consent strings, cookie values, or provider URLs with a query string. EC IDs go through `log_id`. Log provider errors with `current_context()` only, never `{:?}` of the report (spec 9.3).
- Use only example or fictional data in code, tests, comments and docs (`example.com` hosts). Public protocol source domains (`id5-sync.com`, `liveramp.com`, `uidapi.com`, `sharedid.org`) may appear where the spec uses them.
- **Fixed values from the spec. Do not change them:**
  - `__Host-ts-eids-s`: sealed value at most **2,048** bytes. `__Host-ts-eids-r-<8 hex>`: sealed value at most **512** bytes, normally at most **4** cookies. Both: `Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000`, with no `Domain`.
  - Seal: AES-256-GCM, value `v1.<nonce>.<ciphertext+tag>`, base64url without padding, 12-byte nonce, the full EC ID as associated data. Key labels: `trusted-server/ts-eids-s/key/v1` and `trusted-server/ts-eids-r/key/v1`, each derived with HMAC-SHA256 from `ec.passphrase`.
  - KV: `ext` at most **256** serialized bytes, `ps.st` at most **1,024** bytes of embedded JSON, at most **32** `ps` entries, at most **4** `ps.iv` hashes, `rv` is **16** hex digits (64 random bits). `SCHEMA_VERSION` stays **1**.
  - `ProviderRequest` call context: at most **1,024** bytes of JSON. `IdentityProviderId`: `^[a-z][a-z0-9_-]{0,62}$`.
  - `[ec.identity]` defaults: `max_concurrency = 4`, `hold_budget_ms = 100`, `hold_retry_sec = 300`, `max_response_bytes = 65536`, `pending_grace_sec = 300`, `staged_max_age_sec = 604800`, `kv_revalidate_sec = 3600`. Per-provider defaults: `mode = "after_send"`, `timeout_ms` = `hold_budget_ms` in hold mode and `1000` after send, `refresh_after_sec = 86400`, `max_calls_per_minute = 1`, `breaker_failures_per_minute = 50`, `cookie_priority = 0`.
  - Backoff (spec 8): a `no_match` waits `n = now + retry_in`, default 24 h and at most 7 d. An input-caused failure waits `min(1 h × 2^(f-1), 24 h)`. A transport, 429 or 5xx failure waits `min(5 min × 2^(f-1), 1 h)`. A cut-off hold call waits `hold_retry_sec`. A skip waits `refresh_after_sec`. A marked provider that was not dispatched gets `ps.n = max(ps.n, now + 60 s)`.
- **G4 is strict.** A page load makes at most one browser-driven KV write and never retries it. The only exceptions are the consent-withdrawal tombstone path (unchanged) and the removal overflow write (Task 27).
- **Removals win.** No filter (consent re-gate, staleness, ownership, `cf` mismatch, size cap) may drop a removal, an opt-out or an `iv` hash. Only the EC binding and, when present, the revision check apply to them (spec 5.6).
- **Keep `ts-eids` and `x-ts-eids`.** Do not remove either (product requirement).
- **Feature gate for test support.** The spec names a `test-support` feature. `trusted-server-core` already has a `test-utils` feature for the same purpose (`crates/trusted-server-core/Cargo.toml`). Reuse `test-utils` and gate on `#[cfg(any(test, feature = "test-utils"))]`. This is the only intended deviation from the spec. Mention it in the PR description.
- Test discipline:
  1. **Red first.** Run each new test against unchanged code and watch it fail for the expected reason before you implement.
  2. **Pair absence with presence.** Each "X is gone" assertion sits next to a "control Y is still there" assertion on the same value.
  3. Compare structured outputs whole (full `KvEntry`, full `serde_json::Value`). Do not use `..Default::default()` in exhaustiveness tests. List every field.
  4. Use `json!` for JSON fixtures, except where exact byte lengths matter.
- Commits: sentence case, imperative mood, no `fix:`/`feat:` prefixes, no bracketed tags, `git commit --signoff -S`, one `-m` flag per paragraph, no heredoc, no AI attribution lines.

## Test commands

| What                                    | Command                                                                                       |
| --------------------------------------- | --------------------------------------------------------------------------------------------- |
| Core and Fastly adapter (Viceroy)       | `cargo test-fastly`                                                                           |
| One core test by name                   | `cargo test-fastly -- <test_name_substring>`                                                  |
| Fastly adapter with reusable sandbox    | `cargo test-fastly-reuse`                                                                     |
| Other adapters (Part 0 strips, Task 31) | `cargo test-axum && cargo test-cloudflare && cargo test-spin`                                 |
| Lint, Fastly target                     | `cargo clippy-fastly`                                                                         |
| Docs                                    | `cargo doc --no-deps --all-features -p trusted-server-core`                                   |
| Parity crate                            | `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` |
| CLI (config validation, Task 33)        | `./scripts/test-cli.sh`                                                                       |
| Full gate before each PR                | `/check-ci`                                                                                   |

`cargo test-fastly -- <filter>` runs only the tests whose path contains `<filter>`. Every "Run" step below gives the exact filter.

## File map

| File                                                                                                                | Responsibility                                                                                                              | Tasks                      |
| ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | -------------------------- |
| `Cargo.toml` (workspace), `crates/trusted-server-core/Cargo.toml`                                                   | `aes-gcm` dependency                                                                                                        | 5                          |
| `crates/trusted-server-core/src/constants.rs`, `src/cookies.rs`                                                     | Cookie names, identity cookie lists, name-and-prefix strip                                                                  | 1                          |
| `crates/trusted-server-core/src/auction/routing.rs`, `src/integrations/prebid.rs`                                   | Strip identity cookies from Prebid Server requests                                                                          | 2                          |
| `crates/trusted-server-core/src/publisher.rs`                                                                       | Origin strip; `is_prefetch_request` move; page-bids stops ingest; navigation merge; hold mode                               | 3, 23, 26, 28, 36, 37      |
| `crates/trusted-server-core/src/cookies/template_cache_policy.rs`                                                   | New cookies refused as cache keys                                                                                           | 4                          |
| `crates/trusted-server-core/src/ec/identity/mod.rs`, `ids.rs`, `resolution.rs` (new)                                | Module, ID newtypes, error type, results                                                                                    | 5, 10                      |
| `crates/trusted-server-core/src/ec/kv_types.rs`                                                                     | `KvPartnerId` metadata, `KvProviderState`, `KvEntry.ps`, validation                                                         | 6                          |
| `crates/trusted-server-core/src/ec/kv.rs`                                                                           | Constructors; `PartnerIdUpdate.acquired_at`; `compute_cluster_size`; removal overflow write; counting store                 | 6, 7, 22, 24, 27           |
| `crates/trusted-server-core/src/ec/admin.rs`                                                                        | Redact `ps.st`; servability; operator opt-out clear                                                                         | 6, 14, 36                  |
| `crates/trusted-server-core/src/ec/identity/descriptor.rs` (new)                                                    | Descriptor, builder, `Resolves`, `Captures`, `ConsentPolicy`, `EidProvenance`, `Input`, `Access`, `ResolveKind`, `UsOptOut` | 5, 8                       |
| `crates/trusted-server-core/src/ec/identity/input.rs` (new)                                                         | `ResolveInput`, `CaptureInput`, `StoredId`, `Region`, `HashedEmail`, `ProviderConsent`, randomness                          | 9                          |
| `crates/trusted-server-core/src/ec/identity/exchange.rs` (new)                                                      | `ProviderRequest`, `ProviderResponse`                                                                                       | 10                         |
| `crates/trusted-server-core/src/ec/identity/provider.rs` (new)                                                      | `IdentityProvider`, `IdentityProviderConfig`                                                                                | 11                         |
| `crates/trusted-server-core/src/ec/identity/policy.rs` (new), `src/consent/*`                                       | Policy evaluation, GPP opt-outs, `stricter_of`, fingerprint, redacting `Debug`                                              | 12, 34                     |
| `crates/trusted-server-core/src/ec/identity/revision.rs` (new)                                                      | `IdentityKeys`, `Revision`, keyed hashes                                                                                    | 13                         |
| `crates/trusted-server-core/src/ec/identity/catalog.rs`, `servable.rs` (new)                                        | Provider catalog, ownership, `cf`; the servability check                                                                    | 14, 22, 32, 33             |
| `crates/trusted-server-core/src/ec/eids.rs`, `ec/identify.rs`                                                       | Servability for KV IDs; `/identify` stops writing                                                                           | 14, 24                     |
| `crates/trusted-server-core/src/ec/identity/mutation.rs` (new)                                                      | `IdentityMutation` and its apply algorithm                                                                                  | 15                         |
| `crates/trusted-server-core/src/ec/identity/cookie.rs` (new)                                                        | Seal and open, `ts-eids-s`, size cap, KV merge, `ts-eids-r`                                                                 | 16, 17, 18                 |
| `crates/trusted-server-core/src/ec/identity/config.rs` (new), `src/settings.rs`, `src/config.rs`                    | `[ec.identity]`, `owner`, deploy and runtime validation                                                                     | 19, 33                     |
| `crates/trusted-server-core/src/ec/identity/coordinator.rs` (new)                                                   | Eligibility, outcomes, dispatch, breaker, hold, capture, re-grants                                                          | 20, 21, 35, 36, 37, 38     |
| `crates/trusted-server-core/src/ec/identity/generic_pull.rs` (new), `ec/pull_sync.rs`                               | `GenericPullProvider`; pull sync dispatcher removed                                                                         | 22, 29                     |
| `crates/trusted-server-core/src/ec/identity/test_support.rs` (new)                                                  | Shared fixtures, `ProviderHarness`, fake HTTP client and limiter                                                            | 14, 15, 20, 21, 22, 35, 39 |
| `crates/trusted-server-core/src/ec/identity/request.rs` (new), `src/ec/mod.rs`, `src/http_util.rs`                  | Writer decision, phase-1 captures, request identity state, read triggers                                                    | 23, 29                     |
| `crates/trusted-server-core/src/ec/finalize.rs`                                                                     | Mutation instead of writes; stager cookies; `ts-eids-r` on every path; metrics                                              | 25, 26, 27, 29, 31         |
| `crates/trusted-server-core/src/auction/endpoints.rs`                                                               | KV-gap staging; server set in the merge; owner denials; hold mode                                                           | 26, 28, 36, 37             |
| `crates/trusted-server-core/src/ec/identity/flush.rs` (new)                                                         | Phase 4 `run_after_send`                                                                                                    | 29                         |
| `crates/trusted-server-adapter-fastly/src/app.rs`, `src/main.rs`, `src/rate_limiter.rs`, `tests/identity_writes.rs` | Writer decision, status capture, phase 4, `record`/`count`, invariant tests                                                 | 23, 25, 29, 30, 35         |
| `crates/trusted-server-core/src/integrations/registry.rs`                                                           | Provider registration, capture-path check, capture plumbing, `is_real_browser`                                              | 23, 32, 38                 |
| `crates/trusted-server-core/src/ec/rate_limiter.rs`                                                                 | `record` and `count`                                                                                                        | 35                         |
| `crates/trusted-server-core/src/ec/batch_sync.rs`                                                                   | Owned sources fill only when absent                                                                                         | 40                         |
| `crates/trusted-server-core/src/ec/identity/examples.rs` (new, tests only)                                          | Spec 4.6 reference providers                                                                                                | 39                         |
| `crates/trusted-server-core/src/migration_guards.rs`                                                                | Every new `ec/identity/*.rs` file                                                                                           | 5 onward                   |
| `docs/guide/*.md`, `CHANGELOG.md`, `trusted-server.example.toml`                                                    | Docs                                                                                                                        | 4, 19, 31, 41              |

------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- | ---------------------- |
| `Cargo.toml` (workspace), `crates/trusted-server-core/Cargo.toml` | `aes-gcm` dependency | 5 |
| `crates/trusted-server-core/src/constants.rs` | New cookie-name constants | 1 |
| `crates/trusted-server-core/src/cookies.rs` | Identity-cookie lists and the name-and-prefix strip helper | 1 |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Strip identity cookies from Prebid Server requests | 2 |
| `crates/trusted-server-core/src/publisher.rs` | Origin strip; writer-request hooks; page-bids stops ingest; navigation merge and hold mode | 3, 22, 25, 27, 36 |
| `crates/trusted-server-core/src/cookies/template_cache_policy.rs` | New cookies refused as cache keys | 4 |
| `crates/trusted-server-core/src/ec/identity/mod.rs` (new) | Module docs, re-exports | 5 |
| `crates/trusted-server-core/src/ec/identity/ids.rs` (new) | `IdentityProviderId`, `SourceDomain`, `UnixTime` | 5 |
| `crates/trusted-server-core/src/ec/kv_types.rs` | `KvPartnerId` metadata, `KvProviderState`, `KvEntry.ps`, validation | 6 |
| `crates/trusted-server-core/src/ec/kv.rs` | Literals to constructors; `PartnerIdUpdate.acquired_at`; mutation flush; overflow write | 6, 7, 21, 26 |
| `crates/trusted-server-core/src/ec/admin.rs` | Redact `ps.st`; servability in the admin view | 6, 13 |
| `crates/trusted-server-core/src/ec/identity/descriptor.rs` (new) | `ProviderDescriptor`, builder, `Resolves`, `Captures`, `ConsentPolicy`, `EidProvenance`, `Input`, `Access`, `UsOptOut` | 8 |
| `crates/trusted-server-core/src/ec/identity/input.rs` (new) | `ResolveInput`, `CaptureInput`, `StoredId`, `Region`, `HashedEmail`, `ProviderConsent`, `ResolveKind` | 9 |
| `crates/trusted-server-core/src/ec/identity/exchange.rs` (new) | `ProviderRequest`, `ProviderResponse` | 10 |
| `crates/trusted-server-core/src/ec/identity/resolution.rs` (new) | `Resolution`, `ResolvedId`, `When`, `StateChange`, `IdentityProviderError` | 10 |
| `crates/trusted-server-core/src/ec/identity/provider.rs` (new) | `IdentityProvider`, `IdentityProviderConfig` | 11 |
| `crates/trusted-server-core/src/ec/identity/revision.rs` (new) | `Revision`, `rv` generation, keyed hashes (`pv` fallback, `iv`, visitor key, fingerprint) | 12 |
| `crates/trusted-server-core/src/ec/identity/servable.rs` (new) | The shared servability check | 13 |
| `crates/trusted-server-core/src/ec/eids.rs`, `ec/identify.rs` | Use the servability check; `/identify` stops writing | 13, 23 |
| `crates/trusted-server-core/src/ec/identity/mutation.rs` (new) | `IdentityMutation`, `PartnerIdChange`, `ProviderResult`, apply algorithm | 14 |
| `crates/trusted-server-core/src/ec/identity/cookie.rs` (new) | Seal and open, `ts-eids-s` payload, size cap, `ts-eids-r` batches, merge with KV | 15, 16, 17 |
| `crates/trusted-server-core/src/ec/identity/config.rs` (new) | `[ec.identity]` settings and validation | 18, 32 |
| `crates/trusted-server-core/src/settings.rs` | `Ec.identity`; `EcPartner.owner`; deprecate `pull_sync_concurrency` | 18, 32 |
| `crates/trusted-server-core/src/ec/identity/coordinator.rs` (new) | Eligibility, merged view, dispatch, outcomes, markers, breaker | 19, 20, 34, 35 |
| `crates/trusted-server-core/src/ec/identity/generic_pull.rs` (new) | `GenericPullProvider` | 21 |
| `crates/trusted-server-core/src/ec/pull_sync.rs` | Reduced to helpers `GenericPullProvider` reuses; dispatch removed | 21, 28 |
| `crates/trusted-server-core/src/ec/identity/test_support.rs` (new) | `ProviderHarness`, `WriteCountingEcKv`, fake HTTP client | 11, 21, 40 |
| `crates/trusted-server-core/src/ec/mod.rs` | `EcContext` writer flag, phase-1 captures, identity cookie state, mutation | 22 |
| `crates/trusted-server-core/src/http_util.rs` | `is_prefetch_request` moves here; `is_document_navigation` | 22 |
| `crates/trusted-server-core/src/ec/finalize.rs` | Mutation instead of ingest writes; stager cookie saves; `ts-eids-r` on every path | 24 |
| `crates/trusted-server-core/src/auction/endpoints.rs` | KV-gap staging; server set in `merge_auction_eids`; hold mode | 25, 27, 36 |
| `crates/trusted-server-adapter-fastly/src/app.rs`, `src/main.rs` | Writer decision, status capture, phase 4 entry point | 22, 28 |
| `crates/trusted-server-core/src/integrations/registry.rs` | Identity provider registration; capture plumbing; capture-path check | 31, 37 |
| `crates/trusted-server-core/src/config.rs` | `validate_identity_integration`, ownership validation | 32 |
| `crates/trusted-server-core/src/consent/{gpp.rs,tcf.rs,types.rs,mod.rs}` | Sharing and targeted-advertising opt-outs, section 6, GPC and known-child, `has_vendor_consent`, `stricter_of`, redacting `Debug` | 33 |
| `crates/trusted-server-core/src/ec/identity/policy.rs` (new) | `ConsentPolicy` evaluation and the consent fingerprint | 33 |
| `crates/trusted-server-core/src/ec/rate_limiter.rs`, `adapter-fastly/src/rate_limiter.rs` | `record` and `count` | 34 |
| `crates/trusted-server-core/src/ec/batch_sync.rs` | Owned sources fill only when absent | 39 |
| `docs/guide/edge-cookies.md`, `configuration.md`, `api-reference.md`, `ec-setup-guide.md`, `integrations/prebid.md` | Docs | 4, 30, 41 |

---

# Part 0: Strip the identity cookies first (spec 14, step 0)

This part only removes cookies from outbound requests. It sets no new cookies and changes no KV behaviour. It ships one release before Part 1, so a rollback after Part 1 still keeps the new one-year cookies away from the origin and from Prebid Server.

Before you start: create a worktree from the spec branch, `git worktree add -b feat/1246-identity-cookie-strips .claude/worktrees/1246-identity-providers spec/1246-server-side-identity-providers`. The implementation stacks on its spec branch, which already carries the Prebid EID-fidelity work this spec depends on. Build with the toolchain that branch pins in `rust-toolchain.toml`.

### Task 1: Identity cookie names and a name-and-prefix strip helper

**Files:**

- Modify: `crates/trusted-server-core/src/constants.rs` (next to `COOKIE_TS_EIDS`, line 8)
- Modify: `crates/trusted-server-core/src/cookies.rs` (after `CONSENT_COOKIE_NAMES`, and after `strip_cookies`)
- Test: `crates/trusted-server-core/src/cookies.rs` (`mod tests`)

**Interfaces:**

- Produces:
  - `pub const COOKIE_TS_EIDS_S: &str = "__Host-ts-eids-s";`
  - `pub const COOKIE_TS_EIDS_R_PREFIX: &str = "__Host-ts-eids-r-";`
  - `pub const TS_IDENTITY_COOKIE_NAMES: &[&str]` = `ts-ec`, `ts-eids`, `__Host-ts-eids-s`, `ts-ec-pull-complete`
  - `pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str]` = `ts-eids`, `__Host-ts-eids-s`
  - `pub const TS_IDENTITY_COOKIE_PREFIXES: &[&str]` = `__Host-ts-eids-r-`
  - `pub fn strip_cookie_pairs(cookie_header: &str, names: &[&str], prefixes: &[&str]) -> String`
  - `pub fn strip_cookie_headers(headers: &mut http::HeaderMap, names: &[&str], prefixes: &[&str])`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `cookies.rs`:

```rust
    #[test]
    fn strip_cookie_pairs_removes_names_and_prefixes() {
        let header = "ts-ec=abc; ts-eids=W10; __Host-ts-eids-s=v1.x.y; \
                      __Host-ts-eids-r-0a1b2c3d=v1.p.q; other=1; ts-eids-extra=keep";

        let stripped = strip_cookie_pairs(
            header,
            TS_ORIGIN_STRIPPED_COOKIE_NAMES,
            TS_IDENTITY_COOKIE_PREFIXES,
        );

        assert_eq!(
            stripped, "ts-ec=abc; other=1; ts-eids-extra=keep",
            "should drop the ID cookies and keep ts-ec and unrelated cookies"
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

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES, TS_IDENTITY_COOKIE_PREFIXES);

        let values: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
        assert_eq!(values.len(), 1, "should collapse into one Cookie header");
        assert_eq!(values[0], "other=1", "should keep only the unrelated cookie");
    }

    #[test]
    fn strip_cookie_headers_removes_header_when_nothing_is_left() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::COOKIE,
            http::HeaderValue::from_static("ts-ec=abc; __Host-ts-eids-r-00000000=v1.a.b"),
        );
        headers.insert(header::USER_AGENT, http::HeaderValue::from_static("ExampleBrowser/1.0"));

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES, TS_IDENTITY_COOKIE_PREFIXES);

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
/// Server-owned, sealed cache of provider-collected EIDs (spec 6.2).
pub const COOKIE_TS_EIDS_S: &str = "__Host-ts-eids-s";
/// Name prefix of the per-batch removal cookies, `__Host-ts-eids-r-<8 hex>`.
pub const COOKIE_TS_EIDS_R_PREFIX: &str = "__Host-ts-eids-r-";
```

In `cookies.rs`, extend the `crate::constants` import with `COOKIE_SHAREDID` (if you use it later), `COOKIE_TS_EC`, `COOKIE_TS_EC_PULL_COMPLETE`, `COOKIE_TS_EIDS`, `COOKIE_TS_EIDS_R_PREFIX` and `COOKIE_TS_EIDS_S`. Then add, after `CONSENT_COOKIE_NAMES`:

```rust
/// Trusted Server identity cookies that never reach a third party.
///
/// Stripped from Prebid Server requests and never added to proxy or provider
/// requests. The `__Host-ts-eids-r-` family is matched by
/// [`TS_IDENTITY_COOKIE_PREFIXES`].
pub const TS_IDENTITY_COOKIE_NAMES: &[&str] = &[
    COOKIE_TS_EC,
    COOKIE_TS_EIDS,
    COOKIE_TS_EIDS_S,
    COOKIE_TS_EC_PULL_COMPLETE,
];

/// Identity cookies removed from publisher-origin requests.
///
/// `ts-ec` and `ts-ec-pull-complete` still reach the origin. The ID cookies
/// are removed so that they cannot push the origin's `Cookie` header past
/// common 8 KB header limits.
pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str] = &[COOKIE_TS_EIDS, COOKIE_TS_EIDS_S];

/// Name prefixes of identity cookie families, stripped wherever the named
/// identity cookies are.
pub const TS_IDENTITY_COOKIE_PREFIXES: &[&str] = &[COOKIE_TS_EIDS_R_PREFIX];
```

After `strip_cookies`:

```rust
/// Strips cookies matching any exact name or any name prefix from a `Cookie`
/// header value.
///
/// Returns an empty string when nothing is left.
#[must_use]
pub fn strip_cookie_pairs(cookie_header: &str, names: &[&str], prefixes: &[&str]) -> String {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default().trim();
            !names.contains(&name) && !prefixes.iter().any(|prefix| name.starts_with(prefix))
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
pub fn strip_cookie_headers(headers: &mut http::HeaderMap, names: &[&str], prefixes: &[&str]) {
    let retained = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| strip_cookie_pairs(value, names, prefixes))
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
git commit --signoff -S -m "Add identity cookie lists and a name-and-prefix cookie strip"
```

### Task 2: Strip identity cookies from Prebid Server requests in every mode

**Files:**

- Modify: `crates/trusted-server-core/src/auction/routing.rs` (`PrebidTransportHeaders`, around line 205)
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs` (`apply_prebid_header_values` and both callers, around lines 2025-2108)
- Test: `crates/trusted-server-core/src/integrations/prebid.rs` (`mod tests`)

**Interfaces:**

- Consumes: `strip_cookie_pairs`, `TS_IDENTITY_COOKIE_NAMES`, `TS_IDENTITY_COOKIE_PREFIXES`, `CONSENT_COOKIE_NAMES` (Task 1)
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
            HeaderValue::from_static("__Host-ts-eids-s=v1.a.b; __Host-ts-eids-r-0a1b2c3d=v1.c.d"),
            HeaderValue::from_static("ts-eids=W10; ts-ec-pull-complete=v1; second=2"),
        ];
        let mut to = http::Request::new(EdgeBody::empty());

        apply_prebid_header_values(
            &cookies,
            None,
            None,
            None,
            &mut to,
            ConsentForwardingMode::CookiesAndBody,
            None,
        );

        assert_eq!(
            cookie_values(&to),
            vec!["euconsent-v2=tcf; other=1; second=2".to_owned()],
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
            ConsentForwardingMode::CookiesAndBody,
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

In `integrations/prebid.rs`, change the import to `use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES, TS_IDENTITY_COOKIE_PREFIXES, strip_cookie_pairs};`. Then replace the cookie half of `apply_prebid_header_values`:

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
        .map(|value| strip_cookie_pairs(value, TS_IDENTITY_COOKIE_NAMES, TS_IDENTITY_COOKIE_PREFIXES))
        .map(|value| strip_cookie_pairs(&value, consent_names, &[]))
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

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (`rewrite_origin_request`, around line 3124)
- Test: `crates/trusted-server-core/src/publisher.rs` (`mod tests`)

**Interfaces:**

- Consumes: `strip_cookie_headers`, `TS_ORIGIN_STRIPPED_COOKIE_NAMES`, `TS_IDENTITY_COOKIE_PREFIXES` (Task 1)

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
                "__Host-ts-eids-s=v1.a.b; __Host-ts-eids-r-0a1b2c3d=v1.c.d; ts-ec-pull-complete=v1",
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
            .header(header::COOKIE, "ts-eids=W10; __Host-ts-eids-s=v1.a.b")
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
    strip_cookie_headers(
        req.headers_mut(),
        TS_ORIGIN_STRIPPED_COOKIE_NAMES,
        TS_IDENTITY_COOKIE_PREFIXES,
    );
```

Add `use crate::cookies::{TS_IDENTITY_COOKIE_PREFIXES, TS_ORIGIN_STRIPPED_COOKIE_NAMES, strip_cookie_headers};` to the imports at the top of `publisher.rs`.

- [ ] **Step 4: Run and see it pass, then run the other adapters**

Run: `cargo test-fastly -- publisher::tests` then `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS. The strip lives in core, so every adapter gets it (spec 11). If an adapter test asserted that `ts-eids` reaches the origin, update it and note the behaviour change in the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit --signoff -S -m "Strip ID cookies from publisher origin requests" -m "The origin keeps ts-ec and the pull-complete marker. ts-eids, __Host-ts-eids-s and the __Host-ts-eids-r- cookies no longer reach it. An origin that read ts-eids must read x-ts-eids instead."
```

### Task 4: Refuse the new cookies as template-cache keys, and document Part 0

**Files:**

- Modify: `crates/trusted-server-core/src/cookies/template_cache_policy.rs` (around line 35)
- Modify: `docs/guide/edge-cookies.md`, `CHANGELOG.md`
- Test: `crates/trusted-server-core/src/cookies/template_cache_policy.rs` (`mod tests`, line 172)

- [ ] **Step 1: Write the failing tests**

Find the test that rejects `ts-eids` as a key (`grep -n "identity cookie" crates/trusted-server-core/src/cookies/template_cache_policy.rs`). It shows how the validating function is called. Add two cases next to it with the same call shape:

```rust
    #[test]
    fn template_cache_key_rejects_sealed_identity_cookies() {
        for name in ["__Host-ts-eids-s", "__Host-ts-eids-r-0a1b2c3d"] {
            let error = validate_names_for_test(&[name.to_owned()], &[])
                .expect_err("should reject identity cookie as a cache key");
            assert!(
                error.contains("must not key on Trusted Server identity cookie"),
                "should explain the rejection for {name}, got {error}"
            );
        }
        validate_names_for_test(&["session".to_owned()], &[])
            .expect("should still accept an unrelated cookie");
    }
```

`validate_names_for_test` stands for the call the existing identity-cookie test makes. Use that exact call, not a new helper.

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- template_cache_policy::tests::template_cache_key_rejects_sealed`
Expected: FAIL, because both names are accepted today.

- [ ] **Step 3: Implement**

Replace the condition at line 35:

```rust
            if field == "template_cache_key_cookies"
                && ([COOKIE_TS_EC, COOKIE_TS_EIDS, COOKIE_TS_EIDS_S, COOKIE_SHAREDID]
                    .contains(&name.as_str())
                    || name.starts_with(COOKIE_TS_EIDS_R_PREFIX))
            {
```

and extend the `crate::constants` import with `COOKIE_TS_EIDS_R_PREFIX` and `COOKIE_TS_EIDS_S`.

- [ ] **Step 4: Run and see it pass**

Run: `cargo test-fastly -- template_cache_policy`
Expected: PASS.

- [ ] **Step 5: Docs**

In `docs/guide/edge-cookies.md`, in the section that lists TS cookies, add a short "Cookies forwarded to the origin" paragraph. Say that `ts-ec` and `ts-ec-pull-complete` still reach the publisher origin, that `ts-eids` and the server-owned `__Host-ts-eids-s` and `__Host-ts-eids-r-*` cookies do not, and that origin code that read `ts-eids` should read the `x-ts-eids` response header instead. Also say that Prebid Server never receives any TS identity cookie.

In `CHANGELOG.md` under `## [Unreleased]` / `### Changed`, add:

```markdown
- **Breaking:** The publisher origin no longer receives the `ts-eids` cookie, and Prebid Server no longer receives any Trusted Server identity cookie (`ts-ec`, `ts-eids`, `ts-ec-pull-complete`, and the server-owned `__Host-ts-eids-s` and `__Host-ts-eids-r-*` cookies that a later release introduces). The origin still receives `ts-ec`. Origin code that read `ts-eids` should read the `x-ts-eids` response header instead. This release is the rollback floor for the server-side identity provider work.
```

Run: `cd docs && npm run format && cd ..` and `docs/node_modules/.bin/prettier --config docs/.prettierrc --write CHANGELOG.md`

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/cookies/template_cache_policy.rs docs/guide/edge-cookies.md CHANGELOG.md
git commit --signoff -S -m "Refuse the sealed identity cookies as template cache keys"
```

- [ ] **Step 7: Part 0 gate and PR**

Run `/check-ci`. Every gate must pass. Open the PR against `spec/1246-server-side-identity-providers` with the `pr-creator` agent. Title: "Strip identity cookies from origin and Prebid Server requests". The description must say this is spec step 0 and the rollback floor for the later parts, and must name the origin `ts-eids` behaviour change.

---

# Part 1: One write per page load, no new vendors (spec 14, step 1)

Part 1 builds the trait, descriptor, harness and coordinator, moves every browser-driven KV write into the writer request's single flush, adds `__Host-ts-eids-s` and `__Host-ts-eids-r-*`, and replaces pull sync with `GenericPullProvider`. No vendor provider is registered yet, and no provider runs in hold mode or captures. Those come in Part 2.

Before you start: `git switch feat/1246-identity-cookie-strips && git switch -c feat/1246-identity-single-write`.

Tasks 5 to 22 build pure, unit-tested modules with no request wiring. Tasks 23 to 29 wire them into requests. Task 30 proves the one-write invariant, and Task 31 ships.

### Task 5: The `ec::identity` module, its ID newtypes and its error type

**Files:**

- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`, alphabetical), `crates/trusted-server-core/Cargo.toml`
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

- [ ] **Step 1: Add the dependency**

In the workspace `Cargo.toml`, under `[workspace.dependencies]` in alphabetical order:

```toml
aes-gcm = "0.10"
```

In `crates/trusted-server-core/Cargo.toml` `[dependencies]`, alphabetical:

```toml
aes-gcm = { workspace = true }
```

Run: `cargo check-fastly && cargo check-cloudflare`
Expected: both build. `aes-gcm` 0.10 depends on `getrandom` only through its optional `getrandom` feature, which stays off. Nonces come from `rand::thread_rng` (Task 16). If `cargo check-cloudflare` fails on `getrandom`, set `default-features = false, features = ["aes", "alloc"]` in the workspace entry and run it again.

- [ ] **Step 2: Write the failing tests**

Create `ec/identity/ids.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_accepts_the_documented_grammar() {
        for id in ["id5", "pull-ssp-example-com", "lockr_aim", "a"] {
            id.parse::<IdentityProviderId>()
                .unwrap_or_else(|_| panic!("should accept `{id}`"));
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
//! into OpenRTB EIDs. Providers only build requests and parse buffered
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

- [ ] **Step 3: Run and see the tests fail**

Run: `cargo test-fastly -- ec::identity::ids`
Expected: compile errors, because the types do not exist.

- [ ] **Step 4: Implement `resolution.rs` (error only) and the minimal `descriptor.rs`**

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

- [ ] **Step 5: Implement `ids.rs`**

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

    /// From Unix milliseconds, for vendors such as Lockr and UID2. Floors to
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

- [ ] **Step 6: Run and see the tests pass**

Run: `cargo test-fastly -- ec::identity` and `cargo test-fastly -- migration_guards`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/trusted-server-core/Cargo.toml crates/trusted-server-core/src/ec/mod.rs crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add the identity module with provider and source ID newtypes" -m "Adds aes-gcm, which the sealed identity cookies use."
```

### Task 6: KV schema: record metadata, provider state, `Default` and constructors

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv_types.rs` (`KvEntry`, `KvConsent`, `KvGeo`, `KvPartnerId`, `validate`, constructors)
- Modify: every file with a `KvPartnerId {` literal (find them with `grep -rn "KvPartnerId {" crates/`)
- Modify: `crates/trusted-server-core/src/ec/admin.rs` (raw lookup response, around `admin.rs:394`)
- Test: `crates/trusted-server-core/src/ec/kv_types.rs`, `crates/trusted-server-core/src/ec/admin.rs`

**Interfaces:**

- Produces:
  - `KvPartnerId { uid, w: Option<String>, rv: Option<String>, at: Option<u64>, exp: Option<u64>, ra: Option<u64>, ru: Option<u64>, atype: Option<i32>, ext: Option<serde_json::Value> }`, deriving `Default`, with `KvPartnerId::new(uid: impl Into<String>) -> Self` and `KvPartnerId::browser(uid: impl Into<String>, acquired_at: Option<u64>) -> Self`
  - `KvProviderState { n: Option<u64>, f: u32, o: Option<KvOptOut>, oc: Option<u64>, iv: Vec<String>, st: Option<serde_json::Value> }`, deriving `Default`
  - `KvOptOut { at: u64, fp: String }`
  - `KvEntry.ps: BTreeMap<String, KvProviderState>`, and `KvEntry` derives `Default`
  - Constants `MAX_PARTNER_EXT_BYTES = 256`, `MAX_PROVIDER_STATE_BYTES = 1024`, `MAX_PROVIDER_STATES = 32`, `MAX_INVALIDATED_UIDS = 4`, `RECORD_REVISION_HEX_LEN = 16`

- [ ] **Step 1: Write the failing tests**

Add to `kv_types.rs` tests:

```rust
    #[test]
    fn legacy_partner_record_reads_and_writes_unchanged() {
        let legacy = json!({ "uid": "example-uid" });
        let record: KvPartnerId = serde_json::from_value(legacy.clone()).expect("should read legacy");
        assert_eq!(record, KvPartnerId::new("example-uid"), "should fill every new field with None");
        assert_eq!(serde_json::to_value(&record).expect("should write"), legacy, "should add no fields");
    }

    #[test]
    fn provider_record_and_state_round_trip() {
        let stored = json!({
            "v": 1,
            "created": 1_760_000_000,
            "consent": { "ok": true, "updated": 1_760_000_000 },
            "geo": { "country": "US" },
            "ids": {
                "id5-sync.com": {
                    "uid": "ID5*example",
                    "w": "id5",
                    "rv": "9c1d04b7e6a83f2a",
                    "at": 1_760_000_000,
                    "exp": 1_767_776_000,
                    "ra": 1_760_086_400,
                    "ru": 1_770_000_000,
                    "atype": 1,
                    "ext": { "linkType": 2 }
                }
            },
            "ps": {
                "id5": {
                    "n": 1_760_003_600,
                    "f": 1,
                    "o": { "at": 1_759_000_000, "fp": "0123456789abcdef" },
                    "oc": 1_759_500_000,
                    "iv": ["aaaaaaaaaaaaaaaa"],
                    "st": { "signature": "example-opaque-state" }
                }
            }
        });

        let entry: KvEntry = serde_json::from_value(stored.clone()).expect("should read");
        entry.validate().expect("should be valid");
        assert_eq!(serde_json::to_value(&entry).expect("should write"), stored, "should round-trip exactly");
    }

    #[test]
    fn validate_rejects_oversized_ext_state_and_too_many_providers() {
        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").ext =
            Some(json!({ "pad": "x".repeat(MAX_PARTNER_EXT_BYTES) }));
        assert!(entry.validate().expect_err("should reject ext").contains("ext"), "should name ext");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ps.insert(
            "id5".to_owned(),
            KvProviderState { st: Some(json!("x".repeat(MAX_PROVIDER_STATE_BYTES))), ..KvProviderState::default() },
        );
        assert!(entry.validate().expect_err("should reject st").contains("st"), "should name st");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        for index in 0..=MAX_PROVIDER_STATES {
            entry.ps.insert(format!("p{index}"), KvProviderState::default());
        }
        assert!(entry.validate().expect_err("should reject ps size").contains("ps"), "should name ps");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ps.insert(
            "id5".to_owned(),
            KvProviderState { iv: vec!["a".repeat(16); MAX_INVALIDATED_UIDS + 1], ..KvProviderState::default() },
        );
        assert!(entry.validate().expect_err("should reject iv").contains("iv"), "should name iv");

        let mut entry = KvEntry::minimal("example.com", "example-uid", 1);
        entry.ids.get_mut("example.com").expect("should exist").rv = Some("not-hex".to_owned());
        assert!(entry.validate().expect_err("should reject rv").contains("rv"), "should name rv");

        KvEntry::minimal("example.com", "example-uid", 1).validate().expect("control entry should be valid");
    }
```

The validation test builds `KvProviderState` with `..KvProviderState::default()` because it tests one field at a time. It is not an exhaustiveness test.

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

        assert_eq!(body["ps"]["id5"]["st"], json!("[redacted]"), "should redact the state");
        assert_eq!(body["ps"]["id5"]["n"], json!(5), "should keep the rest of ps");
        assert_eq!(body["ids"]["example.com"]["uid"], json!("example-uid"), "should keep the ids");
    }
```

`admin_lookup_body_for_test` stands for whatever the existing raw-lookup test does to get the JSON body: insert the row, call the handler and parse the body. Write it as a small helper in the test module that reuses that test's setup.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::kv_types::tests` and `cargo test-fastly -- ec::admin::tests::admin_lookup_redacts`
Expected: compile errors (`ps`, `KvProviderState` and `KvPartnerId::new` do not exist).

- [ ] **Step 3: Implement the schema**

In `kv_types.rs`, add the constants after `MAX_UID_LENGTH`:

```rust
/// Maximum serialized bytes of a stored UID-level `ext`.
pub const MAX_PARTNER_EXT_BYTES: usize = 256;
/// Maximum serialized bytes of one provider's private state (`ps.st`).
pub const MAX_PROVIDER_STATE_BYTES: usize = 1024;
/// Maximum number of providers with a `ps` entry in one row.
pub const MAX_PROVIDER_STATES: usize = 32;
/// Maximum number of invalidated-UID hashes kept per provider (`ps.iv`).
pub const MAX_INVALIDATED_UIDS: usize = 4;
/// Length of a record revision (`rv`) in lowercase hex digits.
pub const RECORD_REVISION_HEX_LEN: usize = 16;
```

Add `Default` to the derives of `KvEntry`, `KvConsent` and `KvGeo`. Add the `ps` field at the end of `KvEntry`:

```rust
    /// Per-provider scheduling, opt-out and private state, keyed by
    /// identity provider ID. Never sent to bidders or cookies.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ps: BTreeMap<String, KvProviderState>,
```

Replace `KvPartnerId`:

```rust
/// A partner user ID within a KV entry.
///
/// Every field but `uid` is optional and omitted when absent, so legacy rows
/// read and write unchanged and `SCHEMA_VERSION` stays 1 (spec 6.1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KvPartnerId {
    /// The partner's user identifier.
    pub uid: String,
    /// Writer: an identity provider ID. Absent for browser ingest and legacy rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
    /// Record revision: 16 lowercase hex digits, random on every write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rv: Option<String>,
    /// Unix seconds when the ID was acquired or last refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// Unix seconds after which the ID must not be sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<u64>,
    /// Unix seconds after which the ID is due for refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ra: Option<u64>,
    /// Unix seconds after which the ID can no longer be refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ru: Option<u64>,
    /// Provider-supplied `atype`. Absent: use the partner's `openrtb_atype`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atype: Option<i32>,
    /// UID-level `ext`, at most [`MAX_PARTNER_EXT_BYTES`] serialized bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<serde_json::Value>,
}

impl KvPartnerId {
    /// A bare record holding only a UID.
    #[must_use]
    pub fn new(uid: impl Into<String>) -> Self {
        Self { uid: uid.into(), ..Self::default() }
    }

    /// A browser-ingested record: no writer, with its acquisition time.
    #[must_use]
    pub fn browser(uid: impl Into<String>, acquired_at: Option<u64>) -> Self {
        Self { uid: uid.into(), at: acquired_at, ..Self::default() }
    }
}

/// A provider's sticky opt-out (spec 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvOptOut {
    /// Unix seconds when the opt-out was reported.
    pub at: u64,
    /// Consent fingerprint at that time: 16 lowercase hex digits.
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
    /// Keyed hashes of UIDs the owner invalidated, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iv: Vec<String>,
    /// The provider's private state as embedded JSON. KV only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub st: Option<serde_json::Value>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}
```

The listed constructors (`KvEntry::new`, `minimal`, `tombstone`) gain `ps: BTreeMap::new()`. `minimal` uses `KvPartnerId::new(uid)`.

Extend `validate()`. Inside the existing `for (partner_id, partner_uid) in &self.ids` loop, after the UID length check:

```rust
            if let Some(ext) = &partner_uid.ext
                && serde_json::to_vec(ext).map_or(true, |bytes| bytes.len() > MAX_PARTNER_EXT_BYTES)
            {
                return Err(format!("partner ID '{partner_id}' ext exceeds {MAX_PARTNER_EXT_BYTES} bytes"));
            }
            if let Some(rv) = &partner_uid.rv
                && (rv.len() != RECORD_REVISION_HEX_LEN
                    || !rv.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            {
                return Err(format!("partner ID '{partner_id}' rv is not {RECORD_REVISION_HEX_LEN} hex digits"));
            }
```

After the loop:

```rust
        if self.ps.len() > MAX_PROVIDER_STATES {
            return Err(format!("ps exceeds MAX_PROVIDER_STATES ({})", self.ps.len()));
        }
        for (provider, state) in &self.ps {
            if state.iv.len() > MAX_INVALIDATED_UIDS {
                return Err(format!("ps '{provider}' iv exceeds {MAX_INVALIDATED_UIDS} hashes"));
            }
            if let Some(st) = &state.st
                && serde_json::to_vec(st).map_or(true, |bytes| bytes.len() > MAX_PROVIDER_STATE_BYTES)
            {
                return Err(format!("ps '{provider}' st exceeds {MAX_PROVIDER_STATE_BYTES} bytes"));
            }
        }
```

- [ ] **Step 4: Move every literal to the constructor**

Run `grep -rn "KvPartnerId {" crates/` and change each `KvPartnerId { uid: X }` to `KvPartnerId::new(X)`. That covers about 30 sites in 8 files, including `ec/kv.rs` (`apply_cookie_partner_id_updates`, `apply_partner_id_updates` and others), the batch sync and pull sync writers, the admin tests and `kv_types.rs`. Run `grep -rn "KvEntry {" crates/` too, and add `ps: BTreeMap::new()` to any full literal the compiler flags.

- [ ] **Step 5: Redact `ps.st` in the admin lookup**

In `ec/admin.rs`, find where the raw lookup body is returned verbatim (`grep -n "lookup_raw" crates/trusted-server-core/src/ec/admin.rs`). Before it is written to the response, parse it, redact and re-serialize:

```rust
/// Replaces every `ps.<provider>.st` with a marker. Provider state is never
/// returned by the admin lookup (spec 6.1, 9.3).
fn redact_provider_state(body: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    if let Some(states) = value.get_mut("ps").and_then(serde_json::Value::as_object_mut) {
        for state in states.values_mut() {
            if let Some(st) = state.get_mut("st") {
                *st = serde_json::Value::String("[redacted]".to_owned());
            }
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}
```

Call it on `lookup.body` where the handler builds its response. A body that is not JSON is returned unchanged, as today, because the lookup exists to inspect corrupt rows.

- [ ] **Step 6: Run and see the tests pass**

Run: `cargo test-fastly -- ec::` and then `cargo clippy-fastly`
Expected: PASS. Every existing KV, batch sync and pull sync test must still pass unchanged.

- [ ] **Step 7: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Add provider metadata and provider state to the KV schema" -m "New fields are optional and omitted when absent, so legacy rows read and write unchanged and the schema version stays 1. KvPartnerId and KvEntry derive Default, and literals move to constructors. The admin lookup redacts provider state."
```

### Task 7: Browser ingest records when it acquired an ID

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`PartnerIdUpdate`, `apply_cookie_partner_id_updates`, `apply_partner_id_updates`)
- Modify: `crates/trusted-server-core/src/ec/prebid_eids.rs` (`collect_eid_updates`, `collect_sharedid_update`, `collect_prebid_eid_updates_from_eids`)
- Modify: every `PartnerIdUpdate::new` caller the compiler reports
- Test: `crates/trusted-server-core/src/ec/kv.rs`

**Interfaces:**

- Produces: `PartnerIdUpdate { partner_id, uid, acquired_at: Option<u64> }`, `PartnerIdUpdate::new(partner_id, uid)` (`acquired_at = None`), `PartnerIdUpdate::with_acquired_at(self, at: u64) -> Self`. A record that browser ingest inserts now carries `at`.

- [ ] **Step 1: Write the failing test**

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

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- ec::kv::tests::browser_ingest_records_acquisition_time`
Expected: compile error, because `with_acquired_at` does not exist.

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

In `prebid_eids.rs`, give `collect_eid_updates` a `now: Option<u64>` parameter. Call `.with_acquired_at(now)` on each update when it is `Some`, and pass `checked_current_timestamp()` from the callers in `finalize.rs`. Leave batch sync and pull sync without `acquired_at`. They get it through the mutation in Tasks 15 and 22.

- [ ] **Step 4: Run and see it pass**

Run: `cargo test-fastly -- ec::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src/ec
git commit --signoff -S -m "Record when browser ingest acquired each ID"
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
  - `Captures`: `new`, `with_path(Method, &str)`, `with_path_prefix(&str)`, `with_request_consent(pointers)`, plus crate-visible `matches(&Method, &str) -> bool`, `paths()`, `prefixes()`, `consent_pointers()`
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
            "strict should require Purposes 1 and 4"
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
        ];
        for (name, builder) in cases {
            let error = builder.build().expect_err(name);
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

        assert!(!descriptor.is_expiring(&a), "a should not expire");
        assert!(descriptor.is_expiring(&b), "b should expire");
        let provenance = descriptor.provenance(&a).expect("a should have provenance");
        assert_eq!(provenance.inserter(), Some("example-inserter"), "should keep the inserter");
        assert_eq!(provenance.matcher(), None, "should leave the matcher unset");
        assert_eq!(provenance.mm(), Some(2), "should keep mm");
        assert!(descriptor.provenance(&b).is_none(), "b should have none");
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
        assert!(!captures.matches(&Method::POST, "/integrations/p/api/token/x"), "exact means exact");
        assert!(captures.matches(&Method::GET, "/integrations/p/stream/1"), "prefix matches any method");
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
            "Initial needs both"
        );
        assert_eq!(
            resolves.needs(ResolveKind::Refresh),
            [Input::ClientIp].into_iter().collect(),
            "Refresh needs only the IP"
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

    /// EID-level fields for `source`, if declared.
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

    /// EID-level fields for a declared source, added to every server-side
    /// value of that source this provider wrote. No KV field stores them.
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
    /// URL, a host that is not a bare hostname, or provenance or an expiring
    /// mark for a source it does not declare.
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

    /// This provider keeps private state, so it runs only on the writer
    /// request, where state can be stored (spec 5.3).
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

/// Passive capture from responses TS proxies for this provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captures {
    paths: Vec<(Method, String)>,
    prefixes: Vec<String>,
    consent_pointers: Vec<&'static str>,
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

    /// JSON pointers to consent strings in the proxied request's JSON body.
    /// The coordinator extracts only these fields and gates capture by the
    /// stricter of cookie and body consent. The provider never sees the body.
    #[must_use]
    pub fn with_request_consent(mut self, pointers: impl IntoIterator<Item = &'static str>) -> Self {
        self.consent_pointers.extend(pointers);
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

    /// Declared body consent pointers.
    #[must_use]
    pub(crate) fn consent_pointers(&self) -> &[&'static str] {
        &self.consent_pointers
    }
}

/// EID-level fields (OpenRTB 2.6 §3.2.27) that bidders read.
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

`normalize_partner_source_domain` doubles as a hostname check: it accepts exactly a bare, lowercase-normalized hostname. Comparing its output to the input also rejects uppercase. Update `mod.rs`:

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
- Modify: `crates/trusted-server-core/src/ec/identity/mod.rs`, `migration_guards.rs`
- Test: `crates/trusted-server-core/src/ec/identity/input.rs`

**Interfaces:**

- Consumes: `ProviderDescriptor`, `Resolves`, `Input`, `Access`, `ResolveKind` (Task 8), and `ConsentContext` (`consent/types.rs`)
- Produces:
  - `ProviderConsent` with `gdpr_applies`, `raw_tc_string`, `raw_gpp_string`, `gpp_section_ids`, `raw_us_privacy`, and `pub(crate) fn from_context(&ConsentContext) -> Self`
  - `Region` with `country`, `subdivision`, `is`, and `pub(crate) fn from_geo(country: &str, region: Option<&str>) -> Option<Self>`, which normalizes to upper case and strips an `XX-` prefix
  - `HashedEmail` (32 bytes) with `as_bytes`, `to_hex`, `to_base64`, and a redacting `Debug`
  - `StoredId` with `uid`, `atype`, `ext`, `writer`, `acquired_at`, `expires_at`, `refresh_at`, `is_expired`, and `pub(crate) fn from_record(&KvPartnerId) -> Self`
  - `ResolveSignals<'a>`: a crate-private struct holding every signal the coordinator has (IP, UA, page URL, region, HEM, consent, publisher domain, now, random seed source, EC ID, stored IDs, raw state JSON, dry-run flag)
  - `ResolveInput<'a>` with the spec 4.4 accessors, `pub(crate) fn new(descriptor: &'a ProviderDescriptor, kinds: BTreeMap<SourceDomain, ResolveKind>, signals: &'a ResolveSignals<'a>) -> Self`, `pub(crate) fn recorded_errors(&self) -> Vec<Access>`, `pub(crate) fn state_decode_failed(&self) -> bool`, and `pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>>`
  - `CaptureInput<'a>` with the spec 4.4 accessors and `pub(crate) fn new(...)`
  - `RandomSource`: a trait `fn fill(&self, bytes: &mut [u8])`, with `ThreadRandom` for production and `SeededRandom` (a ChaCha-free xorshift from a `u64` seed, test support only)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::IpAddr;

    use super::*;
    use crate::ec::identity::descriptor::{Captures, ProviderDescriptor, Resolves};

    fn descriptor(resolves: Resolves) -> ProviderDescriptor {
        ProviderDescriptor::builder("p".parse().expect("should parse"))
            .with_source("example.com".parse().expect("should parse"))
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
            dry_run: false,
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

        assert_eq!(input.client_ip().expect("should read ip"), "192.0.2.10".parse::<IpAddr>().expect("ip"), "ip");
        let region = input.region().expect("should read region");
        assert_eq!((region.country(), region.subdivision()), ("US", Some("CA")), "should normalize the region");
        assert!(region.is("US", "CA"), "should test country and subdivision");
        let error = input.page_url().expect_err("should be unavailable");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InputUnavailable { input: Input::PageUrl }),
            "a declared but missing input is InputUnavailable"
        );
        assert!(input.recorded_errors().is_empty(), "declared reads record nothing");
    }

    #[test]
    fn undeclared_reads_are_recorded_even_when_ignored() {
        let descriptor = descriptor(Resolves::new().with_endpoint("https://a.example.com"));
        let signals = signals();
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Initial), &signals);

        let ignored = input.user_agent().ok();
        let state = input.state::<serde_json::Value>();

        assert!(ignored.is_none(), "should not return an undeclared input");
        assert!(state.is_err(), "state needs with_private_state");
        assert_eq!(
            input.recorded_errors(),
            vec![Access::Input(Input::UserAgent), Access::PrivateState],
            "should record both undeclared reads"
        );
        input.endpoint().expect("the declared endpoint is always available");
    }

    #[test]
    fn state_decodes_and_a_bad_state_is_recorded() {
        let descriptor = descriptor(Resolves::new().with_endpoint("https://a.example.com").with_private_state());
        let mut good = signals();
        good.state_json = Some(r#"{"signature":"s"}"#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &good);
        let state: serde_json::Value = input.state().expect("should decode").expect("should be present");
        assert_eq!(state["signature"], "s", "should decode the state");
        assert!(!input.state_decode_failed(), "a good state is not a failure");

        #[derive(serde::Deserialize)]
        struct Typed {
            #[allow(dead_code)]
            signature: String,
        }
        let mut bad = signals();
        bad.state_json = Some(r#""a bare string""#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &bad);
        let error = input.state::<Typed>().expect_err("should not decode");
        assert!(matches!(error.current_context(), IdentityProviderError::InvalidState), "InvalidState");
        assert!(input.state_decode_failed(), "should record the failure");
    }

    #[test]
    fn dry_run_is_never_true_for_stateful_providers() {
        let stateful = descriptor(Resolves::new().with_endpoint("https://a.example.com").with_private_state());
        let stateless = descriptor(Resolves::new().with_endpoint("https://a.example.com"));
        let mut dry = signals();
        dry.dry_run = true;
        assert!(!ResolveInput::new(&stateful, kinds(&stateful, ResolveKind::Initial), &dry).is_dry_run(), "stateful");
        assert!(ResolveInput::new(&stateless, kinds(&stateless, ResolveKind::Initial), &dry).is_dry_run(), "stateless");
    }

    #[test]
    fn ec_id_is_only_for_providers_declared_with_ec_id() {
        let generic = descriptor(Resolves::new().with_host("a.example.com").with_ec_id());
        let other = descriptor(Resolves::new().with_host("a.example.com"));
        let signals = signals();
        ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &signals)
            .ec_id()
            .expect("generic pull reads the EC ID");
        let input = ResolveInput::new(&other, kinds(&other, ResolveKind::Initial), &signals);
        input.ec_id().expect_err("others may not");
        assert_eq!(input.recorded_errors().len(), 1, "should record the read");
    }

    #[test]
    fn stored_ids_and_expiry() {
        let record = crate::ec::kv_types::KvPartnerId {
            uid: "u".to_owned(),
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: Some(20),
            ra: Some(15),
            ru: None,
            atype: Some(3),
            ext: None,
        };
        let stored = StoredId::from_record(&record);
        assert_eq!(stored.writer().map(IdentityProviderId::as_str), Some("p"), "writer");
        assert!(!stored.is_expired(UnixTime::from_secs(19)), "not expired before exp");
        assert!(stored.is_expired(UnixTime::from_secs(20)), "expired at exp");
        assert_eq!(stored.refresh_at(), Some(UnixTime::from_secs(15)), "ra");
    }

    #[test]
    fn hashed_email_debug_is_redacted() {
        let email = HashedEmail::from_bytes([0xab; 32]);
        assert_eq!(email.to_hex(), "ab".repeat(32), "hex");
        assert_eq!(format!("{email:?}"), "HashedEmail([redacted])", "should redact Debug");
    }

    fn kinds(descriptor: &ProviderDescriptor, kind: ResolveKind) -> BTreeMap<SourceDomain, ResolveKind> {
        descriptor.sources().iter().map(|source| (source.clone(), kind)).collect()
    }

    #[test]
    fn capture_input_exposes_path_and_never_the_request_body() {
        let descriptor = ProviderDescriptor::builder("c".parse().expect("id"))
            .with_source("example.com".parse().expect("source"))
            .with_captures(Captures::new().with_path(http::Method::POST, "/integrations/c/api"))
            .build()
            .expect("should build");
        let response = crate::ec::identity::exchange::ProviderResponse::for_test(http::StatusCode::OK, b"{}");
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
        assert_eq!(input.path(), "/integrations/c/api", "path");
        assert_eq!(input.primary_source().as_str(), "example.com", "source");
    }
}
```

`ProviderResponse::for_test` arrives in Task 10. Until then, comment out the last test and enable it in Task 10, Step 4.

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
use super::exchange::ProviderResponse;
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::resolution::IdentityProviderError;
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;


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
    /// From digest bytes. No code in v1 calls this outside tests.
    #[must_use]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, as LiveRamp takes it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Standard base64, as UID2 takes it.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

/// A stored record for one of this provider's sources, valid or expired.
#[derive(Clone, PartialEq)]
pub struct StoredId {
    uid: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
    writer: Option<IdentityProviderId>,
    acquired_at: Option<UnixTime>,
    expires_at: Option<UnixTime>,
    refresh_at: Option<UnixTime>,
}

impl fmt::Debug for StoredId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredId")
            .field("uid", &"[redacted]")
            .field("writer", &self.writer)
            .field("acquired_at", &self.acquired_at)
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .finish_non_exhaustive()
    }
}

impl StoredId {
    /// From a KV record. A writer that does not parse is treated as none.
    #[must_use]
    pub(crate) fn from_record(record: &KvPartnerId) -> Self {
        Self {
            uid: record.uid.clone(),
            atype: record.atype,
            ext: record.ext.clone(),
            writer: record.w.as_deref().and_then(|writer| writer.parse().ok()),
            acquired_at: record.at.map(UnixTime::from_secs),
            expires_at: record.exp.map(UnixTime::from_secs),
            refresh_at: record.ra.map(UnixTime::from_secs),
        }
    }

    /// The UID.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// Provider-supplied `atype`.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.atype
    }

    /// UID-level ext, as bidders read it in `uids[].ext`.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.ext.as_ref()
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
pub(crate) struct SeededRandom(std::sync::Mutex<u64>);

#[cfg(any(test, feature = "test-utils"))]
impl SeededRandom {
    pub(crate) fn new(seed: u64) -> Self {
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
            *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
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
    pub(crate) dry_run: bool,
}

/// What one provider may read for one call.
pub struct ResolveInput<'a> {
    descriptor: &'a ProviderDescriptor,
    kinds: BTreeMap<SourceDomain, ResolveKind>,
    signals: &'a ResolveSignals<'a>,
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
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        kinds: BTreeMap<SourceDomain, ResolveKind>,
        signals: &'a ResolveSignals<'a>,
    ) -> Self {
        let available = descriptor
            .resolves()
            .map(|resolves| resolves.declared_inputs())
            .unwrap_or_default();
        Self {
            descriptor,
            kinds,
            signals,
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
        if !self.available.contains(&input) {
            return Err(self.record(Access::Input(input)));
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

    /// True during the dry run (spec 5.2). Always false for a provider
    /// declared `with_private_state()`.
    #[must_use]
    pub fn is_dry_run(&self) -> bool {
        self.signals.dry_run
            && !self.descriptor.resolves().is_some_and(|resolves| resolves.has_private_state())
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
    /// `UndeclaredInput` otherwise. `InputUnavailable` (as `ClientIp`, the
    /// nearest input) when the request has no EC ID.
    pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>> {
        if !self.descriptor.resolves().is_some_and(|resolves| resolves.gets_ec_id()) {
            return Err(self.record(Access::PrivateState));
        }
        self.signals
            .ec_id
            .as_deref()
            .ok_or_else(|| Report::new(IdentityProviderError::InputUnavailable { input: Input::ClientIp }))
    }
}

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

    /// The stricter of the cookie consent and the request-body consent.
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

`CaptureInput::new` takes 7 arguments, which is the maximum `AGENTS.md` allows. Do not add an eighth. Use a struct if you need one. `hex` is already a dependency of core (check with `grep -n '^hex' crates/trusted-server-core/Cargo.toml`). If it is only a build dependency, add `hex = { workspace = true }` to `[dependencies]`. `ResolveKind` stays in `descriptor.rs` and is re-exported from `mod.rs`. Update `mod.rs`:

```rust
pub mod input;
pub use input::{CaptureInput, HashedEmail, ProviderConsent, Region, ResolveInput, StoredId};
```

Add `ec/identity/input.rs` to `migration_guards.rs`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::input`
Expected: PASS for every test except the `capture_input_...` test, which waits for Task 10.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add declared-input enforcement for identity providers"
```

### Task 10: Requests, responses and resolutions

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/exchange.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/resolution.rs` (add `Resolution`, `ResolvedId`, `When`, `StateChange`)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: both files

**Interfaces:**

- Produces: everything listed in spec 4.5, plus:
  - `ProviderRequest::call_context_json(&self) -> Option<&str>` (crate-private)
  - `ProviderRequest::has_cookie_header(&self) -> bool` (crate-private)
  - `ProviderResponse::new(status: StatusCode, headers: HeaderMap, body: Vec<u8>, call_context: Option<String>) -> Self` (crate-private)
  - `ProviderResponse::for_test(status, body: &[u8]) -> Self` (test support)
  - `Resolution::found_ids_by_source(&self)`: the first ID per source, plus the count of duplicates dropped (crate-private)
  - `MAX_CALL_CONTEXT_BYTES = 1024`, `MAX_UID_EXT_BYTES = 256`

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
        let request = ProviderRequest::post_json(&url(), &json!({ "a": 1 })).expect("should build");
        assert_eq!(request.method(), Method::POST, "method");
        assert_eq!(request.headers()["content-type"], "application/json", "content type");
        assert_eq!(request.body(), br#"{"a":1}"#, "body");
    }

    #[test]
    fn query_bearer_and_headers() {
        let request = ProviderRequest::get(&url())
            .with_query("pid", "1 2")
            .with_bearer(&Redacted::new("token".to_owned()))
            .try_with_header("x-example", "v")
            .expect("should accept a valid header");
        assert_eq!(request.url().as_str(), "https://api.example.com/v1/fetch?pid=1+2", "should encode the query");
        assert_eq!(request.headers()["authorization"], "Bearer token", "bearer");
        assert_eq!(request.headers()["x-example"], "v", "header");
        ProviderRequest::get(&url()).try_with_header("bad header", "v").expect_err("should reject a bad name");
        ProviderRequest::get(&url()).try_with_header("x", "bad\nvalue").expect_err("should reject a bad value");
    }

    #[test]
    fn cookie_headers_are_flagged_from_either_helper() {
        let typed = ProviderRequest::get(&url())
            .with_header(HeaderName::from_static("cookie"), HeaderValue::from_static("a=1"));
        let dynamic = ProviderRequest::get(&url()).try_with_header("Cookie", "a=1").expect("should build");
        assert!(typed.has_cookie_header(), "typed helper");
        assert!(dynamic.has_cookie_header(), "dynamic helper");
        assert!(!ProviderRequest::get(&url()).has_cookie_header(), "control");
    }

    #[test]
    fn call_context_round_trips_and_is_capped() {
        let request = ProviderRequest::get(&url()).try_with_call_context(&json!({ "nonce": [1, 2] })).expect("ok");
        let response = ProviderResponse::new(
            StatusCode::OK,
            http::HeaderMap::new(),
            Vec::new(),
            request.call_context_json().map(str::to_owned),
        );
        let context: serde_json::Value = response.call_context().expect("decode").expect("present");
        assert_eq!(context, json!({ "nonce": [1, 2] }), "should round-trip");

        let error = ProviderRequest::get(&url())
            .try_with_call_context(&"x".repeat(MAX_CALL_CONTEXT_BYTES))
            .expect_err("should reject oversize");
        assert!(matches!(error.current_context(), IdentityProviderError::Config { .. }), "Config");
    }

    #[test]
    fn success_helpers_check_status_and_encoding() {
        let ok = ProviderResponse::for_test(StatusCode::OK, br#"{"uid":"u"}"#);
        let value: serde_json::Value = ok.success_json().expect("should parse");
        assert_eq!(value["uid"], "u", "uid");

        let rejected = ProviderResponse::for_test(StatusCode::FORBIDDEN, br#"{}"#);
        let error = rejected.success_json::<serde_json::Value>().expect_err("should reject");
        assert!(
            matches!(error.current_context(), IdentityProviderError::Rejected { status } if *status == StatusCode::FORBIDDEN),
            "Rejected with the status"
        );
        rejected.json::<serde_json::Value>().expect("json ignores the status");

        let binary = ProviderResponse::for_test(StatusCode::OK, &[0xff, 0xfe]);
        let error = binary.success_text().expect_err("should reject non-UTF-8");
        assert!(matches!(error.current_context(), IdentityProviderError::Malformed), "Malformed");
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
        name.parse().expect("should parse")
    }

    #[test]
    fn empty_found_is_no_match() {
        let resolution = Resolution::found([]);
        assert!(resolution.is_no_match(), "should be no_match");
        assert!(resolution.ids().is_empty(), "no ids");
    }

    #[test]
    fn found_keeps_the_first_id_per_source() {
        let a = source("a.example.com");
        let resolution = Resolution::found([ResolvedId::new(&a, "first"), ResolvedId::new(&a, "second")]);
        let (kept, dropped) = resolution.found_ids_by_source();
        assert_eq!(kept.len(), 1, "one per source");
        assert_eq!(kept[0].uid(), "first", "should keep the first");
        assert_eq!(dropped, 1, "should count the duplicate");
    }

    #[test]
    fn invalidations_chain_and_state_changes_are_reported() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let resolution = Resolution::found([ResolvedId::new(&a, "u")])
            .also_invalidate(&b)
            .with_retry_in(Duration::from_secs(60))
            .replacing_state(&json!({ "signature": "s" }));
        assert_eq!(resolution.invalidated(), &[b], "should invalidate b");
        assert_eq!(resolution.retry_in(), Some(Duration::from_secs(60)), "retry");
        assert_eq!(resolution.state_change(), StateChange::Replace(r#"{"signature":"s"}"#), "state");
        assert_eq!(Resolution::no_match().clearing_state().state_change(), StateChange::Clear, "clear");
        assert_eq!(Resolution::no_match().state_change(), StateChange::Keep, "keep");
    }

    #[test]
    fn oversized_state_is_marked_invalid_not_dropped_silently() {
        let resolution = Resolution::found([ResolvedId::new(&source("a.example.com"), "u")])
            .replacing_state(&"x".repeat(2000));
        assert!(resolution.state_rejected(), "should flag the state as rejected");
    }

    #[test]
    fn oversized_ext_is_dropped_and_the_id_kept() {
        let id = ResolvedId::new(&source("a.example.com"), "u").with_ext(json!({ "pad": "x".repeat(300) }));
        assert_eq!(id.uid(), "u", "should keep the id");
        assert!(id.ext().is_none(), "should drop the ext");
        let small = ResolvedId::new(&source("a.example.com"), "u").with_ext(json!({ "linkType": 2 }));
        assert_eq!(small.ext(), Some(&json!({ "linkType": 2 })), "control: a small ext is kept");
    }

    #[test]
    fn opted_out_is_reported() {
        assert!(Resolution::opted_out().is_opted_out(), "opted out");
        assert!(!Resolution::no_match().is_opted_out(), "control");
    }
}
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::exchange` and `cargo test-fastly -- ec::identity::resolution`
Expected: compile errors.

- [ ] **Step 3: Implement `exchange.rs`**

```rust
//! What a provider sends and receives. The coordinator does all I/O.

use core::fmt;

use base64::Engine as _;
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

Check where `Redacted` lives (`grep -rn "pub struct Redacted" crates/trusted-server-core/src`) and fix the import. Drop the unused `base64` import if clippy flags it.

- [ ] **Step 4: Implement the rest of `resolution.rs`**

Append to the file from Task 5:

```rust
use std::time::Duration;

use serde::Serialize;

use super::ids::{SourceDomain, UnixTime};
use crate::ec::kv_types::{MAX_PARTNER_EXT_BYTES, MAX_PROVIDER_STATE_BYTES};

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

/// One ID a provider returned.
#[derive(Clone, PartialEq)]
pub struct ResolvedId {
    source: SourceDomain,
    uid: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
    expiry: Option<When>,
    refresh: Option<When>,
    refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for ResolvedId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedId")
            .field("source", &self.source)
            .field("uid", &"[redacted]")
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

impl ResolvedId {
    /// An ID for `source`.
    #[must_use]
    pub fn new(source: &SourceDomain, uid: impl Into<String>) -> Self {
        Self {
            source: source.clone(),
            uid: uid.into(),
            atype: None,
            ext: None,
            expiry: None,
            refresh: None,
            refresh_until: None,
        }
    }

    /// Sets `atype`.
    #[must_use]
    pub fn with_atype(mut self, atype: i32) -> Self {
        self.atype = Some(atype);
        self
    }

    /// UID-level ext. Over 256 serialized bytes it is dropped and the ID kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = serde_json::to_vec(&ext).is_ok_and(|bytes| bytes.len() <= MAX_PARTNER_EXT_BYTES);
        self.ext = fits.then_some(ext);
        if !fits {
            log::debug!("Dropping oversized ext for source {}", self.source);
        }
        self
    }

    /// Expires after `delay`.
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

    /// The UID.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
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

#[derive(Debug, Clone, PartialEq)]
enum StateOp {
    Keep,
    Replace(String),
    Clear,
    Rejected,
}

/// What one provider call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    ids: Vec<ResolvedId>,
    invalidated: Vec<SourceDomain>,
    opted_out: bool,
    retry_in: Option<Duration>,
    state: StateOp,
}

impl Resolution {
    fn empty() -> Self {
        Self { ids: Vec::new(), invalidated: Vec::new(), opted_out: false, retry_in: None, state: StateOp::Keep }
    }

    /// One or more IDs, at most one per source. An empty set is `no_match`.
    #[must_use]
    pub fn found(ids: impl IntoIterator<Item = ResolvedId>) -> Self {
        Self { ids: ids.into_iter().collect(), ..Self::empty() }
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

    /// The returned IDs, as given.
    #[must_use]
    pub fn ids(&self) -> &[ResolvedId] {
        &self.ids
    }

    /// Invalidated sources.
    #[must_use]
    pub fn invalidated(&self) -> &[SourceDomain] {
        &self.invalidated
    }

    /// Whether nothing was found, invalidated or opted out.
    #[must_use]
    pub fn is_no_match(&self) -> bool {
        self.ids.is_empty() && self.invalidated.is_empty() && !self.opted_out
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

    /// The first ID per source, in order, and how many duplicates were dropped.
    #[must_use]
    pub(crate) fn found_ids_by_source(&self) -> (Vec<&ResolvedId>, usize) {
        let mut seen = std::collections::BTreeSet::new();
        let mut kept = Vec::new();
        let mut dropped = 0;
        for id in &self.ids {
            if seen.insert(id.source()) {
                kept.push(id);
            } else {
                dropped += 1;
            }
        }
        (kept, dropped)
    }
}
```

Add `exchange.rs` to `mod.rs` (`pub mod exchange; pub use exchange::{ProviderRequest, ProviderResponse};`) and `pub use resolution::{Resolution, ResolvedId, StateChange, When};`. Add `exchange.rs` to `migration_guards.rs`. Uncomment the capture test from Task 9.

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
            ProviderDescriptor::builder("p".parse().expect("id"))
                .with_source("example.com".parse().expect("source"))
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

        let error = boxed.capture(&input).expect_err("should be the marker");
        assert!(error.current_context().is_not_implemented(), "should carry the marker");
        assert!(
            !IdentityProviderError::config("other").current_context().is_not_implemented(),
            "control: an ordinary Config error is not the marker"
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

Check where `IntegrationConfig` lives (`grep -rn "pub trait IntegrationConfig" crates/trusted-server-core/src`) and fix the path. Add `pub mod provider; pub use provider::{IdentityProvider, IdentityProviderConfig};` to `mod.rs`, and the file to `migration_guards.rs`.

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
- Modify: `crates/trusted-server-core/src/consent/tcf.rs` (add `has_vendor_consent`)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `policy.rs`, `consent/tcf.rs`

**Interfaces:**

- Consumes: `ConsentPolicy` (Task 8), `ConsentContext`, `allows_eid_persistence`, `ec_consent_granted`, and the TCF and GPP decoded forms in `consent/`
- Produces:
  - `pub(crate) struct PolicySignals { dnt: bool }`. DNT is not in `ConsentContext`, so the caller passes it.
  - `pub(crate) fn policy_allows(policy: &ConsentPolicy, consent: &ConsentContext, signals: PolicySignals) -> bool`
  - `pub(crate) fn tcf_applies(consent: &ConsentContext) -> bool`, the same test `consent_allows_server_side_auction` uses (`consent/mod.rs:298-308`)
  - `pub(crate) fn has_vendor_consent(tcf: &DecodedTcf, vendor_id: u16) -> bool` in `consent/tcf.rs`

Part 2 (Task 34) extends `policy_allows` with the sharing and targeted-advertising opt-outs, GPP section 6, and the GPC and known-child fields. Until then, a strict policy blocks on the signals already decoded: TCF purposes and vendor, the US sale opt-out (GPP and US Privacy), `Sec-GPC`, DNT, and any raw string present without a decoded form.

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
        assert!(policy_allows(&strict, &allowed, PolicySignals { dnt: false }), "control: nothing set");

        for (name, signals, dnt) in [
            ("gdpr without purpose 4", Signals { gdpr: true, purposes: vec![1], ..Signals::default() }, false),
            ("us sale opt-out", Signals { us_sale_opt_out: true, ..Signals::default() }, false),
            ("gpc", Signals { gpc: true, ..Signals::default() }, false),
            ("dnt", Signals::default(), true),
            ("undecodable tc string", Signals { raw_tc_without_decode: true, ..Signals::default() }, false),
        ] {
            assert!(
                !policy_allows(&strict, &consent_with(signals), PolicySignals { dnt }),
                "strict should block: {name}"
            );
        }
    }

    #[test]
    fn relaxations_and_vendor_consent() {
        let gpc = consent_with(Signals { gpc: true, ..Signals::default() });
        assert!(policy_allows(&ConsentPolicy::strict().allowing_gpc(), &gpc, PolicySignals { dnt: false }), "gpc relaxed");
        let sale = consent_with(Signals { us_sale_opt_out: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sale), &sale, PolicySignals { dnt: false }),
            "sale relaxed"
        );
        let gdpr = consent_with(Signals { gdpr: true, purposes: vec![1, 4], vendors: vec![9999], ..Signals::default() });
        assert!(policy_allows(&ConsentPolicy::strict().with_tcf_vendor(9999), &gdpr, PolicySignals { dnt: false }), "vendor granted");
        assert!(!policy_allows(&ConsentPolicy::strict().with_tcf_vendor(1234), &gdpr, PolicySignals { dnt: false }), "vendor missing");
    }

    #[test]
    fn eid_persistence_only_ignores_provider_signals() {
        let consent = consent_with(Signals { gpc: true, us_sale_opt_out: true, ..Signals::default() });
        assert!(
            policy_allows(&ConsentPolicy::eid_persistence_only(), &consent, PolicySignals { dnt: true }),
            "generic pull's parity policy checks only the global gates, which the coordinator applies separately"
        );
    }
}
```

`Signals` and `consent_with` are a small test helper you write in this module. It builds a `ConsentContext` with GDPR applying, the given TCF purposes and vendor consents decoded, the US sale opt-out decoded from a US Privacy string `1YYN`, `gpc` set, and, for `raw_tc_without_decode`, `raw_tc_string = Some(..)` with no decoded TCF. Build it on whatever constructor the consent tests use. Read `consent/types.rs` first.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::policy`
Expected: compile errors.

- [ ] **Step 3: Implement**

Add to `consent/tcf.rs`, next to the purpose-consent helpers at about line 145:

```rust
/// Whether the TC string grants consent to an IAB Global Vendor List vendor.
#[must_use]
pub(crate) fn has_vendor_consent(tcf: &TcfConsent, vendor_id: u16) -> bool {
    tcf.vendor_consents.contains(&vendor_id)
}
```

Use the real decoded type name and its vendor-consent field. Read the struct at the top of `tcf.rs`. If vendor consents are stored as a bitfield, index into it instead. Add a unit test next to the existing TCF tests that decodes a fixture TC string already used in that file and asserts one vendor present and one absent.

`policy.rs`:

```rust
//! Evaluates a provider's [`ConsentPolicy`] against a request's consent.

use super::descriptor::{ConsentPolicy, UsOptOut};
use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;

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
        let Some(tcf) = consent.tcf.as_ref() else {
            return false;
        };
        if !policy.tcf_purposes().iter().all(|purpose| tcf.has_purpose_consent(*purpose)) {
            return false;
        }
        if let Some(vendor) = policy.tcf_vendor()
            && !crate::consent::tcf::has_vendor_consent(tcf, vendor)
        {
            return false;
        }
    }
    if !policy.allows_us_opt_out(UsOptOut::Sale) && crate::consent::us_sale_opted_out(consent) {
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

Every `consent.*` field name and helper above (`tcf`, `gpp`, `us_privacy`, `gpc`, `jurisdiction`, `has_purpose_consent`, `us_sale_opted_out`) must be matched to the real API in `consent/`. Run `grep -n "pub fn\|pub(crate) fn\|pub [a-z_]*:" crates/trusted-server-core/src/consent/*.rs` and use what exists. If there is no single US-sale helper, combine `decode_us_sale_opt_out` (`gpp.rs:125`) and the US Privacy check (`us_privacy.rs:50-63`) in a private `fn us_sale_opt_out(consent: &ConsentContext) -> bool` in this file. Do not change any existing consent function's behaviour.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::policy` and `cargo test-fastly -- consent::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/consent/tcf.rs crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Evaluate provider consent policies against decoded consent"
```

### Task 13: Revisions and keyed hashes

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/revision.rs`
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `revision.rs`

**Interfaces:**

- Produces:
  - `pub(crate) struct IdentityKeys`, derived once per settings load, with `pub(crate) fn from_settings(settings: &Settings) -> Self`. It holds an HMAC key for each of: `revision` (legacy `pv` fallback), `invalidated` (`ps.iv`), `visitor` (rate-limit key), `fingerprint` (consent), and the AES keys `cookie_s` and `cookie_r` (Task 16 uses them). Labels: `trusted-server/identity/revision/v1`, `trusted-server/identity/iv/v1`, `trusted-server/identity/visitor/v1`, `trusted-server/identity/fingerprint/v1`, `trusted-server/ts-eids-s/key/v1`, `trusted-server/ts-eids-r/key/v1`.
  - `pub(crate) struct Revision(String)`: 16 lowercase hex digits, with `as_str`
  - `pub(crate) fn new_revision(random: &dyn RandomSource) -> Revision`: 8 random bytes as hex
  - `pub(crate) fn record_revision(keys: &IdentityKeys, record: &KvPartnerId) -> Revision`: `rv` when present, else the first 16 hex digits of HMAC(`revision` key, `uid \0 at`)
  - `pub(crate) fn invalidated_uid_hash(keys: &IdentityKeys, provider: &IdentityProviderId, uid: &str) -> String`: 16 hex
  - `pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String`: 16 hex
  - `pub(crate) fn consent_fingerprint(keys: &IdentityKeys, bits: &str) -> String`: 16 hex. Task 34 builds `bits`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::kv_types::KvPartnerId;
    use crate::test_support::tests::create_test_settings;

    fn keys() -> IdentityKeys {
        IdentityKeys::from_settings(&create_test_settings())
    }

    #[test]
    fn new_revisions_are_hex_and_differ() {
        let random = SeededRandom::new(7);
        let first = new_revision(&random);
        let second = new_revision(&random);
        assert_eq!(first.as_str().len(), 16, "16 hex digits");
        assert!(first.as_str().bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "lowercase hex");
        assert_ne!(first, second, "two writes of the same UID get different revisions");
    }

    #[test]
    fn record_revision_prefers_rv_and_hashes_legacy_records() {
        let keys = keys();
        let with_rv = KvPartnerId { rv: Some("0123456789abcdef".to_owned()), ..KvPartnerId::new("u") };
        assert_eq!(record_revision(&keys, &with_rv).as_str(), "0123456789abcdef", "should use rv");

        let legacy_a = KvPartnerId::browser("u", Some(10));
        let legacy_b = KvPartnerId::browser("u", Some(11));
        assert_eq!(record_revision(&keys, &legacy_a), record_revision(&keys, &legacy_a.clone()), "stable");
        assert_ne!(record_revision(&keys, &legacy_a), record_revision(&keys, &legacy_b), "at changes it");
    }

    #[test]
    fn hashes_are_keyed_and_namespaced() {
        let keys = keys();
        let provider: IdentityProviderId = "p".parse().expect("id");
        let iv = invalidated_uid_hash(&keys, &provider, "u");
        let visitor = visitor_key(&keys, "u");
        assert_eq!(iv.len(), 16, "iv length");
        assert_ne!(iv, visitor, "different labels give different hashes for the same input");

        let mut other_settings = create_test_settings();
        other_settings.ec.passphrase = crate::redacted::Redacted::new("another-example-passphrase-value".to_owned());
        let other = IdentityKeys::from_settings(&other_settings);
        assert_ne!(invalidated_uid_hash(&other, &provider, "u"), iv, "keyed by the passphrase");
    }
}
```

Find the settings test helper with `grep -rn "fn create_test_settings" crates/trusted-server-core/src`, and fix the import path to match.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::revision`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Record revisions and keyed hashes of identifiers.

use hmac::{Hmac, Mac as _};
use sha2::Sha256;

use super::ids::IdentityProviderId;
use super::input::RandomSource;
use crate::ec::kv_types::KvPartnerId;
use crate::settings::Settings;

type HmacSha256 = Hmac<Sha256>;

/// Keys derived from `ec.passphrase`, once per settings load.
#[derive(Clone)]
pub(crate) struct IdentityKeys {
    revision: [u8; 32],
    invalidated: [u8; 32],
    visitor: [u8; 32],
    fingerprint: [u8; 32],
    pub(crate) cookie_s: [u8; 32],
    pub(crate) cookie_r: [u8; 32],
}

impl core::fmt::Debug for IdentityKeys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("IdentityKeys([redacted])")
    }
}

fn derive(passphrase: &[u8], label: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(passphrase).expect("should accept any HMAC key length");
    mac.update(label);
    mac.finalize().into_bytes().into()
}

fn keyed_hex16(key: &[u8; 32], parts: &[&[u8]]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("should accept a 32-byte key");
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            mac.update(b"\0");
        }
        mac.update(part);
    }
    hex::encode(&mac.finalize().into_bytes()[..8])
}

impl IdentityKeys {
    /// Derives every identity key from `ec.passphrase`.
    #[must_use]
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let passphrase = settings.ec.passphrase.expose().as_bytes();
        Self {
            revision: derive(passphrase, b"trusted-server/identity/revision/v1"),
            invalidated: derive(passphrase, b"trusted-server/identity/iv/v1"),
            visitor: derive(passphrase, b"trusted-server/identity/visitor/v1"),
            fingerprint: derive(passphrase, b"trusted-server/identity/fingerprint/v1"),
            cookie_s: derive(passphrase, b"trusted-server/ts-eids-s/key/v1"),
            cookie_r: derive(passphrase, b"trusted-server/ts-eids-r/key/v1"),
        }
    }
}

/// A record's opaque revision: 16 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Revision(String);

impl Revision {
    /// Wraps a stored `rv` or `pv`. Callers have validated the format.
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

/// A fresh random revision, drawn on every write of a record.
#[must_use]
pub(crate) fn new_revision(random: &dyn RandomSource) -> Revision {
    let mut bytes = [0u8; 8];
    random.fill(&mut bytes);
    Revision(hex::encode(bytes))
}

/// The revision of a stored record: its `rv`, or for a legacy record without
/// one, the keyed hash of its `(uid, at)` until its next write.
#[must_use]
pub(crate) fn record_revision(keys: &IdentityKeys, record: &KvPartnerId) -> Revision {
    if let Some(rv) = &record.rv {
        return Revision(rv.clone());
    }
    let at = record.at.unwrap_or_default().to_string();
    Revision(keyed_hex16(&keys.revision, &[record.uid.as_bytes(), at.as_bytes()]))
}

/// The keyed hash stored in `ps.iv` for a UID the owner invalidated.
#[must_use]
pub(crate) fn invalidated_uid_hash(keys: &IdentityKeys, provider: &IdentityProviderId, uid: &str) -> String {
    keyed_hex16(&keys.invalidated, &[provider.as_str().as_bytes(), uid.as_bytes()])
}

/// The per-visitor rate-limit key component, from the full EC ID.
#[must_use]
pub(crate) fn visitor_key(keys: &IdentityKeys, ec_id: &str) -> String {
    keyed_hex16(&keys.visitor, &[ec_id.as_bytes()])
}

/// The consent fingerprint over the signals a provider's policy reads.
#[must_use]
pub(crate) fn consent_fingerprint(keys: &IdentityKeys, bits: &str) -> String {
    keyed_hex16(&keys.fingerprint, &[bits.as_bytes()])
}
```

Check that `Settings.ec.passphrase` is `Redacted<String>` with `.expose()`. `pull_sync_marker.rs:293` uses exactly that. Register the module in `mod.rs` (`pub(crate) mod revision;`) and in `migration_guards.rs`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::revision`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add record revisions and keyed identity hashes"
```

---

### Task 14: The provider catalog and the shared servability check

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/catalog.rs`
- Create: `crates/trusted-server-core/src/ec/identity/servable.rs`
- Modify: `crates/trusted-server-core/src/ec/eids.rs` (`resolve_partner_ids`, and the stale module comment at lines 1-5)
- Modify: callers of `resolve_partner_ids`: `auction/endpoints.rs:465`, `ec/admin.rs:485`
- Modify: `crates/trusted-server-core/src/ec/identify.rs` (`entry.ids` read at about lines 99-103)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `servable.rs`, `ec/eids.rs`, `ec/identify.rs`

**Interfaces:**

- Produces:
  - `pub(crate) struct ProviderCatalog` with `pub(crate) fn new(providers: Vec<Arc<dyn IdentityProvider>>, owners: BTreeMap<SourceDomain, IdentityProviderId>) -> Self`, `get(&IdentityProviderId) -> Option<&Arc<dyn IdentityProvider>>`, `providers() -> impl Iterator<Item = &Arc<dyn IdentityProvider>>`, `owner_of(&SourceDomain) -> Option<&IdentityProviderId>`, `is_configured(&IdentityProviderId) -> bool`, `configured_sources() -> &[SourceDomain]` (sorted and de-duplicated, the order that `ts-eids-s` indexes use), `fingerprint() -> &str` (`cf`: the first 8 hex digits of SHA-256 over the sorted `provider|source|owner` lines), and `ProviderCatalog::empty()`. Part 1 builds it from generic pull providers only (Task 22), with no owners.
  - `pub(crate) struct ServeContext<'a> { now: UnixTime, consent: &'a ConsentContext, signals: PolicySignals, catalog: &'a ProviderCatalog, opted_out: &'a BTreeSet<IdentityProviderId> }`
  - `pub(crate) fn is_servable(context: &ServeContext<'_>, source: &SourceDomain, record: &KvPartnerId) -> bool`
  - `resolve_partner_ids(registry: &PartnerRegistry, entry: &KvEntry, serve: &ServeContext<'_>) -> Vec<ResolvedPartnerId>`. The new third argument is required. `ResolvedPartnerId` gains `atype` from the record (`record.atype.unwrap_or(partner.openrtb_atype)`), `ext: Option<serde_json::Value>`, and `writer: Option<IdentityProviderId>`.

The rule (spec 6.1 and 7.1), applied in this order:

1. An empty UID is not servable.
2. `exp` at or before `now` is not servable.
3. A record with a writer `w`:
   - The writer must be in the catalog. A disabled or unconfigured writer means not servable.
   - The writer must not be in `opted_out`, which is the union of KV `ps.o` and the `ts-eids-r` opt-outs. The caller computes it.
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
    use crate::ec::identity::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::provider::IdentityProvider;
    use crate::ec::kv_types::KvPartnerId;

    struct Fixed(ProviderDescriptor);
    impl IdentityProvider for Fixed {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.0
        }
    }

    fn provider(id: &str, source: &str, policy: ConsentPolicy, expiring: bool) -> Arc<dyn IdentityProvider> {
        let mut builder = ProviderDescriptor::builder(id.parse().expect("id"))
            .with_source(source.parse().expect("source"))
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
        is_servable(&context, &source.parse().expect("source"), record)
    }

    #[test]
    fn expiry_writer_presence_and_opt_out() {
        let catalog = ProviderCatalog::new(
            vec![provider("p", "a.example.com", ConsentPolicy::eid_persistence_only(), false)],
            BTreeMap::new(),
        );
        let none = BTreeSet::new();
        assert!(check(&catalog, &none, "a.example.com", &record(Some("p"), Some(101))), "control: valid");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("p"), Some(100))), "expired at exp");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("gone"), None)), "unknown writer");
        let opted: BTreeSet<_> = ["p".parse().expect("id")].into_iter().collect();
        assert!(!check(&catalog, &opted, "a.example.com", &record(Some("p"), None)), "opted-out writer");
        assert!(check(&catalog, &none, "b.example.com", &record(None, None)), "browser record, unowned");
    }

    #[test]
    fn rollback_rule_for_expiring_owned_sources() {
        let owner: IdentityProviderId = "owner".parse().expect("id");
        let catalog = ProviderCatalog::new(
            vec![provider("owner", "a.example.com", ConsentPolicy::eid_persistence_only(), true)],
            [("a.example.com".parse().expect("source"), owner)].into_iter().collect(),
        );
        let none = BTreeSet::new();
        assert!(check(&catalog, &none, "a.example.com", &record(Some("owner"), Some(200))), "control: owner value");
        assert!(!check(&catalog, &none, "a.example.com", &record(None, None)), "browser-seeded, never from server");
        assert!(!check(&catalog, &none, "a.example.com", &record(Some("owner"), None)), "stripped exp");
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
        let source = "a.example.com".parse().expect("source");
        assert!(!is_servable(&context, &source, &record(Some("p"), None)), "writer's vendor denied");
        assert!(is_servable(&context, &source, &record(None, None)), "control: browser value unaffected");
    }
}
```

`test_consent_allowing_everything` and `test_consent_gdpr_without_vendor` reuse the helper you wrote in Task 12. Move it to `ec/identity/test_support.rs` now (create the file with `#![cfg(any(test, feature = "test-utils"))]` items and `pub(crate) mod test_support;` in `mod.rs`) so every identity test can share it.

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
        entry.ids.get_mut("a.example.com").expect("a").atype = Some(3);

        let resolved = resolve_partner_ids(&registry, &entry, &serve_context_at(100));

        assert_eq!(resolved.len(), 1, "should drop the expired b");
        assert_eq!(resolved[0].source_domain, "a.example.com", "should keep a");
        assert_eq!(resolved[0].openrtb_atype, 3, "should prefer the stored atype");
    }
```

Write `registry_with` and `serve_context_at` as local helpers. `registry_with` follows the existing test's registry setup, and `serve_context_at` uses `ProviderCatalog::empty()` with permissive consent.

In `ec/identify.rs` tests, add one that stores a row with an expired record and a valid one and checks that the identify response lists only the valid one. Copy the setup from the nearest existing identify test (`grep -n "fn .*identify" crates/trusted-server-core/src/ec/identify.rs`).

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::servable`, `cargo test-fastly -- ec::eids`, `cargo test-fastly -- ec::identify`
Expected: compile errors, then the identify test fails because it returns the expired ID.

- [ ] **Step 3: Implement `catalog.rs`**

```rust
//! The enabled identity providers and source ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::ids::{IdentityProviderId, SourceDomain};
use super::provider::IdentityProvider;

/// Enabled providers, source owners, and the derived source index and `cf`.
/// Built once per settings load.
#[derive(Clone)]
pub(crate) struct ProviderCatalog {
    providers: BTreeMap<IdentityProviderId, Arc<dyn IdentityProvider>>,
    owners: BTreeMap<SourceDomain, IdentityProviderId>,
    sources: Vec<SourceDomain>,
    fingerprint: String,
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
        let sources: Vec<SourceDomain> = providers
            .values()
            .flat_map(|provider| provider.descriptor().sources().iter().cloned())
            .chain(owners.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut hasher = Sha256::new();
        for (id, provider) in &providers {
            for source in provider.descriptor().sources() {
                let owner = owners.get(source).map(IdentityProviderId::as_str).unwrap_or_default();
                hasher.update(format!("{id}|{source}|{owner}\n").as_bytes());
            }
        }
        let fingerprint = hex::encode(&hasher.finalize()[..4]);
        Self { providers, owners, sources, fingerprint }
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

    /// Every configured source, sorted. Indexes into this list are the
    /// `s` values of `ts-eids-s`.
    #[must_use]
    pub(crate) fn configured_sources(&self) -> &[SourceDomain] {
        &self.sources
    }

    /// `cf`: 8 hex digits over providers, sources and owners.
    #[must_use]
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
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
    /// KV `ps.o` plus `ts-eids-r` opt-outs.
    pub(crate) opted_out: &'a BTreeSet<IdentityProviderId>,
}

/// Whether a stored record may be sent from the server.
#[must_use]
pub(crate) fn is_servable(context: &ServeContext<'_>, source: &SourceDomain, record: &KvPartnerId) -> bool {
    if record.uid.is_empty() {
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

In `ec/eids.rs`, replace the module comment at lines 1-5 with one that says KV IDs are filtered by the shared servability check in `ec::identity::servable`. Add `serve: &ServeContext<'_>` to `resolve_partner_ids`, and inside the loop add `if !is_servable(serve, &source, partner_uid) { continue; }` after parsing `source_domain` into a `SourceDomain`. Skip any key that does not parse. Take `atype` from the record and keep `ext` and the writer on `ResolvedPartnerId` (Task 28 adds provenance in `to_eids`). Update `to_eids` to emit the UID-level `ext` when present.

At each caller, build a `ServeContext`. Until Task 23 puts the catalog and `ts-eids-r` opt-outs on `EcContext`, pass `ProviderCatalog::empty()` and the KV `ps.o` keys as `opted_out`. Task 23 replaces both. `now` comes from `checked_current_timestamp()`. If that fails, resolve no KV IDs, because a zero time would make every expiry check pass (spec 4.4). `consent` is the request's `ConsentContext`. DNT is read from the request header there (`auction/routing.rs:336` shows how).

In `ec/identify.rs`, replace the direct `entry.ids` read with `resolve_partner_ids`, using the same context built from the request's consent, never the row's stored `consent` (spec 7.1).

- [ ] **Step 6: Run and see them pass**

Run: `cargo test-fastly -- ec::` and `cargo test-fastly -- auction::endpoints`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Filter served KV IDs through one servability check" -m "Expired IDs, IDs from disabled or opted-out writers, and IDs whose writer or owner policy fails are no longer sent to bidders, the admin view or /identify. /identify uses the request's consent, never the row's stored snapshot."
```

### Task 15: `IdentityMutation` and its merge into a live row

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/mutation.rs`
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `mutation.rs`

**Interfaces:**

- Consumes: `KvEntry`, `KvPartnerId`, `KvProviderState`, `KvOptOut` (Task 6), `Revision`, `new_revision`, `record_revision`, `invalidated_uid_hash`, `IdentityKeys` (Task 13), `ProviderCatalog` (Task 14), `RandomSource` (Task 9)
- Produces:
  - `pub(crate) enum PartnerIdChange`:
    - `FillMissing { source, record: KvPartnerId, writer: Option<IdentityProviderId>, staged_at: Option<u64> }`
    - `OwnerWrite { source, record, writer, expected: Option<Revision>, staged_at: Option<u64> }`
    - `OwnerRemove { source, writer, expected: Revision }`
    - `WriterRemove { source, writer, staged_at: Option<u64> }`
  - `pub(crate) enum ExpectedState { NotRead, Absent, Json(String) }`
  - `pub(crate) enum StateUpdate { Keep, Replace(serde_json::Value), Clear }`
  - `pub(crate) struct ProviderStateChange { n: Option<u64>, f: Option<u32>, opt_out: Option<KvOptOut>, opt_out_clear: Option<u64>, iv_add: Vec<String>, st: StateUpdate }`, deriving `Default`
  - `pub(crate) struct ProviderResult { provider, ids: Vec<PartnerIdChange>, state: ProviderStateChange, expected_state: ExpectedState }`
  - `pub(crate) struct IdentityMutation`, deriving `Default`, with `add_change(PartnerIdChange)`, `add_result(ProviderResult)`, `merge_state(IdentityProviderId, ProviderStateChange)`, `set_cluster_size(u32)`, `is_empty()`, `has_removals()`
  - `pub(crate) struct ApplyContext<'a> { now: u64, keys: &'a IdentityKeys, random: &'a dyn RandomSource, catalog: &'a ProviderCatalog }`
  - `pub(crate) struct Applied { entry: KvEntry, changed: bool, removals_only: bool }`
  - `pub(crate) fn apply(mutation: &IdentityMutation, live: &KvEntry, context: &ApplyContext<'_>) -> Option<Applied>`. `None` means drop everything: the live row is a tombstone, or even the removals-only result failed validation.

Apply order (spec 5.4). Copy it into the doc comment of `apply`:

1. A tombstone (`consent.ok == false`) gives `None`.
2. Removals. Every `OwnerRemove` whose `expected` equals the live record's revision. Every `WriterRemove` whose live record has `w == writer` and, when `staged_at` is set, `at <= staged_at` (equal seconds go to the removal). Each new opt-out applies when `at > live.o.at` (or there is no live `o`) and `at >= live.oc` (or there is no live `oc`). Applying it removes every live record that provider wrote, at any revision, and clears its `st`. Then each staged clear (`opt_out_clear`) that is strictly later than the provider's latest opt-out `at`, live or staged, is stored as the live `oc`. A clear that is not later is dropped. `iv_add` hashes are appended, de-duplicated, and trimmed to the newest 4.
3. Positive changes. Skip every change by a provider whose (post-step-2) live `o` is set and not cleared (`oc` strictly later than `o.at`), and skip a staged change whose `staged_at <= o.at` even after a clear. A `FillMissing` applies when the source is absent, or its record is expired, or its writer is not configured. It is skipped when its UID's hash is in the source owner's `ps.iv`. An `OwnerWrite` applies when the live revision equals `expected`, or when `expected` is `None` and the source has no valid record with `w == writer`. A written record gets `w = writer`, a fresh `rv`, and `at = record.at.or(now)`. Each `ProviderResult` applies only when every one of its ID changes would apply on its own and the live `ps.st` matches `expected_state`. Its state change then applies with its IDs. Otherwise the whole result is dropped.
4. `ps` changes: `n` takes the later value, `f` is replaced, `st` is replaced or cleared. Then `cluster_size`. Then prune the `ps` entries of providers the catalog does not hold. Then drop expired records, except refresh anchors: the source's owner holds a `st` and the record's `ru` has not passed.
5. `validate()`. On failure, rebuild from the live entry with only step 2 applied (removals, opt-outs, clears, `iv`) and validate again. If that also fails, give `None` and log.
6. `changed` is whether the result differs from `live`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::revision::IdentityKeys;
    use crate::ec::identity::test_support::{fixed_provider, test_keys};
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

    const NOW: u64 = 1_000;

    fn src(name: &str) -> SourceDomain {
        name.parse().expect("source")
    }

    fn pid(name: &str) -> IdentityProviderId {
        name.parse().expect("id")
    }

    fn catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![fixed_provider("owner", &["a.example.com"]), fixed_provider("other", &["b.example.com"])],
            [(src("a.example.com"), pid("owner"))].into_iter().collect(),
        )
    }

    fn run(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let keys = test_keys();
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply(mutation, live, &ApplyContext { now: NOW, keys: &keys, random: &random, catalog: &catalog })
    }

    fn owned(uid: &str, rv: &str) -> KvPartnerId {
        KvPartnerId { w: Some("owner".to_owned()), rv: Some(rv.to_owned()), at: Some(900), ..KvPartnerId::new(uid) }
    }

    #[test]
    fn tombstone_drops_everything() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::FillMissing {
            source: src("b.example.com"),
            record: KvPartnerId::browser("u", Some(NOW)),
            writer: None,
            staged_at: None,
        });
        assert!(run(&mutation, &KvEntry::tombstone(1)).is_none(), "should drop on a tombstone");
    }

    #[test]
    fn fill_missing_fills_absent_and_expired_only() {
        let mut live = KvEntry::minimal("b.example.com", "existing", 1);
        live.ids.insert("c.example.com".to_owned(), KvPartnerId { exp: Some(NOW), ..KvPartnerId::new("old") });
        let mut mutation = IdentityMutation::default();
        for source in ["b.example.com", "c.example.com", "d.example.com"] {
            mutation.add_change(PartnerIdChange::FillMissing {
                source: src(source),
                record: KvPartnerId::browser("new", Some(NOW)),
                writer: None,
                staged_at: None,
            });
        }

        let applied = run(&mutation, &live).expect("should apply");

        assert!(applied.changed, "should change");
        assert_eq!(applied.entry.ids["b.example.com"].uid, "existing", "should not replace a live value");
        assert_eq!(applied.entry.ids["c.example.com"].uid, "new", "should replace an expired value");
        assert_eq!(applied.entry.ids["d.example.com"].uid, "new", "should fill an absent source");
    }

    #[test]
    fn owner_write_checks_revision_and_gets_a_fresh_rv() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));

        let mut stale = IdentityMutation::default();
        stale.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: Some(Revision::from_stored("2222222222222222")),
            staged_at: None,
        });
        let applied = run(&stale, &live).expect("should apply");
        assert!(!applied.changed, "a stale revision loses to the newer write");

        let mut fresh = IdentityMutation::default();
        fresh.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: Some(Revision::from_stored("1111111111111111")),
            staged_at: None,
        });
        let applied = run(&fresh, &live).expect("should apply");
        let record = &applied.entry.ids["a.example.com"];
        assert_eq!(record.uid, "v2", "should write");
        assert_eq!(record.w.as_deref(), Some("owner"), "should set the writer");
        assert_ne!(record.rv.as_deref(), Some("1111111111111111"), "should draw a new rv");
        assert_eq!(record.at, Some(NOW), "should stamp at");
    }

    #[test]
    fn browser_seeded_value_never_blocks_the_owner() {
        let mut live = KvEntry::minimal("a.example.com", "seeded", 1);
        live.ids.get_mut("a.example.com").expect("a").at = Some(5);
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("owned"),
            writer: pid("owner"),
            expected: None,
            staged_at: None,
        });
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(applied.entry.ids["a.example.com"].uid, "owned", "owner takes over a seeded value");
    }

    #[test]
    fn removals_win_ties_and_opt_out_removes_everything_the_writer_wrote() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        live.ids.insert(
            "c.example.com".to_owned(),
            KvPartnerId { w: Some("owner".to_owned()), at: Some(950), ..KvPartnerId::new("unowned-by-owner") },
        );

        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange { opt_out: Some(KvOptOut { at: NOW, fp: "0".repeat(16) }), ..ProviderStateChange::default() },
        );
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: None,
            staged_at: Some(NOW),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert!(!applied.entry.ids.contains_key("a.example.com"), "should remove the owned record");
        assert!(!applied.entry.ids.contains_key("c.example.com"), "should remove the unowned record it wrote");
        assert_eq!(applied.entry.ids["b.example.com"].uid, "x", "control: other writers untouched");
        assert_eq!(applied.entry.ps["owner"].o.as_ref().map(|o| o.at), Some(NOW), "should store the opt-out");
    }

    #[test]
    fn writer_remove_without_revision_respects_staged_at() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let remove = |staged_at| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::WriterRemove {
                source: src("a.example.com"),
                writer: pid("owner"),
                staged_at: Some(staged_at),
            });
            run(&mutation, &live).expect("should apply")
        };
        assert!(remove(900).entry.ids.get("a.example.com").is_none(), "same second: the removal wins");
        assert!(remove(899).entry.ids.get("a.example.com").is_some(), "re-acquired after sa: kept");
    }

    #[test]
    fn opt_out_and_clear_in_the_same_second_leave_it_opted_out() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out: Some(KvOptOut { at: NOW, fp: "0".repeat(16) }),
                opt_out_clear: Some(NOW),
                ..ProviderStateChange::default()
            },
        );
        let applied = run(&mutation, &live).expect("should apply");
        assert!(applied.entry.ps["owner"].o.is_some(), "should stay opted out");
        assert_eq!(applied.entry.ps["owner"].oc, None, "should drop a clear that is not later");
    }

    #[test]
    fn invalidated_uid_is_not_reseeded() {
        let keys = test_keys();
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: vec![crate::ec::identity::revision::invalidated_uid_hash(&keys, &pid("owner"), "rejected")],
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::FillMissing {
            source: src("a.example.com"),
            record: KvPartnerId::browser("rejected", Some(NOW)),
            writer: None,
            staged_at: None,
        });
        let applied = run(&mutation, &live).expect("should apply");
        assert!(!applied.entry.ids.contains_key("a.example.com"), "should not re-seed the rejected UID");
    }

    #[test]
    fn provider_result_applies_atomically() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), owned("newer", "3333333333333333"));
        live.ps.insert("owner".to_owned(), KvProviderState { st: Some(json!({ "token": "B" })), ..KvProviderState::default() });

        let mut mutation = IdentityMutation::default();
        mutation.add_result(ProviderResult {
            provider: pid("owner"),
            ids: vec![PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId::new("mine"),
                writer: pid("owner"),
                expected: Some(Revision::from_stored("1111111111111111")),
                staged_at: None,
            }],
            state: ProviderStateChange { st: StateUpdate::Replace(json!({ "token": "A" })), f: Some(0), ..ProviderStateChange::default() },
            expected_state: ExpectedState::Json(r#"{"token":"A0"}"#.to_owned()),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(applied.entry.ids["a.example.com"].uid, "newer", "should keep the other writer's ID");
        assert_eq!(applied.entry.ps["owner"].st, Some(json!({ "token": "B" })), "with that writer's state");
        assert!(!applied.changed, "nothing else changed");
    }

    #[test]
    fn refresh_anchor_survives_until_ru_or_state_loss() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId { exp: Some(NOW - 1), ru: Some(NOW + 100), ..owned("expired", "1111111111111111") },
        );
        live.ps.insert("owner".to_owned(), KvProviderState { st: Some(json!({ "r": 1 })), ..KvProviderState::default() });
        let mut unrelated = IdentityMutation::default();
        unrelated.set_cluster_size(1);

        let kept = run(&unrelated, &live).expect("should apply");
        assert!(kept.entry.ids.contains_key("a.example.com"), "anchor kept while state lives");

        live.ps.get_mut("owner").expect("owner").st = None;
        let dropped = run(&unrelated, &live).expect("should apply");
        assert!(!dropped.entry.ids.contains_key("a.example.com"), "anchor dropped without state");
    }

    #[test]
    fn unconfigured_provider_state_is_pruned() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert("retired".to_owned(), KvProviderState { n: Some(5), ..KvProviderState::default() });
        live.ps.insert("other".to_owned(), KvProviderState { n: Some(5), ..KvProviderState::default() });
        let mut mutation = IdentityMutation::default();
        mutation.set_cluster_size(1);
        let applied = run(&mutation, &live).expect("should apply");
        assert!(!applied.entry.ps.contains_key("retired"), "should prune");
        assert!(applied.entry.ps.contains_key("other"), "control: configured kept");
    }

    #[test]
    fn invalid_result_falls_back_to_removals_only() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::FillMissing {
            source: src("d.example.com"),
            record: KvPartnerId::browser("x".repeat(600), Some(NOW)),
            writer: None,
            staged_at: None,
        });
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            writer: pid("owner"),
            expected: Revision::from_stored("1111111111111111"),
        });
        let applied = run(&mutation, &live).expect("should apply removals");
        assert!(applied.removals_only, "should fall back");
        assert!(!applied.entry.ids.contains_key("a.example.com"), "removal applied");
        assert!(!applied.entry.ids.contains_key("d.example.com"), "invalid fill dropped");
    }
}
```

Add `fixed_provider(id: &str, sources: &[&str]) -> Arc<dyn IdentityProvider>` (a descriptor-only provider with a dummy `https://api.example.com` endpoint and the `eid_persistence_only` policy) and `test_keys() -> IdentityKeys` to `ec/identity/test_support.rs`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::mutation`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `mutation.rs` with the types listed under Interfaces, then implement `apply` as five private steps over a cloned `KvEntry`. Use one function per step so each rule can be read on its own:

```rust
pub(crate) fn apply(mutation: &IdentityMutation, live: &KvEntry, context: &ApplyContext<'_>) -> Option<Applied> {
    if !live.consent.ok {
        return None;
    }

    let mut entry = live.clone();
    apply_removals(&mut entry, mutation, context);
    let removals_only = entry.clone();

    apply_positive(&mut entry, mutation, context);
    apply_state_and_cleanup(&mut entry, mutation, context);

    let (entry, removals_only_used) = match entry.validate() {
        Ok(()) => (entry, false),
        Err(message) => {
            log::warn!("Identity mutation produced an invalid row, keeping removals only: {message}");
            let mut fallback = removals_only;
            prune_and_drop_expired(&mut fallback, context);
            if let Err(message) = fallback.validate() {
                log::warn!("Identity mutation removals-only row is invalid, dropping mutation: {message}");
                return None;
            }
            (fallback, true)
        }
    };

    let changed = &entry != live;
    Some(Applied { entry, changed, removals_only: removals_only_used })
}
```

- `apply_removals`: iterate `OwnerRemove` and `WriterRemove` from `partner_ids` and from every `ProviderResult`'s state-free removals. When building a `ProviderResult`, the coordinator always puts removals in `partner_ids`, never in `ProviderResult.ids` (spec 5.4 comment on `ProviderResult`). Then opt-outs, clears and `iv` from `provider_state` and from each result's `state`. A revision is compared with `record_revision(context.keys, live_record)`.
- `apply_positive`: compute `opted_out(provider)` from the post-removal `ps`. For each plain change, call `fn change_applies(entry, change, context) -> bool` and, when true, `fn write_change(entry, change, context)`. For each `ProviderResult`, first check that every ID change applies and that `expected_state` matches `entry.ps[provider].st` serialized with `serde_json::to_string`. `ExpectedState::NotRead` always matches. Then apply all of them and the result's `state` together.
- `apply_state_and_cleanup`: merge `provider_state` (`n` = max, `f` replaced when `Some`, `st` per `StateUpdate`), set `entry.network`'s `cluster_size` when given (match the existing `KvNetwork` field), then call `prune_and_drop_expired`.
- `prune_and_drop_expired`: `entry.ps.retain(|id, _| parsed id is configured)`. Drop records with `exp <= now` unless they are anchors: the catalog's owner of that source has `entry.ps[owner].st.is_some()` and `record.ru.is_some_and(|ru| now < ru)`.

`IdentityMutation::merge_state(provider, change)` merges into any existing change for the same provider: later `n`, last `f`, union of `iv_add`, the later `opt_out`, the later `opt_out_clear`, and the last non-`Keep` `st`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::mutation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add IdentityMutation and its merge into a live identity row" -m "Removals apply first and win equal-second ties, a provider opt-out removes every record that provider wrote, stateful results apply atomically, refresh anchors survive while their owner holds state, and an invalid row falls back to removals only."
```

### Task 16: Sealing and opening the identity cookies

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/cookie.rs` (seal, open, read and format helpers)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `cookie.rs`

**Interfaces:**

- Consumes: `IdentityKeys.cookie_s`, `.cookie_r` (Task 13), `RandomSource` (Task 9), `COOKIE_TS_EIDS_S`, `COOKIE_TS_EIDS_R_PREFIX` (Task 1)
- Produces:
  - `pub(crate) fn seal(key: &[u8; 32], ec_id: &str, payload: &[u8], random: &dyn RandomSource) -> String`, which returns `v1.<b64url nonce>.<b64url ciphertext+tag>`
  - `pub(crate) fn open(key: &[u8; 32], ec_id: &str, value: &str) -> Option<Vec<u8>>`
  - `pub(crate) enum CookieRead { Absent, Duplicate, Value(String) }` and `pub(crate) fn read_single_cookie(headers: &HeaderMap, name: &str) -> CookieRead`, which treats two cookies of the same name as none
  - `pub(crate) fn read_prefixed_cookies(headers: &HeaderMap, prefix: &str) -> Vec<(String, String)>`
  - `pub(crate) fn set_cookie_header(name: &str, value: &str) -> String` and `pub(crate) fn delete_cookie_header(name: &str) -> String`
  - `pub(crate) const COOKIE_MAX_AGE_SECS: u64 = 31_536_000;`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::*;
    use crate::ec::identity::input::SeededRandom;

    const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";
    const OTHER_EC: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123";

    #[test]
    fn seal_and_open_round_trip_bound_to_the_ec_id() {
        let key = [7u8; 32];
        let random = SeededRandom::new(3);
        let sealed = seal(&key, EC, br#"{"v":1}"#, &random);

        assert!(sealed.starts_with("v1."), "should carry the version prefix");
        assert_eq!(sealed.split('.').count(), 3, "should have three parts");
        assert!(!sealed.contains('='), "should be unpadded base64url");
        assert_eq!(open(&key, EC, &sealed).as_deref(), Some(&br#"{"v":1}"#[..]), "should open for its EC");
        assert!(open(&key, OTHER_EC, &sealed).is_none(), "should not open for another EC");
        assert!(open(&[8u8; 32], EC, &sealed).is_none(), "should not open with another key");
        let mut tampered = sealed.clone();
        tampered.pop();
        tampered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert!(open(&key, EC, &tampered).is_none(), "should reject tampering");
        assert!(open(&key, EC, "v2.x.y").is_none(), "should reject another version");
    }

    #[test]
    fn duplicate_cookie_names_count_as_none() {
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, HeaderValue::from_static("__Host-ts-eids-s=one"));
        headers.append(header::COOKIE, HeaderValue::from_static("other=1; __Host-ts-eids-s=two"));
        assert_eq!(read_single_cookie(&headers, "__Host-ts-eids-s"), CookieRead::Duplicate, "duplicate");

        let mut single = HeaderMap::new();
        single.insert(header::COOKIE, HeaderValue::from_static("__Host-ts-eids-s=one; x=2"));
        assert_eq!(read_single_cookie(&single, "__Host-ts-eids-s"), CookieRead::Value("one".to_owned()), "single");
    }

    #[test]
    fn prefixed_cookies_and_attributes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-eids-r-0a1b2c3d=a; __Host-ts-eids-r-11111111=b; x=1"),
        );
        let found = read_prefixed_cookies(&headers, "__Host-ts-eids-r-");
        assert_eq!(found.len(), 2, "should find both removal cookies");
        assert_eq!(
            set_cookie_header("__Host-ts-eids-s", "v1.a.b"),
            "__Host-ts-eids-s=v1.a.b; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000",
            "should use the spec attributes and no Domain"
        );
        assert_eq!(
            delete_cookie_header("__Host-ts-eids-s"),
            "__Host-ts-eids-s=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0",
            "should expire with matching attributes"
        );
    }
}
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::cookie`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The server-owned identity cookies: sealing, reading and formatting.

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, header};

use super::input::RandomSource;

/// Lifetime of both identity cookie kinds.
pub(crate) const COOKIE_MAX_AGE_SECS: u64 = 31_536_000;

const SEAL_VERSION: &str = "v1";

/// Seals `payload` with AES-256-GCM, the EC ID as associated data.
#[must_use]
pub(crate) fn seal(key: &[u8; 32], ec_id: &str, payload: &[u8], random: &dyn RandomSource) -> String {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; 12];
    random.fill(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: payload, aad: ec_id.as_bytes() })
        .expect("should encrypt a bounded in-memory payload");
    format!(
        "{SEAL_VERSION}.{}.{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext)
    )
}

/// Opens a sealed value. `None` for any failure; never log the value.
#[must_use]
pub(crate) fn open(key: &[u8; 32], ec_id: &str, value: &str) -> Option<Vec<u8>> {
    let mut parts = value.split('.');
    if parts.next()? != SEAL_VERSION {
        return None;
    }
    let nonce = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let ciphertext = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some() || nonce.len() != 12 {
        return None;
    }
    Aes256Gcm::new(key.into())
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ciphertext, aad: ec_id.as_bytes() })
        .ok()
}

/// One cookie read by exact name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CookieRead {
    Absent,
    /// Two or more cookies share the name; treated as none (spec 6.2).
    Duplicate,
    Value(String),
}

fn cookie_pairs(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
}

/// Reads a cookie that must occur at most once.
#[must_use]
pub(crate) fn read_single_cookie(headers: &HeaderMap, name: &str) -> CookieRead {
    let mut found = cookie_pairs(headers).filter(|(cookie, _)| *cookie == name).map(|(_, value)| value);
    match (found.next(), found.next()) {
        (None, _) => CookieRead::Absent,
        (Some(_), Some(_)) => CookieRead::Duplicate,
        (Some(value), None) => CookieRead::Value(value.to_owned()),
    }
}

/// Every cookie whose name starts with `prefix`, as `(name, value)`.
#[must_use]
pub(crate) fn read_prefixed_cookies(headers: &HeaderMap, prefix: &str) -> Vec<(String, String)> {
    cookie_pairs(headers)
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// A `Set-Cookie` value with the identity cookie attributes.
#[must_use]
pub(crate) fn set_cookie_header(name: &str, value: &str) -> String {
    format!("{name}={value}; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age={COOKIE_MAX_AGE_SECS}")
}

/// A `Set-Cookie` value that deletes an identity cookie.
#[must_use]
pub(crate) fn delete_cookie_header(name: &str) -> String {
    format!("{name}=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0")
}
```

If the `aes-gcm` 0.10 key conversion `key.into()` does not infer, use `Aes256Gcm::new_from_slice(key).expect("should accept a 32-byte key")`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::cookie` and `cargo check-cloudflare`
Expected: PASS and a clean build. The Cloudflare check proves that `aes-gcm` compiles for `wasm32-unknown-unknown`, because core is shared.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Seal and read the server-owned identity cookies"
```

### Task 17: The `ts-eids-s` payload, its size cap and its merge with KV

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/cookie.rs`
- Test: `cookie.rs`

**Interfaces:**

- Consumes: Task 16, `ProviderCatalog` (Task 14), `KvEntry` (Task 6), `record_revision` (Task 13), `EcKvSnapshot` (`ec/mod.rs:118`)
- Produces:
  - Payload types with the spec 6.2 short field names, and serde `skip_serializing_if` on every optional field:
    - `SCookie { v: u8, kc: u64, cf: String, m: bool, ids: Vec<SEntry>, ps: BTreeMap<String, SProviderState> }`
    - `SEntry { s: usize, u, w, t, e, r, a, x, rv, pv, sa, b, k, d }`. `b`, `k` and `d` serialize as `1`/`0` through small `bool_as_int` helpers.
    - `SProviderState { n, f, o: Option<KvOptOut>, iv: Vec<String>, oc, p, sg, kn }`. `kn` is the KV `n` that a cookie-only `n` was derived from (spec 6.3).
  - `pub(crate) const MAX_S_COOKIE_BYTES: usize = 2048;`
  - `pub(crate) fn open_s_cookie(keys, ec_id, catalog, value) -> SOpen`, where `SOpen` is `Missing | Failed | ConfigChanged(SCookie) | Valid(SCookie)`. `ConfigChanged` keeps only markers.
  - `pub(crate) fn encode_s_cookie(keys, ec_id, cookie: &mut SCookie, random) -> String`, which applies the drop order until the sealed value fits in 2,048 bytes, else falls back to a minimal cookie with `kc = 0` and the markers only (spec 5.6)
  - `pub(crate) fn merge_with_kv(cookie: &SCookie, snapshot: &EcKvSnapshot, context: &CookieMergeContext<'_>) -> MergeOutcome`, where `MergeOutcome { cookie: SCookie, changed: bool, expire: bool }`
  - `pub(crate) fn cookie_view(cookie: &SCookie, catalog) -> BTreeMap<SourceDomain, (KvPartnerId, Staging)>`: the cookie's entries resolved to sources. `Staging` is `Confirmed | Staged { pv, sa, browser }`.
  - `pub(crate) fn minimal_missing_cookie(now) -> SCookie` (`m: true`, no entries)

Merge rules (spec 6.3, step 3). Each is one test:

- A tombstone or `consent.ok = false` gives `expire: true`.
- `Missing` gives `minimal_missing_cookie`. Markers are dropped too.
- For an owned source, KV wins, except that a staged provider entry `OwnerWrite` would still apply (its `pv` matches the live revision, or it has no `pv` and KV holds only a value the provider did not write) stays `k: 0`.
- A `k: 1` entry whose source is gone from KV, or present at another revision, is replaced by the KV record (when owned) or dropped.
- A staged entry KV already holds (same UID, same writer) becomes `k: 1` with KV's `rv`.
- A `b: 1` entry is dropped once KV holds any value for its source.
- A stub becomes the KV record.
- Markers (`p`) are cleared when answered: KV `n >= p`, KV `o` set, a claimed source with `at >= p` or a source that lost the cookie's value, or the provider ineligible on the row. The last one takes a callback, `answered_by_eligibility`, which Task 20 supplies.
- `m` is cleared when the row is present.
- `kc = now`. `changed` is true when anything but `kc` changed, or the inbound `kc` is older than `kv_revalidate_sec / 2`.

The cookie caches values only for owned sources once they are in KV (spec 5.3). Unowned confirmed values are not copied into the cookie by the merge. In Part 1 no source is owned, so the cookie holds only staged entries, markers and mirrored `ps`.

Size-cap drop order (spec 6.2):

1. `k: 1` entries become stubs `{s, d: 1}`.
2. `ps` entries with no `p` and no `sg` are dropped.
3. Provider `k: 0` entries, lowest `cookie_priority` first. Task 19 adds the priority. Pass it in as `&dyn Fn(&str) -> i32`.
4. `b: 1` entries, last.

Markers are never dropped. Log each drop with the provider and source only.

- [ ] **Step 1: Write the failing tests**

```rust
    mod s_cookie {
        use super::super::*;
        use crate::ec::identity::test_support::{catalog_with_owner, test_keys};
        use crate::ec::identity::input::SeededRandom;
        use crate::ec::kv_types::{KvEntry, KvPartnerId, KvProviderState};
        use crate::ec::EcKvSnapshot;

        const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

        fn present(entry: KvEntry) -> EcKvSnapshot {
            EcKvSnapshot::Present { ec_id: EC.to_owned(), entry: Box::new(entry), generation: Some(1) }
        }

        #[test]
        fn round_trips_and_detects_config_change() {
            let keys = test_keys();
            let catalog = catalog_with_owner("owner", "a.example.com");
            let random = SeededRandom::new(1);
            let mut cookie = SCookie::new(&catalog, 100);
            cookie.ps.insert("owner".to_owned(), SProviderState { p: Some(90), ..SProviderState::default() });
            let sealed = encode_s_cookie(&keys, EC, &mut cookie, &random, &|_| 0);

            match open_s_cookie(&keys, EC, &catalog, &sealed) {
                SOpen::Valid(opened) => assert_eq!(opened, cookie, "should round-trip"),
                other => panic!("should open, got {other:?}"),
            }
            let other_catalog = catalog_with_owner("owner", "b.example.com");
            match open_s_cookie(&keys, EC, &other_catalog, &sealed) {
                SOpen::ConfigChanged(kept) => {
                    assert!(kept.ids.is_empty(), "should drop positive entries");
                    assert_eq!(kept.ps["owner"].p, Some(90), "should keep markers");
                }
                other => panic!("should flag the config change, got {other:?}"),
            }
        }

        #[test]
        fn tombstone_expires_and_missing_row_minimizes() {
            let catalog = catalog_with_owner("owner", "a.example.com");
            let cookie = SCookie::new(&catalog, 100);
            let context = CookieMergeContext::for_test(&catalog, 200);
            assert!(merge_with_kv(&cookie, &present(KvEntry::tombstone(1)), &context).expire, "tombstone");
            let missing = merge_with_kv(&cookie, &EcKvSnapshot::Missing { ec_id: EC.to_owned() }, &context);
            assert!(missing.cookie.m, "should set m");
            assert!(missing.cookie.ids.is_empty() && missing.cookie.ps.is_empty(), "no entries, no markers");
        }

        #[test]
        fn staged_entry_flips_to_confirmed_once_in_kv() {
            let catalog = catalog_with_owner("owner", "a.example.com");
            let mut cookie = SCookie::new(&catalog, 100);
            cookie.ids.push(SEntry::staged_browser(0, "u", 100));
            let mut entry = KvEntry::minimal("a.example.com", "u", 1);
            entry.ids.get_mut("a.example.com").expect("a").rv = Some("1111111111111111".to_owned());

            let merged = merge_with_kv(&cookie, &present(entry), &CookieMergeContext::for_test(&catalog, 200));

            assert!(merged.changed, "should change");
            assert!(merged.cookie.ids.is_empty(), "a b:1 entry is dropped once KV holds the source");
            assert_eq!(merged.cookie.kc, 200, "should stamp kc");
        }

        #[test]
        fn marker_is_answered_by_a_newer_n() {
            let catalog = catalog_with_owner("owner", "a.example.com");
            let mut cookie = SCookie::new(&catalog, 100);
            cookie.ps.insert("owner".to_owned(), SProviderState { p: Some(150), ..SProviderState::default() });
            let mut entry = KvEntry::minimal("b.example.com", "x", 1);
            entry.ps.insert("owner".to_owned(), KvProviderState { n: Some(160), ..KvProviderState::default() });

            let merged = merge_with_kv(&cookie, &present(entry), &CookieMergeContext::for_test(&catalog, 200));
            assert_eq!(merged.cookie.ps.get("owner").and_then(|state| state.p), None, "should clear the marker");
        }

        #[test]
        fn size_cap_drops_in_order_and_keeps_markers() {
            let keys = test_keys();
            let catalog = catalog_with_owner("owner", "a.example.com");
            let random = SeededRandom::new(1);
            let mut cookie = SCookie::new(&catalog, 100);
            cookie.ps.insert("owner".to_owned(), SProviderState { p: Some(90), ..SProviderState::default() });
            for _ in 0..40 {
                cookie.ids.push(SEntry::staged_browser(0, &"u".repeat(60), 100));
            }
            let sealed = encode_s_cookie(&keys, EC, &mut cookie, &random, &|_| 0);
            assert!(sealed.len() <= MAX_S_COOKIE_BYTES, "should fit the cap");
            assert_eq!(cookie.ps["owner"].p, Some(90), "should keep the marker");
        }
    }
```

Add `catalog_with_owner(provider, source)` to `test_support.rs`. It builds a `ProviderCatalog` with one `fixed_provider` that owns `source`. Add `CookieMergeContext::for_test(catalog, now)`, which uses `kv_revalidate_sec = 3600`, test keys, and an `answered_by_eligibility` that always returns `false`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::cookie::tests::s_cookie`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write the payload types with serde attributes, then `SCookie::new(catalog, now)` (`v: 1`, `kc: now`, `cf: catalog.fingerprint()`), `SEntry::staged_browser(index, uid, sa)`, `open_s_cookie`, `encode_s_cookie`, `merge_with_kv` and `cookie_view`. Give each merge rule above its own private helper, named after the rule (`fn flip_confirmed`, `fn drop_browser_entries_kv_holds`, `fn answer_markers`, and so on). That way a reviewer can match each helper to its spec bullet. A source index that is out of range for `catalog.configured_sources()` is dropped on open.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::cookie`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity
git commit --signoff -S -m "Encode the ts-eids-s cookie and reconcile it with KV"
```

### Task 18: The `ts-eids-r` removal cookies

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/cookie.rs`
- Test: `cookie.rs`

**Interfaces:**

- Produces:
  - `RBatch { rm: Vec<RRemoval>, o: BTreeMap<String, u64>, iv: BTreeMap<String, Vec<String>> }` and `RRemoval { s: String /* the source domain itself */, w: String, pv: Option<String>, sa: u64 }`
  - `pub(crate) const MAX_R_COOKIE_BYTES: usize = 512;`, `pub(crate) const R_COOKIE_SOFT_LIMIT: usize = 4;`
  - `pub(crate) struct OpenedRemovals { cookies: Vec<(String, RBatch)>, unopenable: Vec<String> }` and `pub(crate) fn open_r_cookies(keys, ec_id, headers) -> OpenedRemovals`
  - `pub(crate) fn r_batch_to_changes(batch: &RBatch) -> (Vec<PartnerIdChange>, BTreeMap<IdentityProviderId, ProviderStateChange>)`, which drops entries whose source or provider does not parse
  - `pub(crate) fn r_batch_applied(batch: &RBatch, entry: &KvEntry, keys) -> bool`, the "applied" definition of spec 6.2
  - `pub(crate) enum RPlan { Cookie { name, value }, Overflow }` and `pub(crate) fn plan_r_batch(keys, ec_id, batch, inbound_openable: usize, random) -> RPlan`. It returns `Overflow` when the batch does not fit one 512-byte cookie or the request already carries four openable cookies.
  - `pub(crate) fn split_r_batch(keys, ec_id, batch, random) -> Vec<(String, String)>`: the overflow fallback, as many 512-byte cookies as it takes, never coarsened (each removal, opt-out and `iv` hash kept exactly)
  - `pub(crate) fn new_r_cookie_name(random) -> String`: `__Host-ts-eids-r-` plus 8 random hex digits

- [ ] **Step 1: Write the failing tests**

```rust
    mod r_cookie {
        use super::super::*;
        use crate::ec::identity::input::SeededRandom;
        use crate::ec::identity::test_support::test_keys;
        use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

        const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

        fn removal(source: &str, writer: &str, sa: u64) -> RRemoval {
            RRemoval { s: source.to_owned(), w: writer.to_owned(), pv: None, sa }
        }

        #[test]
        fn a_typical_batch_fits_one_cookie_and_a_long_domain_fits_alone() {
            let keys = test_keys();
            let random = SeededRandom::new(1);
            let four = RBatch { rm: (0..4).map(|i| removal(&format!("s{i}.example.com"), "provider", 1)).collect(), ..RBatch::default() };
            assert!(matches!(plan_r_batch(&keys, EC, &four, 0, &random), RPlan::Cookie { .. }), "four typical removals fit");

            let label = "a".repeat(63);
            let long_domain = format!("{label}.{label}.{label}.{}.com", "b".repeat(57));
            assert_eq!(long_domain.len(), 253, "should be a 253-byte domain");
            let one = RBatch { rm: vec![removal(&long_domain, "a-long-provider-id", 1)], ..RBatch::default() };
            assert!(matches!(plan_r_batch(&keys, EC, &one, 0, &random), RPlan::Cookie { .. }), "one long removal fits");
        }

        #[test]
        fn overflow_when_too_big_or_four_cookies_present() {
            let keys = test_keys();
            let random = SeededRandom::new(1);
            let big = RBatch { rm: (0..12).map(|i| removal(&format!("s{i}.example.com"), "provider", 1)).collect(), ..RBatch::default() };
            assert!(matches!(plan_r_batch(&keys, EC, &big, 0, &random), RPlan::Overflow), "too big");
            let small = RBatch { rm: vec![removal("a.example.com", "p", 1)], ..RBatch::default() };
            assert!(matches!(plan_r_batch(&keys, EC, &small, 4, &random), RPlan::Overflow), "four present");
        }

        #[test]
        fn split_keeps_every_entry_exactly() {
            let keys = test_keys();
            let random = SeededRandom::new(1);
            let big = RBatch {
                rm: (0..12).map(|i| removal(&format!("s{i}.example.com"), "provider", 1)).collect(),
                o: [("provider".to_owned(), 5)].into_iter().collect(),
                iv: [("provider".to_owned(), vec!["0123456789abcdef".to_owned()])].into_iter().collect(),
            };
            let cookies = split_r_batch(&keys, EC, &big, &random);
            assert!(cookies.iter().all(|(_, value)| value.len() <= MAX_R_COOKIE_BYTES), "each fits");
            let mut headers = http::HeaderMap::new();
            let joined = cookies.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("; ");
            headers.insert(http::header::COOKIE, http::HeaderValue::from_str(&joined).expect("header"));
            let opened = open_r_cookies(&keys, EC, &headers);
            let rebuilt = opened.cookies.iter().fold(RBatch::default(), |mut all, (_, batch)| {
                all.rm.extend(batch.rm.clone());
                all.o.extend(batch.o.clone());
                for (provider, hashes) in &batch.iv {
                    all.iv.entry(provider.clone()).or_default().extend(hashes.clone());
                }
                all
            });
            assert_eq!(rebuilt.rm.len(), 12, "every removal survives");
            assert_eq!(rebuilt.o, big.o, "the opt-out survives unchanged");
            assert_eq!(rebuilt.iv, big.iv, "the iv hash survives");
        }

        #[test]
        fn applied_rules() {
            let keys = test_keys();
            let mut entry = KvEntry::minimal("b.example.com", "x", 1);
            entry.ids.insert(
                "a.example.com".to_owned(),
                KvPartnerId { w: Some("p".to_owned()), at: Some(10), ..KvPartnerId::new("u") },
            );
            assert!(!r_batch_applied(&RBatch { rm: vec![removal("a.example.com", "p", 10)], ..RBatch::default() }, &entry, &keys), "pending");
            assert!(r_batch_applied(&RBatch { rm: vec![removal("a.example.com", "p", 9)], ..RBatch::default() }, &entry, &keys), "re-acquired after sa");
            assert!(r_batch_applied(&RBatch { rm: vec![removal("a.example.com", "q", 10)], ..RBatch::default() }, &entry, &keys), "other writer");

            entry.ps.insert("p".to_owned(), KvProviderState { o: Some(KvOptOut { at: 5, fp: "0".repeat(16) }), ..KvProviderState::default() });
            let opt_out = |at| RBatch { o: [("p".to_owned(), at)].into_iter().collect(), ..RBatch::default() };
            assert!(r_batch_applied(&opt_out(5), &entry, &keys), "KV o.at at or after");
            assert!(!r_batch_applied(&opt_out(6), &entry, &keys), "newer opt-out pending");
            entry.ps.get_mut("p").expect("p").oc = Some(6);
            assert!(!r_batch_applied(&opt_out(6), &entry, &keys), "same-second clear does not count");
            entry.ps.get_mut("p").expect("p").oc = Some(7);
            assert!(r_batch_applied(&opt_out(6), &entry, &keys), "later clear makes it stale");
        }

        #[test]
        fn unopenable_cookies_are_listed_for_deletion() {
            let keys = test_keys();
            let mut headers = http::HeaderMap::new();
            headers.insert(http::header::COOKIE, http::HeaderValue::from_static("__Host-ts-eids-r-00000000=v1.bad.value"));
            let opened = open_r_cookies(&keys, EC, &headers);
            assert!(opened.cookies.is_empty(), "nothing opens");
            assert_eq!(opened.unopenable, vec!["__Host-ts-eids-r-00000000".to_owned()], "listed for deletion");
        }
    }
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::cookie::tests::r_cookie`
Expected: compile errors.

- [ ] **Step 3: Implement**

Serialize `RBatch` as compact JSON and omit empty maps. `plan_r_batch` seals once and measures. `split_r_batch` packs greedily: removals one by one, then each provider's opt-out, then each `iv` hash, sealing after each addition and starting a new cookie when the sealed value would pass 512. A single item that can never fit (impossible within the measured budget) is logged at error and still emitted in its own cookie. Never drop it. `r_batch_applied` implements exactly the three spec 6.2 bullets. `iv` entries never keep a cookie alive.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::cookie`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity
git commit --signoff -S -m "Add per-batch ts-eids-r removal cookies"
```

### Task 19: `[ec.identity]` settings

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/config.rs`
- Modify: `crates/trusted-server-core/src/settings.rs` (`Ec`, around line 542)
- Modify: `trusted-server.example.toml` (the `[ec]` section)
- Test: `config.rs`, `settings.rs`

**Interfaces:**

- Produces:
  - `IdentitySettings { max_concurrency: Option<usize>, hold_budget_ms: u64, hold_retry_sec: u64, max_response_bytes: usize, pending_grace_sec: u64, staged_max_age_sec: u64, kv_revalidate_sec: u64, providers: BTreeMap<String, ProviderSettings> }`, with the defaults from Global Constraints and `Default`
  - `ProviderSettings { mode: ProviderMode, timeout_ms: Option<u64>, sources: Option<Vec<String>>, refresh_after_sec: u64, max_calls_per_minute: u32, breaker_failures_per_minute: u32, cookie_priority: i32 }`
  - `enum ProviderMode { Hold, AfterSend }`, serde `snake_case`, default `AfterSend`
  - `IdentitySettings::effective_max_concurrency(&self, legacy_pull_concurrency: usize) -> usize`
  - `ProviderSettings::effective_timeout_ms(&self, settings: &IdentitySettings) -> u64`
  - `IdentitySettings::provider(&self, id: &IdentityProviderId) -> ProviderSettings`, which returns the defaults when there is no table
  - `pub(crate) fn validate_identity_settings(settings: &IdentitySettings) -> Result<(), Report<TrustedServerError>>`, covering the numeric rules of spec 10.2: `hold_budget_ms <= 1000`, `max_concurrency != Some(0)`, `refresh_after_sec > 0`, `kv_revalidate_sec < 86_400`, `timeout_ms <= hold_budget_ms` for hold mode. Unknown provider IDs are checked later, in Task 22, when the catalog exists.
  - `Ec.identity: IdentitySettings` with `#[serde(default)]`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_any_table() {
        let settings: IdentitySettings = toml::from_str("").expect("should parse empty");
        assert_eq!(settings.hold_budget_ms, 100, "hold budget");
        assert_eq!(settings.hold_retry_sec, 300, "hold retry");
        assert_eq!(settings.max_response_bytes, 65_536, "max bytes");
        assert_eq!(settings.pending_grace_sec, 300, "grace");
        assert_eq!(settings.staged_max_age_sec, 604_800, "staged age");
        assert_eq!(settings.kv_revalidate_sec, 3600, "revalidate");
        assert_eq!(settings.effective_max_concurrency(4), 4, "falls back to the legacy key");
        let provider = settings.provider(&"anything".parse().expect("id"));
        assert_eq!(provider.mode, ProviderMode::AfterSend, "mode");
        assert_eq!(provider.effective_timeout_ms(&settings), 1000, "after-send timeout");
        assert_eq!((provider.refresh_after_sec, provider.max_calls_per_minute, provider.breaker_failures_per_minute, provider.cookie_priority), (86_400, 1, 50, 0), "provider defaults");
        validate_identity_settings(&settings).expect("defaults are valid");
    }

    #[test]
    fn hold_mode_timeout_defaults_to_the_budget_and_bounds_are_enforced() {
        let settings: IdentitySettings = toml::from_str(
            "hold_budget_ms = 120\n[providers.id5]\nmode = \"hold\"\n",
        )
        .expect("should parse");
        let provider = settings.provider(&"id5".parse().expect("id"));
        assert_eq!(provider.effective_timeout_ms(&settings), 120, "hold timeout = budget");

        for (name, toml_text) in [
            ("budget over 1000", "hold_budget_ms = 1001"),
            ("zero concurrency", "max_concurrency = 0"),
            ("revalidate a day", "kv_revalidate_sec = 86400"),
            ("zero refresh", "[providers.p]\nrefresh_after_sec = 0"),
            ("timeout over budget", "[providers.p]\nmode = \"hold\"\ntimeout_ms = 101"),
        ] {
            let settings: IdentitySettings = toml::from_str(toml_text).expect(name);
            validate_identity_settings(&settings).expect_err(name);
        }
        toml::from_str::<IdentitySettings>("[providers.p]\nmode = \"sometimes\"").expect_err("bad mode");
    }
}
```

Use whichever TOML crate core already uses in tests (`grep -n "toml" crates/trusted-server-core/Cargo.toml`). If only `config` is available, build the value through the same path `settings.rs` tests use.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::config`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write the structs with `#[serde(default = "...")]` per field and `#[serde(deny_unknown_fields)]`, which matches the rest of `settings.rs`. Check the convention there first. Add to `Ec`:

```rust
    /// Server-side identity provider policy (`[ec.identity]`).
    #[serde(default)]
    pub identity: IdentitySettings,
```

Call `validate_identity_settings(&self.ec.identity)` from both `validate_settings_for_deploy` and `validate_settings_for_runtime` in `config.rs`, next to `PartnerRegistry::...`. Mark `pull_sync_concurrency`'s doc comment as deprecated in favour of `[ec.identity].max_concurrency`, and log a `warn!` once at settings load when `identity.max_concurrency` is unset and `pull_sync_concurrency` is not its default. In `trusted-server.example.toml`, add a commented `[ec.identity]` block that copies spec 10.1's comments and values, with `example.com` partner names.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::config`, `cargo test-fastly -- settings`, `cargo test-fastly -- config::`
Expected: PASS. Existing configs with no `[ec.identity]` still parse.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src trusted-server.example.toml
git commit --signoff -S -m "Add [ec.identity] coordinator settings"
```

### Task 20: Coordinator eligibility and outcomes

**Files:**

- Create: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (pure logic only in this task)
- Modify: `mod.rs`, `migration_guards.rs`
- Test: `coordinator.rs`

**Interfaces:**

- Consumes: catalog, servable, mutation, cookie view, config, policy (Tasks 12 to 19)
- Produces:
  - `pub(crate) struct EligibilityView`, the merged view of spec 4.7, built by `EligibilityView::merge(kv: Option<&KvEntry>, cookie: Option<&SCookie>, removals: &[RBatch], catalog, now) -> Self`. It holds per-source records (with KV as the base when read, else the cookie's last confirmed copy, cookie `k: 0` entries on top when their `pv` matches, and `ts-eids-r` removals hiding records), per-provider `n` (the later of cookie and KV), `iv` (union), `o` (the staged opt-out counts unless `ts-eids-s` stages a later `oc`, and the mirrored opt-out yields to KV when KV was read), and markers (cookie only).
  - `pub(crate) struct RequestFacts { is_real_browser, ec_persistence_allowed, policy_signals: PolicySignals, writer: bool, new_visitor: bool, runs_auction: bool, now: UnixTime }`
  - `pub(crate) enum Phase { Hold, AfterSendDryRun, AfterSend }`
  - `pub(crate) enum Eligibility { Eligible { kinds: BTreeMap<SourceDomain, ResolveKind> }, Skip(SkipReason) }`, with a `SkipReason` enum naming each of steps 1 to 8 (step 9, the burst cap, is checked at dispatch)
  - `pub(crate) fn eligibility(provider: &dyn IdentityProvider, settings: &ProviderSettings, identity: &IdentitySettings, view: &EligibilityView, facts: &RequestFacts, consent: &ConsentContext, phase: Phase, catalog: &ProviderCatalog) -> Eligibility`
  - `pub(crate) enum CallOutcome { Resolved(Resolution), Skipped, InputUnavailable, InvalidState(Option<Resolution>), HoldCutOff, InputFailure, TransportFailure, BreakerOpen }`
  - `pub(crate) fn outcome_to_changes(provider: &dyn IdentityProvider, outcome: CallOutcome, view: &EligibilityView, context: &OutcomeContext<'_>) -> OutcomeChanges`, where `OutcomeChanges { removals: Vec<PartnerIdChange>, result: Option<ProviderResult>, state: ProviderStateChange }`. This is the section 8 table, row by row. `OutcomeContext` holds `now`, `keys`, `catalog`, provider settings, `identity` settings, `expected_state` and the consent fingerprint bits.

Eligibility steps, in order (spec 4.7):

1. `facts.is_real_browser`
2. `facts.ec_persistence_allowed`
3. `policy_allows(descriptor.consent_policy(), consent, facts.policy_signals)`
4. Per-source kinds:
   - **Owned by this provider.** `Initial` when there is no record. `Refresh` when there is one and it is due: expired, written by someone else, `ra` passed, or no `ra` and `at + refresh_after_sec` passed or `at` missing. A browser-seeded UID whose hash is in `iv` is never due.
   - **Unowned.** `Initial` only, and only when there is no servable record. An expired record, or one whose writer is unconfigured, counts as absent.
   - A source whose kind the provider does not list, or whose needs (`resolves.needs(kind)`) are not available this request, is left out. No sources left means skip.
5. `n` has passed, and no sticky `o` is set (`oc` not strictly later than `o.at`).
6. No unanswered marker for the provider.
7. (Breaker: checked at dispatch in Part 2, Task 35. Part 1 has no breaker.)
8. Mode:
   - No provider runs when `facts.new_visitor`.
   - `Hold` needs `settings.mode == Hold` and `facts.runs_auction`. A stateful provider also needs `facts.writer`.
   - `AfterSendDryRun` and `AfterSend` need `facts.writer`, and either `settings.mode == AfterSend`, or a stateful hold-mode provider on a writer navigation that does not run an auction.

The needs check in step 4 needs to know which inputs are available. Add `available_inputs: BTreeSet<Input>` to `RequestFacts`.

- [ ] **Step 1: Write the failing tests**

Write one test per eligibility step and one per section 8 row, using `fixed_provider` and `ProviderSettings::default()`. Name them after the rule, for example `skips_when_not_a_real_browser`, `skips_new_visitor_in_every_mode`, `owned_record_written_by_someone_else_is_due_for_refresh`, `browser_seeded_uid_in_iv_is_never_due`, `unowned_source_with_servable_record_is_not_initial`, `expired_unowned_record_counts_as_absent`, `stateful_hold_provider_runs_after_send_without_auction`, `hold_provider_never_runs_on_stager_when_stateful`, `no_match_waits_retry_in_capped_at_seven_days`, `input_failure_backs_off_exponentially_to_a_day`, `transport_failure_backs_off_to_an_hour`, `hold_cut_off_waits_hold_retry_without_counting_a_failure`, `skip_waits_refresh_after`, `invalid_state_clears_state_and_keeps_f`, `oversized_state_rejects_positive_part_but_keeps_removals`, `opted_out_removes_everything_and_clears_state`, `invalidate_records_iv_hash_and_sets_n_now`. Here are three written out in full. Write the rest in the same shape:

```rust
    #[test]
    fn skips_new_visitor_in_every_mode() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new());
        for phase in [Phase::Hold, Phase::AfterSendDryRun, Phase::AfterSend] {
            let facts = RequestFacts { new_visitor: true, ..facts_for_writer() };
            assert!(
                matches!(
                    eligibility(provider.as_ref(), &ProviderSettings::default(), &IdentitySettings::default(), &view, &facts, &allowing_consent(), phase, &catalog),
                    Eligibility::Skip(SkipReason::NewVisitor)
                ),
                "no provider runs on a new visitor's first navigation ({phase:?})"
            );
        }
        let returning = facts_for_writer();
        assert!(
            matches!(
                eligibility(provider.as_ref(), &ProviderSettings::default(), &IdentitySettings::default(), &view, &returning, &allowing_consent(), Phase::AfterSend, &catalog),
                Eligibility::Eligible { .. }
            ),
            "control: a returning writer is eligible"
        );
    }

    #[test]
    fn no_match_waits_retry_in_capped_at_seven_days() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let context = outcome_context_at(1_000);
        let default = outcome_to_changes(provider.as_ref(), CallOutcome::Resolved(Resolution::no_match()), &EligibilityView::empty(), &context);
        assert_eq!(default.state.n, Some(1_000 + 86_400), "24 h default");
        let long = outcome_to_changes(
            provider.as_ref(),
            CallOutcome::Resolved(Resolution::no_match().with_retry_in(Duration::from_secs(30 * 86_400))),
            &EligibilityView::empty(),
            &context,
        );
        assert_eq!(long.state.n, Some(1_000 + 7 * 86_400), "capped at 7 days");
    }

    #[test]
    fn transport_failure_backs_off_to_an_hour() {
        let provider = fixed_provider("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 5);
        let changes = outcome_to_changes(provider.as_ref(), CallOutcome::TransportFailure, &view, &outcome_context_at(1_000));
        assert_eq!(changes.state.f, Some(6), "should count the failure");
        assert_eq!(changes.state.n, Some(1_000 + 3_600), "min(5 min x 2^5, 1 h) = 1 h");
    }
```

Add `facts_for_writer()`, `allowing_consent()` and `outcome_context_at(now)` to `test_support.rs`. `facts_for_writer` returns a real browser, persistence allowed, a writer, a returning visitor, no auction, and every input available.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::coordinator`
Expected: compile errors.

- [ ] **Step 3: Implement**

Implement `EligibilityView::merge`, `eligibility` and `outcome_to_changes` as plain functions. In `outcome_to_changes`, `Resolved(resolution)` does the following:

- It drops IDs for sources the provider does not claim (after `sources` narrowing from `ProviderSettings`), IDs with an empty UID or one over `MAX_UID_LENGTH`, and duplicates. It logs each drop with the provider ID and source only.
- An ID of an expiring source without an expiry keeps the stored `exp` when its UID is unchanged and is dropped otherwise.
- `with_refresh_until` from a provider without private state is ignored with a warning.
- Each kept ID becomes `OwnerWrite` with `expected = record_revision(stored)` (or `None` when no record) when this provider owns the source, else `FillMissing { writer: Some(provider) }`.
- `invalidated` becomes `OwnerRemove` with the revision read, plus an `iv` hash of the stored UID, plus `n = now`.
- `opted_out` becomes `WriterRemove` for every source the provider claims (the mutation also removes the unowned records it wrote), plus `opt_out = KvOptOut { at: now, fp }` and `st = Clear`.
- When the provider has private state, the positive IDs plus the state change plus `n` and `f` go into one `ProviderResult`. Otherwise they go into `removals`, a plain change list, and `state`.
- `state_rejected()` drops the positive part and applies the `Malformed` backoff.

Backoff arithmetic uses `saturating_pow` and `min`, so a large `f` cannot overflow.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity::coordinator`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity crates/trusted-server-core/src/migration_guards.rs
git commit --signoff -S -m "Add coordinator eligibility and outcome rules"
```

### Task 21: Coordinator dispatch

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs`
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (fake HTTP client)
- Test: `coordinator.rs`

**Interfaces:**

- Consumes: `RuntimeServices::http_client().send_async`, `services.backend().ensure(&PlatformBackendSpec { .. })` exactly as `dispatch_pull_sync` uses them (`ec/pull_sync.rs`, the body of `dispatch_pull_sync`), the bounded decoders in `streaming_processor.rs`, and the rate limiter's `exceeded_per_minute`
- Produces:
  - `pub(crate) struct IdentityDeps<'a> { settings: &'a Settings, catalog: &'a ProviderCatalog, keys: &'a IdentityKeys, services: &'a RuntimeServices, rate_limiter: Option<&'a dyn RateLimiter> }`. This one struct keeps `handle_auction` and `handle_publisher_request` within 7 arguments.
  - `pub(crate) struct Call { provider: IdentityProviderId, kinds: BTreeMap<SourceDomain, ResolveKind> }`
  - `pub(crate) fn dispatch(deps: &IdentityDeps<'_>, calls: Vec<Call>, signals: &ResolveSignals<'_>, budget: Option<Duration>) -> Vec<(IdentityProviderId, CallOutcome)>`
  - `pub(crate) fn check_request(request: &ProviderRequest, descriptor: &ProviderDescriptor) -> Result<(), Report<IdentityProviderError>>`: `https` only, the host in `resolves.hosts()`, no `Cookie` header
  - `pub(crate) fn decode_body(encoding: Option<&str>, body: Vec<u8>, cap: usize) -> Option<Vec<u8>>`: identity, `gzip` or `br`, the cap applied to the decoded size, `None` when over the cap or for an unknown encoding

Dispatch per call:

1. Build the `ResolveInput` and call `request`.
   - `Ok(None)` is `Skipped`.
   - `Err` with `InputUnavailable` is `InputUnavailable`.
   - `Err` with `UndeclaredInput` or any other `Config` is `InputFailure`, logged as `Config` with `current_context()` only.
   - Any recorded undeclared read is also logged as `Config` and turns the outcome into `InputFailure`.
2. `check_request`. On failure: log `Config`, `InputFailure`.
3. The burst cap: `rate_limiter.exceeded_per_minute(&format!("identity:{provider}:{}", visitor_key(keys, ec_id)), settings.max_calls_per_minute)`. Exceeded means not dispatched (Task 29 adds the 60-second marker backoff). A limiter error is logged and the call is skipped.
4. `ensure` a backend with first-byte timeout `effective_timeout_ms`, or in hold mode `min(timeout, remaining budget)` rounded up to the orchestrator's buckets (`auction/orchestrator.rs`, search for the bucket rounding). Do not follow redirects. Send with `send_async`, keeping at most `effective_max_concurrency` in flight.
5. Collect responses: check `Content-Length` against `max_response_bytes`, buffer, `decode_body`. Over the cap is `TransportFailure`. Build the `ProviderResponse` with the request's call context. Call `response`.
   - `Ok(resolution)` is `Resolved`, or `InvalidState(Some(resolution))` when the input recorded a state decode failure.
   - `Rejected` with 429 or 5xx is `TransportFailure`.
   - Other `Rejected`, `Malformed` and `Config` are `InputFailure`.
   - `InvalidState` is `InvalidState(None)`.
6. A hold-mode call still pending when the budget runs out is dropped and becomes `HoldCutOff`.

- [ ] **Step 1: Write the failing tests**

Add a `FakeHttpClient` to `test_support.rs`. It implements the platform HTTP client trait that `RuntimeServices` holds (`grep -n "trait .*HttpClient" crates/trusted-server-core/src/platform/*.rs`) and returns canned responses per host, with an optional delay counter for hold-budget tests. Check first whether `auction/test_support.rs` already has one, and reuse it if so. Then test:

```rust
    #[test]
    fn check_request_enforces_https_host_and_no_cookie() {
        let descriptor = fixed_provider("p", &["a.example.com"]).descriptor().clone();
        let good = ProviderRequest::get(&"https://api.example.com/x".parse().expect("url"));
        check_request(&good, &descriptor).expect("control: allowed");
        for (name, request) in [
            ("http", ProviderRequest::get(&"http://api.example.com/x".parse().expect("url"))),
            ("host", ProviderRequest::get(&"https://evil.example.net/x".parse().expect("url"))),
            ("cookie", good.clone().try_with_header("cookie", "a=1").expect("header")),
        ] {
            check_request(&request, &descriptor).expect_err(name);
        }
    }

    #[test]
    fn decode_body_caps_decoded_size() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &vec![b'a'; 10_000]).expect("write");
        let gzipped = encoder.finish().expect("finish");
        assert_eq!(decode_body(Some("gzip"), gzipped.clone(), 10_000).map(|b| b.len()), Some(10_000), "fits");
        assert!(decode_body(Some("gzip"), gzipped, 9_999).is_none(), "over the cap after decoding");
        assert!(decode_body(Some("zstd"), vec![1], 10).is_none(), "unknown encoding");
        assert_eq!(decode_body(None, b"ok".to_vec(), 10).as_deref(), Some(&b"ok"[..]), "identity");
    }
```

Also write `dispatch_maps_status_codes_to_outcomes` (200 is `Resolved`, 503 is `TransportFailure`, 400 is `InputFailure`, a body over the cap is `TransportFailure`) and `undeclared_read_is_an_input_failure_even_when_ignored`, using a provider that reads `user_agent().ok()` without declaring it, against the fake client.

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
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (`ProviderCatalog::from_settings`)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`KvIdentityGraph::write_counting` constructor, test support)
- Test: `generic_pull.rs`

**Interfaces:**

- Consumes: `PartnerConfig` (`ec/registry.rs:22`), `validated_pull_sync_url` and `build_pull_request_url` (`ec/pull_sync.rs`, made `pub(crate)`)
- Produces:
  - `pub(crate) struct GenericPullProvider` with `pub(crate) fn from_partner(partner: &PartnerConfig) -> Result<Self, Report<IdentityProviderError>>`
  - `pub(crate) fn generic_pull_id(source_domain: &str) -> IdentityProviderId`: `pull-` plus the domain with `.` replaced by `-`. Over 63 characters, it is cut to 54 characters, followed by `-` and the first 8 hex digits of SHA-256 of the full domain.
  - `ProviderCatalog::from_settings(settings: &Settings, registry: &PartnerRegistry) -> Result<Self, Report<TrustedServerError>>`. In Part 1 it holds generic pull providers only, with no owners, and rejects a duplicate provider ID and more than 32 providers.
  - `pub(crate) fn legacy_rate_limit_per_minute(pull_sync_rate_limit: u32) -> u32` = `max(1, ceil(limit / 60))`
  - Test support:
    - `WriteCountingEcKv` (an in-memory store that counts `Add`, `IfGenerationMatch` and `Overwrite` writes and lookups) with `pub(crate) fn writes(&self) -> WriteCounts`
    - `KvIdentityGraph::write_counting(name) -> (KvIdentityGraph, Arc<WriteCountingEcKv>)`
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

Its `request` is a `GET` of `validated_pull_sync_url(partner)` with `?ec_id=<input.ec_id()?>` (via `build_pull_request_url`) and `with_bearer(ts_pull_token)`. Skip (`Ok(None)`) when there is no valid URL or token. Its `response` is `success_json::<{ uid: String }>()`. An empty UID or one over `MAX_UID_LENGTH` (`is_valid_eid_uid`) is `no_match`, and any other UID is `found([ResolvedId::new(source, uid)])`. The provider's settings default to `mode = AfterSend`, `max_calls_per_minute = legacy_rate_limit_per_minute(partner.pull_sync_rate_limit)`, and `refresh_after_sec = partner.pull_sync_ttl_sec` (used only when Part 2's `owner` makes it authoritative). A `[ec.identity.providers.<id>]` table overrides these. Expose `fn default_settings_for(partner) -> ProviderSettings` and make `IdentitySettings::provider` fall back to it for `pull-` IDs. To do that, `provider()` takes an `Option<ProviderSettings>` base.

- [ ] **Step 1: Port today's pull sync tests**

List them with `grep -n "fn " crates/trusted-server-core/src/ec/pull_sync.rs | sed -n '/mod tests/,$p'`. For each test of protocol behaviour (URL building, bearer, response parsing, 64 KiB cap, allowlist, empty or oversized UID), write the equivalent against `GenericPullProvider` through `ProviderHarness`. For example:

```rust
    #[test]
    fn calls_the_partner_url_with_the_ec_id_and_bearer() {
        let partner = partner_config("ssp.example.com", "https://sync.ssp.example.com/pull", "example-token-value-0123456789abcdef");
        let provider = GenericPullProvider::from_partner(&partner).expect("should build");
        let mut harness = ProviderHarness::new(&provider).with_ec_id(TEST_EC_ID);

        let request = harness.request().expect("should build").expect("should not skip");

        assert_eq!(request.url().host_str(), Some("sync.ssp.example.com"), "host");
        assert_eq!(
            request.url().query_pairs().find(|(name, _)| name == "ec_id").map(|(_, value)| value.into_owned()),
            Some(TEST_EC_ID.to_owned()),
            "should pass the EC ID"
        );
        assert_eq!(request.headers()["authorization"], "Bearer example-token-value-0123456789abcdef", "bearer");

        let resolution = harness.respond(StatusCode::OK, &json!({ "uid": "partner-uid" })).expect("should parse");
        assert_eq!(resolution.ids()[0].uid(), "partner-uid", "uid");
    }

    #[test]
    fn provider_ids_are_derived_and_bounded() {
        assert_eq!(generic_pull_id("ssp.example.com").as_str(), "pull-ssp-example-com", "simple");
        let long = format!("{}.example.com", "a".repeat(70));
        let id = generic_pull_id(&long);
        assert!(id.as_str().len() <= 63, "bounded");
        assert_ne!(id, generic_pull_id(&format!("{}.example.com", "a".repeat(71))), "unique by hash");
    }

    #[test]
    fn legacy_hourly_limit_maps_to_per_minute() {
        assert_eq!(legacy_rate_limit_per_minute(0), 1, "at least one");
        assert_eq!(legacy_rate_limit_per_minute(59), 1, "rounds up");
        assert_eq!(legacy_rate_limit_per_minute(65), 2, "65/hr is 2/min, as the limiter does today");
    }
```

`ProviderHarness::with_ec_id` is crate-private (only generic pull reads the EC ID). Add it next to the public setters with `pub(crate)`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::identity::generic_pull`
Expected: compile errors.

- [ ] **Step 3: Implement**

Write `generic_pull.rs`, `ProviderCatalog::from_settings`, the harness basics and the counting store. The harness builds `ResolveSignals` from its setters (`SeededRandom` from `with_random_seed`, default seed `1`, default `now` 1,760,000,000). It computes kinds as spec 13 says: a source with a `with_stored` record is `Refresh`, any other is `Initial`, and sources whose kind's needs are missing are dropped, with `Ok(None)` and no provider call when none are left. It keeps the built request's call context for `respond`. `respond` builds a `ProviderResponse` from `serde_json::to_vec(body)`.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::identity` and `cargo test-fastly -- ec::pull_sync`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Model pull sync partners as GenericPullProvider" -m "Adds the provider harness basics and a KV store that counts writes by mode."
```

---

### Task 23: The writer decision and request-scoped identity state on `EcContext`

**Files:**

- Modify: `crates/trusted-server-core/src/http_util.rs` (add `is_document_navigation`, move `is_prefetch_request` here)
- Modify: `crates/trusted-server-core/src/publisher.rs` (`is_prefetch_request` at about line 3200 becomes a re-export or its callers switch)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`EcContext` fields and accessors)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`build_ec_request_state`, the publisher fallback, and the integration-proxy EC generation path)
- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (integration-proxy EC generation, about line 1046, gains the real-browser check)
- Test: `http_util.rs`, `ec/mod.rs`, `adapter-fastly/src/app.rs`

**Interfaces:**

- Produces:
  - `pub fn is_document_navigation(req: &Request<EdgeBody>) -> bool`: `Sec-Fetch-Dest: document` only, with no `Accept` fallback
  - `pub fn is_prefetch_request(req: &Request<EdgeBody>) -> bool`, moved unchanged and made public
  - `pub(crate) struct RequestIdentity` stored on `EcContext`, with accessors:
    - `writer: bool`
    - `user_agent: Option<String>`, `dnt: bool`, `page_url: Option<Url>` (captured in phase 1)
    - `s_cookie: SOpen`, `removals: OpenedRemovals` (opened lazily, at most once, Task 16 and 18 types)
    - `mutation: IdentityMutation`
    - `staged: StagedCookies`: what this response must set or delete. `s_cookie: Option<SCookie>` to set, `expire_s: bool`, `new_r_batch: Option<RBatch>`, `delete_r: Vec<String>`.
  - `EcContext::identity(&self) -> &RequestIdentity` and `identity_mut(&mut self) -> &mut RequestIdentity`
  - `EcContext::decide_writer(&mut self, req: &Request<EdgeBody>, organic_route: bool, is_real_browser: bool)`. It sets `writer = organic_route && is_document_navigation(req) && !is_prefetch_request(req) && is_real_browser` and captures the user agent, DNT and the page URL (the request URL without query or fragment).
  - `EcContext::open_identity_cookies(&mut self, keys: &IdentityKeys, catalog: &ProviderCatalog, headers: &HeaderMap)`: idempotent, and a no-op without an EC ID

The writer decision is made in the adapter where routing already decides EC generation (spec 5.1). That is after pre-route filters allow dispatch:

- In the publisher fallback branch of `app.rs`, next to `let is_navigation = is_navigation_request(&req);`, call `ec.ec_context.decide_writer(&req, true, ec.is_real_browser)`.
- In the integration-proxy branch, call `decide_writer(&req, true, is_real_browser)` before `handle_proxy`, and make proxy EC generation require `is_real_browser` explicitly, not just a present KV graph (`registry.rs:1048`). `ProxyDispatchInput` gains `is_real_browser: bool` for this.
- Every other route leaves `writer = false`.

- [ ] **Step 1: Write the failing tests**

In `http_util.rs`:

```rust
    #[test]
    fn document_navigation_requires_sec_fetch_dest_document() {
        let with_dest = Request::builder()
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("request");
        let accept_only = Request::builder()
            .header(header::ACCEPT, "text/html")
            .body(EdgeBody::empty())
            .expect("request");
        let iframe = Request::builder()
            .header("sec-fetch-dest", "iframe")
            .body(EdgeBody::empty())
            .expect("request");
        assert!(is_document_navigation(&with_dest), "document counts");
        assert!(!is_document_navigation(&accept_only), "the Accept fallback does not");
        assert!(is_navigation_request(&accept_only), "control: the broad check still accepts it");
        assert!(!is_document_navigation(&iframe), "iframes do not");
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
                .expect("request")
        };
        let mut context = EcContext::default();
        context.decide_writer(&navigation(), true, true);
        assert!(context.identity().writer(), "organic real-browser document navigation");
        assert_eq!(context.identity().page_url().map(Url::as_str), Some("https://www.example.com/a"), "no query");
        assert_eq!(context.identity().user_agent(), Some("ExampleBrowser/1.0"), "ua captured");
        assert!(context.identity().dnt(), "dnt captured");

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
                    .expect("request"),
            ),
        ] {
            let mut context = EcContext::default();
            context.decide_writer(&request, organic, browser);
            assert!(!context.identity().writer(), "not a writer: {name}");
        }
    }
```

Use the prefetch headers `is_prefetch_request` actually checks. Read it before writing the test.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- http_util::tests::document_navigation` and `cargo test-fastly -- ec::tests::decide_writer`
Expected: compile errors.

- [ ] **Step 3: Implement**

Add `RequestIdentity` (in `ec/identity/mod.rs` as `pub(crate) mod request;`, file `ec/identity/request.rs`, registered in `migration_guards.rs`) with `Default`, the accessors above, and setters used by later tasks. Add the field `identity: RequestIdentity` to `EcContext` (it already derives `Default`, so this needs `RequestIdentity: Default + Clone`). `IdentityMutation`, `SCookie`, `OpenedRemovals` and `RBatch` need `Clone`, so add `#[derive(Clone)]` where it is missing. `EcFinalizeState` must stay `Clone + Send + Sync`, and every type here is plain data, so it does.

Move `is_prefetch_request` into `http_util.rs` as `pub`. Replace the body in `publisher.rs` with `use crate::http_util::is_prefetch_request;` at the top, and delete the old definition.

Wire `decide_writer` in `app.rs` as described above. Add `is_real_browser` to `ProxyDispatchInput` and set it from `EcRequestState`. In `IntegrationRegistry::handle_proxy`, require it before proxy EC generation.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly` (full: the adapter and core)
Expected: PASS. Behaviour is unchanged so far, apart from integration-proxy EC generation now needing a real browser. If an existing proxy test minted an EC for a non-browser request, update it and note the change in the commit body.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Decide the writer request once routing allows dispatch" -m "Only an organic real-browser top-level document navigation that is not a prefetch is the writer. Integration-proxy EC generation now requires a real browser explicitly."
```

### Task 24: `/identify` stops writing; cluster size rides on the writer

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identify.rs` (the `evaluate_cluster` call at about line 107)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`evaluate_cluster` at about line 1484 gains a non-writing variant)
- Test: `ec/identify.rs`, `ec/kv.rs`

**Interfaces:**

- Produces:
  - `KvIdentityGraph::compute_cluster_size(&self, ec_id: &str) -> Result<u32, Report<TrustedServerError>>`: the prefix list and count only, no write
  - `/identify` uses a stored `cluster_size` when present and `compute_cluster_size` otherwise, and never writes
  - `pub(crate) fn queue_cluster_size(context: &mut EcContext, graph: &KvIdentityGraph, ec_id: &str)`, called by phase 4 (Task 29) when its live row lacks `cluster_size`

- [ ] **Step 1: Write the failing test**

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
```

`seed_row_without_cluster_size`, `identify_for_test` and `response_body_json` are thin wrappers around what the existing identify tests already do. Write them in the test module from that setup.

- [ ] **Step 2: Run and see it fail**

Run: `cargo test-fastly -- ec::identify::tests::identify_computes_cluster_size_without_writing`
Expected: FAIL with one write.

- [ ] **Step 3: Implement**

Split `evaluate_cluster` into `compute_cluster_size` (list and count) and the existing write. `/identify` calls only `compute_cluster_size`. Keep `evaluate_cluster` for now. Task 29 calls it through the mutation (`IdentityMutation::set_cluster_size`), and once nothing else calls it, remove it in that task.

- [ ] **Step 4: Run and see it pass**

Run: `cargo test-fastly -- ec::identify` and `cargo test-fastly -- ec::kv`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src/ec
git commit --signoff -S -m "Stop writing cluster size from /identify"
```

### Task 25: Finalize builds the mutation, and stagers save into cookies

This is the core behaviour change of Part 1. `ec_finalize_response` stops writing KV for returning visitors. On the writer request it adds changes to the mutation. On stagers it saves changes into `__Host-ts-eids-s` and `__Host-ts-eids-r-*`.

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (`ec_finalize_response`, `finalize_unusable_consent`, `confirm_then_recover_orphaned_ec`, `recover_orphaned_ec`)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`generate_if_needed` takes the browser ingest)
- Modify: `crates/trusted-server-core/src/ec/kv.rs` (`create_if_absent` callers pass the seeded entry)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (pass the parsed `ts-eids` and sharedId to `generate_if_needed`)
- Test: `ec/finalize.rs`

**Interfaces:**

- Consumes: `RequestIdentity` (Task 23), `IdentityMutation` (Task 15), the cookie functions (Tasks 16 to 18), `ProviderCatalog`, `IdentityKeys`
- Produces:
  - `ec_finalize_response` gains one argument, `identity: &IdentityFinalize<'_>`, which bundles `keys`, `catalog`, `identity_settings`, `now` and `random`, so the function stays within 7 arguments
  - Writer, returning EC: adds `FillMissing` browser ingest (`acquired_at = now`) for `ts-eids` and sharedId, plus the flush-checked staged entries from `ts-eids-s` and every `ts-eids-r` batch, to `ec_context.identity_mut().mutation`. It writes nothing.
  - Stager, returning EC: stages its changes in `staged.s_cookie` (positive) and `staged.new_r_batch` (removals).
  - `pub(crate) fn emit_identity_cookies(context: &mut EcContext, identity: &IdentityFinalize<'_>, response: &mut Response<EdgeBody>)`, called at the end of every finalize path (the unusable-consent path included). It appends the `Set-Cookie` headers for `staged`, and it is the only place `__Host-ts-eids-*` cookies are set.
  - `generate_if_needed(settings, kv, seed: &[PartnerIdUpdate])`: the new-visitor create carries the browser ingest, so finalize no longer writes for a new EC (spec 5.2, phase 1)
  - Orphan rotation creates the new row carrying this request's browser ingest plus the removals, opt-outs, `oc` and `iv` from `ts-eids-r`, never positive staged IDs. When the create is `Written`, the response deletes the `ts-eids-r` cookies and sets a fresh `ts-eids-s` from the created row. Rotation empties the mutation and the markers.

What each path does (spec 5.2, 5.3, 5.6):

| Path                                                      | KV                                                                    | Cookies                                                                                                                  |
| --------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| New EC (writer or not)                                    | create before routing, carrying browser ingest (`generate_if_needed`) | `ts-ec` as today. `ts-eids-s` from the created row.                                                                      |
| Returning EC, writer                                      | none (phase 4 flushes the mutation)                                   | `ts-eids-s` only under phase 3's rules: something staged, a marker, a merge change, or a missing, failed or stale cookie |
| Returning EC, stager                                      | none                                                                  | `ts-eids-s` when it saved an entry or its KV read changed the cookie. A new `ts-eids-r` when it produced a removal.      |
| Unusable consent, not withdrawn                           | none                                                                  | a new `ts-eids-r` when it produced a removal. `ts-eids-s` expired.                                                       |
| Consent withdrawn                                         | today's tombstone path, unchanged                                     | `ts-eids-s` expired. Every `ts-eids-r` deleted when the tombstone write succeeded, kept when it failed.                  |
| Any KV read that found a tombstone                        | none                                                                  | `ts-eids-s` expired, every `ts-eids-r` deleted                                                                           |
| Any KV read that found the row missing (no recovery here) | none                                                                  | `ts-eids-s` replaced by the minimal `m: 1` cookie. `ts-eids-r` kept.                                                     |

Flush checks for positive staged entries (spec 5.3). Write each as a predicate with its own test:

- The cookie opened with this EC's ID (guaranteed by `open`).
- `now - sa <= staged_max_age_sec`.
- The writer `w` is in the catalog and still claims the source. For an owned source it also owns it.
- `gate_eid_updates_by_consent` and the writer's `policy_allows` pass under this request's consent.
- The writer has no sticky opt-out, unless the cookie stages a clear `oc` later than that opt-out's `at`.
- Conversion: `OwnerWrite { expected: pv }` when the writer owns the source, else `FillMissing`. A `b: 1` entry becomes `FillMissing { writer: None }`.

Removals and staged `o`, `oc` and `iv` skip every check except the EC binding (spec 5.6).

- [ ] **Step 1: Write the failing tests**

Add a `mod identity_flow` test module to `finalize.rs`. Build requests and contexts the way the existing finalize tests do (`grep -n "fn finalize_" crates/trusted-server-core/src/ec/finalize.rs | head -40`). Use `KvIdentityGraph::write_counting`. Write these tests. The first two are shown in full:

```rust
    #[test]
    fn returning_writer_navigation_adds_ingest_to_the_mutation_and_writes_nothing() {
        let (graph, store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_live_row(&graph, KvEntry::minimal("other.example.com", "x", 1));
        let mut context = returning_context(&ec_id, Writer::Yes);
        let mut response = html_response();

        ec_finalize_response(
            &settings_with_partner("sharedid.org"),
            &mut context,
            Some(&graph),
            &registry_with_partner("sharedid.org"),
            None,
            Some("example-shared-id"),
            &mut response,
            &identity_finalize_for_test(),
        );

        assert_eq!(store.writes().total(), 0, "finalize must not write for a returning writer");
        assert!(!context.identity().mutation().is_empty(), "should queue the sharedId fill");
        assert!(set_cookie_names(&response).is_empty(), "no Set-Cookie on an unchanged returning HTML response");
    }

    #[test]
    fn auction_stager_saves_the_kv_gap_into_ts_eids_s() {
        let (graph, store) = KvIdentityGraph::write_counting("ec-store");
        let ec_id = seed_live_row(&graph, KvEntry::minimal("other.example.com", "x", 1));
        let mut context = returning_context(&ec_id, Writer::No);
        context.set_eid_sync_source(EidSyncSource::Auction);
        context.set_client_eids(vec![eid("gap.example.com", "body-only-uid")]);
        context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let mut response = json_response();

        ec_finalize_response(
            &settings_with_partner("gap.example.com"),
            &mut context,
            Some(&graph),
            &registry_with_partner("gap.example.com"),
            None,
            None,
            &mut response,
            &identity_finalize_for_test(),
        );

        assert_eq!(store.writes().total(), 0, "a stager never writes KV");
        let cookie = open_set_s_cookie(&response, &ec_id).expect("should set ts-eids-s");
        assert_eq!(cookie.ids.len(), 1, "should save one entry");
        assert!(cookie.ids[0].is_browser_gap(), "as a b:1 KV-gap entry");
        assert_eq!(cookie.ids[0].uid(), "body-only-uid", "with the body's UID");
    }
```

The others:

- `stager_never_saves_positive_entries_when_consent_denies_persistence` (it may still set a `ts-eids-r`)
- `unusable_consent_path_still_sets_removal_cookie_and_expires_ts_eids_s`
- `withdrawal_expires_ts_eids_s_and_deletes_removals_only_after_tombstone_lands`
- `tombstone_read_expires_ts_eids_s_and_deletes_removal_cookies`
- `missing_row_on_stager_sets_minimal_m_cookie_and_keeps_removals`
- `orphan_rotation_carries_removals_but_never_positive_staged_ids` (with a `ts-eids-s` holding a `k: 0` provider entry and a `ts-eids-r` removal: the created row has the removal's opt-out and none of the staged IDs)
- `new_visitor_create_carries_browser_ingest_in_one_write` (one `Add` write, and the row holds the sharedId)
- `flush_checks_drop_stale_unowned_disabled_and_opted_out_entries_but_never_removals` (one assertion per check, each paired with a control entry that survives)
- `config_change_discards_positive_entries_and_keeps_markers_and_removals`

`identity_finalize_for_test()`, `open_set_s_cookie`, `set_cookie_names`, `seed_live_row`, `returning_context` and the response builders go in the test module. Build them on `ec/identity/test_support.rs`.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- ec::finalize::tests::identity_flow`
Expected: compile errors, then assertion failures (finalize still writes).

- [ ] **Step 3: Implement**

Restructure `ec_finalize_response`:

1. At the top, call `ec_context.open_identity_cookies(...)` once (it is idempotent and needs the EC ID).
2. Unusable consent: keep today's code, then stage any removals the route produced (Task 38 adds captures, so in Part 1 these come only from `ts-eids-r` carry-over), expire `ts-eids-s`, and call `emit_identity_cookies`. On withdrawal, delete the `ts-eids-r` cookies only when every tombstone write returned success. Check what `apply_withdrawal_tombstones` reports. If it returns nothing, make it return whether every write succeeded.
3. Returning EC with a graph:
   - Collect browser ingest as today, then consent-gate it.
   - If the request read KV (`kv_snapshot` is not `NotRead`), run `merge_with_kv` on the cookie and handle tombstone, missing and orphan recovery as the table says. Today's `confirm_then_recover_orphaned_ec` stays, with its `updates` argument extended to carry the `ts-eids-r` removals into the created row.
   - **Writer:** add ingest, then the flush-checked staged entries, then every `ts-eids-r` batch, to the mutation. Set `staged.s_cookie` only under the phase 3 rules. Record which `ts-eids-r` cookies the phase 1 read shows applied (`r_batch_applied`) or unopenable, in `staged.delete_r`.
   - **Stager:** run the KV-gap logic (Task 26 adds the `/auction` part) and save. Never touch `staged.delete_r`.
   - Remove the call to `sync_eid_cookie_updates` and the `EidCookieSyncOutcome` measurement for these paths. Keep `record_eid_sync_terminal` for the new-EC create only, or delete it if nothing uses it.
4. New EC: `generate_if_needed` already wrote the seeded row. Finalize sets `ts-ec` as today and stages `ts-eids-s` from the created row.
5. Call `reconcile_pull_sync_marker` as today, then `emit_identity_cookies`.

In `ec/mod.rs`, give `generate_if_needed` a `seed: &[PartnerIdUpdate]` parameter and apply it to the `KvEntry::new(..)` it creates, via `apply_partner_id_updates`, before `create_if_absent`. In `app.rs`, parse `ec.eids_cookie` and `ec.sharedid_cookie` with `collect_eid_updates` and `gate_eid_updates_by_consent` before calling `generate_if_needed`. Both helpers are `pub(crate)` in core, so add a `pub fn browser_seed_updates(settings, registry, consent, eids_cookie, sharedid_cookie, now) -> Vec<PartnerIdUpdate>` wrapper in `ec/finalize.rs` for the adapter.

`ec_finalize_response` callers: the Fastly adapter's `finalize_with_ec_state` (`main.rs`, about line 597) and `publisher.rs` tests. Build `IdentityFinalize` from `AppState`, which holds the catalog and keys built once per settings load (Task 29 adds them to `AppState`). Until then, construct them in `finalize_with_ec_state` from settings.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- ec::finalize` and then the full `cargo test-fastly`
Expected: PASS. Existing finalize tests that asserted a KV write on a returning navigation, `/auction` or `/_ts/page-bids` must now assert no write plus a queued mutation or a staged cookie. Update each one and list them in the commit body.

- [ ] **Step 5: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Queue browser ingest in the writer's mutation and stage it on stagers" -m "Returning-visitor finalize no longer writes KV. The writer adds ingest, staged entries and removal batches to its single mutation. Stagers save positive entries in __Host-ts-eids-s and removals in a new __Host-ts-eids-r cookie. A new visitor's create now carries browser ingest, so it stays one write."
```

### Task 26: `/auction` saves the KV gap; `/_ts/page-bids` stops ingesting

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (after the KV read at about lines 307-311)
- Modify: `crates/trusted-server-core/src/publisher.rs` (the `/_ts/page-bids` ingest at about line 6925)
- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (stager branch)
- Test: `auction/endpoints.rs`, `publisher.rs`

**Interfaces:**

- Produces: `pub(crate) fn kv_gap_entries(body_eids: &[Eid], snapshot: &EcKvSnapshot, registry: &PartnerRegistry, consent: &ConsentContext, invalidated: &BTreeSet<String>, now: u64) -> Vec<PartnerIdUpdate>`. For each registered source in the consent-gated body set that the KV row lacks or holds only expired, it picks the UID with `collect_eid_updates`'s rules and skips a UID whose hash is in the owner's `iv`.

Rules (spec 5.3):

- Save only when the client set came from the body, not from the `ts-eids` cookie.
- Save nothing when the KV read failed, found the row missing, or found a tombstone.
- `/auction` returns early on consent denial before it reads KV. Then nothing is saved.
- `/_ts/page-bids` no longer ingests `ts-eids` into KV. Its `EidSyncSource::PageBids` still lets finalize run cookie upkeep from its KV read.

- [ ] **Step 1: Write the failing tests**

In `auction/endpoints.rs`, next to the existing `/auction` + KV tests:

- `auction_body_only_source_is_staged_not_written`: a registered source in the body and not in KV. Assert zero writes and a `ts-eids-s` `Set-Cookie` holding it.
- `auction_cookie_sourced_client_set_stages_nothing`: no body EIDs, `ts-eids` cookie present. No `ts-eids-s`.
- `auction_with_missing_row_stages_no_gap_and_sets_m`.
- `auction_skips_source_kv_already_holds_and_fills_expired`.

In `publisher.rs`:

- `page_bids_never_writes_kv`: with a `ts-eids` cookie holding a registered source missing from KV, zero writes.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test-fastly -- auction::endpoints::tests::auction_` and `cargo test-fastly -- publisher::tests::page_bids_never_writes_kv`
Expected: failures (today `/auction` and page-bids write).

- [ ] **Step 3: Implement**

In finalize's stager branch, when `eid_sync_source == Some(EidSyncSource::Auction)` and `client_eids` came from the body (add a `client_eids_from_body: bool` to `EcContext`, set by `handle_auction` where it chooses body or cookie), call `kv_gap_entries` and save each as `SEntry::staged_browser`. In `publisher.rs`, remove the page-bids path that leads to a KV write. With Task 25 in place, finalize no longer writes for a returning EC, so check that page-bids has no other direct write (`grep -n "upsert\|sync_eid\|create" crates/trusted-server-core/src/publisher.rs`).

- [ ] **Step 4: Run and see them pass**

Run: `cargo test-fastly -- auction::` and `cargo test-fastly -- publisher::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Stage the /auction KV gap and stop /_ts/page-bids KV ingest"
```

### Task 27: The removal overflow write

**Files:**

- Modify: `crates/trusted-server-core/src/ec/kv.rs` (new `apply_removals_now`)
- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (`emit_identity_cookies`)
- Test: `ec/kv.rs`, `ec/finalize.rs`

**Interfaces:**

- Produces: `KvIdentityGraph::apply_removals_now(&self, ec_id: &str, mutation: &IdentityMutation, context: &ApplyContext<'_>) -> RemovalWriteOutcome` with `RemovalWriteOutcome { Written, AlreadyApplied, Failed }`. It reads the row, applies only the batch's removals, opt-outs, clears and `iv` (through `mutation::apply` on a removals-only mutation), and writes with `IfGenerationMatch`. On a generation mismatch it re-reads and retries, up to 5 attempts in total, as the withdrawal path does. Any other error (a per-key rate limit included) stops at once with `Failed`.

`emit_identity_cookies` calls `plan_r_batch`. On `RPlan::Overflow` it calls `apply_removals_now` before the response is sent. `Written` and `AlreadyApplied` set no removal cookie. `Failed` sets `split_r_batch`'s cookies. This is the second and last page-load exception to G4 (spec 6.2).

- [ ] **Step 1: Write the failing tests**

- `overflow_batch_is_written_before_send_with_no_cookie`: a batch of 12 removals, a live row. Exactly one `IfGenerationMatch` write and no `ts-eids-r` `Set-Cookie`.
- `overflow_write_failure_falls_back_to_split_cookies`: a failing store. Every removal is present across the new cookies, and no other cookie is rewritten.
- `overflow_retries_only_on_generation_mismatch`: a store that fails the CAS twice and then succeeds gives 3 attempts. A store that returns a non-CAS error gives 1 attempt.
- `four_openable_cookies_force_overflow`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- overflow`
Expected: FAIL, then PASS after implementing. Model the retry loop on `ec/kv.rs`'s withdrawal tombstone loop (`grep -n "fn write_withdrawal_tombstone\|attempt" crates/trusted-server-core/src/ec/kv.rs`).

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Write overflowing removal batches before send" -m "When a removal batch does not fit one cookie, or four removal cookies are already present, it is applied to KV with the withdrawal path's CAS retries. If that write fails it is split across new cookies, never coarsened."
```

### Task 28: The server set in the auction merge

**Files:**

- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (`merge_auction_eids`, about line 505)
- Modify: `crates/trusted-server-core/src/publisher.rs` (navigation and page-bids merge, about lines 5547-5568)
- Modify: `crates/trusted-server-core/src/ec/eids.rs` (`to_eids` adds provenance)
- Test: `auction/endpoints.rs`, `publisher.rs`

**Interfaces:**

- Produces: `merge_auction_eids(client: &[Eid], server: &ServerSet, registry: &PartnerRegistry) -> Vec<Eid>`. `ServerSet` holds, per source, the servable record from the `ts-eids-s` cookie view (staged entries included) overlaid on KV, minus stubs, removed entries (`ts-eids-r`) and expired entries. Build it with `pub(crate) fn server_set(context: &EcContext, serve: &ServeContext<'_>) -> ServerSet`.

Precedence (spec 7.1):

| Source  | Browser sent it                                                         | Browser did not send it                                                            |
| ------- | ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| Owned   | the owner's valid value (`w == owner`, not expired), else the browser's | the owner's valid value, else a browser-seeded value if the source does not expire |
| Unowned | the browser's (today)                                                   | the server value (today)                                                           |

Part 1 has no owners, so only the unowned row applies. Part 2 (Task 36) adds owner denials of browser candidates. A server value uses its stored `atype`, else the partner's, plus its stored UID-level `ext`. A value a provider wrote also gets that provider's EID-level `inserter`, `matcher` and `mm` from `descriptor.provenance(source)`. Sources with `bidstream_enabled = false` are left out. The merged set goes through `gate_eids_by_consent` and into `x-ts-eids` exactly as today.

- [ ] **Step 1: Write the failing tests**

- `server_set_includes_staged_ts_eids_s_entry_for_unowned_source_browser_did_not_send`
- `browser_value_wins_for_unowned_source` (control: today's rule)
- `removed_entry_in_ts_eids_r_is_not_sent_before_it_reaches_kv`
- `expired_entry_is_not_sent`
- `stored_atype_and_ext_are_emitted`
- `x_ts_eids_matches_what_was_sent`

Compare whole `Vec<Eid>` values with every field written out, with no `..Default::default()`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- merge` and `cargo test-fastly -- auction::endpoints`
Expected: FAIL, then PASS.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Merge the server identity set into auction EIDs"
```

### Task 29: Phase 4: the single flush after send, replacing pull sync

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (`run_edgezero_pull_sync_after_send` and its callers at about lines 482-560 and 618-668)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` (`EcFinalizeState` gains the writer flag and the response status; `AppState` gains `ProviderCatalog` and `IdentityKeys`)
- Create: `crates/trusted-server-core/src/ec/identity/flush.rs` (`run_after_send`)
- Modify: `crates/trusted-server-core/src/ec/pull_sync.rs` (delete `dispatch_pull_sync`, `build_pull_sync_context` and `PullSyncContext`. Keep `entry_is_pull_complete` and the helpers generic pull uses)
- Modify: `crates/trusted-server-core/src/ec/finalize.rs` (writer phase 3: dry runs and pending markers)
- Test: `flush.rs`, `adapter-fastly/src/main.rs`

**Interfaces:**

- Produces: `pub fn run_after_send(input: AfterSendInput<'_>) -> FlushOutcome`. `AfterSendInput` holds `settings`, `catalog`, `keys`, `services`, `graph: &KvIdentityGraph`, `context: &EcContext`, `status: StatusCode` and `rate_limiter`. `FlushOutcome` is `Skipped | NoChange | Written | Dropped(&'static str) | WriteFailed`.

Phase 3 additions in finalize (writer, returning EC, status 2xx or 304; spec 5.2):

- For each provider that is eligible on the merged view in phase `AfterSendDryRun`:
  - **Without private state:** build its `ResolveInput` with `dry_run = true` and call `request`. `Some` sets a pending marker `ps.<id>.p = now` in `staged.s_cookie`. `None` sets `ps.n = now + refresh_after_sec` in the mutation.
  - **With private state:** no dry run. Set the marker when it is due on the cookie view.
- Unowned dry runs return `Some` only when the phase 1 read showed the source missing and the pull-complete marker is absent (spec 5.6). Enforce this in the coordinator before the dry run. The provider does not decide it.
- Section 6.3's phase-1 read triggers must hold. The adapter reads KV in phase 1 (the existing preload) whenever any trigger is true. Add `pub(crate) fn phase1_read_needed(context: &EcContext, catalog, settings, auction_needs_row: bool) -> bool` in `ec/identity/request.rs`, and make the publisher preload decision (`publisher.rs`, `auction_needs_row` and the preload at about lines 3110-3117 and 4819-4831) consult it in addition to today's rule.

Phase 4 (`run_after_send`, spec 5.2, steps 1 to 5):

1. If the request is not a returning-EC writer, or `status` is not 2xx or 304: `Skipped`. Hold-mode answers already live in the cookie.
2. If the mutation is empty and no marker was set this request: `Skipped`. No read.
3. If phase 1 read the row (`Present` with a generation) and no marker is set, use that snapshot. Otherwise call `graph.get(ec_id)`. A tombstone, `consent.ok = false`, a missing row or a failed read means drop and stop.
4. Recompute eligibility steps 4 to 6 from the live row for every marked provider. Rebuild each request from the live row (the live state for stateful providers). Dispatch with `coordinator::dispatch`. Turn the outcomes into changes and add them to a copy of the mutation. A marked provider not dispatched for a reason the live row does not show (the burst cap, or the breaker in Part 2) gets `n = max(n, now + 60)`.
5. If the live row has no `cluster_size`, compute it with `compute_cluster_size` and `set_cluster_size`.
6. `mutation::apply` against the live entry. If `changed`, write once with `IfGenerationMatch(generation)`. Never retry. Log failure at `warn` with `log_id`, and return `WriteFailed`.

Remove `run_edgezero_pull_sync_after_send` and `prepare_pull_sync_after_send`. Their callers call `run_after_send`. The adapter captures the response status before `send_edgezero_response` consumes the response (`main.rs` around lines 510-517) and passes it in. Build the KV graph after send exactly as today (`require_identity_graph`).

- [ ] **Step 1: Write the failing tests**

In `flush.rs`:

- `writer_flushes_mutation_once_with_generation_match` (1 `IfGenerationMatch` write)
- `writer_with_nothing_to_do_reads_and_writes_nothing` (0 reads, 0 writes)
- `non_2xx_or_304_status_drops_the_mutation`
- `tombstone_on_reread_drops_mutation_and_calls_no_provider` (the fake HTTP client sees zero calls)
- `marked_generic_pull_provider_dispatches_after_reread_and_its_uid_lands_in_the_same_write`
- `cas_conflict_is_not_retried` (1 attempt, `WriteFailed`)
- `burst_capped_marked_provider_gets_sixty_second_backoff`
- `missing_cluster_size_is_added_to_the_one_write`

In the adapter (`main.rs` tests): replace `pull_sync_noop_states_skip_post_send_graph_factory` with `after_send_noop_states_skip_post_send_graph_factory`, which checks that no graph is built when there is nothing to flush.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- flush` and `cargo test-fastly`
Expected: FAIL, then PASS.

- [ ] **Step 3: Delete pull sync's dispatcher**

Delete `dispatch_pull_sync`, `build_pull_sync_context`, `PullSyncContext`, `InFlightPull` and `pull_rate_limit_key` from `ec/pull_sync.rs`. Port every remaining pull sync test whose behaviour is unchanged to `generic_pull.rs` or `flush.rs` before deleting it. For each behaviour change in spec 10.4, write a test named after it:

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
git commit --signoff -S -m "Flush identity changes once after send and run pull partners there" -m "Phase 4 replaces the pull sync after-send hook. It reuses the phase 1 snapshot when no provider is marked, otherwise re-reads the row, dispatches marked providers, and applies the whole mutation in one IfGenerationMatch write that is never retried. Pull partners now run only on a returning visitor's writer navigation, and back off through ps."
```

### Task 30: Prove the one-write invariant, reads and G10

**Files:**

- Create: `crates/trusted-server-adapter-fastly/tests/identity_writes.rs` (or a `#[cfg(test)] mod identity_writes` in `app.rs` if the adapter has no integration-test harness; check how existing adapter tests drive a full request: `grep -rn "fn .*end_to_end\|TrustedServerApp" crates/trusted-server-adapter-fastly/src | head`)
- Test only

**Interfaces:**

- Consumes: `WriteCountingEcKv`, the fake HTTP client, and the adapter's request-driving test helpers

- [ ] **Step 1: Write the scenario table as tests**

One test per row of spec 13's "One-write invariant" table. Each drives a full request through the adapter (routing, finalize, phase 4) and asserts the exact write counts by mode:

| Test                                                        | Writes                                            |
| ----------------------------------------------------------- | ------------------------------------------------- |
| `new_visitor_navigation_writes_once`                        | 1 `Add`                                           |
| `new_visitor_collision_remints_and_writes_once`             | 1 `Add` that lands (a precondition failure first) |
| `returning_visitor_with_changes_writes_once`                | 1 `IfGenerationMatch`                             |
| `returning_visitor_without_changes_writes_nothing`          | 0                                                 |
| `revalidation_304_with_changes_writes_once`                 | 1                                                 |
| `returning_navigation_answered_3xx_4xx_5xx_writes_nothing`  | 0 each                                            |
| `new_visitor_prefetch_writes_once`                          | 1 `Add`                                           |
| `returning_prefetch_writes_nothing`                         | 0                                                 |
| `new_visitor_without_sec_fetch_dest_writes_once`            | 1 `Add`                                           |
| `returning_iframe_or_no_sec_fetch_dest_writes_nothing`      | 0                                                 |
| `integration_proxy_navigation_minting_ec_writes_once`       | 1 `Add`                                           |
| `auction_page_bids_identify_proxy_and_assets_write_nothing` | 0 each                                            |
| `consent_withdrawal_writes_tombstone_and_marker`            | today's count, per ID                             |
| `overflowing_removal_batch_writes_before_send`              | 1 `IfGenerationMatch`                             |
| `cas_conflict_on_flush_is_one_attempt`                      | 1 failed, no retry                                |

Also write:

- **Reads:** `returning_visitor_with_recent_cookie_and_nothing_due_reads_zero_times_on_navigation`, and `auction_and_page_bids_keep_one_read`.
- **G10:** `body_only_source_reaches_kv_on_next_navigation` (an `/auction` with a body-only registered source, then a navigation: the source is in KV after the navigation's single write), and `gap_too_large_for_cookie_is_saved_again_by_next_auction`.
- **Failed flush:** `refused_flush_keeps_staged_entries_for_next_navigation`.
- **Orphans:** `deleted_row_is_found_within_kv_revalidate_sec_and_rotated` and `stager_that_sees_missing_row_sets_m_and_next_writer_recovers`.
- **Withdrawal:** `withdrawal_on_auction_after_navigation_send_makes_phase_four_drop_and_call_nothing`.

- [ ] **Step 2: Run them**

Run: `cargo test-fastly -- identity_writes`
Expected: PASS. A failure here is a real bug in Tasks 23 to 29. Fix it in the task that owns the behaviour, and do not loosen the test.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-adapter-fastly
git commit --signoff -S -m "Test the one-write-per-page-load invariant end to end"
```

### Task 31: Metrics, docs and the Part 1 PR

**Files:**

- Modify: `crates/trusted-server-core/src/ec/finalize.rs` or `ec/identity/request.rs` (metrics)
- Modify: `docs/guide/edge-cookies.md`, `docs/guide/configuration.md`, `docs/guide/ec-setup-guide.md`, `docs/guide/api-reference.md`, `CHANGELOG.md`, `trusted-server.example.toml`

- [ ] **Step 1: Metrics**

Find how the repo emits request metrics or counters today (`grep -rn "record_eid_sync_terminal\|metric\|counter" crates/trusted-server-core/src/ec/finalize.rs | head`). Emit the same way, with no IDs and no cookie values:

- the inbound `Cookie` header size in bytes
- the number and total sealed size of `__Host-ts-eids-r-*` cookies per request
- KV reads and writes per request, labelled by route kind (writer, stager, new EC)
- phase 4 outcome (`FlushOutcome` as a label)

Add one test that asserts the counts are recorded for a writer navigation.

- [ ] **Step 2: Docs**

- `edge-cookies.md`: add sections for `__Host-ts-eids-s` and `__Host-ts-eids-r-*` (purpose, sealed, host-only, `HttpOnly`, one-year lifetime, 2,048 and 512 byte caps, never read by the shim, never sent to the origin or Prebid Server). Also describe the writer-request rule in plain words: only a full page navigation writes KV, once, after the page is sent.
- `configuration.md`: the `[ec.identity]` table, with every key, its default and its meaning, from spec 10.1. Note that `ec.pull_sync_concurrency` is deprecated.
- `ec-setup-guide.md`: pull partners now run from a visitor's second page load and back off on errors.
- `api-reference.md`: `/_ts/api/v1/identify` and `/_ts/page-bids` no longer write KV, and `/identify` omits expired IDs.
- `CHANGELOG.md` `### Changed`: one entry each for fewer KV writes (with the writer-request rule), `/auction`, `/_ts/page-bids` and `/identify` no longer writing, pull partners from the second page load, and the two new cookies. Mark the rollback note: after this release, do not roll back past it once providers write metadata (spec 14).

Run `cd docs && npm run format` and the root markdown prettier command from `AGENTS.md` gate 8.

- [ ] **Step 3: Commit**

```bash
git add -A crates/ docs/ CHANGELOG.md trusted-server.example.toml
git commit --signoff -S -m "Document the single identity write and the new identity cookies"
```

- [ ] **Step 4: Part 1 gate and PR**

Run `/check-ci`. Every gate must pass, including `scripts/lint-wasm-clock.sh` (new core code must not call `std::time::{Instant,SystemTime}::now`. Use `checked_current_timestamp()` and `web_time` where you need a clock) and the parity crate. Open the PR against `feat/1246-identity-cookie-strips` with `pr-creator`. Title: "Write identity KV once per page load". The description lists the behaviour changes from spec 14 step 1 and the `test-utils` feature reuse.

---

# Part 2: Providers (spec 14, step 2)

Part 2 lets integrations register providers, adds ownership, hold mode, passive capture, provider state and sticky opt-outs, the circuit breaker, and the consent decoding that strict policies need. No vendor ships here: ID5, LiveRamp, Lockr and UID2 each get their own spec and plan (spec 2.2). This part proves the framework with the fictional `exampleid` and capture providers from spec 4.6, registered only in tests.

Before you start: `git switch feat/1246-identity-single-write && git switch -c feat/1246-identity-providers`.

### Task 32: Registering identity providers

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (`IntegrationRegistration`, the builder around line 641, `with_plan` around line 842)
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (`from_settings` adds registered providers)
- Test: `integrations/registry.rs`

**Interfaces:**

- Produces:
  - `IntegrationRegistrationBuilder::with_identity_provider(self, provider: Arc<dyn IdentityProvider>) -> Self`
  - `IntegrationRegistrationBuilder::try_with_identity_provider<P: IdentityProvider + 'static>(self, provider: Result<P, Report<IdentityProviderError>>) -> Result<Self, Report<TrustedServerError>>`. A failure becomes `TrustedServerError::Integration { integration: <id>, message: <current_context()> }`.
  - `IntegrationRegistry::identity_providers(&self) -> Vec<Arc<dyn IdentityProvider>>`
  - In `with_plan`: every capture path and prefix must lie under one of the registering integration's `IntegrationProxy::routes()`. Otherwise it is a `Configuration` error naming the integration.
  - `ProviderCatalog::from_settings(settings, partner_registry, integration_registry)` adds the registered providers after generic pull, and rejects duplicate IDs.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn try_with_identity_provider_converts_a_constructor_error_once() {
        let failing: Result<ExampleTestProvider, Report<IdentityProviderError>> =
            Err(IdentityProviderError::config("bad partner id"));
        let error = IntegrationRegistration::builder("exampleid")
            .try_with_identity_provider(failing)
            .expect_err("should fail");
        assert!(
            matches!(error.current_context(), TrustedServerError::Integration { integration, .. } if integration == "exampleid"),
            "should name the integration"
        );

        let built = IntegrationRegistration::builder("exampleid")
            .try_with_identity_provider(Ok(ExampleTestProvider::new()))
            .expect("should register")
            .build();
        assert_eq!(built.identity_providers.len(), 1, "control: one provider registered");
    }

    #[test]
    fn capture_rule_outside_the_integrations_routes_is_rejected_at_startup() {
        let settings = settings_with_capture_integration("/integrations/other/api");
        let error = IntegrationRegistry::with_plan(&settings, empty_plan()).expect_err("should reject");
        assert!(format!("{error}").contains("examplecapture"), "should name the integration");
        let ok = settings_with_capture_integration("/integrations/examplecapture/api/token");
        IntegrationRegistry::with_plan(&ok, empty_plan()).expect("control: a rule under its own routes");
    }
```

`ExampleTestProvider` and the capture integration are test-only types defined in this module's tests. The capture integration registers an `IntegrationProxy` with routes under `/integrations/examplecapture/` and a capture provider whose path comes from the helper's argument. Follow how existing registry tests register a stub integration (`grep -n "from_request_filters\|fn stub_" crates/trusted-server-core/src/integrations/registry.rs | head`).

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- integrations::registry::tests::try_with_identity_provider` and `cargo test-fastly -- integrations::registry::tests::capture_rule`
Expected: FAIL, then PASS. Add `identity_providers: Vec<Arc<dyn IdentityProvider>>` to `IntegrationRegistration` and merge it in `with_plan`, as proxies are merged. A route matches a capture path when the path equals the route's path or, for a wildcard route, starts with its prefix. Mirror how `handle_proxy` matches routes today.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Let integrations register identity providers"
```

### Task 33: Ownership and deploy-time validation

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (`EcPartner` gains `owner`)
- Modify: `crates/trusted-server-core/src/ec/registry.rs` (`PartnerConfig.owner`)
- Modify: `crates/trusted-server-core/src/config.rs` (`validate_identity_integration`, the identity validation pass in deploy and runtime validation)
- Modify: `crates/trusted-server-core/src/ec/identity/config.rs` (`validate_identity_catalog`)
- Modify: `crates/trusted-server-core/src/ec/identity/catalog.rs` (owners from `[[ec.partners]].owner`, plus `sources` narrowing)
- Modify: `crates/trusted-server-core/src/ec/identity/generic_pull.rs` (an owned pull partner uses `pull_sync_ttl_sec` as `refresh_after_sec`)
- Test: `config.rs`, `ec/identity/config.rs`

**Interfaces:**

- Produces:
  - `EcPartner.owner: Option<String>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`
  - `fn validate_identity_integration<T: IdentityProviderConfig + DeserializeOwned>(settings: &Settings, id: &str) -> Result<Option<ProviderDescriptor>, Report<TrustedServerError>>`. It parses `[integrations.<id>]` as `T` and, when enabled, calls `build_descriptor`. It never builds a provider and never reads secrets.
  - `pub(crate) fn validate_identity_catalog(descriptors: &[ProviderDescriptor], partners: &[EcPartner], identity: &IdentitySettings) -> Result<(), Report<TrustedServerError>>`, with every error of spec 10.2:
    - `owner` names a provider that is not enabled
    - `owner` names a provider whose (narrowed) descriptor lacks that source
    - an enabled provider claims a source with no `[[ec.partners]]` entry
    - two enabled providers claim a source one of them owns (`sources` narrowing applied first)
    - `sources` names an undeclared source, or narrows to nothing
    - two providers have the same ID
    - `[ec.identity.providers.<id>]` names an unknown provider
    - more than 32 enabled providers, generic pull included
  - The validation runs in `validate_settings_for_deploy` (next to `PartnerRegistry::validate_config_for_deploy`) and in `validate_settings_for_runtime`. The deploy path collects descriptors from a new `identity_descriptors_for_deploy(settings)` list, which has one `validate_identity_integration::<T>` line per identity integration. It is empty in this PR except under `#[cfg(test)]`, where the test `ExampleIdConfig` is added, so the list's shape is exercised.

- [ ] **Step 1: Write the failing tests**

One test per error above, each paired with a control config that passes. Also:

- `config_without_ec_identity_is_accepted`
- `build_descriptor_runs_in_deploy_validation_without_secrets`: a config whose secret field holds a key name, not a value, still validates

For example:

```rust
    #[test]
    fn owner_must_name_an_enabled_provider_that_declares_the_source() {
        let descriptors = vec![descriptor("exampleid", &["exampleid.example.com"])];
        let ok = vec![partner("exampleid.example.com", Some("exampleid"))];
        validate_identity_catalog(&descriptors, &ok, &IdentitySettings::default()).expect("control");

        let unknown = vec![partner("exampleid.example.com", Some("missing"))];
        let error = validate_identity_catalog(&descriptors, &unknown, &IdentitySettings::default()).expect_err("unknown owner");
        assert!(format!("{error}").contains("missing"), "should name the owner");

        let wrong_source = vec![partner("exampleid.example.com", None), partner("other.example.com", Some("exampleid"))];
        validate_identity_catalog(&descriptors, &wrong_source, &IdentitySettings::default()).expect_err("owner lacks source");
    }
```

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- ec::identity::config` and `cargo test-fastly -- config::`
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
- Modify: `crates/trusted-server-core/src/consent/mod.rs` (`stricter_of`)
- Modify: `crates/trusted-server-core/src/ec/identity/policy.rs` (new blocks, and the fingerprint bits)
- Test: `consent/gpp.rs`, `consent/mod.rs`, `policy.rs`

**Interfaces:**

- Produces:
  - Decoded on the GPP form: `us_sharing_opt_out: Option<bool>`, `us_targeted_advertising_opt_out: Option<bool>`, `us_gpc: Option<bool>` and `us_known_child: Option<bool>` (US National section), and the section 6 US Privacy string read through the existing US Privacy decoder. Use the `iab_gpp` fields the spec names: `sharing_opt_out` (US National, California) and `targeted_advertising_opt_out` (US National and the state sections that define it). Check the `iab_gpp` version in `Cargo.lock` and read its section structs before writing the extraction.
  - `policy_allows` additionally blocks a strict policy on the sharing opt-out, the targeted-advertising opt-out, GPP's GPC field, and a known child without consent (a known child blocks every provider, whatever the relaxations).
  - `pub(crate) fn fingerprint_bits(policy: &ConsentPolicy, consent: &ConsentContext, signals: PolicySignals) -> String`: a canonical string of the TCF purpose bits and vendor bit the policy reads, the US opt-outs it does not relax, and GPC and DNT unless relaxed. Never the raw consent string.
  - `pub(crate) fn stricter_of(cookie: &ConsentContext, body: &ConsentContext) -> ConsentContext`: field by field, any opt-out, missing purpose or missing vendor consent in either wins
  - A hand-written `impl fmt::Debug for ConsentContext` that prints `[redacted]` for every raw string

- [ ] **Step 1: Write the failing tests**

- In `gpp.rs`: one decode test per new field, per section that defines it, using GPP strings built the way existing tests build them. Each asserts the decoded value, next to a control string where the field is not set.
- In `policy.rs`: `strict_blocks_sharing_targeted_gpc_field_and_known_child`, and `targeted_advertising_opt_out_without_sale_opt_out_blocks_strict`.
- In `consent/mod.rs`: `stricter_of_takes_any_opt_out_or_missing_consent_from_either_side`.
- `consent_context_debug_redacts_raw_strings`: `format!("{context:?}")` does not contain the TC string.
- `fingerprint_is_stable_across_cmp_resaves_and_changes_with_a_purpose`: two TC strings with identical decisions but different `LastUpdated` give the same bits, and dropping purpose 4 changes them.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- consent::` and `cargo test-fastly -- ec::identity::policy`
Expected: FAIL, then PASS. Do not change any existing auction consent decision. Run `cargo test-fastly -- auction::` and `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity` to confirm.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src/consent crates/trusted-server-core/src/ec/identity
git commit --signoff -S -m "Decode the GPP opt-outs strict identity policies need" -m "Adds the US sharing and targeted-advertising opt-outs, GPP section 6, and the US National GPC and known-child fields. ConsentContext's Debug output no longer prints raw consent strings."
```

### Task 35: Rate-limiter `record` and `count`, and the circuit breaker

**Files:**

- Modify: `crates/trusted-server-core/src/ec/rate_limiter.rs` (trait)
- Modify: `crates/trusted-server-adapter-fastly/src/rate_limiter.rs` (`FastlyRateLimiter`)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (breaker in dispatch, step 7)
- Test: all three

**Interfaces:**

- Produces:
  - `RateLimiter::record(&self, key: &str) -> Result<(), Report<TrustedServerError>>` and `RateLimiter::count(&self, key: &str) -> Result<u32, Report<TrustedServerError>>`. Both are trait methods without defaults, so every implementation must decide. Fastly uses `counter.increment(key, 1)` and `counter.lookup_count(key, CounterDuration::SixtySecs)`.
  - Breaker: before dispatch, `count("identity-fail:<provider>") >= breaker_failures_per_minute` gives `CallOutcome::BreakerOpen` (not dispatched, `ps` unchanged). After dispatch, only `TransportFailure` outcomes (transport errors, timeouts, 429 and 5xx) call `record("identity-fail:<provider>")`. `HoldCutOff` is a budget cut, not a provider failure, and never records (spec 4.7).
  - A limiter error is logged, and the breaker is treated as closed. Skip the burst cap check on that request too, failing closed for disclosure: no dispatch.

- [ ] **Step 1: Write the failing tests**

In core, with a `FakeRateLimiter` in `test_support.rs` that stores counts in a `Mutex<HashMap>`:

- `breaker_opens_at_threshold_and_skips_every_visitor`
- `four_xx_and_malformed_never_open_the_breaker`: 1,000 visitors with 400 responses leave `count` at 0
- `hold_cut_off_does_not_record_a_failure`
- `burst_cap_is_checked_after_every_other_eligibility_step`: an ineligible provider never increments the burst counter

In the adapter: a unit test for `record` and `count` against Viceroy's rate counter, following the existing `mod tests` in `rate_limiter.rs`.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- rate_limiter` and `cargo test-fastly -- ec::identity::coordinator`
Expected: FAIL, then PASS.

- [ ] **Step 3: Commit**

```bash
git add -A crates/
git commit --signoff -S -m "Add a per-provider circuit breaker on transport failures"
```

### Task 36: Provider state, sticky opt-outs, re-grants and owner denials

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (opt-out clearing on re-grant)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` and `publisher.rs` (owner denials filter browser candidates in the merge)
- Modify: `crates/trusted-server-core/src/ec/admin.rs` (an operator clear of `ps.o`)
- Test: `coordinator.rs`, `auction/endpoints.rs`, `ec/admin.rs`

**Interfaces:**

- Produces:
  - `pub(crate) fn regrant_clears_opt_out(descriptor: &ProviderDescriptor, opt_out: &KvOptOut, consent: &ConsentContext, keys: &IdentityKeys, signals: PolicySignals) -> bool` (spec 8):
    - **GVL vendor:** a TC string whose `LastUpdated` is after `o.at` and that grants the vendor and purposes.
    - **Any provider, under GPP:** the applicable US section shows none of the opt-outs the policy reads, where the stored `fp` showed one. Compare the current `fingerprint_bits`-derived fingerprint's opt-out part.
    - A jurisdiction change, a newly present GPP string, or a relaxed signal never clears.
  - When it clears, the writer adds `opt_out_clear = Some(now)` to that provider's `ProviderStateChange`. Clears are writer-only, so they ride the one flush.
  - Merge (spec 7.1, "Owner denials apply to browser candidates"): before the precedence table and before `gate_eids_by_consent`, drop the browser's candidate for an owned source when its UID's keyed hash is in the owner's `iv` (KV `ps.iv` ∪ `ts-eids-r` `iv`), or when the owner has a sticky opt-out (KV ∪ `ts-eids-r`). Unowned sources are unaffected.
  - The owned rows of the precedence table become active: the owner's valid value wins over the browser's. When the browser did not send the source, a browser-seeded value is used only if the source does not expire.
  - Admin: `DELETE /_ts/admin/ec/{id}/providers/{provider}/opt-out`, which clears `ps.o` by writing `oc = now` through the existing admin write path. Add it to the admin route table next to the EC lookup, behind the same Basic-auth coverage, and add the route to the admin coverage test.

- [ ] **Step 1: Write the failing tests**

- `tcf_regrant_after_opt_out_clears_it` and `jurisdiction_change_never_clears_it`
- `gpp_section_without_opt_out_clears_when_fingerprint_had_one`
- `invalidated_uid_in_auction_body_is_not_sent_before_or_after_removal_reaches_kv`
- `owner_opt_out_drops_browser_value_for_every_owned_source`
- `owner_valid_value_beats_browser_value`
- `browser_seeded_value_never_beats_browser_current_value`
- `expiring_owned_source_never_serves_browser_seeded_value`
- `unowned_source_unaffected_by_owner_denials` (control)
- `admin_clears_provider_opt_out`

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- ec::identity` and `cargo test-fastly -- auction::endpoints` and `cargo test-fastly -- ec::admin`
Expected: FAIL, then PASS.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Apply owner precedence, owner denials and opt-out re-grants"
```

### Task 37: Hold mode

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (before the navigation auction dispatch, about lines 4886-4889)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (`handle_auction`, before the merge)
- Modify: `crates/trusted-server-core/src/publisher.rs` (the `/_ts/page-bids` auction, about lines 7077-7090)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (`run_hold`)
- Test: `coordinator.rs`, `auction/endpoints.rs`, `publisher.rs`

**Interfaces:**

- Produces: `pub(crate) fn run_hold(deps: &IdentityDeps<'_>, context: &mut EcContext) -> HoldResults`. It runs eligible `Hold`-phase providers within `hold_budget_ms` from the first dispatch, after recomputing eligibility from the KV row this request read (pre-disclosure revalidation, spec 4.7). It returns the answers as server-set additions for this auction and stages them:
  - A stateless provider's IDs go into `staged.s_cookie` as `k: 0` with `pv`, its `ps` changes are marked `sg: 1`, and its removals and opt-outs go into `staged.new_r_batch`. On the writer they also join the mutation.
  - A stateful provider (writer only) puts its whole `ProviderResult` in the mutation only. Never in the cookie.
  - A cut-off call sets `n = now + hold_retry_sec` and counts no failure.
- `handle_auction` and the navigation take `IdentityDeps` inside the existing deps or context struct so neither passes 7 arguments.
- `/_ts/page-bids` runs hold mode only when its auction runs, because only then does it read KV.

- [ ] **Step 1: Write the failing tests**

With the fake HTTP client's latency control:

- `auction_starts_at_the_hold_budget_when_a_provider_is_slow` (the elapsed fake time equals the budget, and the auction ran)
- `hold_answer_feeds_this_auction_and_is_saved_with_k0`
- `cut_off_call_sets_hold_retry_without_counting_a_failure`
- `stateful_hold_provider_never_runs_on_a_stager`
- `stateful_hold_answer_stays_out_of_the_cookie`
- `writer_navigation_with_non_2xx_status_keeps_hold_answers_in_its_cookie`
- `cookie_only_no_match_stops_a_recall_on_the_next_auction` (the merged view)
- `kv_opt_out_blocks_dispatch_even_when_the_cookie_lacks_it`

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- hold`
Expected: FAIL, then PASS. Use the platform's time source the auction orchestrator uses for its own timeouts. Do not use `std::time::Instant::now` (`scripts/lint-wasm-clock.sh` rejects it in shared crates).

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Run hold-mode identity providers before the auction"
```

### Task 38: Passive capture

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs` (`handle_proxy`, about lines 1031-1061)
- Modify: `crates/trusted-server-core/src/ec/identity/coordinator.rs` (`run_capture`)
- Test: `integrations/registry.rs`, `coordinator.rs`

**Interfaces:**

- Produces: `pub(crate) fn run_capture(deps: &IdentityDeps<'_>, context: &mut EcContext, method: &Method, path: &str, request_consent: Option<ConsentContext>, response_status: StatusCode, response_headers: &HeaderMap, response_body: &[u8])`

`handle_proxy` per request, only when some provider's `Captures::matches(method, path)`:

1. **Before `handle`:** if a matching descriptor declares `with_request_consent` and the request has a body, buffer it with `collect_body_bounded` (`integrations/mod.rs:182`, 256 KiB). Extract only the declared JSON pointers, decode them into a `ConsentContext` with the same decoders the cookie path uses, and re-wrap the bytes with `EdgeBody::from(bytes)` before calling `handle`. A body over the cap or not valid JSON gives `request_consent = None` and allows no positive IDs.
2. **After `handle`,** on 2xx only: buffer the response up to `max_response_bytes`. A larger body is passed through uncaptured. Decode a copy with `decode_body`, call `capture` per matching provider, and re-wrap the original bytes so the browser receives the response unchanged.
3. **Apply:**
   - Removals and opt-outs always apply, whatever the consent.
   - Positive IDs apply only when eligibility steps 1 to 3 pass under `stricter_of(cookie, body)` and the provider has no sticky opt-out, unless this result clears it. A capture-only provider without a GVL ID that returns IDs under consenting cookie and body consent stages `ps.oc = now` beside them.
   - Capture never touches `n`, `f` or the breaker.
   - A state change in a capture result is ignored with a warning.
   - Results are staged on `EcContext` and written by finalize: positive into `ts-eids-s`, removals into `ts-eids-r`. On a writer request they also join the mutation.

- [ ] **Step 1: Write the failing tests**

Using a test capture integration (from Task 32) and its provider:

- `capture_runs_only_for_matching_rules`
- `gzip_and_brotli_responses_are_decoded_for_capture_and_forwarded_unchanged` (byte-equal body to the browser)
- `only_declared_consent_pointers_are_read_from_the_request_body` (a body with an email field. The provider sees no body, and a log capture shows no email)
- `stricter_body_consent_blocks_positive_ids_but_not_removals`
- `revoke_style_opt_out_removes_every_record_the_provider_wrote_including_stubbed_sources`
- `capture_removal_survives_a_concurrent_auction_rewriting_ts_eids_s` (two responses built from the same inbound cookies: the `ts-eids-r` cookie from the capture is still present after the `/auction` response's `ts-eids-s` is applied)
- `revoke_then_reconsent_capture_ends_with_new_ids_and_oc_in_kv` (across a navigation, and again after a failed flush)
- `capture_never_touches_n_f_or_the_breaker`
- `capture_state_change_is_ignored`
- `oversized_proxied_body_passes_through_uncaptured`

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- capture`
Expected: FAIL, then PASS.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Capture identity IDs from proxied provider responses"
```

### Task 39: The full provider harness and conformance checks

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs`
- Create: `crates/trusted-server-core/src/ec/identity/examples.rs` (`#[cfg(test)]` only): the spec 4.6 `ExampleIdProvider` and the capture example, as reference providers
- Test: `examples.rs`

**Interfaces:**

- Produces everything in spec 13's harness listing: `ProviderHarness::{with_setup, with_client_ip, with_user_agent, with_page_url, with_region, with_hashed_email, with_consent, with_stored, with_state, with_now, with_random_seed, as_dry_run, request, request_json, respond, respond_raw, capture, resolve, state_of, check_conformance, check_conformance_with_state}`, `HarnessSetup`, `TestConsent`, `CaptureFixture`, `ConformanceCases`, `StoredId::{for_test, written_by, expiring_at}`, `HashedEmail::for_test`, and `check_descriptor_ignores_secrets`. All of them sit behind `#[cfg(any(test, feature = "test-utils"))]` and are reachable from outside the crate as `trusted_server_core::ec::identity::test_support::*`, so the module is `pub` under the gate.

`check_conformance` asserts every bullet of spec 13's conformance list:

- inputs hold only what the descriptor declares
- a resolver returns `Some` for at least one listed setup, and its `response` never returns the `not_implemented` marker
- a capturer returns a non-`no_match` result for at least one fixture, and never the marker
- no undeclared input or state read is recorded
- no state is returned without `with_private_state()`
- requests go only to declared hosts, over HTTPS, with no `Cookie` header and with valid headers
- IDs are only for declared sources, at most one per source
- `expiry()` and `refresh()` report what was set, and a fixture ID of an expiring source carries an expiry
- `capture` never returns a state change
- a bare-JSON-string stored state is never treated as valid
- the dry run is discarded unsent, and `as_dry_run()` fails for a stateful provider

`check_conformance` fails with a pointer to `check_conformance_with_state` for a stateful provider. `check_conformance_with_state::<S>` also round-trips `S` through `replacing_state` and `input.state()`.

- [ ] **Step 1: Write the tests (the spec's examples are the first harness cases)**

In `examples.rs`, copy `ExampleIdConfig`, `ExampleIdProvider` and the capture-only example verbatim from spec 4.6, then:

```rust
    #[test]
    fn example_provider_round_trip() {
        let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
        let mut harness = ProviderHarness::new(&provider)
            .with_client_ip("192.0.2.10")
            .with_user_agent("ExampleBrowser/1.0");

        let request = harness.request().expect("should build a request").expect("should not skip the request");
        assert_eq!(request.url().host_str(), Some("api.exampleid.example.com"), "should call the declared host");

        let resolution = harness
            .respond(StatusCode::OK, &json!({ "uid": "example-uid", "signature": "example-signature" }))
            .expect("should parse the response");
        assert_eq!(resolution.ids()[0].uid(), "example-uid", "should return the uid");
        let state: ExampleIdState = harness.state_of(&resolution).expect("should replace state");
        assert_eq!(state.signature, "example-signature", "should keep the signature");
    }

    #[test]
    fn example_provider_conforms() {
        let provider = ExampleIdProvider::new(&example_config()).expect("should build");
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
        let result = std::panic::catch_unwind(|| {
            ProviderHarness::new(&provider).check_conformance(
                &ConformanceCases::default().with_resolve_case(HarnessSetup::default(), StatusCode::OK, &json!({})),
            );
        });
        assert!(result.is_err(), "should fail a provider that reads an undeclared input through .ok()");
    }

    #[test]
    fn descriptor_ignores_secrets() {
        check_descriptor_ignores_secrets(&example_config(), &example_config());
    }
```

Add negative conformance tests the same way for: a `Cookie` header, a request to an undeclared host, two IDs for one source, a capability declared but not overridden (the `not_implemented` marker), a stateless provider returning state, a capture returning a state change, and an expiring source without an expiry. Each uses a tiny test provider written in the test module, and each asserts the harness panics.

- [ ] **Step 2: Run, see them fail, implement, see them pass**

Run: `cargo test-fastly -- ec::identity::examples`
Expected: FAIL, then PASS.

- [ ] **Step 3: Check the feature gate from outside the crate**

Run: `cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1 --tests --features trusted-server-core/test-utils` (or whichever adapter already enables `test-utils` in its dev-dependencies)
Expected: builds. This proves a provider crate can use the harness.

- [ ] **Step 4: Commit**

```bash
git add -A crates/trusted-server-core/src
git commit --signoff -S -m "Add the provider harness and conformance checks"
```

### Task 40: Batch sync never replaces an owned value

**Files:**

- Modify: `crates/trusted-server-core/src/ec/batch_sync.rs` (around line 247)
- Test: `ec/batch_sync.rs`

- [ ] **Step 1: Write the failing test**

`batch_sync_update_to_owned_source_applies_only_when_absent`: with the source owned by a provider, a batch update applies when the source is absent, and is `Unchanged` (no write) when a value exists. A control update to an unowned source keeps today's behaviour.

- [ ] **Step 2: Run, see it fail, implement, see it pass**

Run: `cargo test-fastly -- ec::batch_sync`
Expected: FAIL, then PASS. The batch sync handler needs to know the owners. Pass `&ProviderCatalog` (or just the owned source set) through its existing deps.

- [ ] **Step 3: Commit**

```bash
git add -A crates/trusted-server-core/src/ec/batch_sync.rs
git commit --signoff -S -m "Keep batch sync from replacing an owned source's value"
```

### Task 41: Docs, the provider author guide, and final verification

**Files:**

- Create: `docs/guide/identity-providers.md` (provider author guide)
- Modify: `docs/guide/configuration.md` (`[[ec.partners]].owner`, per-provider tables), `docs/guide/edge-cookies.md`, `docs/.vitepress` sidebar config (find it with `grep -rn "edge-cookies" docs/.vitepress`), `CHANGELOG.md`

- [ ] **Step 1: Write the author guide**

Base it on spec 4.2 to 4.6 and 13, using only the fictional `exampleid` provider:

- what a provider is
- the descriptor
- declaring inputs and reading them with `?` or `.ok()`
- building requests
- parsing responses
- returning a `Resolution`
- private state
- capture
- registration with `try_with_identity_provider`
- testing with `ProviderHarness` and `check_conformance`
- what the coordinator does for you (consent, transport, backoff, KV)

Link to the spec for the protocol.

- [ ] **Step 2: Config and changelog**

Document `owner`, `mode`, `timeout_ms`, `sources` narrowing, `refresh_after_sec`, `max_calls_per_minute`, `breaker_failures_per_minute` and `cookie_priority`, each with its default and a validation error example. In `CHANGELOG.md` under `### Added`, add the provider framework, ownership, hold mode, passive capture and the breaker. Under `### Changed`, add stricter consent decoding for identity providers.

Run the docs and markdown format commands.

- [ ] **Step 3: Commit**

```bash
git add docs/ CHANGELOG.md
git commit --signoff -S -m "Document the identity provider framework for operators and authors"
```

- [ ] **Step 4: Full verification**

Run `/check-ci`, then `cargo test-fastly-reuse`, `./scripts/test-cli.sh`, the parity crate, `scripts/lint-wasm-clock.sh`, and `cargo doc --no-deps --all-features -p trusted-server-core`. Every one must pass. Then use the `verify-app` agent for an independent pass.

- [ ] **Step 5: PR**

Open the PR against `feat/1246-identity-single-write` with `pr-creator`. Title: "Add server-side identity providers". The description lists spec 14 step 2's contents, says no vendor ships in it, and names the follow-up vendor specs (ID5, LiveRamp, Lockr, UID2).

---

## Spec coverage

| Spec section                                                          | Tasks                                                                                          |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| 2.1 G1, G2, G11 (trait, coordinator, ergonomics)                      | 8 to 11, 20, 21, 39                                                                            |
| 2.1 G3 (hold or after-send)                                           | 19, 20, 29, 37                                                                                 |
| 2.1 G4 (one write)                                                    | 25, 26, 27, 29, 30                                                                             |
| 2.1 G5 (cookie first, then KV)                                        | 16, 17, 25, 28                                                                                 |
| 2.1 G6 (ownership)                                                    | 33, 36                                                                                         |
| 2.1 G7 (expiry, refresh)                                              | 14, 15, 20, 36                                                                                 |
| 2.1 G8 (pull sync on the trait)                                       | 22, 29                                                                                         |
| 2.1 G9 (HEM slot)                                                     | 9 (always empty in v1)                                                                         |
| 2.1 G10 (no lost EIDs)                                                | 25, 26, 30                                                                                     |
| 2.1 G12 (removals win)                                                | 15, 18, 25, 27, 38                                                                             |
| 3.4 platform facts (status capture, real-browser gate, clock failure) | 14, 23, 29                                                                                     |
| 4.2 trait and additivity rules                                        | 11, 39                                                                                         |
| 4.3 descriptor                                                        | 8, 32 (capture-path check)                                                                     |
| 4.4 inputs                                                            | 9, 23 (phase-1 captures)                                                                       |
| 4.5 requests, responses, results                                      | 10, 20                                                                                         |
| 4.6 example provider and registration                                 | 32, 39                                                                                         |
| 4.7 eligibility, dispatch, hold timing, breaker, capture              | 20, 21, 35, 37, 38                                                                             |
| 5.1 writer and stagers; withdrawal exception                          | 23, 25                                                                                         |
| 5.2 phases 1 to 4                                                     | 23, 25, 29                                                                                     |
| 5.3 stagers, KV gap, flush checks                                     | 25, 26                                                                                         |
| 5.4 `IdentityMutation`                                                | 15                                                                                             |
| 5.5 orphans, collisions, deletion                                     | 25, 30 (the deletion framework itself is deferred; its tombstone rule is a note for that work) |
| 5.6 removal, marker and cookie rules                                  | 15, 17, 18, 25, 29                                                                             |
| 6.1 KV record, rollback rule, admin redaction                         | 6, 14                                                                                          |
| 6.2 cookies, sealing, size caps, overflow, outbound strips            | 1 to 4, 16, 17, 18, 27                                                                         |
| 6.3 read order                                                        | 17, 29                                                                                         |
| 7.1 merge, servability, `/identify` consent                           | 14, 28, 36                                                                                     |
| 7.2 ownership semantics, batch sync                                   | 33, 36, 40                                                                                     |
| 8 errors and outcomes, sticky opt-outs, fingerprint                   | 13, 20, 34, 36                                                                                 |
| 9.1 consent                                                           | 12, 34                                                                                         |
| 9.2 outbound requests                                                 | 1 to 3, 21                                                                                     |
| 9.3 logging and redaction                                             | 9, 10, 34, every task's logging                                                                |
| 9.4 caching                                                           | 25 (cookies only when needed; the privacy rule already applies)                                |
| 10.1 to 10.3 configuration, validation, registration                  | 19, 32, 33                                                                                     |
| 10.4 pull sync on the trait                                           | 22, 29                                                                                         |
| 11 adapters                                                           | 3 (strips on every adapter), 31                                                                |
| 12 vendor examples                                                    | Out of scope (spec 2.2). The fictional examples in 39 prove the API                            |
| 13 testing                                                            | each task, 30, 39                                                                              |
| 14 rollout                                                            | Parts 0, 1, 2                                                                                  |
| 15 open questions                                                     | Not implemented. Track as issues after Part 1 ships                                            |
