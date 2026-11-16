# Identity conformance implementation plan

**Goal:** Deliver T5's reusable EC lifecycle contract and document the integrations needed for complete #1586 acceptance.

**Architecture:** Shared test scenarios run through thin native and WASM application runners. Deterministic platform fixtures prove storage/cookie behavior; separate runtime tests prove executable wiring and identify any remaining ingress qualification gap.

**Tech stack:** Rust test targets, existing EdgeZero application lifecycle, Viceroy, the native Axum server, existing integration-test helpers and GitHub Actions.

**Execution status:** Suite implementation authorized on 2026-11-16, on `feat/1586-identity-conformance` based at `cc811fc7aa0d234e447a868af5b8ab4ea359326a`. Read the [specification](../specs/2026-11-16-identity-conformance-design.md) first. Tasks 1–4 cover this delivery; dependency-bound Tasks 5–6 remain outstanding. Implementation workers must follow the repository's incremental development and verification workflow.

## Base and execution gates

- T5 is blocked by #1507. If PR #1506 is still open, stack code on its approved `spec/edgezero-pr275-migration` head; otherwise branch from main containing the cutover. Record the exact SHA and inspect its instructions and CI before starting.
- The inspected source baseline is main `1810c763a`, with a read-only check of #1506 at `cc811fc7aa0d234e447a868af5b8ab4ea359326a`. API names below are design responsibilities, not claims that future shared-app constructors already exist.
- #1600 owns shared identity application extraction and removal of direct adapter identity calls. #1574 owns Axum activation. Do not implement either to make T5 green.
- Plan Tasks 1–4 establish suite delivery. Tasks 5–6 establish later integration/qualification; leave their results outstanding until their prerequisites exist. No infrastructure-only PR should auto-close #1586.
- A missing fixture path or unsupported runtime is a qualification gap, not permission to bypass production gates or count a skipped scenario as passed.

## File map

Paths marked proposed are created only during authorized implementation. Reconfirm their placement after choosing the cutover base.

| Path                                                                                              | Responsibility                                                                                    |
| ------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/test_support/identity_conformance.rs` (proposed)                  | Portable scenario inputs, observations and assertions, behind existing test support configuration |
| `crates/trusted-server-core/src/test_support.rs`                                                  | Export test support only where its existing feature contract allows                               |
| `crates/trusted-server-core/src/ec/kv_backend.rs` test support and existing `kv.rs` test wrappers | Reuse record-store semantics; extend fixture recording/fault injection only if necessary          |
| `crates/trusted-server-adapter-fastly/src/main.rs`                                                | Exact production lifecycle helper and minimal test-only store injection if Task 1 requires it     |
| `crates/trusted-server-adapter-fastly/src/app.rs` test module                                     | WASM adapter application runner, with complete lifecycle rather than router-only assertions       |
| `crates/trusted-server-adapter-axum/src/app.rs` test module                                       | Native application runner when #1574 supplies identity                                            |
| `crates/trusted-server-integration-tests/tests/identity_conformance.rs` (proposed)                | Host orchestration and adapter result reporting                                                   |
| `crates/trusted-server-integration-tests/tests/common/identity_conformance.rs` (proposed)         | Runtime lifecycle scenarios using existing clients and process helpers                            |
| `crates/trusted-server-integration-tests/Cargo.toml`                                              | Runtime test target registration only if required                                                 |
| `.github/workflows/test.yml`, `.github/workflows/integration-tests.yml`                           | Explicit PR execution and runtime artifact prerequisites                                          |
| `scripts/lint-identity-boundaries.mjs` (proposed) and adjacent `.test.mjs`                        | SH-5 production-call check and positive/negative fixtures                                         |
| `crates/trusted-server-integration-tests/README.md`                                               | Commands, evidence levels, prerequisite and outstanding integration documentation                 |

Reuse existing helpers before adding these files. The architecture check and runtime layers can increase the initial 6–12-file estimate; record the final inventory rather than treating the estimate as a constraint.

## Task 1: Establish the actual fixture and lifecycle contract

**Inspect:** Fastly `src/app.rs`, `src/main.rs`, core `src/ec/{mod,finalize,kv_backend}.rs`, selected-base application constructors, integration `tests/common/ec.rs`, `tests/environments/fastly.rs`, `tests/html_eof_runtime/`, and the resolved EdgeZero/Viceroy APIs.

- [x] Record the chosen base, current tool pins and supported identity configuration. Confirm which components build state and which execute response finalization.
- [x] Locate or add a narrowly scoped test-only way to supply the app's platform services: store, geo, client info and typed browser metadata. Prefer existing injection points; never add an externally selectable bypass.
- [x] Write the smallest application-path acceptance probe: empty store, GDPR geo, serialized TCF Purpose-1 grant parsed from the actual request, successful origin response and trusted browser metadata. Require a new cookie and matching newly saved record. Do not invoke identity helpers manually from the runner.
- [x] Remove the trusted metadata and separately deny consent; show that the controls reach the intended gates. On the inspected baseline both prevent creation; after T12/#1592, absent JA4 must follow ID-7/DEC-18 and must not itself prevent creation. In this GDPR fixture, Purpose-1 denial or missing TCF consent always prevents fresh issuance. Preserve the existing non-regulated policy that permits creation without consent signals. Record operation counts and final response collection ordering.
- [x] Reconfirm the Viceroy constraint at execution: installed 0.21.0 `downstream_tls_ja4` returns absent metadata, so the unmodified production artifact cannot mint on local HTTP. Do not assume a UA header creates JA4. Also reassess after T12/#1592 permits absent JA4 under ID-7/DEC-18: that change can unblock local issuance without supplying real JA4, while real metadata extraction still requires separate qualification. Run a permitted local runtime probe during implementation if a documented mechanism exists.
- [x] Decide the evidence boundary from the probe. If the application cannot reach C1 through an existing or minimal test seam, stop detailed runner implementation and revise the design with the user. If only the production runtime ingress is unavailable, continue deterministic tests with that named qualification gap.

Expected result: a demonstrated C1 path with meaningful controls, plus a recorded blocked conclusion for production-artifact fresh issuance under Viceroy 0.21.0 unless a newly verified capability changes that fact. Merely seeing `is_real_browser = true` is insufficient to pass this task.

## Task 2: Build the shared scenario contract and fixture observations

**Files:** proposed core test-support module; existing core test-support exports/store fakes.

- [x] Define portable scenario descriptions for C1–C7 and observations for all response cookies, normalized record identity, final store contents and ordered store/response events. Keep native HTTP clients and process code out of portable support.
- [x] Add failing fixture tests demonstrating that an always-present fake record and a rejected write cannot satisfy C1. Inspect existing store tests before implementing another fake. Current backend fakes are private `cfg(test)` items: if reused downstream, minimally expose the necessary fixture under `cfg(any(test, feature = "test-utils"))` through test support, not as an unconditional production API.
- [x] Reuse or minimally extend the store fake to record successful and rejected creation, support generation/conditional-write semantics and inject the intended write failure without corrupting unrelated requests.
- [x] Implement shared assertions: C1 matches cookie to authoritative new row; C2 requires the injected creation error to have occurred; C3 rejects deletion/rotation; C5 checks both Purpose-1 denial and absent TCF consent in GDPR geo; C6 rejects creation on assets/APIs; C7 replays the issued identity.
- [x] Ensure assertions do not require all adapters to have identical incidental headers or timestamps. Preserve production cookie attribute assertions already covered by existing tests.
- [x] Verify on both the native target and `wasm32-wasip1`; keep the module under existing test-support features and add no production dependency.

Commands from repository root, after confirming the selected base's aliases:

```bash
cargo test -p trusted-server-core --lib identity_conformance
cargo test -p trusted-server-core --target wasm32-wasip1 --lib identity_conformance
cargo fmt --all -- --check
```

Expected: the intended tests execute (nonzero test count) and pass in both environments; a missing runner or compilation failure is not behavioral evidence.

## Task 3: Run Fastly scenarios through the application lifecycle

**Files:** Fastly app tests and the lifecycle test entry point identified in Task 1; portable support.

- [x] Add C1 first and demonstrate a behavioral failure if response finalization is disconnected. Use a successful origin fixture so a fallback proxy error cannot mask the intended path.
- [x] Connect the runner to the real entry-point lifecycle or the exact production lifecycle callable used by that entry point, with test-only service injection. If that callable must be exposed, keep the change minimal and verify production still invokes it.
- [x] Add C2 with an explicit failed-write observation and response boundary assertion. Release no response before the creation outcome is known.
- [x] Add C3 for absent geo and geo errors, C5, C6 and C7. Check raw cookies as well as client replay behavior.
- [x] Keep existing seeded integration scenarios and withdrawal tests. Report missing C4/shared-app behavior as an outstanding prerequisite rather than adding a second Fastly startup policy.
- [x] Run target-matched verification:

```bash
cargo test-fastly identity_conformance
cargo test-fastly
cargo test-fastly-reuse
cargo clippy-fastly
```

Expected: selected Fastly scenarios execute and pass. Full acceptance is still pending where #1600 or other integration work is required. Fix ownership of exposed runtime defects before changing production behavior.

## Task 4: Add explicit CI execution and suite-delivery documentation

**Files:** workflow files, `scripts/test-identity-conformance.mjs` and its tests, and the integration README. The existing native core and WASM Cargo test targets are sufficient; use a small launcher rather than add a host target that recompiles unrelated adapters.

- [x] Add host orchestration that lists required scenarios for each explicitly selected adapter and fails if an expected runner or scenario is missing. Distinguish startup-rejection checks from supported-adapter lifecycle tests.
- [x] Add a PR job invoking Fastly conformance under Viceroy and the portable core fixture tests on the native target (not a positive Axum identity claim before Task 5), using the repository's tool setup and Node pin before cargo. Never label a runner with zero positive cases as conformance.
- [x] Document the exact invocation, required artifacts, supported adapter list and outstanding issue dependencies. Early suite delivery selects Fastly explicitly; this is not an assertion that Axum is supported.
- [ ] Include the runtime commands added in Task 5 in the runtime workflow only when they execute the advertised scenarios. An ignored test must have an explicit invocation with failures propagated.
- [x] Verify deliberately selecting an unavailable Axum lifecycle runner fails with an actionable message. CI must not convert it to a pass or silently omit it from a claimed MVP matrix.
- [ ] Run the available conformance targets and required checks before a suite-only handoff. Report remaining #1586 criteria without closing the issue.

Suite launcher command:

```bash
node scripts/test-identity-conformance.mjs fastly
```

Expected: assertions cover the advertised selected adapters and report the exact scenario set; qualification gaps remain explicit in the handoff.

## Task 5: Integrate Axum, startup rejection and actual runtime requests

**Prerequisites:** #1600 and #1574 for Axum positive behavior; shared startup/store capability validation for C4. These are follow-on integration obligations, not T5 production implementation tasks.

**Files:** Axum tests, runtime target/helpers, integration workflow and README.

- [ ] Run the unchanged C1–C7 contract on Axum through the shared app. Add C4 through the real startup initializer with valid configuration except the deliberately unsupported capability; prove the port never becomes available. Add an identity-disabled startup control.
- [ ] Run C4 for Cloudflare/Spin while unsupported, using their real initialization boundaries; do not pretend the host wrapper is the deployed runtime. When support lands, replace that unsupported expectation with the full positive contract for its valid capability configuration.
- [ ] Launch current native Axum and Fastly WASM artifacts with isolated config/store/origin resources, bounded startup/request deadlines and reliable cleanup after failure. Reuse the managed-process helpers instead of inventing another lifecycle framework.
- [ ] Send real HTTP navigation and replay requests, inspect cookies and records, and capture the build revision and artifact path. On Axum use the trusted proxy configuration for geo/client metadata; verify forged metadata from an untrusted peer cannot activate identity.
- [ ] Execute fresh creation on the production artifact only where Task 1 established supported runtime metadata. If an alternate fixture binary is needed, name that evidence level explicitly and keep production ingress qualification outstanding.
- [ ] Implement local runtime scenarios under the `identity_runtime_` test prefix in the new host target, explicitly invoked by CI with `--ignored --test-threads=1` if they require prebuilt artifacts. The command below runs both required runtime runners; absence of either must fail the selected MVP invocation.

```bash
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo build --package trusted-server-adapter-axum
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test identity_conformance identity_runtime_ -- --ignored --test-threads=1
cargo test-axum
cargo clippy-axum
```

The implementation must document the configuration/artifact environment selected by these tests using the existing integration harness conventions. These commands describe follow-on runtime targets; they are not implemented or exercised by the suite-only delivery. Local runtime success does not certify a deployed provider or real TLS ingress.

## Task 6: Enforce the architecture and qualify the claims

**Prerequisite:** #1600 removes existing production adapter-owned identity calls before the clean architecture check can become blocking.

**Files:** boundary lint script and tests; workflow; README.

- [ ] Inventory production EC generation/finalization and handler call sites on the selected base, including aliases. Keep this separate from behavior assertions.
- [ ] Add regression fixtures for direct calls, renamed imports, qualified calls, test-only modules, external test files and unrelated similarly named methods. Choose a narrow parser or existing tooling; do not add a general Rust parser dependency without justification.
- [ ] Run `node --test scripts/lint-identity-boundaries.test.mjs` and `node scripts/lint-identity-boundaries.mjs`. After #1600, require no production violations in all adapter crates. Before it, report violations; do not make a permanent allowlist or return success for a failed requirement.
- [ ] In a disposable copy, run the five behavioral broken controls listed in the spec, with a clean baseline and restored result. Retain the intended assertion failure, rather than counting compile errors as detections.
- [ ] Run the full applicable `AGENTS.md` CI gate list, including target-matched lints, adapter suites, parity, JS/docs formatting, domain lint and the native build-digest checks. The scenario commands supplement those gates. Apply conditional EOF/runtime gates if changes reach those surfaces.
- [ ] Prepare a matrix separating application conformance, local runtime integration and production qualification for each adapter. Record revisions and tools, failures, skips, unexecuted paths and remaining dependencies.
- [ ] Prepare, but do not execute without authorization, production ingress qualification: deploy the exact tested artifact to an approved isolated service, navigate with a fresh real browser, inspect the matching provider record, replay the cookie and clean up owned fixture data. Never use a deployed fault injection that can affect unrelated users.

Expected: a truthful suite-delivery or integrated-conformance conclusion. Production qualification remains pending until actually executed; #1586's startup and SH-5 criteria remain pending until their real integration passes. Publication and deployment require separate authorization; this implementation does not publish changes.

## Planning validation

Validate Markdown formatting and relative links for the two planning documents. Do not run Rust builds or claim behavior verification merely to review documentation. Reconcile independent reviews against the entire final spec and plan before handing them back to the user.

## Review log

- Round 1 (Codex): no blocking plan findings. Added the Fastly entry-point file to the map, clarified gated reuse of private store fakes, separated portable native tests from Axum activation, and scoped missing-JA4 rejection to the current baseline.

- Round 2 (Codex, independent session): no blocking findings in the full plan. Confirmed target/feature wiring, scenario coverage and dependency boundaries. Corrected the inspection path from nonexistent `ec/context.rs` to `ec/mod.rs`. Review was source-only; commands and runtime scenarios remain unexecuted.

## Suite delivery progress

Tasks 1–3 and Task 4's selector, CI and documentation wiring are implemented.
The shared suite currently executes nine Fastly scenarios. Eight native tests
check the assertion/store contract, including rejected tombstones, late writes,
late failures and expired returning cookies; a portable store test also executes
under WASM. Five launcher tests reject unavailable adapters and vacuous success.

An isolated copy passed conformance, failed at the missing-cookie assertion when
the shared production finalization callback was disconnected, and passed again
after restoration. No production browser or store-provider qualification was
performed. The application's ordinary and reusable Fastly test suites and target
clippy are checked locally; the full PR gate list remains required before handoff.

The supplemental native core all-targets/test-utils clippy invocation reports
`result_large_err` in the existing `run_pre_dispatch_hook` helper. Restoring core
sources to the unchanged cutover base in an isolated copy reproduces that lint.
It is not suppressed or changed by this suite. Tasks 5–6, including startup,
Axum activation, SH-5 enforcement and production qualification, remain pending.
