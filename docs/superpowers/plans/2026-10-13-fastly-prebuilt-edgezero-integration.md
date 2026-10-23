# Fastly prebuilt EdgeZero integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add byte-preserving online production/staging deployment of verified Fastly wasm through a typed EdgeZero API with durable recoverable observations.

**Architecture:** Trusted Server owns official verification, operator authorization and a private retained artifact. EdgeZero owns Fastly packaging/transport, observed context and concrete lifecycle operations. A durable operation sink records intent before every write and response afterward; uncertain provider state is reconciled read-only before any retry.

**Tech Stack:** Existing Rust/error-stack consumer, reviewed upstream EdgeZero/Fastly CLI 15.1.0 or qualified API transport, restricted atomic local journal, provider-free runner fixtures and authorized disposable Fastly qualification.

---

## Authority and prerequisites

Read the [primary plan](2026-10-13-attested-release-implementation.md) and [contract R-DEPLOY through R-RECOVERY](../specs/2026-10-09-1235-release-attestation-design.md#r-deploy). This is AC-8, a separate integration milestone, not an initial #1235 closure condition. It depends on the primary verifier/retained artifact, package inspector and public qualification; it may be prepared upstream in parallel but cannot ship an official release path without that verifier.

Baseline EdgeZero revision is `683202c66948146ec360f490126f1d60f920288b`. Its generic adapter operation returns unit, source deploy can execute manifest shell commands, and Fastly staging builds/autoclones before staging. These are reference mechanisms, not an existing typed prebuilt seam.

Use a separate upstream EdgeZero checkout/worktree; do not edit Cargo's shared cached checkout. Upstream relative paths below belong to that repo. After review, update Trusted Server's Cargo pin/lock rather than vendor-patching the cache.

## Proposed API boundaries

Keep small concrete APIs, with default unsupported behavior for other adapters. Do not add speculative Cloudflare/Spin/Axum payload variants.

The owning upstream design MUST implement these concrete records (not empty adapter variants):

| Record / interface                                 | Required fields / behavior                                                                                                                                                                                  |
| -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PreparedFastlyPackage`                            | Private retained archive handle/lifetime, checked archive length/SHA-256, canonical module length/SHA-256, deployment-only manifest SHA-256 and inspected package name                                      |
| `PrebuiltFastlyRequest`                            | Strong service ID, exact base version, production/staging mode, prepared payload and resolved context/sharing policy                                                                                        |
| `WriteIntent`                                      | Stable operation ID, monotonically increasing sequence, phase (`clone`, `upload`, `relink`, `selector-write`, `stage`, `activate`), exact resource/version where known, request identity digest             |
| `WriteObservation`                                 | Matching operation/sequence/phase, actual response resource/version, known/unknown result and sanitized evidence references                                                                                 |
| `OperationSink::before_write(&WriteIntent)`        | Synchronous concrete fallible result; caller MUST flush durable intent and failure forbids the following mutation                                                                                           |
| `OperationSink::after_response(&WriteObservation)` | Synchronous concrete fallible result; failure stops subsequent writes while preserving already attempted effects                                                                                            |
| `PrebuiltOutcome`                                  | Exact module/package/service identities, optional exact version, last confirmed phase, mutation-attempted flag and per-phase known/unknown results on success and error; no inferred active-version success |

E1 follows the owning repository's AGENTS.md for concrete error conventions and visibility. A result-only deploy method is insufficient: it cannot let the caller flush a journal before hidden internal link/selector mutations. No publisher signing or universal attestation registry is introduced.

### Task E1: Approve upstream API and operation-state contract

**Upstream modify:** `crates/edgezero-adapter/src/registry.rs`, `crates/edgezero-cli/src/lib.rs`.
**Upstream create:** `crates/edgezero-adapter/src/prebuilt.rs`, `crates/edgezero-adapter-fastly/src/prebuilt.rs`.

- [ ] Write fake adapter tests for unsupported adapter/action, payload/context mismatch, mutation attempts before authorization, unit-success ambiguity and failed durability sink, truncated payload, pre-send failure versus sent/no-response uncertainty and missing exact version.
- [ ] Run upstream targeted prebuilt tests; expect missing API.
- [ ] Define private retained payload identity, exact service/base version, action/mode, logical/physical mappings, staging selector identity and sharing policy. Return structured observed IDs/phase states and error outcomes with known/unknown state, not an active-version lookup.
- [ ] Define explicit phase enum and WriteIntent/WriteObservation with stable operation/sequence IDs; before_write MUST synchronously succeed before every provider/config mutation. Journal persistence errors halt further writes, even after earlier successful writes.
- [ ] Approve exact-source Fastly unsupported defaults and review public API/docs/error types. Commit `Add a typed prebuilt Fastly operation interface`.

### Task E2: Prepare byte-preserving packages from retained inputs

**Upstream modify/create:** Fastly `src/prebuilt.rs`, `src/prebuilt/package.rs`, provider-free package fixtures.

- [ ] Write tests for retained-file/path replacement, changed package/member/manifest, generic archive selection, local-server config leakage, extra executable, traversal and unsafe names.
- [ ] Run package tests; expect no validated prepared-payload constructor.
- [ ] Adopt the primary plan's exact deployment-only manifest and bounded four-member inspection semantics. Rehash retained wasm, manifest and selected archive before transport. Never run source build, compute publish, annotation/strip/sign hooks or arbitrary manifest shell overrides.
- [ ] Keep module/archive digests distinct. Public callers cannot construct prepared identity from arbitrary asserted hashes; constructors validate owned bytes and context.
- [ ] Run byte-retention/adversarial fixtures against pinned real pack output. Commit `Prepare verified Fastly packages without rebuilding`.

### Task E3: Resolve and validate actual deployment context

**Upstream create:** Fastly `src/prebuilt/context.rs`; modify existing selector/link helper seams only as required.

- [ ] Test declared logical IDs vs observed links mismatch, wrong selector/config key, unknown secret-reference metadata, conflicting shared staging selector and disallowed resource-sharing policy.
- [ ] Run context tests; expect absent observed-context validation.
- [ ] Resolve service/base version and fetch read-only provider links/selector state. Validate repository logical IDs, selected physical links/key, runtime selector entries and required secret-reference metadata. Secret values are write-only; do not claim plaintext readback or log/store resolved secrets.
- [ ] Declare which KV/secret/external-effect resources staging shares versus isolates. Config-selector isolation alone does not imply all resource isolation. Validate resulting exact draft again before stage/activation.
- [ ] Commit `Validate observed Fastly deployment bindings` after fake-provider negatives pass.

### Task E4: Implement exact clone/upload/context/stage or activation

**Upstream create:** Fastly `src/prebuilt/lifecycle.rs` and fake provider/command tests.

- [ ] Write sequence tests: verify/package/context before first write; explicit clone returning draft ID; upload to that ID without autoclone; exact draft readback; stage or activate only that ID. Test same-hash skip, partial response and unsupported staged request.
- [ ] Run lifecycle tests; expect absent state machine.
- [ ] Invoke OperationSink before each mutation, including clone, upload, resource relinks, selector writes, stage and activation. Use explicit version update rather than compute deploy's automatic activation or combined update/autoclone.
- [ ] Capture returned IDs from operation responses. Require validated resulting context before activation; staged success reports staged, not activated. Never guess success from exit zero or whichever service revision is currently active.
- [ ] Stop on sink/provider uncertainty and preserve last known observation. Commit `Deploy prepared Fastly packages through exact lifecycle phases`.

### Task E5: Implement durable journal and scoped serialization

**Trusted Server create:** CLI `src/release/deployment/mod.rs`, `journal.rs`, `context.rs`; internal tests and `tests/release_deploy.rs`.

- [ ] Write crash-injection tests before send/after send/before response, failed journal flush, receipt path replacement, process restart, shared-selector races and loss of temporary files.
- [ ] Run CLI deployment tests; expect absent journal integration.
- [ ] Implement persistent restricted storage, stable operation ID, private retained evidence/package/context and atomic state writes. Flush file plus directory at each durable intent/response boundary. A TempDir that disappears on process exit is not recovery storage.
- [ ] Lock by service and relevant shared-resource identity across processes/environments; define stale-lock inspection/recovery without blindly deleting locks. Preserve production previous-version observation before mutation.
- [ ] Record exact known/unknown state, policy/source/module/package hashes and sanitized context identities; no active config, token or resolved secrets. Deployment-only health is not_performed. Commit `Persist deployment intent and recovery observations`.

### Task E6: Reconcile uncertainty without guessing or replaying mutations

**Upstream create:** Fastly `src/prebuilt/reconcile.rs`; Trusted Server internal journal reconciliation tests.

- [ ] Test known draft inspection, lost first-clone response, multiple indistinguishable candidate versions, changed selector ownership, unknown upload/activation and receipt failure after success.
- [ ] Run reconciliation tests; expect missing read-only recovery.
- [ ] Query exact known resources/versions where IDs exist; attach observations to the saved operation. A lost clone ID without provider idempotency cannot always be recovered. Keep it unknown; do not adopt a candidate by timestamp/package resemblance or mutate a guess.
- [ ] Require explicit operator-approved reconciliation before another write. Do not automatically rollback after uncertain state, bypass serialization, or replay create/clone on restart. Health and rollback remain separate operations consuming known exact identities.
- [ ] Review any public recovery command separately before adding it; internal support and runbook are required here. Commit `Reconcile uncertain Fastly operations without replay`.

### Task E7: Review upstream revision and integrate release arguments

**Trusted Server modify:** root `Cargo.toml`, `Cargo.lock`, CLI `src/run.rs`, `src/commands/mod.rs`.
**Create:** CLI `src/commands/deploy.rs`, `src/release/deployment/transport.rs`.

- [ ] Land reviewed upstream tests/docs, pin its exact revision and rerun source deploy regression tests. Do not modify the shared Cargo cache or float a branch pin.
- [ ] Write command tests for source deployment preserved, release/passthrough/package/source conflicts, unsupported offline/adapter/variant, failed verification before journal/provider writes and unavailable prebuilt capability.
- [ ] Add wrapper flattening existing DeployArgs plus reviewed release/source/receipt inputs. Source mode delegates as before. Release mode selects a compatible retained VerifiedArtifact, validates local action/context and invokes only the typed prepared path. No fallback to a source build.
- [ ] Keep producer artifact capability and consumer implemented transport intersection: initially production only. Enable fastly-staging only after E8 qualification and reviewed cohort/policy change; old releases do not gain it by downloaded metadata.
- [ ] Run CLI source/release regressions and all AGENTS gates. Commit `Integrate verified release deployment with EdgeZero`.

### Task E8: Qualify production/staging, recovery and operator guidance

**Modify:** CLI/release guide, changelog and evidence records; upstream full validation.

- [ ] Execute fake-provider coverage for all writes, crash boundaries, context mismatches, locks and unknown results; verify no provider writes occur on failed official evidence.
- [ ] Before enabling integrated production transport, execute end-to-end `ts deploy --release` production qualification on a separately authorized disposable Fastly service using an independently verified production-capable release. Assert canonical module bytes, explicit clone/upload to the exact returned draft, context validation before activation, the exact activated version, durable per-write observations and receipt/restart behavior. The primary manual compute-deploy rehearsal is a different transport and cannot satisfy this gate.
- [ ] First qualify direct upstream prepared-byte staging on a separately authorized disposable Fastly service, using source-built fixtures and no claim of an official staging-capable release. Compare actual module/context and shared/isolated bindings, plus separate health/rollback commands. The initial production-only CLI/release MUST continue to reject staging before writes.
- [ ] After that transport proof, approve the cohort and embedded policy capability change, then publish a separately authorized new staging-enabled release through the primary pipeline. Use the independently trusted verifier to qualify end-to-end `ts deploy --release --staging` against that new release. Test that historical production-only releases still reject staging; no downloaded metadata or test policy override enables it in the production CLI.
- [ ] Qualify durable recovery across process restart and uncertain responses without exposing secrets. An irreducibly unknown clone remains a documented manual resolution case, not a passed exactly-once guarantee.
- [ ] Record the already reviewed/qualified staging capability and historical producer/consumer compatibility explicitly. Document receipt limits and the actual loaded snapshot boundary for #161; unsigned operator observations are not publisher authority or remote execution proof.
- [ ] Run upstream tests and all Trusted Server AGENTS gates, independent staff review, then hand off for maintainer deployment approval. Commit `Qualify integrated Fastly artifact deployment and recovery`.

## Exit criteria

AC-8 is met only when reviewed upstream code is pinned, production/staging preserve canonical bytes, actual context is validated before finalization, durability precedes every write, exact/unknown outcomes survive failure and live disposable qualification is recorded. No other adapter, publisher claim signer, remote execution attestation or generic deploy framework is implied.
