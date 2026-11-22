# Auctioneer Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** First adopt Auctioneer's OpenRTB types throughout Trusted Server, then run Auctioneer in-process as a Fastly banner/display provider using shared configuration, services, routing, and delivery, with equivalent engine outcomes and verified artifact ownership.

**Architecture:** Land a feature-independent migration of existing OpenRTB wire paths to `auctioneer-openrtb` before host binding. Then extend the production compiled provider plan with local execution, bind the settings-first API inside a request-owned operation, co-drive it during publisher I/O and EOF work, and retain funded results through delivery. Keep neutral orchestration types and reuse shared preparation, discovered bidders, and EdgeZero transport.

**Tech Stack:** Rust 2024, Auctioneer's typed bridge, EdgeZero PR 275 integration APIs, Fastly Compute, Viceroy, existing native configuration and HTML EOF harnesses.

---

## Scope and execution prerequisites

Specification: [Auctioneer integration design](../specs/2026-11-22-auctioneer-integration-design.md). Tracking: [Trusted Server #1509](https://github.com/IABTechLab/trusted-server/issues/1509), under [#1505](https://github.com/IABTechLab/trusted-server/issues/1505). This planning PR contains prose only and uses `Refs #1509`; it does not close an issue or implement integration.

The user requested the spec and plan together for PR review. Implementation starts only after the planning PR is approved and merged, in separate phase worktrees/PRs. Use the installed PRfection CREATOR workflow for that transition; this plan does not authorize merging or deployment.

All product commands below describe **future implementation validation and expected results**, not results executed for this planning PR. The planning PR runs documentation checks only. At implementation time, capture actual commands, exit codes, result lines, exact head, artifact identity, and external evidence owner.

Task 0 is the required foundation under #1505: shared OpenRTB types replace the duplicate model/codegen across existing paths and all adapters. Its merged compatibility evidence is a prerequisite to #1510. Subsequent delivery stays decomposed across the existing leaves, not a single implementation in #1509:

| Delivery                 | Tasks | Acceptance boundary                                                                                                         |
| ------------------------ | ----- | --------------------------------------------------------------------------------------------------------------------------- |
| #1510, host binding      | 1–4   | Shared typed configuration, input/restrictions, actual SDK services and funded request ownership; coordinate Auctioneer #51 |
| #1511, local provider    | 5–8   | Production plan, dispatch/progress/collection, delivery, and actual Fastly artifact; coordinate Auctioneer #76 and #88      |
| #1512, artifact evidence | 9–10  | Controlled host flow and evidence consumed by Auctioneer #91                                                                |

Task 0 is a mandatory portable-model prerequisite and can land independently of the EdgeZero cutover. Tasks 1–10 implement host integration against the cutover API. Before Task 1, fetch the default branch, verify Task 0's merged commit/evidence, and confirm #1507/PR #1506 has landed, or rebase a separately approved implementation branch onto its reviewed candidate. Do not port host integration onto `PlatformHttpClient`, restore deleted pending handles, or absorb the entire SDK cutover into an Auctioneer implementation PR. The planning base is `1810c763add220dd34fd5faaffe2c69aab1d556f`; the inspected post-cutover source is `feaf3df5d1c9bb2dca64158480725cb555ab9c26`.

Agree one immutable EdgeZero source/revision with Auctioneer before linking shared SDK objects. The spec records the inspected pins; none is an assertion of the final common graph. Review Auctioneer's workspace-root patches, schema support, feature lanes, and the linked host release profile. Missing shared SDK, timer, schema, funding, or serving capabilities are explicit dependency blockers, not invitations to add permissive defaults. EZ-Q1 qualification does not gate cutover merge/deployment; Auctioneer's selected Fastly acceptance gates still apply.

## File map

Paths are repository-relative. New file names below are planned; their APIs do not exist yet. Keep additions private unless an existing caller requires visibility, and document any public interface. Existing files stay in place; no unrelated refactor is planned.

Task 0 additionally migrates `crates/trusted-server-core/src/openrtb.rs`, current/legacy `integrations/{prebid,aps,adserver_mock}.rs`, `auction/openrtb/{tests,test_executor}.rs`, and `ec/{mod,admin,identify,eids,prebid_eids}.rs`. It retires `crates/trusted-server-openrtb/` and `crates/trusted-server-openrtb-codegen/`, with corresponding workspace/lockfile, `.cargo/config.toml`, CI and current `AGENTS.md`/generation-guide changes.

| Files                                                                                                                                                           | Responsibility                                                                             |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `Cargo.toml`, `Cargo.lock`; core, CLI, Fastly, and integration-tests `Cargo.toml` files                                                                         | One dependency graph, optional `auctioneer` feature, and explicit test lanes               |
| `crates/trusted-server-core/src/auction_config_types.rs`, `auction/plan.rs`, `auction/config.rs`                                                                | Serialized remote/local declarations, common plan facts, routing and target validation     |
| `crates/trusted-server-core/src/auction/local/mod.rs`                                                                                                           | Neutral local operation/result contract; no bidder policy or Auctioneer internals          |
| `crates/trusted-server-core/src/auction/local/auctioneer/{mod,input,services,result}.rs`                                                                        | Concrete local binding, typed conversion, services/funding, and retained output projection |
| `crates/trusted-server-core/src/{config,config_payload,settings,settings_data,secret_resolution}.rs`; CLI `src/app_config.rs` and `tests/config_env_overlay.rs` | Existing authoring/push/blob/runtime validation and deferred secret-reference semantics    |
| `crates/trusted-server-core/src/consent/{types,extraction,mod}.rs`, `auction/{types,endpoints}.rs`, `publisher.rs`                                              | Request-bound signal evidence, existing consent/identity gates, common ingress mapping     |
| `crates/trusted-server-core/src/platform/types.rs`; Fastly `src/app.rs`                                                                                         | Request SDK objects and actual Fastly binding                                              |
| `crates/trusted-server-core/src/auction/{mod,provider,routing,orchestrator,formats,telemetry}.rs`                                                               | Production driver construction, exact routing, competition, direct output and outcomes     |
| `crates/trusted-server-core/src/publisher/html_finalization.rs`, `integrations/{aps,registry}.rs`                                                               | Streaming/EOF progress and existing typed renderer capability registration                 |
| `crates/trusted-server-integration-tests/tests/html_eof_runtime{.rs,/config.rs,/process.rs,/origin.rs,/auctioneer.rs}`                                          | Actual artifact orchestration, deterministic bidder captures and equivalence controls      |
| `scripts/async-html-finalization-local-test.sh`, `.github/workflows/{test,format}.yml`                                                                          | Explicit feature-on runtime and lint/test execution; preserve existing lanes               |
| `trusted-server.example.toml`, `docs/guide/{auction-orchestration,auction-testing}.md`, `CHANGELOG.md`                                                          | Operator configuration, documented limits and rollback                                     |

Tests belong beside changed Rust units, with the existing CLI and HTML EOF harnesses for their respective boundaries. The `auctioneer_` prefix below names **new planned tests** so command filters can prove they executed a nonempty population.

## Task 0 Adopt shared OpenRTB types before host integration

**Files:** root/core manifests and lockfile; `crates/trusted-server-core/src/openrtb.rs`, `auction/{openrtb,formats,types,endpoints}.rs`, `auction/openrtb/{tests,test_executor}.rs`, legacy `integrations/{prebid,aps,adserver_mock}.rs`, and `ec/{mod,admin,identify,eids,prebid_eids,finalize}.rs` under that same core source root; retire both `trusted-server-openrtb` crate trees; update `.cargo/config.toml`, `.github/workflows/{format,test}.yml`, `AGENTS.md` and current type-generation documentation.

- [ ] Capture source-bound pre-migration request/output goldens and response-admission cases for standard, Prebid, APS and mediator paths. Include current/legacy builders, direct `/auction`, cookies/headers/identity rows, unknown extensions, consent/EID placement, signing, stored-request precedence, no-bid, and malformed bids with valid siblings. Use fictional fixtures, existing bounded harnesses and semantic JSON comparison; assert exact signed payload behavior where bytes matter.
- [ ] Add `openrtb_shared_types_` compatibility tests beside the relevant owners and migrate applicable tests from the retiring model crate into core. Cover omitted/default fields; accepted bool/integer/string flag forms; required/optional IDs and price; numeric width/range and dimension inference; extension casing/placement; missing/null/value; notifications; EID/UID limits, sanitization, source merging and permission gates. Each negative control starts with a nonempty valid population so sibling loss is observable.
- [ ] Pin `auctioneer-openrtb` as an unconditional shared-model dependency and migrate the core facade/reexports, all live and legacy builders, and direct wire output to its raw types. Retain project extension serializers and a small local `ToExt` helper if needed. Keep neutral `AuctionRequest`/host `Bid` and browser-facing types; do not replace them with protocol DTOs.
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

- [ ] Run the full applicable runtime-change CI suite, with retired codegen gates replaced by surviving compatibility-owner checks. Land this as a separate reviewed prerequisite PR under #1505 and record its exact merged commit, source revision and evidence. Do not start Task 1 or claim #1510 host handoff from this migration alone. Any subsequent repin that changes the shared model reruns the affected compatibility controls.

## Task 1 Add the reviewed engine dependency and feature boundary

**Files:** root/core/CLI/Fastly manifests and lockfile; `auction/mod.rs` and new `auction/local/auctioneer/mod.rs`.

- [ ] Record the implementation base, selected Auctioneer commit, common EdgeZero revision/source, effective patches, and target features. Compare their source/API deltas against the spec baselines before changing the lockfile.
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

**Files:** `auction_config_types.rs`, `auction/{config,plan}.rs`, `config.rs`, `config_payload.rs`, `settings_data.rs`, and CLI `tests/config_env_overlay.rs`.

- [ ] Add failing tests named `auctioneer_config_round_trip`, `auctioneer_config_invalid_policy`, `auctioneer_config_unavailable_feature`, and `auctioneer_config_deferred_credentials`. Assert remote-only serialized shape stays unchanged; explicit local configuration carries the real typed subtree; invalid mixed remote/local fields and unsupported target declarations refuse.
- [ ] Run the new native tests and verify their intended red state. Distinguish a missing implementation failure from a harness/setup error.

```bash
cargo test -p trusted-server-core --lib auctioneer_config_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_config_
```

Expected before implementation: a meaningful failure for the missing local contract. Expected after implementation: nonzero matching test populations, zero failures in both applicable feature lanes.

- [ ] Add `execution` selection, preserving omitted remote defaults. Local configuration requires `auctioneer: AuctioneerAppConfig`, explicit routing, and common timeout/notification settings; reject remote endpoint/protocol/profile fields. Keep the shared Auctioneer model in the concrete configuration binding, not as bidder policy inside neutral scheduling types.
- [ ] Extend plan compilation with common provider identity/timing/notification facts plus distinct remote and local execution data. Only the remote variant owns transport/profile fields. Resolve the local executor in composition code, not test-only registration.
- [ ] Invoke Auctioneer's shared contextual validator from both authoring and runtime validation with explicit immutable catalog/default/resource facts. Keep exact Settings/wrapper/blob semantics, environment overlays, chunking and signature checks.
- [ ] Preserve deferred bidder credential references. Do not attach them to the eager static-secret resolver. Test with a spy store that config validation/extraction does not turn deferred bidder credentials into unconditional reads; validate values at their shared permitted lookup boundary.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_config_
cargo test -p trusted-server-cli --features auctioneer --test config_env_overlay
cargo test -p trusted-server-cli --features auctioneer --lib auctioneer_
```

Expected: accepted policy changes affect the borrowed Auctioneer subtree, unrelated TS settings do not, and invalid policy fails without bidder I/O. Commit the configuration slice after target-matched lint/test checks.

## Task 3 Preserve routed typed input and privacy evidence

**Files:** new `auction/local/auctioneer/input.rs`; `consent/{types,extraction,mod}.rs`, `auction/{types,endpoints,routing}.rs`, and `publisher.rs`.

- [ ] Add failing `auctioneer_input_` tests for exact routed bidder/slot membership, all admitted sizes, floors/currency, approved EC/EIDs, device/geo, and request/impression IDs. Include distinct aliases sharing an implementation.
- [ ] Add failing `auctioneer_privacy_` controls for host-deny/engine-allow and host-allow/engine-deny. Seed nonempty IDs/geo/FPD/TIDs before denial and inspect captured serialized bidder calls rather than only the intermediate request.
- [ ] Capture the narrow authoritative ingress evidence needed for missing/malformed/conflicting signals before the request is consumed. Preserve raw GPP section-ID evidence and GPC presence/value distinctions across direct auction, navigation and page-bids; unsupported evidence remains absent. Do not change the existing EC/EID or contextual-auction permission rules.
- [ ] Build the already-shared `auctioneer-openrtb` request explicitly from neutral `ProviderAuctionInput`, approved common context, and its local slots. Reuse the prerequisite's raw model, not a second DTO or old-model conversion. Populate the fields canonical preparation consumes; serializing TS `AuctionRequest` is prohibited because it omits consent and EIDs.
- [ ] Bind `HostRestrictionSource` by requested identity and canonical implementation. Keep host ceiling decisions separate from raw/decoded legal signals. Admit/fund conversion storage before copying, without inventing raw-wire provenance or repeating Auctioneer's policy resolver.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_input_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_privacy_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_privacy_
```

Expected: exact nonempty sent coverage, no resurrected identifiers, unchanged existing consent regressions, and preserved failure precedence. Commit the typed input/restriction slice.

## Task 4 Bind real host services and original funding

**Files:** new `auction/local/auctioneer/services.rs`; `platform/types.rs`, Fastly `src/app.rs`, and concrete binding `mod.rs`.

- [ ] Write failing `auctioneer_services_` tests using the real SDK object boundary plus spy transport/secret backends. Check the settings-first subtree argument, client/clock identity, original timing, required capabilities, and independent policies in a reused sandbox.
- [ ] Establish a request-owned binding for `EdgeZeroFanout::with_wave_scheduler`: actual client, timer, clock, decode cooperator, strategy, resolved limits and shared stream gate. Supply matching discovered registry/bindings, GVL/GCL, explicit RNG, defaults and restrictions. Missing services refuse; do not call standalone bootstrap or install ambient policy fallbacks.
- [ ] Construct the exported `CredentialLoader` from the host-bound secret store and matching clock. Exercise delayed lookup and resource/deadline refusal before any credential I/O. Preserve the actual upstream credential byte and lookup bounds.
- [ ] Derive `AuctionResourceProfile` from the accepted complete tuple. Prepay using the same ledger and an already-acquired permit before bridge-owned conversion/preparation. Do not create a second transport/admission pool or use a fabricated derived budget.
- [ ] Cover prepayment rollback, zero remaining deadline, preparation delay, two isolated accepted policies/secret references, missing service, configuration recovery, and retained nonempty failures. Assert exact-once release and a nonempty denied dispatch/secret control population.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_services_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_services_
cargo test-fastly-reuse --features auctioneer auctioneer_
```

Expected: actual compatible SDK objects reach the shared API, expired/refused work performs no forbidden I/O, and accepted snapshots remain isolated. Complete #1510's handoff evidence with Auctioneer #51; portable fake-service results alone cannot close that boundary. Commit after checks.

## Task 5 Add an owned local operation and cutoff completion

**Files:** new `auction/local/mod.rs`, concrete Auctioneer `mod.rs` and `result.rs`, and `auction/orchestrator.rs`.

- [ ] Add failing `auctioneer_operation_` tests for start once, pending progress, completed result retention, preparation/run drop, original deadline, funded failure, and admission at/over capacity.
- [ ] Define a neutral operation/result boundary for the orchestrator. The concrete implementation owns its service binding, input and original funding; create borrowed `AuctionServices` inside an owning pinned async operation. Preserve non-`Send` execution and response-stream ownership without manually self-referential fields or detached tasks.
- [ ] Invoke the existing API in its implemented order:

```rust
let prepared = prepare_auction(auctioneer_settings, auction_input, services, funding)?;
let completed = prepared.run().await?;
```

The snippet is the shared call shape; the enclosing owned binding, error mapping and progress driver are defined by this task, not omitted production code to copy verbatim.

- [ ] Make the cutoff behavior an early acceptance check: one bidder completes a valid local bid while another remains suspended. At the original cutoff, poll the engine's bounded completion path, retain the permitted candidate, and perform no additional network wait. If the selected upstream API cannot supply that result, request the minimal upstream contract change and wait; do not fake a snapshot or discard the future first.
- [ ] Retain `CompletedAuction`/funded failure behind the concrete result owner. Any TS projection/copy obtains its host storage funding before allocation. Observation finalization and owner destruction remain distinct and exact-once.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_operation_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_operation_
```

Expected: nonempty success/failure retention controls, no repeated run/dispatch, permitted completed bids survive cutoff, and holding completion at capacity blocks the next admission. Commit the operation boundary.

## Task 6 Reach the local operation from the production plan

**Files:** `auction/{mod,plan,provider,routing,orchestrator,telemetry}.rs`.

- [ ] Add failing `auctioneer_plan_` and `auctioneer_routing_` tests through real Settings-to-plan-to-orchestrator construction. Cover local-only and mixed explicit routes, remote `all_eligible`, aliases, unknown/unavailable identities, opaque remote stored demand, direct/split parity and independent provider failure.
- [ ] Replace remote-only provider construction with execution-kind dispatch. Preserve the post-cutover remote `OutboundBatch` and indexed completion correlation. Keep Auctioneer-specific binding outside neutral routing/competition types.
- [ ] Run a routing preflight before any affected dispatch. Assign each requested identity once and strip other bidders' parameters. Refuse simultaneous local demand and opaque remote stored expansion; remote-only stored requests retain their behavior. Do not add an operator assertion as a substitute for inspectable ownership.
- [ ] Start eligible local execution before returning the dispatch token, without awaiting its entire result. Store pending remote and local state together; collection joins those same operations and preserves existing mediation, outcome categories and ordering. Fatal local policy failures remain failures; valid siblings and independent remote providers retain their established behavior.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_plan_
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_routing_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_
```

Expected: exact per-provider/per-alias sent membership, zero duplicate local/remote dispatch, refusal before I/O for opaque overlap, and production execution without a fake endpoint or test-only registry. Commit the production seam.

## Task 7 Drive auctions through publisher streaming and EOF

**Files:** `publisher.rs`, `publisher/html_finalization.rs`, integration-tests manifest and HTML EOF harness files, and `scripts/async-html-finalization-local-test.sh`.

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

The final option and integration-tests `auctioneer` feature are **created by this task**, not existing command support. Expected: the actual feature-on Fastly artifact progresses local work across the suspended body/EOF boundaries, preserves prefix timing, and releases owners on each failure/drop path. Commit the streaming integration.

## Task 8 Preserve local selection targeting and delivery

**Files:** concrete `auction/local/auctioneer/result.rs`; `auction/{types,orchestrator,formats}.rs`, `publisher.rs`, and `integrations/{aps,registry}.rs`.

- [ ] Add failing `auctioneer_result_` tests for deal preference, stable equal-price order, original candidate indices, exact attribution/seat identity, no-bid versus error, response admission, and remote/mediator winners.
- [ ] Offer only the engine-selected local candidate per slot to host competition. Keep other admitted local outcomes under their retained owner for permitted response/diagnostic use. Preserve the host's outer selection policy without reranking local candidates.
- [ ] Carry selected targeting explicitly into direct and publisher delivery. Preserve valid known impression IDs, finite nonnegative USD price, admitted dimensions, and renderable markup **or** an admitted existing typed renderer/capability with required metadata. Reject invalid delivery without selecting a local runner-up.
- [ ] Register required existing renderer capabilities from compiled local requirements, including APS display with no remote APS profile. Do not admit arbitrary executable descriptors or introduce a new renderer.
- [ ] Preserve creative checking/rewriting and notification suppression. Forward permitted local winner `nurl`/`burl` through direct `/auction` and existing publisher delivery. Assert local selection itself fires no win/billing notification and losing local candidates leave no targeting or notification URLs on a remote winner.
- [ ] Transfer or retain funding through creative copies, serialization, retained output and terminal egress. Test finalization, destruction order, at-capacity retention, and exactly one restored slot after release.

```bash
cargo test -p trusted-server-core --features auctioneer --lib auctioneer_result_
cargo test-fastly --features trusted-server-core/auctioneer,trusted-server-adapter-fastly/auctioneer auctioneer_result_
```

Expected: shared local winners/targeting survive outer competition, valid renderer output delivers, rejected creatives do not report success, and notifications remain owned by final delivery. Complete #1511 evidence on the actual selected Fastly artifact with upstream #51/#88 requirements satisfied; commit after checks.

## Task 9 Produce controlled artifact equivalence evidence

**Files:** new `tests/html_eof_runtime/auctioneer.rs`; existing HTML EOF runtime/config/process/origin modules; integration-tests manifest and auction testing guide.

- [ ] Build the feature-on Fastly release artifact once and record its digest, revisions, feature graph, dependency/profile facts, accepted policy identity, SDK services and resource owners. Reuse that same binary across policy and bidder batches.
- [ ] Record the digest and full features of the artifact actually built and executed by the EOF harness, including `test-utils`, separately from the plain release build. Bind each observation to its executed binary; reconcile the instrumentation/layout differences before claiming deployment-artifact resource fit.
- [ ] Add deterministic fixture cases using shared effective policy, typed input, clock/RNG behavior, reference data, and captured bidder responses. Compare shared standalone and embedded candidates, selected winner/ties, targeting, attribution and permitted diagnostics. Reuse Auctioneer's actual shared/conformance entry points; do not create a second engine or pretend raw-only malformed JSON has a typed equivalent.
- [ ] Run successful, no-bid, privacy-denied, invalid account/input, delayed credential, timeout, completed-plus-suspended bidder, partial failure, resource refusal, cancellation and recovery cases. Keep the engine comparison separate from outer floor/mediation/creative decisions.
- [ ] Derive required executable display identities/aliases from Auctioneer discovery and capabilities. Exercise every required implemented row in deterministic within-limit batches, retaining explicit verified unimplemented inventory without I/O. Include installed concurrency and one-over refusal; no handwritten bidder list or increased cap.
- [ ] Use existing test-utils fixture control and captured outbound calls against the actual linked Fastly application. Do not add a production acceptance endpoint. Run two isolated runtime policies in that same artifact and account combined host/engine/egress lifetimes rather than adding standalone totals.

```bash
cargo build --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --release --features auctioneer
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime,auctioneer --test html_eof_runtime auctioneer_ -- --nocapture
./scripts/async-html-finalization-local-test.sh fastly --release --auction --auctioneer
```

Expected: exact discovery-derived membership and sent coverage, equal shared outcomes for equivalent inputs, and an explicit combined-artifact resource/serving result. Native replay is supplementary; it is not actual guest execution. Attach the host evidence to #1512 and coordinate Auctioneer #91's independent equivalence result. Standalone harness work remains upstream-owned if its artifact is not yet available; #1512 is not accepted until that comparison is recorded. No live bidder authorization or publisher pilot is claimed.

## Task 10 Complete CI operator guidance and PR handoff

**Files:** `.github/workflows/{test,format}.yml`, example config, auction guides, and `CHANGELOG.md`.

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
cargo clippy -p trusted-server-cli --all-targets --all-features -- -D warnings
cargo build --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 --release --features auctioneer
```

```bash
(cd crates/trusted-server-js/lib && npm ci && npm run build && npx vitest run && npm run format)
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
```

Task 0 retires `cargo clippy-codegen` and the old codegen package test together with the obsolete package/jobs; the commands above target surviving compatibility owners. This changes the reviewed future runtime migration, not the current planning PR's CI. Refresh any new feature-on CI/tooling commands introduced by Tasks 0–10 before implementation handoff. Record Fastly resource/serving and upstream acceptance separately from compile/test success. Preserve all actual GitHub-required checks and review/merge gates.

## Review log

- Round 1 (Codex plan-document reviewer, 2026-11-22): approved the full plan against the spec and inspected source boundaries, with no blocking gaps. Clarified that the controlled EOF artifact's digest and `test-utils` features are recorded separately from the plain release build; observations must identify the binary actually executed.
- Round 2 (Codex plan-document reviewer, 2026-11-22): approved the user-directed Task 0 prerequisite and its source/compatibility/retirement checks, including unconditional raw types and the later optional engine feature. Found no blocking mismatch or validation-order gap; a separate spec reviewer also approved the revised design.
