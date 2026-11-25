# Auctioneer Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** First adopt Auctioneer's OpenRTB types throughout Trusted Server, then run Auctioneer in-process as a Fastly banner/display provider using shared configuration, services, routing, and delivery, with equivalent engine outcomes and verified artifact ownership.

**Architecture:** Land a feature-independent migration of existing OpenRTB wire paths to `auctioneer-openrtb` before host binding. Then extend the production compiled provider plan with local execution, bind the settings-first API inside a request-owned operation, co-drive it during publisher I/O and EOF work, and retain funded results through delivery. Keep neutral orchestration types and reuse shared preparation, discovered bidders, and EdgeZero transport.

**Tech Stack:** Rust 2024, Auctioneer's typed bridge, EdgeZero PR 275 integration APIs, Fastly Compute, Viceroy, existing native configuration and HTML EOF harnesses.

---

## Scope and execution prerequisites

Specification: [Auctioneer integration design](../specs/2026-11-22-auctioneer-integration-design.md). Tracking: [Trusted Server #1509](https://github.com/IABTechLab/trusted-server/issues/1509), under [#1505](https://github.com/IABTechLab/trusted-server/issues/1505). This planning PR contains prose only and uses `Refs #1509`; it does not close an issue or implement integration.

Implementation starts only after the planning PR is approved and merged, in separate phase worktrees/PRs. Use the installed PRfection CREATOR workflow for that transition; this plan does not authorize merging or deployment.

All product commands below describe **future implementation validation and expected results**, not results executed for this planning PR. The planning PR runs documentation checks only. At implementation time, capture actual commands, exit codes, result lines, exact head, artifact identity, and external evidence owner.

Task 0 is the required foundation tracked by [#1652](https://github.com/IABTechLab/trusted-server/issues/1652), a P1/L Task under #1505: shared OpenRTB types replace the duplicate model/codegen across existing paths and all adapters. Its separate implementation PR closes #1652; its merged evidence is a prerequisite to #1510. Subsequent delivery stays decomposed across the existing leaves:

| Delivery                 | Tasks | Acceptance boundary                                                                                                         |
| ------------------------ | ----- | --------------------------------------------------------------------------------------------------------------------------- |
| #1509, contract record   | P     | Record host deltas and upstream accepted decisions on open Auctioneer #51/#76                                               |
| #1652, shared model      | 0     | Default-build compatibility, model retirement and release impact before host binding                                        |
| #1510, host binding      | 1–4   | Shared typed configuration, input/restrictions, actual SDK services and funded request ownership; coordinate Auctioneer #51 |
| #1511, local provider    | 5–8a  | Production plan, lifecycle, browser delivery and releasable non-test Fastly/CLI variants; coordinate Auctioneer #76/#88     |
| #1512, artifact evidence | 9–10  | Controlled host flow and evidence consumed by Auctioneer #91                                                                |

Task 0 is a mandatory portable-model prerequisite and can land independently of the EdgeZero cutover. Tasks 1–10 implement host integration against the cutover API. Before Task 1, fetch the default branch, verify Task 0's merged commit/evidence, and confirm #1507/PR #1506 has landed, or rebase a separately approved implementation branch onto its reviewed candidate. Do not port host integration onto `PlatformHttpClient`, restore deleted pending handles, or absorb the entire SDK cutover into an Auctioneer implementation PR. The planning base is `1810c763add220dd34fd5faaffe2c69aab1d556f`; the inspected post-cutover source is `feaf3df5d1c9bb2dca64158480725cb555ab9c26`.

The selected SDK direction is Auctioneer repinning to cutover EdgeZero `9d02d7b3185b49e81d9ac972ca3feef932ed848a`, tracked by [Auctioneer #265](https://github.com/IABTechLab/auctioneer/issues/265). Review a named replacement on both sides if the cutover advances. [Auctioneer #266](https://github.com/IABTechLab/auctioneer/issues/266) gates the original-permit lifetime, unchanged-envelope host storage and bounded fallible cutoff contract; source availability is not acceptance. Review root patches, schema support, feature lanes and the linked host release profile. EZ-Q1 does not gate cutover merge/deployment; selected Auctioneer Fastly acceptance still applies.

## Global Constraints

- Shared `auctioneer-openrtb` is unconditional. Start Task 0 at Auctioneer `05f2a56e9555be9c46a789c868100e37162abe5b`; any later model/engine repin is atomic and reruns affected compatibility controls.
- One immutable source identity per shared EdgeZero package; no SDK type-disguising conversion or restored host transport.
- Local runtime feature name: `auctioneer`. First production host: Fastly site/banner/display. Native harness access never authorizes an Axum local host.
- Non-`Send`, request-owned execution; original admission/deadline through origin/body/EOF, completion and egress; no detached task, restarted preparation or automatic remote fallback.
- Only plans containing a local Auctioneer provider install the AppState-owned admission/stream domain and per-outer-auction envelope. Remote-only plans on either binary keep one existing outbound batch and no new admissions; a Cargo feature alone never changes their scheduling. Local-containing plans enroll remote waves in the shared gate. No increased ceiling or second funding pool.
- Planned TS types/signatures below are private unless a caller requires visibility. They are contracts to implement, not existing upstream exports. Upstream gaps are named prerequisites, never placeholder implementations.
- Default remote configuration/output and privacy policy stay compatible. Local optional response fields and mixed opaque-demand refusal are explicit host deltas.
- #1510 coordinates the host resource proof required by Auctioneer #76; #1511 supplies the artifact and #1512 the controlled observations. Their commits can land before final #1510 acceptance, avoiding a cycle on issue closure.

## Review Focus

Verify shared-model wire/admission compatibility, account/header/SID trust boundaries, CLI/runtime artifact identity, target refusal despite Cargo feature unification, exact routed membership, cutoff under stream contention, funding before every host copy, dynamic browser targeting cleanup, and non-test release artifact qualification. Treat instrumented EOF results as supplementary.

## File map

Paths are repository-relative. New file names below are planned; their APIs do not exist yet. Keep additions private unless an existing caller requires visibility, and document any public interface. Existing files stay in place; no unrelated refactor is planned.

Task 0 additionally migrates `crates/trusted-server-core/src/openrtb.rs`, current/legacy `integrations/{prebid,aps,adserver_mock}.rs`, `auction/openrtb/{tests,test_executor}.rs`, and `ec/{mod,admin,identify,eids,prebid_eids}.rs`. It retires `crates/trusted-server-openrtb/` and `crates/trusted-server-openrtb-codegen/`, with corresponding workspace/lockfile, `.cargo/config.toml`, CI and current `AGENTS.md`/generation-guide changes.

| Files                                                                                                                                                                                           | Responsibility                                                                                   |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `Cargo.toml`, `Cargo.lock`; core, CLI, Fastly, and integration-tests `Cargo.toml` files                                                                                                         | One dependency graph, optional `auctioneer` feature, and explicit test lanes                     |
| `crates/trusted-server-core/src/auction_config_types.rs`, `auction/plan.rs`, `auction/config.rs`                                                                                                | Serialized remote/local declarations, common plan facts, routing and target validation           |
| `crates/trusted-server-core/src/auction/local/mod.rs`                                                                                                                                           | Neutral local operation/result contract; no bidder policy or Auctioneer internals                |
| `crates/trusted-server-core/src/auction/local/auctioneer/{mod,input,services,result}.rs`                                                                                                        | Concrete local binding, typed conversion, services/funding, and retained output projection       |
| `crates/trusted-server-core/src/{config,config_payload,settings,settings_data,secret_resolution}.rs`; CLI `src/run.rs`, planned `src/commands/auctioneer.rs` and `tests/config_env_overlay.rs`  | Actual typed push/validation, artifact facts and deferred secret-reference semantics             |
| `crates/trusted-server-core/src/consent/{types,extraction,mod}.rs`, `auction/{types,endpoints}.rs`, `publisher.rs`                                                                              | Request-bound signal evidence, existing consent/identity gates, common ingress mapping           |
| `crates/trusted-server-core/src/platform/types.rs`; Fastly `src/app.rs`                                                                                                                         | Request SDK objects and actual Fastly binding                                                    |
| `crates/trusted-server-core/src/auction/{mod,provider,routing,orchestrator,formats,telemetry}.rs`                                                                                               | Production driver construction, exact routing, competition, direct output and outcomes           |
| `crates/trusted-server-core/src/publisher/html_finalization.rs`, `integrations/{aps,registry}.rs`                                                                                               | Streaming/EOF progress and existing typed renderer capability registration                       |
| `crates/trusted-server-integration-tests/tests/html_eof_runtime{.rs,/config.rs,/process.rs,/origin.rs,/auctioneer.rs}`, planned `fixtures/auctioneer/` and browser auction-runtime config/spec  | Production fixture DNS/TLS, browser rendering, artifact orchestration and equivalence controls   |
| `scripts/async-html-finalization-local-test.sh`, `.github/workflows/{test,format}.yml`                                                                                                          | Explicit feature-on runtime and lint/test execution; preserve existing lanes                     |
| `crates/trusted-server-js/lib/src/core/{types,auction}.ts`, `src/integrations/{gpt,prebid}/index.ts` and adjacent Vitest files                                                                  | Optional finalized targeting carrier, parser/projection, GPT application and dynamic-key cleanup |
| `fastly.toml`, `.github/workflows/{release-build,release}.yml`, `scripts/release/{phase1-build.sh,phase1.mjs,phase1.test.mjs,publish.mjs}`, CLI `src/run.rs` and `tests/release_build_facts.rs` | Non-test feature-on artifacts, build facts, feature-matched SBOM, signing and publication        |
| `trusted-server.example.toml`, `docs/guide/{auction-orchestration,auction-testing}.md`, `CHANGELOG.md`                                                                                          | Operator configuration, documented limits and rollback                                           |

Tests belong beside changed Rust units, with the existing CLI and HTML EOF harnesses for their respective boundaries. The `auctioneer_` prefix below names **new planned tests** so command filters can prove they executed a nonempty population.

## Task P Record host decisions and dependency gates

**Files:** this spec/plan; tracking bodies #1509/#1510 and upstream #51/#76/#265/#266. No runtime code.

**Interfaces:** Consumes the accepted parent host contract and the spec's enumerated host deltas. Produces a source-pinned decision record linking each delta to its upstream acknowledgement and each missing capability to its delivery issue. This task does not claim upstream acceptance from a planning merge.

- [ ] Replace #1509's stale done-when referencing completed #34 with explicit host-delta reconciliation on open Auctioneer #51/#76.
- [ ] Record trusted account/header/debug mapping, artifact identity/target view, affected-provider opaque refusal, malformed SID typed refusal, granularity/browser projection and release variants on those open issues.
- [ ] Keep #265/#266 and any required selected-Fastly discovery export under #51 open until accepted source/verification is available. Record exact source/API changes before the consuming task begins.
- [ ] File combined host resource observations under #1510 with links to #1511/#1512 and upstream #76. Do not require a leaf's closure before the commits needed to satisfy it can land.
- [ ] Verify link targets and decision consistency with documentation checks. Commit the contract record separately from runtime work.

## Task 0 Adopt shared OpenRTB types before host integration

**Files:** root/core manifests and lockfile; `crates/trusted-server-core/src/openrtb.rs`, `auction/{openrtb,formats,types,endpoints}.rs`, `auction/openrtb/{tests,test_executor}.rs`, legacy `integrations/{prebid,aps,adserver_mock}.rs`, and `ec/{mod,admin,identify,eids,prebid_eids,finalize}.rs` under that same core source root; retire both `trusted-server-openrtb` crate trees; update `.cargo/config.toml`, `.github/workflows/{format,test}.yml`, `AGENTS.md` and current type-generation documentation.

**Interfaces:** Consumes existing neutral `AuctionRequest`/`Bid` and profile-specific wire/admission contracts. Produces facade reexports of `auctioneer_openrtb::types::{BidRequest, BidResponse, SeatBid, Bid}` and shared nested raw entities, plus the host extension serializers. Existing builders/readers keep their caller signatures and admitted host outputs. No engine-preparation call is introduced in remote paths.

Key new test `openrtb_shared_types_preserves_valid_sibling` uses before/after raw fixtures and the same admitted-host projection. Assertion sketches throughout this plan use task-owned fixtures to be written with the tests; they specify required assertions and are syntax examples, not existing compiling tests:

```rust
let case = shared_types_sibling_fixture();
assert_eq!(case.before_admitted, case.after_admitted, "should preserve admitted siblings");
assert_eq!(case.after_admitted.len(), 1, "should retain the seeded valid bid");
assert_eq!(case.before_signed_payload, case.after_signed_payload, "should preserve signing bytes");
```

- [ ] Capture source-bound pre-migration request/output goldens and response-admission cases for standard, Prebid, APS and mediator paths. Include current/legacy builders, direct `/auction`, cookies/headers/identity rows, unknown extensions, consent/EID placement, signing, stored-request precedence, no-bid, and malformed bids with valid siblings. Use fictional fixtures, existing bounded harnesses and semantic JSON comparison; assert exact signed payload behavior where bytes matter.
- [ ] Add `openrtb_shared_types_` compatibility tests beside the relevant owners and migrate applicable tests from the retiring model crate into core. Cover omitted/default fields; accepted bool/integer/string flag forms; required/optional IDs and price; numeric width/range and dimension inference; extension casing/placement; missing/null/value; notifications; EID/UID limits, sanitization, source merging and permission gates. Each negative control starts with a nonempty valid population so sibling loss is observable.
- [ ] Pin unconditional `auctioneer-openrtb` from canonical Git source `https://github.com/IABTechLab/auctioneer` at `05f2a56e9555be9c46a789c868100e37162abe5b`. Migrate the core facade/reexports, all live/legacy builders and direct wire output. Retain extension serializers and a small `ToExt` helper if needed; keep neutral host and browser types.
- [ ] Preserve bounded tolerant raw response framing and per-bid/profile admission. Introduce the shared types at existing typed protocol boundaries without a blanket whole-response decoder, new loss of valid siblings, accidental response forwarding, or a change to existing numeric/delivery policy. Shared model adoption does not run remote traffic through Auctioneer engine preparation.
- [ ] Where a required field presence, supported wire form, extension, or admitted value is not representable, obtain the minimal reviewed upstream raw-model/codec change and rerun its standalone validation controls. Keep this prerequisite open until that change is available. Do not fabricate IDs, silently narrow accepted ranges, weaken engine validation, fork the standard structs, or use serialization/reparse conversion between old and new models.
- [ ] Reconcile standard EID/UID uses with the shared entities while preserving host-owned permission/data normalization. Preserve existing restrictive forwarding: additional upstream fields do not gain disclosure permission merely by being representable. Coordinate with #1583/#518 without making those entire issues prerequisites or claiming their other acceptance criteria.
- [ ] After all consumers and required tests migrate, remove the old generated model/proto/codegen packages, obsolete `prost-build` dependency if still exclusive, workspace members, Fastly alias package selections, codegen-only lint/test jobs and current generation instructions. Keep historical design records historical. Retain equivalent runtime/protocol test coverage; update current CI commands to test the surviving owners.

```bash
cargo test -p trusted-server-core --lib openrtb_shared_types_
cargo test-fastly openrtb_shared_types_
cargo check-fastly
cargo check-axum
cargo check-cloudflare
cargo check-spin
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo tree --locked -p trusted-server-core
```

Expected before migration: baseline compatibility cases pass and the new shared-type contract exposes the intended missing implementation. Expected after migration: nonempty compatibility populations and all cross-adapter checks pass in default/feature-off builds; the resolved graph has the shared raw model and neither retired package. The `auctioneer` runtime feature must not be needed. Use source search and manifest/CI inspection to prove there are no live old-model/codegen references, while allowing historical documents to describe the old system.

- [ ] Produce comparable default base/head release builds in isolated checkouts using the actual producer for shipping Fastly/CLI assets and target-matched release builds for Axum/Cloudflare/Spin. Record digests, sizes, source graph and initialization impact against actual limits, preserving profiles/features. Pre-merge evidence covers locked Git-source resolution, feature-matched offline SBOM, source/build facts and packaging guards. Actual signing and digest/attestation-subject agreement are first proven by the authorized release workflow on a `v*` tag push with publication enabled; record that release evidence as pending and prohibit publication/deployment until it passes. This task creates no tag or deployment.
- [ ] Run the full applicable runtime-change CI suite, replacing retired codegen gates with surviving compatibility-owner checks. Land a separate reviewed PR closing #1652 and record its merged commit/source/evidence. Task 1 waits for that evidence; migration alone does not establish #1510 handoff. Later raw-model repins rerun affected controls.

## Task 1 Add the reviewed engine dependency and feature boundary

**Files:** root/core/CLI/Fastly manifests and lockfile; `auction/mod.rs` and new `auction/local/auctioneer/mod.rs`.

**Interfaces:** Consumes Task 0's facade and #265's immutable shared SDK/Auctioneer source decision. Produces optional engine exports under `cfg(feature = "auctioneer")`, CLI/Fastly/integration-tests feature forwarding, and one locked source graph. It does not introduce a local production adapter or config type in feature-off builds.

Test `auctioneer_dependency_graph_has_one_sdk_source` inspects locked Cargo metadata for the actual target/feature graph:

```rust
let graph = linked_fastly_graph_fixture();
assert!(graph.shared_sdk_sources_are_unique(), "should link one source for each SDK package");
assert_eq!(graph.raw_model_sources.len(), 1, "should use one raw model source");
assert!(!graph.contains_retired_model, "should retire the duplicate model");
```

- [ ] Wait for #265's reviewed Auctioneer commit on the selected common SDK revision and #1652's merged evidence. Record implementation base, source deltas, effective patches and target features. Repin every Auctioneer package together, including the unconditional raw model; rerun affected compatibility controls before changing the lockfile is accepted.
- [ ] Keep Task 0's `auctioneer-openrtb` dependency unconditional and single-source. Add optional engine/runtime dependencies on the actual Auctioneer public packages used by the binding. Forward `auctioneer` consistently from CLI and Fastly into core. Add the same named integration-tests feature for native harness controls; do not enable a production Axum/Cloudflare/Spin local host.
- [ ] Verify the resolved SDK graph and patch identities. Inspect duplicate packages individually; identical versions do not establish identical source/type identity.

```bash
cargo tree --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --features auctioneer -e features
cargo tree --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --features auctioneer -d
cargo check-fastly
cargo check-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer
```

Expected: default and feature-on checks compile; each shared EdgeZero package has one reviewed source/revision. Preserve approved security-feature boundaries. Do not add root patches speculatively: reproduce required approved source identities and verify the resulting artifact.

- [ ] Commit the reviewed graph and feature scaffolding after its checks pass, using an imperative sentence-case message such as `Add Auctioneer dependency and feature wiring`.

## Task 2 Compile typed local provider declarations and policy

**Files:** core `auction_config_types.rs`, `auction/{config,plan}.rs`, `config.rs`, `config_payload.rs`, `settings_data.rs`; CLI `tests/config_env_overlay.rs`; all four `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/app.rs`; integration-tests `Cargo.toml` and `tests/parity.rs`.

**Interfaces:** Consumes Task 1 features and shared selected-target discovery. Extend `deserialize_provider_map<'de, D>(D) -> Result<BTreeMap<ProviderId, ProviderConfig>, D::Error>` where `D: Deserializer<'de>`. Produce `ProviderConfig::{Remote(RemoteProviderConfig), Auctioneer(AuctioneerProviderConfig)}`, with the latter feature-gated; `LocalAccountSelection::{Default, Named { id: AccountId }}`; and `AuctioneerProviderConfig { auctioneer: AuctioneerAppConfig, account: LocalAccountSelection, artifact_identity: AuctioneerArtifactIdentity, timeout_ms: Option<u32>, routing: RoutingMode, notifications: NotificationConfig, debug_requested: bool }`. Local routing permits only `Explicit`; common notification semantics reuse the existing type.

The compiled local execution branch retains `policy: Arc<AuctioneerProviderConfig>` from the accepted snapshot. Task 5 receives that same owner; it does not reconstruct policy from the typed request or ambient settings.

Produce `validate_auction_execution(plan: &AuctionPlan, target: ExecutionTarget, support: &ExecutionSupport) -> Result<(), Report<TrustedServerError>>` for every adapter. `ExecutionTarget` names Fastly/Axum/Cloudflare/Spin; support carries compiled feature and optional accepted artifact facts. `AuctioneerArtifactFacts` holds selected target, canonical catalog/binding/schema/default identities, accepted resource facts and source/features. `artifact_identity(facts: &AuctioneerArtifactFacts) -> AuctioneerArtifactIdentity` is pure and canonical. The CLI and runtime contextual validators consume those facts explicitly.

Test `auctioneer_config_artifact_mismatch_has_no_io` seeds a valid local policy before changing the expected identity:

```rust
let case = artifact_mismatch_fixture();
assert!(case.error.is_artifact_mismatch(), "should identify the mismatched artifact");
assert_eq!(case.secret_reads, 0, "should refuse before secret access");
assert_eq!(case.bidder_calls, 0, "should refuse before dispatch");
assert_eq!(case.remote_round_trip, case.original_remote, "should preserve remote shape");
```

- [ ] Add failing tests named `auctioneer_config_round_trip`, `auctioneer_config_invalid_policy`, `auctioneer_config_unavailable_feature`, and `auctioneer_config_deferred_credentials`. Assert remote-only serialized shape stays unchanged; explicit local configuration carries the real typed subtree; invalid mixed remote/local fields and unsupported target declarations refuse.
- [ ] Run the new native tests and verify their intended red state. Distinguish a missing implementation failure from a harness/setup error.

```bash
cargo test -p trusted-server-core --lib auctioneer_config_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_config_
```

Expected before implementation: a meaningful failure for the missing local contract. Expected after implementation: nonzero matching test populations, zero failures in both applicable feature lanes.

- [ ] Extend `deserialize_provider_map` with discriminator-aware branch decoding and branch-local unknown-field validation. Use `ProviderConfig::{Remote,Auctioneer}`; omitted/explicit remote serializes to the unchanged old remote shape. Local requires the actual `AuctioneerAppConfig`, `LocalAccountSelection`, expected artifact identity and common `timeout_ms`/notification policy. Feature-off local input returns `AuctioneerUnavailable` before policy decoding, with no arbitrary JSON policy stand-in.
- [ ] Extend plan compilation with common provider identity/timing/notification facts plus distinct remote and local execution data. Only the remote variant owns transport/profile fields. Resolve the local executor in composition code, not test-only registration.
- [ ] Implement canonical `AuctioneerArtifactFacts`/identity from the selected Fastly discovery/bindings/schema/default/resource facts and reviewed source/features. Require the expected identity only on local declarations in the signed blob; compare during CLI/shared/runtime validation and report `AuctioneerArtifactMismatch` before credential/bidder I/O. Expose the planned `ts auctioneer facts --target fastly` command. If the shared selected-target view is unavailable, stop on #51's minimal export rather than substituting native catalog membership.
- [ ] Call `validate_auction_execution` from every production adapter state builder after common compilation. Axum/Cloudflare/Spin refuse local execution even with unified core features. Fastly validates support/facts and remote-only backend bounds. Config push's required `--adapter` selects destination/adapter checks, but typed `Validate` receives no adapter context: a structurally valid local blob may be pushed with `--adapter axum` and must then fail startup before bidder credentials/dispatch. Keep that operator outcome explicit; no new target-aware typed-validation API is introduced.
- [ ] Add nonempty controls for omitted/explicit remote normalization, mixed/unknown fields, feature-off error, native-only identity, catalog/default/feature mismatch and all non-Fastly state builders. Assert zero secret/dispatch calls on refusal; run an Axum refusal control with integration-tests unifying the feature.
- [ ] Preserve deferred bidder credential references. Do not attach them to the eager static-secret resolver. Test with a spy store that config validation/extraction does not turn deferred bidder credentials into unconditional reads; validate values at their shared permitted lookup boundary.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_config_
cargo test -p trusted-server-cli --features auctioneer --test config_env_overlay
cargo test -p trusted-server-cli --features auctioneer --lib auctioneer_
validation_host_target="$(rustc -vV | sed -n 's/host: //p')"
target_refusal_count="$(cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target "$validation_host_target" --features auctioneer --test parity \
  auctioneer_axum_refuses_local_with_unified_core_feature -- --exact --list | awk '/: test$/ { n += 1 } END { print n + 0 }')"
test "$target_refusal_count" -gt 0 || exit 1
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target "$validation_host_target" --features auctioneer --test parity \
  auctioneer_axum_refuses_local_with_unified_core_feature -- --exact
```

This task creates the top-level parity test `auctioneer_axum_refuses_local_with_unified_core_feature`. Feed valid local settings through the actual Axum state-construction seam; assert the target-support error and zero bidder-credential/transport calls. Task 1 already supplies the integration-tests feature forwarding.

Expected: accepted policy changes affect the borrowed Auctioneer subtree, unrelated TS settings do not, and invalid policy fails without bidder I/O. Commit the configuration slice after target-matched lint/test checks.

## Task 3 Bind real host services and original funding

**Files:** new `auction/local/auctioneer/services.rs`; `platform/types.rs`, Fastly `src/app.rs`, and concrete binding `mod.rs`.

**Interfaces:** Consumes checked shared `AuctionResourceInputs`/schema qualification and #266's accepted partition/permit/timer contract. Produces `InstalledAuctionResources::try_new(install: HostResourceInstall) -> Result<Arc<InstalledAuctionResources>, Report<TrustedServerError>>` and `try_begin(self: &Arc<Self>, timing: OriginalAuctionTiming) -> Result<HostAuctionLease, Report<TrustedServerError>>`. `HostResourceInstall` contains complete profile inputs, schema qualification and accepted host storage; constructors accept no independent capacity numbers. `InstalledAuctionResources` holds the checked profile, one admission gate, one stream gate and that partition. `HostAuctionLease` retains the single original outer permit and per-auction ledger through all enrolled branches/egress, using the upstream-approved handoff.

This task defines private `OriginalAuctionTiming { auction_start: edgezero_core::time::MonotonicInstant, auction_deadline: edgezero_core::time::Deadline }`. Populate it from the existing original dispatch values before conversion, preparation or queueing; retain it in `HostAuctionLease`. Later tasks only read those values. Fastly AppState stores `Option<Arc<InstalledAuctionResources>>`: `None` for a remote-only compiled plan, including on a feature-on binary; initialize profiles/gates only for a local-containing plan.

Produce request-owned `AuctioneerServiceBinding` with the actual client/clock/accepted wait adapter, credential loader, immutable registry/reference data and RNG. Restrictions belong only to Task 4's `OwnedAuctioneerInput`; borrow them while constructing services inside Task 5's owning future. No public generic budget transfer is assumed.

Test `auctioneer_services_mixed_capacity_and_egress` seeds nonempty local/remote bodies and host copies:

```rust
let case = mixed_capacity_fixture();
assert!(case.local_and_remote_share_stream_gate(), "should enroll one stream capacity domain");
assert!(case.copy_reserved_before_allocation, "should fund positive host copies first");
assert!(case.next_auction_refused_until_egress, "should retain the original outer slot");
assert_eq!(case.slots_restored_after_drop, 1, "should release exactly one original slot");
```

- [ ] Write failing `auctioneer_services_` tests using the real SDK object boundary plus spy transport/secret backends. Check the settings-first subtree argument, client/clock identity, original timing, required capabilities, and independent policies in a reused sandbox.
- [ ] Wait for #266's reviewed original-permit/host-storage/fallible-timer contract. For a compiled plan containing a local provider, install `InstalledAuctionResources` once in AppState from checked complete profile facts; capacities come only from accepted auction/active-stream limits. Clone it into request services and acquire one outer lease before conversion/credential/dispatch. Remote-only plans bypass profile initialization and admission. Never reset gates per request.
- [ ] Bind `EdgeZeroFanout::with_wave_scheduler` to the actual client, paired clock and accepted timer/decode/strategy/limits/shared stream gate. Implement the host's fallible absolute wait from `HttpClient::time().wait_until` inside managed Fastly progress, according to #266's accepted trait/error mapping. Missing scope, timer failure or no-op timer refuses; standalone bootstrap and ambient fallback remain forbidden.
- [ ] Construct the exported `CredentialLoader` from the host-bound secret store and matching clock. Exercise delayed lookup and resource/deadline refusal before any credential I/O. Preserve the actual upstream credential byte and lookup bounds.
- [ ] Derive the checked profile from the accepted complete tuple. Create one per-outer-auction ledger and use the accepted original-permit handoff. Current canonical prepay consumes the whole peak, so #266's unchanged-envelope host partition/export must be accepted before any positive host-copy reservation. Do not invent spare capacity, transfer APIs, second ledgers or increased ceilings.
- [ ] Only for a local-containing compiled plan, enroll remote waves in the same stream gate, bounded by `min(capacity, pending_legs)`. Race acquisition with the original cutoff, recheck expiry before batch start, preserve global indices and hold permits through body/decode/drop. Co-drive local/remote work and retain the outer lease through final egress. Remote-only plans keep the existing one-batch dispatch without Auctioneer admission.
- [ ] Cover prepayment rollback, zero remaining deadline, preparation delay, two isolated accepted policies/secret references, missing service, configuration recovery, and retained nonempty failures. Assert exact-once release and a nonempty denied dispatch/secret control population.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_services_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_services_
cargo test-fastly-reuse --features auctioneer auctioneer_
```

Expected: actual compatible SDK objects reach the shared API, expired/refused work performs no forbidden I/O, and accepted snapshots remain isolated. Commit the services/funding slice; #1510 handoff still needs Task 4's input/privacy evidence.

## Task 4 Preserve routed typed input and privacy evidence

**Files:** new `auction/local/auctioneer/input.rs`; `consent/{types,extraction,mod}.rs`, `auction/{types,endpoints,routing}.rs`, and `publisher.rs`.

**Interfaces:** Consumes `ProviderAuctionInput`, trusted `LocalAccountSelection`, Task 2 artifact facts, the original ingress snapshot and Task 3's lease. Produces `LocalIngressEvidence` and `OwnedAuctioneerInput` (shared raw request, owned header backing and the sole host restriction owner). Original timing remains in the lease. `build_auctioneer_input(input: &ProviderAuctionInput, ingress: &LocalIngressEvidence, policy: &AuctioneerProviderConfig, lease: &HostAuctionLease) -> Result<OwnedAuctioneerInput, Report<TrustedServerError>>` allocates only under accepted host storage. Borrow `AuctionInput::Typed`, headers and restrictions from this owned input inside Task 5; no temporary-header references escape.

Test `auctioneer_input_account_and_header_spoofing` repeats every ingress kind with conflicting client account/IP fields:

```rust
for ingress in [IngressKind::Direct, IngressKind::Navigation, IngressKind::PageBids] {
    let case = spoofed_ingress_fixture(ingress).with_client_debug_and_test_flags(true);
    assert_eq!(case.selected_account, case.configured_account, "should use trusted account selection");
    assert!(case.forwarded_ip_headers_are_absent(), "should drop spoofed forwarding addresses");
    assert_eq!(case.prepared_remote_addr, case.attested_client_ip, "should preserve the attested ingress address");
    assert_eq!(case.entry_point, EntryPoint::Auction, "should use the display auction entry point");
    assert!(!case.debug_requested, "should ignore client debug and test flags when trusted debug is false");
    assert_eq!(case.sent_denied_identifiers, 0, "should preserve the host denial");
    assert!(case.malformed_sid_refuses_before_io(), "should reject malformed typed privacy input");
}
```

- [ ] Add failing `auctioneer_input_` tests for exact routed bidder/slot membership, all admitted sizes, floors/currency, approved EC/EIDs, device/geo, and request/impression IDs. Include distinct aliases sharing an implementation.
- [ ] Add failing `auctioneer_privacy_` controls for host-deny/engine-allow and host-allow/engine-deny. Seed nonempty IDs/geo/FPD/TIDs before denial and inspect captured serialized bidder calls rather than only the intermediate request.
- [ ] Capture original header/legal-signal evidence before ingress consumes the request. Separate raw SID hint presence/value, decoded IDs, proxy/hint provenance and GPC presence/value. Absent hint stays absent, present empty stays empty, valid hint is supplied exactly, malformed hint refuses local input without I/O. Never present decoded envelope IDs as caller-supplied SID. Cover proxy/decoded, matching/conflicting, envelope-only/SID-only and GPC absent/0/1; preserve remote projection.
- [ ] Build the already-shared `auctioneer-openrtb` request explicitly from neutral `ProviderAuctionInput`, approved common context, and its local slots. Reuse the prerequisite's raw model, not a second DTO or old-model conversion. Populate the fields canonical preparation consumes; serializing TS `AuctionRequest` is prohibited because it omits consent and EIDs.
- [ ] Bind `HostRestrictionSource` by requested identity and canonical implementation. Keep host ceiling decisions separate from raw/decoded legal signals. Admit/fund conversion storage before copying, without inventing raw-wire provenance or repeating Auctioneer's policy resolver.
- [ ] Map the required trusted account mode: default clears all account-ID lookup paths; named sets only `site.publisher.ext.prebid.parentAccount` from configured ID. Keep app/dooh and raw publisher ID absent for the current site host. Test known/default/unknown-fallback/disabled policies and browser/context/bidder spoofing across all ingress paths.
- [ ] Implement every header, `EntryPoint::Auction` and trusted-debug row in the spec's mapping table. Project Low/Medium/Dense/host-Auto/High to `low`/`med`/`dense`/`dense`/`high`; capture prepared and serialized bidder calls to prove neither header fallback nor enrichment restores denied data.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_input_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_privacy_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_privacy_
```

Expected: exact nonempty sent coverage, no resurrected identifiers, unchanged existing consent regressions and preserved failure precedence. Combine Tasks 3–4 evidence for #1510's handoff with Auctioneer #51; portable fakes alone cannot close that boundary. Commit the typed input/restriction slice, retaining the later combined-artifact acceptance gates.

## Task 5 Add an owned local operation and cutoff completion

**Files:** new `auction/local/mod.rs`, concrete Auctioneer `mod.rs` and `result.rs`, and `auction/orchestrator.rs`.

**Interfaces:** Consumes Task 2's accepted `Arc<AuctioneerProviderConfig>`, Task 4 `OwnedAuctioneerInput` and Task 3 `AuctioneerServiceBinding`/`HostAuctionLease`. Produces neutral `LocalAuctionOperation` holding `Pin<Box<dyn Future<Output = Result<LocalAuctionCompletion, Report<TrustedServerError>>> + 'static>>` without `Send`. Concrete composition implements `start_auctioneer(policy: Arc<AuctioneerProviderConfig>, input: OwnedAuctioneerInput, binding: AuctioneerServiceBinding, lease: HostAuctionLease) -> LocalAuctionOperation`. Inside that future borrow `&policy.auctioneer` for preparation, restrictions only from `input`, and original `request_start`/`outer_deadline` from `lease.timing.auction_start`/`Some(lease.timing.auction_deadline.instant())`. Scheduler code receives only the neutral operation.

`LocalAuctionCompletion` owns a provider success/failure outcome, funded selected host candidates and `Box<dyn RetainedAuctionOwner>`. The private trait exposes `finish(&mut self, status: AuctionCompletionStatus)`; its concrete implementation retains the full engine completion/failure and host receipts. `selected(&self) -> &[SelectedLocalBid]` borrows admitted candidates; finishing observation never drops storage. Engine failures remain inside this retained completion until bounded diagnostics disappear; the future's outer `Report` is reserved for setup failures before a funded operation exists.

Test `auctioneer_operation_completed_sibling_survives_cutoff` runs under a contended shared gate:

```rust
let case = completed_and_suspended_fixture().await;
assert_eq!(case.selected_bid_ids, vec![case.completed_bid_id], "should retain the permitted completed bid");
assert_eq!(case.network_waits_after_cutoff, 0, "should complete without another network wait");
assert_eq!(case.run_starts, 1, "should not restart the operation");
```

- [ ] Add failing `auctioneer_operation_` tests for start once, pending progress, completed result retention, preparation/run drop, original deadline, funded failure, and admission at/over capacity.
- [ ] Define a neutral operation/result boundary for the orchestrator. The concrete implementation owns its service binding, input and original funding; create borrowed `AuctionServices` inside an owning pinned async operation. Preserve non-`Send` execution and response-stream ownership without manually self-referential fields or detached tasks.
- [ ] Invoke the existing API in its implemented order:

```rust
let prepared = prepare_auction(auctioneer_settings, auction_input, services, funding)?;
let completed = prepared.run().await?;
```

The snippet is the shared call shape; the enclosing owned binding, error mapping and progress driver are defined by this task, not omitted production code to copy verbatim.

- [ ] After #266's bounded completion contract is accepted, make cutoff an early control: one valid local bidder completes, another stays suspended, and the shared stream gate stays contended past the cutoff. Pass `AuctionServices.outer_deadline` and drive the existing `run()` future; no separate snapshot method exists. Preserve the permitted candidate with no additional network wait. Include timer setup/error/drop and unproductive timer paths. Current BestEffort/unbounded fallback cannot satisfy this gate.
- [ ] Retain `CompletedAuction`/funded failure behind the concrete result owner. Any TS projection/copy obtains its host storage funding before allocation. Observation finalization and owner destruction remain distinct and exact-once.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_operation_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_operation_
```

Expected: nonempty success/failure retention controls, no repeated run/dispatch, permitted completed bids survive cutoff, and holding completion at capacity blocks the next admission. Commit the operation boundary.

## Task 6 Reach the local operation from the production plan

**Files:** `auction/{mod,plan,provider,routing,orchestrator,telemetry}.rs`.

**Interfaces:** Consumes Task 2 compiled execution branches and Task 5's neutral operation/completion. `build_orchestrator_with_plan(plan: Arc<AuctionPlan>, settings: &Settings)` keeps its existing result/error signature and constructs real execution drivers. Extend dispatched state with remote wave state and owned local operation; both use the same original timing/lease. Produce `preflight_provider_ownership(input: &AuctionRequest, plan: &AuctionPlan) -> Result<ProviderOwnershipPlan, Report<TrustedServerError>>`; its plan includes exact explicit demand plus affected-provider refusal outcomes after outgoing stored expansion is resolved.

Test `auctioneer_routing_opaque_overlap_preserves_independent_remote` includes inline and actual fallback controls:

```rust
let case = opaque_overlap_fixture();
assert_eq!(case.affected_provider_calls, 0, "should refuse both opaque-overlap invocations before I/O");
assert_eq!(case.independent_explicit_calls, 1, "should retain independent remote work");
assert!(case.inline_override_dispatches_once(), "should inspect actual outgoing stored expansion");
```

- [ ] Add failing `auctioneer_plan_` and `auctioneer_routing_` tests through real Settings-to-plan-to-orchestrator construction. Cover local-only and mixed explicit routes, remote `all_eligible`, aliases, unknown/unavailable identities, opaque remote stored demand, direct/split parity and independent provider failure.
- [ ] Add `auctioneer_plan_remote_only_keeps_single_batch` on the feature-on build. With multiple remote legs and no local provider in the compiled plan, assert `installed_resources.is_none()`, `new_admissions == 0`, `remote_batches_started == 1` and exact unchanged request/result membership. A separate local-containing-plan control proves common gate enrollment, including requests with no routed local demand.
- [ ] Replace remote-only provider construction with execution-kind dispatch. Preserve the post-cutover remote `OutboundBatch` and indexed completion correlation. Keep Auctioneer-specific binding outside neutral routing/competition types.
- [ ] Preflight actual outgoing demand after inline/profile overrides. Assign each requested identity once and strip others' parameters. Actual opaque stored overlap refuses the affected local and opaque remote invocations before either I/O; independent inspectable providers continue. Test Legacy/Explicit/Disabled, inline suppression, affected-provider errors and zero duplicate dispatch; remote-only stored behavior is unchanged.
- [ ] Start eligible local execution before returning the dispatch token, without awaiting its entire result. Store pending remote and local state together; collection joins those same operations and preserves existing mediation, outcome categories and ordering. Fatal local policy failures remain failures; valid siblings and independent remote providers retain their established behavior.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_plan_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_routing_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_
```

Expected: exact per-provider/per-alias sent membership, zero duplicate local/remote dispatch, refusal before I/O for opaque overlap, and production execution without a fake endpoint or test-only registry. Commit the production seam.

## Task 7 Drive auctions through publisher streaming and EOF

**Files:** `publisher.rs`, `publisher/html_finalization.rs`, integration-tests manifest and HTML EOF harness files, and `scripts/async-html-finalization-local-test.sh`.

**Interfaces:** Consumes Task 6 dispatched owner and Task 5 operation. Produce `poll_auction_progress(state: &mut DispatchedAuction, context: &mut Context<'_>) -> Poll<()>` at existing origin/body/EOF awaits, without collecting/restarting. Response/finalizer guards own the same dispatched state and retained lease. Task 3's accepted deadline wait wakes this driver independently of origin readiness. New `--auctioneer` harness support selects only the instrumented Fastly local controls; default modes retain their current behavior.

Test `auctioneer_progress_origin_stall_preserves_prefix` uses independent origin and bidder barriers:

```rust
let case = stalled_origin_fixture().await;
assert!(case.bidder_progress_before_origin_release, "should co-drive the same local operation");
assert!(case.prefix_visible_before_auction_complete, "should preserve prefix streaming");
assert_eq!(case.abandons, 1, "should abandon once on response drop");
```

- [ ] Add failing `auctioneer_progress_` controls around pending origin headers, pending body chunks, async EOF stages, ready prefix bytes, cutoff wakeup, suffix failure and response abandonment. Assert bidder dispatch/progress before releasing each origin barrier, not only eventual completion.
- [ ] Co-poll the same pinned local operation at the existing origin/body/finalizer awaits and collection seams. Retain an early completion while publisher work continues. An origin stall must not prevent the auction deadline from waking the driver; a completed auction must not delay independent prefix delivery.
- [ ] Transfer the owned operation and retained results through existing hold/EOF guards. Drop/error paths abandon once; do not restart preparation or create a fresh timeout at an ownership transfer.
- [ ] Extend the existing harness with an explicit `--auctioneer` option valid with Fastly `--auction`. Forward the feature into both the native harness and the actual Fastly test-utils artifact build. Native core driver tests may exercise the local binding; this does not enable a production Axum host. Preserve the old Axum/Fastly remote control commands.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_progress_
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime,auctioneer --test html_eof_runtime auctioneer_
./scripts/async-html-finalization-local-test.sh axum --auction
./scripts/async-html-finalization-local-test.sh fastly --release --auction
./scripts/async-html-finalization-local-test.sh fastly --release --auction --auctioneer
```

The final `--auctioneer` harness option is **created by this task**; the integration-tests Cargo feature was created by Task 1. Expected: the instrumented feature-on Fastly artifact progresses local work across suspended body/EOF boundaries, preserves prefix timing and releases owners on each failure/drop path. Commit the streaming integration.

## Task 8 Preserve local selection targeting and delivery

**Files:** concrete `auction/local/auctioneer/result.rs`; `auction/{types,orchestrator,formats}.rs`, `publisher.rs`, `integrations/{aps,registry}.rs`; JS `src/core/{types,auction}.ts`, `src/integrations/{gpt,prebid}/index.ts` and adjacent Vitest tests under `crates/trusted-server-js/lib`.

**Interfaces:** Consumes Task 5's retained engine owner and selected indices, Task 4's effective granularity and existing host response admission. Produces `SelectedTargeting(BTreeMap<String, String>)` on selected host delivery facts, direct `bid.ext.prebid.targeting` and publisher bid-object `targeting`. Extend JS `AuctionBid`/`AuctionBidData` with `targeting?: Record<string, string>`. Parser and Prebid/GPT consumers retain that optional map; GPT records `Object.keys(appliedTargeting)` for cleanup. Host render/cache fields stay distinct.

Test `auctioneer_result_local_targeting_survives_outer_competition` compares exact carriers and a remote-winner negative control:

```rust
let case = selected_targeting_fixture();
assert_eq!(case.direct_targeting, case.engine_targeting, "should preserve finalized direct targeting");
assert_eq!(case.publisher_targeting, case.engine_targeting, "should preserve finalized publisher targeting");
assert!(case.remote_winner_local_keys.is_empty(), "should omit losing local keys");
assert_eq!(case.notifications_during_selection, 0, "should leave notifications to final delivery");
```

- [ ] Add failing `auctioneer_result_` tests for deal preference, stable equal-price order, original candidate indices, exact attribution/seat identity, no-bid versus error, response admission, and remote/mediator winners.
- [ ] Offer only the engine-selected local candidate per slot to host competition. Keep other admitted local outcomes under their retained owner for permitted response/diagnostic use. Preserve the host's outer selection policy without reranking local candidates.
- [ ] Carry selected targeting explicitly into direct and publisher delivery. Preserve valid known impression IDs, finite nonnegative USD price, admitted dimensions, and renderable markup **or** an admitted existing typed renderer/capability with required metadata. Reject invalid delivery without selecting a local runner-up.
- [ ] Preserve exact engine keys/prefix/truncation in optional typed JS carriers, direct parser, Prebid projection and GPT application. Engine keys override configured slot collisions for that impression; host render/cache identity remains explicit and authoritative. Track every dynamic applied key for navigation/refresh cleanup; do not hardcode the engine key set in browser code.
- [ ] Verify the pinned public Prebid lifecycle: `setupBidTargeting` merges generated keys over bid defaults. A carrier alone therefore does not preserve local `hb_pb`. Use adapter-scoped public bidder-settings projection to retain engine price/bidder keys for local bids while preserving remote defaults and publisher settings. Assert the final GPT targeting after Prebid setup, not just the adapter's returned object; do not patch private Prebid methods.
- [ ] Add Vitest controls for custom prefix, deal/size/format/bidder keys, slot collisions, and local winner → remote winner/no-bid cleanup. Check every granularity preset/boundary, direct/publisher parity and unbucketed macro CPM. Local direct optional targeting/`nurl`/`burl` fields may be absent for remote responses; preserve and test that compatibility contract.
- [ ] Register required existing renderer capabilities from compiled local requirements, including APS display with no remote APS profile. Do not admit arbitrary executable descriptors or introduce a new renderer.
- [ ] Preserve creative checking/rewriting and notification suppression. Forward permitted local winner `nurl`/`burl` through direct `/auction` and existing publisher delivery. Assert local selection itself fires no win/billing notification and losing local candidates leave no targeting or notification URLs on a remote winner.
- [ ] Retain original completion/failure and the #266-approved host receipts through creative copies, serialization and terminal egress. No public generic funding transfer exists today. Test positive copy funding/refusal, finalization versus destruction, remote-winner lifetime, at-capacity retention and exactly one restored outer slot.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_result_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_result_
(cd crates/trusted-server-js/lib && npx vitest run)
```

Expected: shared local winners/targeting survive outer competition, valid renderer output delivers, rejected creatives do not report success, and notifications remain owned by final delivery. Complete #1511 evidence on the actual selected Fastly artifact with upstream #51/#88 requirements satisfied; commit after checks.

## Task 8a Produce releasable non-test Auctioneer variants

**Files:** `fastly.toml`, `.github/workflows/{release-build,release}.yml`, `scripts/release/{phase1-build.sh,phase1.mjs,phase1.test.mjs,publish.mjs}`, CLI `src/run.rs` and `tests/release_build_facts.rs`.

**Interfaces:** Consumes Task 1's `auctioneer` feature and Task 2's artifact facts. Produces explicit `trusted-server-fastly-auctioneer.wasm` and `ts-auctioneer-<target>` assets alongside unchanged default assets, with feature-matched build/SBOM/source facts and signed publication membership. The planned producer interface adds a fourth `default|auctioneer` argument; current three-argument calls retain default behavior.

- [ ] Add failing producer inventory/feature-forwarding tests, including unsupported selection and `test-utils` contamination refusal. Extend clean-environment and container/non-container builds, locked metadata, offline CycloneDX feature selection, CLI `variant` reporting, smoke, signing/download/publication allowlists and tests together. Preserve source/canary/lock guards and existing privilege separation.
- [ ] Add an explicit feature-on local Fastly build selection while preserving default behavior. Document which paired CLI/wasm bundle operators deploy and how its artifact identity enters local config.
- [ ] Before PR handoff, run real non-test Fastly and shipping native CLI variants through the producer in separate clean checkouts, with fresh `RUNNER_TEMP` and empty release-assets paths. Verify digests, feature/source identities, SBOM, binary facts and packaging guards. Actual signing and digest/attestation-subject agreement wait for the first authorized `v*` tag release with publication enabled; record them as pending and block publication/deployment until verified. No branch-only signing rehearsal or workflow expansion is implied.

```bash
shellcheck scripts/release/phase1-build.sh
node --test scripts/release/phase1.test.mjs
```

The new producer invocation below is **created by this task**. Run it separately for each supported package/target/asset tuple; the existing producer refuses reused output directories:

```bash
RUNNER_TEMP="$(mktemp -d)" SOURCE_COMMIT="$(git rev-parse HEAD)" \
  bash scripts/release/phase1-build.sh trusted-server-adapter-fastly \
  wasm32-wasip1 trusted-server-fastly-auctioneer.wasm auctioneer
```

Expected before merge: non-test candidate assets, paired CLI facts, matching SBOM and tested release inventory/guards; default assets remain compatible. Expected at authorized release: successful signing, attestation-subject/digest verification and publication through existing gates. If the released binary identity differs from the qualified candidate, rerun affected artifact controls before deployment. This plan creates no tag or deployment. Commit after pre-merge checks; keep release evidence explicitly pending.

## Task 9 Produce controlled artifact equivalence evidence

**Files:** new `tests/html_eof_runtime/auctioneer.rs` and `fixtures/auctioneer/{Dockerfile,network.mjs,tls-origin.mjs}` under `crates/trusted-server-integration-tests/`; existing HTML EOF runtime/config/process/origin modules and `tests/common/trace_boundary.rs`; browser `playwright.auction-runtime.config.ts`, `tests/shared/auction-runtime.spec.ts`, new `helpers/aps-runner-fixture.js` and existing `helpers/trace-gpt-fixture.js`; integration-tests/browser manifests and auction testing guide.

**Interfaces:** Consumes Task 8a production artifact/CLI paths, Task 2 artifact identity and Task 8 delivery contracts. Produces a production-mode fixture runner plus an evidence manifest: host/Auctioneer/SDK commits, features/profile/patches, binary digests, accepted config identity, services/resources, discovery-derived case membership and captured outcomes. `run_production_auctioneer_case(artifacts: &ProductionAuctioneerArtifacts, case: &AuctioneerFixtureCase) -> Result<ArtifactObservation, Report<HarnessError>>` must execute the supplied binary; instrumented runner results carry a different artifact identity.

### Production fixture topology

Prepare a pinned `linux/amd64` image containing the precompiled native fixture worker, repository-pinned Viceroy/Node and lockfile-installed Playwright/Chromium with OS dependencies. Freeze its source at the worker's compile-time `CARGO_MANIFEST_DIR` layout. Finish builds and downloads before isolation; the production worker bypasses `Harness::build` and the hardcoded development CLI path. It executes the supplied, unchanged wasm and Task 8a's matching producer `x86_64-unknown-linux-gnu` Auctioneer CLI, including when the coordinator runs on macOS.

The host coordinator creates one owned `--internal` network and launches one explicit production worker with `--platform linux/amd64` and a `--read-only` root filesystem. Supply separate writable work/results mounts and bounded temporary storage for browser profiles and child-process scratch data. Attach only that network: no published ports, host networking, second network or Docker socket mount. The worker never launches Docker or recursively invokes the host coordinator. Control uses Docker stdin/stdout/exit status; retained evidence uses the results mount. The host does not dial the container or start its origin fixture/browser on host loopback.

| Process or files                                                           | Location and connection                                                                                                                      |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Native fixture worker                                                      | Inside the worker container; owns all child processes and their timeout/drop cleanup                                                         |
| Publisher origin and TLS bidder fixtures                                   | Same container; publisher origin binds `127.0.0.1` at its selected port, bidder fixtures bind loopback at each original HTTPS port           |
| Supplied wasm, producer Linux CLI, source and transient certificate inputs | Read-only image/mounts, with recorded digests; do not overlay macOS dependencies onto the Linux image                                        |
| CLI local config push, generated `fastly.toml`, blob/store state           | Worker-created writable work directory; Viceroy reads that exact generated config and read-only wasm path                                    |
| Viceroy and native HTTP driver                                             | Same container; Viceroy binds `127.0.0.1` at its selected listener port, and the worker calls that port's public routes                      |
| Playwright/Chromium                                                        | Worker child processes; `AUCTIONEER_BROWSER_ORIGIN` is `http://localhost:<same Viceroy port>`, with no mocked auction or second setup server |
| Temporary files, Playwright output and retained observations               | Separate writable work/results locations; browser `outputDir` uses results rather than the read-only source tree                             |

Map every compiled public bidder hostname to `127.0.0.1` in that container with `--add-host`. Keep original HTTPS ports, URLs, Host/SNI and certificate verification. Explicitly set the owned fixture `SSL_CERT_FILE` on the Viceroy child after environment filtering; set the installed `PLAYWRIGHT_BROWSERS_PATH` on the browser child the same way. Do not forward arbitrary host environment values. Record the image/platform, native worker, guest/CLI/runtime identities, actual network attachments and mount modes. No host-side port publication is needed on Linux or Docker Desktop.

Install the repository's [`browser/helpers/trace-gpt-fixture.js`](https://github.com/IABTechLab/trusted-server/blob/feaf3df5d1c9bb2dca64158480725cb555ab9c26/crates/trusted-server-integration-tests/browser/helpers/trace-gpt-fixture.js) through `page.addInitScript` before navigation. The fixture publisher page omits the vendor GPT bootstrap while keeping the production TSJS/GPT integration enabled, so it does not request the external GPT CDN. Parameterize the helper's creative iframe origin for this suite, preserving existing consumers' default; stage the pinned Prebid bundle and creative support scripts before isolation and serve them in-container. Label GPT callbacks and the APS runner API below as fixture evidence: these external vendor APIs are explicit exceptions to the no-mock rule, not evidence of live GPT/GAM or Amazon runner compatibility. Auction responses, Prebid projection, production TSJS/render-cache bytes and the delivered creative remain unmocked. Drive the existing TSJS render bridge with the ad ID captured from actual targeting, and assert the delivered creative DOM/assets; a synthetic render callback alone is not rendering evidence.

Extract the test-owned `FAKE_RUNNER` behavior from the [pinned APS renderer assertions](https://github.com/IABTechLab/trusted-server/blob/feaf3df5d1c9bb2dca64158480725cb555ab9c26/crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts) into `browser/helpers/aps-runner-fixture.js`. Stage those bytes and serve them at the exact production `APS_PREBID_CREATIVE_RUNNER_URL`, adding its hostname to the container's loopback mappings alongside bidder hosts. The internal TLS fixture uses SNI/Host routing on port 443 for that runner and `https://creative.example.com`, which serves fixture creative scripts, frames and assets. Supply those creative URLs in bidder fixture responses; all creative asset/frame URLs and notification endpoints must resolve to declared in-container fixtures. Preserve the production runner URLs, APS renderer CSP and both rendering paths; pass the actual admitted APS descriptor/envelope to the fixture runner and observe its resulting creative requests/DOM.

Prepare `certutil` in the image. Before Chromium starts, initialize the disposable worker user's `.pki/nssdb` with `certutil -N --empty-password`, then import only the owned fixture CA with `certutil -A -t "C,,"`. Both commands select that SQL database with `-d sql:<owned-directory>`; the import supplies the recorded fixture nickname and CA file with `-n` and `-i`. Keep that user's home and trust database under the writable work mount and explicitly set the browser child's `HOME` to that owned home after environment filtering. Use that CA's matching-SAN leaf certificates for the runner and creative HTTPS listeners. Keep `ignoreHTTPSErrors: false`, with no certificate-error bypass flags; Viceroy's `SSL_CERT_FILE` does not configure browser trust. Record the Chromium version, CA fingerprint and NSS database identity, and require browser HTTPS receipt plus untrusted-CA/wrong-SAN rejection before the APS cases.

Before creating pages, install a context-wide browser request guard with `serviceWorkers: 'block'`. Its allowed scheme/hostname/port tuples come only from owned listeners and explicit container DNS mappings, never from observed requests. Abort and record unmatched routable requests; fail the case on any off-list request observation, including redirect hops, child frames/popups, bundled Prebid calls and `nurl`/`burl` notifications. Block unexpected WebSockets as well. Do not add page-local routes that bypass the guard or silently count blocked dependency requests as a pass. The internal-only network remains the physical egress boundary.

Run `auctioneer_browser_refuses_off_container_requests` as three isolated subcases, each in a fresh browser context: a direct off-list subresource, an in-list fixture URL that redirects off-list, and an off-list WebSocket. Require all three to execute. Each must record a violation identifying the attempted scheme/authority and reject the case, independently of DOM assertions or network errors. Observe HTTP requests at the context event level so redirected hops are recorded even when route handlers are bypassed; use separate WebSocket routing/observation. A network error alone is not detection. Assert an empty violation list in every positive case.

Test `auctioneer_artifact_policy_change_render_and_rollback` asserts observed production behavior, not metadata-only acceptance:

```rust
let case = production_policy_change_fixture().await;
assert_eq!(case.first_binary_digest, case.changed_policy_binary_digest, "should reuse the production binary");
assert!(case.creative_rendered && case.changed_policy_observed, "should observe rendering and accepted config change");
assert!(case.compatible_rollback_serves_without_duplicate_calls(), "should roll back artifact and config together");
```

- [ ] Consume Task 8a's producer-built non-test Fastly artifact and matching CLI. Record digests, revisions, feature graph, dependency/profile facts, accepted policy identity, services and owners; reuse that same binary across policy/bidder batches. Never rebuild it with `test-utils` for acceptance.
- [ ] Add production coordinator/worker modes using the topology above. Mount supplied production inputs read-only, generate local config/store state in the worker's writable work directory, and pass their container paths to the matching Linux CLI and Viceroy. Keep instrumented EOF controls separately identified; never rebuild production inputs or launch a host-bound origin/browser for this mode.
- [ ] Complete DNS/TLS setup with transient certificates matching each original hostname and port, trusting only the fixture CA. Use ordinary dynamic backend registration; the pinned SDK refuses same-name static overrides. Record mappings, certificate fingerprints and unchanged wasm digest. Required child CA/browser-cache settings must survive the harness's environment filtering through explicit child injection.
- [ ] Extend `auctioneer_artifact_https_fixture_route` before catalog/display cases to prove the whole path: worker reaches publisher origin, CLI-generated config is readable by Viceroy, worker reaches the guest's public routes, a real non-test bidder request reaches TLS fixtures and parses, and Chromium loads/render-observes that same Viceroy through localhost. Prove runner/creative HTTPS receipt and both APS render paths under the browser trust and request-guard controls above. Wrong CA and wrong SAN must fail in Viceroy and Chromium; an unmapped bidder must have no path to live demand. Inspect the single internal-only attachment and absence of published ports/host-network exceptions. Enumerate every required authority/port and prove each is routable; failure blocks its row and #1512/#76/#91 acceptance. Source inspection or a rebuilt insecure variant cannot substitute for execution.
- [ ] Add the dedicated Playwright auction runtime config/spec using the existing Rust-owned trace-runtime launch pattern and labelled GPT/APS vendor fixtures above. From the integration-tests browser directory run `node node_modules/@playwright/test/cli.js test --config playwright.auction-runtime.config.ts`; the harness supplies `AUCTIONEER_BROWSER_ORIGIN` and `AUCTIONEER_EVIDENCE_MANIFEST` for its already-running production runtime. No setup starts a second server, and the spec must not intercept `/auction` or inject a fabricated bid. Extend the APS creative assertions while observing the actual fixture-to-parser-to-Prebid/TSJS-to-creative flow and vendor API behavior under those fixture boundaries.
- [ ] Add deterministic fixture cases using shared effective policy, typed input, clock/RNG behavior, reference data, and captured bidder responses. Compare shared standalone and embedded candidates, selected winner/ties, targeting, attribution and permitted diagnostics. Reuse Auctioneer's actual shared/conformance entry points; do not create a second engine or pretend raw-only malformed JSON has a typed equivalent.
- [ ] Run success, no-bid, privacy denial, invalid account/input, delayed credential, timeout, completed-plus-suspended/contended gate, partial failure, resource refusal, cancellation and recovery. Observe direct/page-bids/navigation delivery and browser-rendered creative/APS, dynamic targeting and notification ownership. Keep shared-engine comparison separate from host floor/mediation/creative decisions.
- [ ] Derive required executable display identities/aliases from Auctioneer discovery and capabilities. Exercise every required implemented row in deterministic within-limit batches, retaining explicit verified unimplemented inventory without I/O. Include installed concurrency and one-over refusal; no handwritten bidder list or increased cap.
- [ ] Run two isolated policies and apply an accepted config change in the same production binary without rebuilding; observe independently varied bidder/alias/account/privacy/secret behavior, invalid config refusal and recovery. Drive public routes and capture fixture-origin calls; add no production acceptance endpoint.
- [ ] Exercise compatible artifact/config rollback and record pre/post identities and serving results. Before restoring an older schema-incompatible binary, restore compatible routing/config; prove no implicit fallback or duplicate dispatch. Account combined host/engine/egress lifetimes rather than adding standalone totals. File host ownership evidence under #1510/#76 and flow/equivalence evidence under #1512/#91.

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime,auctioneer --test html_eof_runtime auctioneer_ -- --nocapture
./scripts/async-html-finalization-local-test.sh fastly --release --auction --auctioneer
./scripts/async-html-finalization-local-test.sh fastly --release --auction --auctioneer \
  --production-artifact "$FASTLY_AUCTIONEER_ARTIFACT" --cli "$AUCTIONEER_CLI"
```

`--production-artifact` and `--cli` are new harness support created by this task; supplied paths must be producer outputs with verified non-test features. Expected: exact discovery-derived membership/sent coverage, equal shared outcomes, rendered display, accepted config change and paired rollback, plus explicit combined production-artifact resource/serving evidence. Native and instrumented runs are supplementary. #1512 stays open until Auctioneer #91 records its independent comparison; missing upstream harness evidence is not a pass. No live bidder authorization or publisher pilot is claimed.

## Task 10 Complete CI operator guidance and PR handoff

**Files:** `.github/workflows/{test,format}.yml`, example config, auction guides, and `CHANGELOG.md`.

**Interfaces:** Consumes Tasks P–9 contracts/evidence and current CI job inventories. Produces default/feature-on validation lanes, operator bundle/config/rollback guidance and one head-bound PRfection handoff per delivery leaf. No task may claim upstream acceptance, deployment or issue closure from command examples or another artifact's logs.

- [ ] Add explicit feature-on Fastly, reusable-sandbox, native core/CLI and actual runtime harness steps without replacing feature-off lanes. Assert each new filtered test population is nonempty, including tests with `required-features`; explicitly enable any reused Auctioneer `conformance-suite` lane. Keep Node setup before every new cargo step and retain the existing pin guard.
- [ ] Document the typed local declaration, explicit routing/opaque stored-demand refusal, Fastly/display scope, required services, typed renderer support, diagnostics and compatible artifact/config rollback. Use fictional configuration and public approved source links only.
- [ ] Run the full implementation verification below at each implementation PR's actual head. Compare actual base/head for any suspected existing failure; do not weaken tests, budgets, catalog membership or CI to obtain a pass.
- [ ] Open each implementation PR against the actual default branch from its own phase worktree, link the merged planning commit, and use closing language only for the leaf it fully delivers. Preserve issue ownership across #1510/#1511/#1512. Ask for REVIEWER handoff under PRfection and keep the issue open until its actual acceptance is delivered.

## Implementation validation commands

Run from the implementation worktree root using the repository-pinned toolchain. Install JS/docs dependencies with their lockfiles as part of setup. These commands are a future execution checklist; **none is product-test evidence for the planning PR**. Stop and record failures rather than skipping them. Re-read current `AGENTS.md` and CI when implementation starts and include any added mandatory lanes.

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
cargo test -p trusted-server-core --test template_build_digest
cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
./scripts/test-cli.sh
cargo test -p trusted-server-core --lib openrtb_shared_types_
cargo test-fastly openrtb_shared_types_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer
cargo test-fastly-reuse --features auctioneer
cargo test -p trusted-server-core --features auctioneer
cargo test -p trusted-server-cli --features auctioneer
cargo clippy -p trusted-server-cli --all-targets --features auctioneer -- -D warnings
cargo build --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --release --features auctioneer
cargo build --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --release
cargo bench -p trusted-server-core --bench html_processor_bench -- --test
cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check
cargo clippy -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare,test-utils --lib -- -D warnings
cargo clippy -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin,test-utils --lib -- -D warnings
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://127.0.0.1:8080 \
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=integration-test-proxy-secret \
TRUSTED_SERVER__EC__PASSPHRASE=integration-test-ec-secret-padded-32 \
  cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
```

```bash
(cd crates/trusted-server-js/lib && npm ci && npm run build && npm run lint && npx vitest run && npm run format)
(cd docs && npm ci && npm run lint && npm run format && npm run build)
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
scripts/lint-wasm-clock.sh
shellcheck scripts/docs-proposal/*.sh
node --test scripts/docs-proposal/hunks.test.mjs
scripts/docs-proposal/test.sh
node --test scripts/check-ci-node-pin.test.mjs
node scripts/check-ci-node-pin.mjs
node --test scripts/release/phase1.test.mjs
shellcheck scripts/release/phase1-build.sh
node crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs "$(command -v viceroy)"
cargo build --locked -p trusted-server-adapter-cloudflare --example deadline-runtime --target wasm32-unknown-unknown --features cloudflare
cargo clippy -p trusted-server-adapter-cloudflare --example deadline-runtime --target wasm32-unknown-unknown --features cloudflare -- -D warnings
node scripts/test-worker-auction-deadlines.mjs cloudflare --runtime "$(command -v wrangler)"
cargo build --locked -p trusted-server-adapter-spin --example deadline-runtime --target wasm32-wasip1 --features spin
cargo clippy -p trusted-server-adapter-spin --example deadline-runtime --target wasm32-wasip1 --features spin -- -D warnings
node scripts/test-worker-auction-deadlines.mjs spin --runtime "$(command -v spin)"
```

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime -- -D warnings
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime,auctioneer --test html_eof_runtime
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime,auctioneer --test html_eof_runtime -- -D warnings
./scripts/async-html-finalization-local-test.sh axum --auction
./scripts/async-html-finalization-local-test.sh fastly --release --auction
./scripts/async-html-finalization-local-test.sh fastly --release --auction --auctioneer
BID_DELAY=3 ./scripts/template-cache-local-test.sh esi
BID_DELAY=3 ./scripts/template-cache-local-test.sh inline
BID_DELAY=3 ./scripts/template-cache-local-test.sh purge
```

Run the URL-host check with the detected host target and actual base:

```bash
validation_host_target="$(rustc -vV | sed -n 's/host: //p')"
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml --target "$validation_host_target" --bin ts -- dev lint domains --changed-vs origin/main
cargo test -p trusted-server-core --target "$validation_host_target" --lib trace_document_skipped_emitted_script_leaves_ad_state_untouched -- --ignored
cargo test -p trusted-server-core --target "$validation_host_target" --lib trace_document_emitted_script_deep_freezes_owned_context_in_node -- --ignored
```

Preserve these additional existing CI lanes with their setup and nonempty assertions. These are mandatory future runtime-change checks, not optional substitutes for the commands above:

| Lane                                       | Execution and feature policy                                                                                                                                                                                                                                    |
| ------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TSJS source digest parity                  | In a temporary output directory run `node crates/trusted-server-js/lib/build-all.mjs --out-dir "$bundle_dir"`, then `TSJS_PREBUILT_DIR="$bundle_dir" cargo check-axum --locked`, as in `test.yml`; remove only the owned temporary output                       |
| Native integration and Fastly EC lifecycle | Run `./scripts/integration-tests.sh` and `./scripts/integration-tests.sh test_ec_lifecycle_fastly` with pinned Docker/Viceroy/runtime setup; retain default remote fixtures                                                                                     |
| Browser integration and initial render     | Run `./scripts/integration-tests-browser.sh` for its external Prebid/trace fixtures and Playwright run. It does not run initial-render checks: also execute both commands below with browser CI setup and the config generator build                            |
| Prepared runtime/browser trace acceptance  | With the exact artifact paths, ports, fixture config, Spin build and browser setup from `.github/workflows/integration-tests.yml`, run the two ignored `--test integration` commands below; do not count the ordinary integration script's skipped trace probes |
| Native CLI matrix                          | Preserve Linux/macOS tests, `release_build_facts`, rejected release branch-source check, macOS Chrome fixtures and Linux real-NSS ignored tests/count assertions from `test.yml`                                                                                |
| New feature-on validation                  | Add explicit Fastly/reuse/core/CLI build-test-clippy lanes and nonempty local test populations, plus Task 9 production-artifact display/trace observations. Do not enable local Axum/Cloudflare/Spin hosts through parity                                       |

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --target "$validation_host_target" --test integration trace_runtime_boundary -- --ignored --test-threads=1 --nocapture
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --target "$validation_host_target" --test integration trace_browser_workflow -- --ignored --test-threads=1 --nocapture
cargo build --package trusted-server-integration-tests --bin generate-viceroy-config
(cd crates/trusted-server-integration-tests/browser && node --test initial-render/pages.test.cjs)
(cd crates/trusted-server-integration-tests/browser && node initial-render/run.cjs --out "$RUNNER_TEMP/initial-render-evidence")
```

The cutover introduces the response-diff script; it is absent at the planning base. Run this post-cutover check for Tasks 1 onward, and for Task 0 only if its actual base already includes the cutover. Task 0 still runs every CI check present at its own base:

```bash
node --test scripts/fastly-response-diff.test.mjs
```

For feature-on native CLI browser tests, the existing `scripts/test-cli.sh` accepts a host target, not `--features`. Add a deliberate feature-aware helper interface or run the four existing filter populations directly with the pinned Chrome setup. For each filter below, first list and require a positive test count, then run with `TS_AUDIT_BROWSER_TESTS=1`, `--features auctioneer` and one test thread:

```bash
for browser_filter in \
  'commands::audit::browser::tests::' \
  'commands::audit::generate::browser_collector::tests::' \
  'generate_deploy_validation_' \
  'generate_unavailable_proxy_'; do
  browser_count="$(cargo test -p trusted-server-cli --target "$validation_host_target" \
    --features auctioneer "$browser_filter" -- --ignored --list | awk '/: test$/ { n += 1 } END { print n + 0 }')"
  test "$browser_count" -gt 0 || exit 1
  TS_AUDIT_BROWSER_TESTS=1 cargo test -p trusted-server-cli --target "$validation_host_target" \
    --features auctioneer "$browser_filter" -- --ignored --test-threads=1
done
```

Keep default full CI and new feature-on tests as separate evidence. Real-NSS setup/permissions remain their existing job's responsibility; record external results by runner. Task 8a's producer/SBOM/build-fact verification and Task 9's supplied-binary mode are required before their implementation handoff. Actual signing/attestation is pending until the gated tag workflow executes; publication/deployment waits on it. Refresh this inventory against the actual implementation base before execution.

Task 0 retires `cargo clippy-codegen` and the old codegen package test together with the obsolete package/jobs; the commands above target surviving compatibility owners. This changes the reviewed future runtime migration, not the current planning PR's CI. Refresh any new feature-on CI/tooling commands introduced by Tasks 0–10 before implementation handoff. Record Fastly resource/serving and upstream acceptance separately from compile/test success. Preserve all actual GitHub-required checks and review/merge gates.

## Review log

- Round 1 (Codex plan-document reviewer, 2026-11-22): approved the full plan against the spec and inspected source boundaries, with no blocking gaps. Clarified that the controlled EOF artifact's digest and `test-utils` features are recorded separately from the plain release build; observations must identify the binary actually executed.
- Round 2 (Codex plan-document reviewer, 2026-11-22): approved the user-directed Task 0 prerequisite and its source/compatibility/retirement checks, including unconditional raw types and the later optional engine feature. Found no blocking mismatch or validation-order gap; a separate spec reviewer also approved the revised design.
- Round 3 (Codex advisory plan-document reviewer, 2026-11-22): approved the complete feedback revision, including task interfaces/assertions, service-before-input ordering, named upstream gates, production release producer and supplied-binary acceptance, browser propagation and the full future CI inventory. Command assumptions were checked against source; no product commands were executed by this document review.
- Round 4 (Codex advisory plan-document reviewer, 2026-11-23): approved the seven remaining feedback corrections, consistent private interfaces, feature-on remote-only control, explicit target refusal test, CI/release staging and the production DNS/TLS plus Playwright proof gates. This is a document review; no product or fixture route was executed.
- Round 5 (Codex advisory plan-document reviewer, 2026-11-24): approved the explicit single-container production worker, Linux artifact pairing, read-only inputs, writable outputs and child environment injection, plus removal of the stale Spin TLS setting. Found no blocking spec mismatch or buildability gap; no product or fixture route was executed by this review.
- Round 6 (Codex advisory plan-document reviewer, 2026-11-24): approved the pinned local GPT helper, vendor-bootstrap omission, staged in-container browser/creative assets and explicit external GPT API fixture boundary. Actual auction/Prebid/TSJS/render bytes and creative DOM observations remain required; no product, browser or Docker execution was performed by this review.
- Round 7 (Codex advisory plan-document reviewer, 2026-11-24): approved the test-owned APS runner source at its unchanged HTTPS URL, SNI-routed creative hosting, disposable NSS trust, positive/negative TLS controls and context-wide request enforcement. Found no blocking buildability or spec-alignment gap against pinned renderer/fixture sources; no runtime test was performed.
- Round 8 (Codex advisory plan-document reviewer, 2026-11-25): approved the explicit direct-request, redirect-hop and WebSocket negative controls, each isolated and required to record a violation and reject the case independently of network failure. Found no correction-related blocker or spec mismatch; no runtime control was executed.
