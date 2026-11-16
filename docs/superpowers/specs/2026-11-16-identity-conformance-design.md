# Identity conformance across adapters

Status: approved for suite implementation on 2026-11-16. Tasks 1–4 are in progress; later integration and production qualification remain outstanding.

Issue: [#1586](https://github.com/IABTechLab/trusted-server/issues/1586) (T5).
Implementation plan: [identity conformance](../plans/2026-11-16-identity-conformance.md).
Program requirements: ID-1 and SH-5 in the [server-side identity design](https://github.com/IABTechLab/trusted-server/blob/c7dc9de77d31692412aec2768ae3f11fe0d52bbb/docs/superpowers/specs/2026-11-11-server-side-identity-all-adapters-design.md).

## Problem and outcome

An Edge Cookie (`ts-ec`) identifies a reader's server-side record. A new cookie must only leave the application after its matching record has been persisted. An unknown location must not erase a returning reader's identity. Every adapter must either support the identity contract or reject identity-enabled startup explicitly.

Core already implements parts of these rules. The missing assurance is that each adapter actually runs the complete lifecycle. Tests of a helper cannot detect an adapter that forgets finalization, and equality between two empty responses cannot prove successful identity creation.

T5 supplies reusable behavioral scenarios, adapter runners, failure fixtures, architecture checks and CI execution. It does not implement the shared identity application or add production identity to Axum.

## Source baseline and feasibility

Inspected on 2026-11-16: main `1810c763add220dd34fd5faaffe2c69aab1d556f` and cutover PR #1506 at `cc811fc7aa0d234e447a868af5b8ab4ea359326a`. These are source observations, not runtime qualification.

| Source                                                                                     | Established fact                                                                            | Design consequence                                                                                                          |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/ec/finalize.rs`                                            | New cookies require an authoritative record; unknown jurisdiction has preservation coverage | Exercise these rules through application entry points; retain existing tests                                                |
| `crates/trusted-server-adapter-fastly/src/app.rs` and `src/main.rs`                        | Fastly owns EC dispatch and response finalization today                                     | Router-only assertions are insufficient; the runner must execute response finalization too                                  |
| `crates/trusted-server-adapter-axum/src/app.rs`                                            | Axum does not yet mount the full identity lifecycle                                         | Axum's positive conformance result depends on #1574                                                                         |
| `crates/trusted-server-integration-tests/tests/parity.rs`                                  | Host adapter parity excludes Fastly                                                         | Share scenarios across host and WASM runners, not one host executable that imports Fastly                                   |
| `crates/trusted-server-integration-tests/tests/integration.rs`, `test_ec_lifecycle_fastly` | Existing runtime scenarios seed a record because local HTTP lacks browser fingerprints      | Seeded tests remain useful but cannot certify fresh creation                                                                |
| Fastly `browser_device_signals_from_extension_reach_ec_finalize_state`                     | Typed device metadata can reach the app's browser gate                                      | Controlled application fixtures are feasible; this does not establish metadata arrival through a production network ingress |

Before implementing runners, repeat these checks against the selected cutover revision and prove a supported fixture can reach fresh creation. The locally installed Viceroy 0.21.0 source (`viceroy-lib`, `src/wiggle_abi/http_downstream.rs`, `downstream_tls_ja4`) explicitly returns absent metadata. Its device-detection configuration does not supply JA4. The current browser gate requires a nonempty parsed JA4 section and recognized UA platform (`core/src/ec/device.rs`); H2 metadata is not an additional minting requirement. Therefore local HTTP to the unmodified Fastly artifact cannot prove fresh creation on this runtime. Retain that qualification gap rather than weakening browser checks.

## Delivery boundaries and stacking

PR #1506 implements blocker #1507 and was open when inspected. Base the code PR on its branch, `spec/edgezero-pr275-migration`, if it remains unmerged and that head is still the approved cutover base. Otherwise use main after the cutover lands. Record and revalidate the exact base SHA before coding. Do not stack on the superseded Axum-specific #1536, #1537 or #1566 wiring.

The planning documents can be reviewed independently of that code stack. No PR creation, push or deployment is part of this planning task.

The program deliberately schedules T5 before #1600 (T21, shared identity app) and #1574 (A2, Axum activation). Consequently:

1. **Suite delivery:** T5 defines the contract and runs applicable Fastly scenarios. Explicitly report missing startup, shared-app or Axum capabilities as outstanding integration obligations. Existing defects exposed by these tests are not silently fixed inside T5.
2. **MVP integration:** #1600 removes direct identity calls from adapter orchestration; #1574 enables Axum and runs the same full scenario set. This is required before claiming Fastly/Axum conformance and closing the corresponding acceptance obligations.
3. **Later platforms:** #1588 and the Spin rollout supply their platform support and execute the same contract. Until then, identity-enabled startup must reject unsupported capabilities once the common startup validation exists.

T5 must not be described as fulfilling all of #1586's acceptance criteria merely because its test infrastructure merged. No ignored, empty or silently omitted scenario counts as a pass. Track remaining acceptance in the PR description and linked issues; do not auto-close #1586 on infrastructure delivery alone.

## Design

### Shared scenarios and observations

Define adapter-neutral inputs and expected outcomes once in test support. Each scenario gets an isolated store namespace, settings, client identity and origin. A runner supplies startup, request execution, response collection, store inspection and teardown. Share behavior and assertions; keep runtime startup and transport specific to the adapter.

Observe all `Set-Cookie` headers separately, normalized EC keys, persisted record contents and a fixture operation log. The log distinguishes a successful durable write from an attempted or rejected write. The response returned to the scenario must include the application's real response-finalization stage. Never have a test call EC generation or finalization manually to repair missing application wiring.

Use existing EC clients, minimal origins, process cleanup helpers and settings constructors where suitable. Avoid a new general-purpose test framework or production dependency. WASM tests and native tests may compile the same portable scenario module separately; do not require native-only test crates in the Fastly target.

### Acceptance scenarios

| ID                     | Input and setup                                                                                                                                     | Required observations                                                                                                                                                               |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1 Fresh creation      | Empty store; no EC cookie; serialized TCF Purpose-1 grant parsed by the application; GDPR geo; trusted browser metadata; successful HTML navigation | Successful publisher document response; exactly one non-deletion `ts-ec`; its normalized key identifies the new authoritative record; persistence completes before response release |
| C2 Write failure       | Same reachable input as C1; fail the record-creation write                                                                                          | Fault log proves creation was attempted and rejected; publisher page remains available; no non-deletion `ts-ec`; no newly persisted identity                                        |
| C3 Unknown geo         | Valid existing EC and record; no withdrawal; geo absent or lookup error                                                                             | Existing cookie value remains the client's identity; no expiry or replacement; no new identity record                                                                               |
| C4 Unsupported startup | Identity explicitly enabled; record contract or required platform capability unsupported                                                            | Startup fails with an actionable capability error before serving requests; a generic configuration parse failure does not satisfy the assertion                                     |
| C5 No consent          | Fresh C1 input in GDPR geo with serialized Purpose-1 denial or missing TCF consent                                                                  | No identity created and no new EC cookie; consent fixture is checked against the actual parser                                                                                      |
| C6 Ineligible response | Asset/API request using otherwise eligible input                                                                                                    | No new identity cookie or record; retain existing withdrawal behavior in its own tests                                                                                              |
| C7 Returning reader    | Replay C1 cookie with known geo and existing record                                                                                                 | Same identity remains usable on the next request; no second identity minted                                                                                                         |

C1/C2/C5 use an explicitly GDPR-regulated location and consent encoded in the request representation consumed by the real parser, never a hand-built `ConsentContext`. Verify both denied and missing consent variants in C5. Absence of consent in a non-regulated jurisdiction currently permits creation; preserve that policy and its existing tests.

C3 checks raw response headers and a replaying cookie jar: an omitted response cookie is acceptable; deletion, rotation or replacement is not. Check both unknown-geo forms. C2 and C5 cannot pass by taking the non-browser fallback: they inherit C1's successful ingress setup and assert the relevant gate or failed operation was reached.

Identity startup validation must distinguish identity enablement from ordinary EC cryptographic settings used by consent-only adapters. Reuse the configuration contract delivered by the shared-app/store work; do not invent a second enablement key in T5. A disabled identity startup control must succeed with otherwise valid settings.

The EC route is delivered by #1598. When it becomes available, apply C1/C2/C5 to its supported POST flow and test its separate GET semantics there. Do not introduce the route in this PR or claim it was exercised before that integration lands.

### Fixtures and production boundaries

The browser gate observations above describe the inspected baseline only. Program ID-7/DEC-18 later permits absent or unrecognized trusted JA4 and rejects a known family contradiction. T5 must follow the active production policy rather than freeze the old absence rejection: keep the current missing-metadata probe as baseline evidence, and update its expectation when #1592 delivers T12. C1–C7 do not require an absent-JA4 rejection. Reassess whether ordinary Viceroy HTTP can exercise fresh creation after that change; inability to expose JA4 remains a limit on testing real fingerprint extraction, not necessarily on minting.

Provide a deterministic store with recording and fault injection at the platform-service boundary used by the application. Preserve conditional-write semantics, generation markers and authoritative reads needed by the EC contract. A fake that always returns a record cannot test creation or write failure. Existing EC backend fakes are private under `cfg(test)` on the inspected baseline. If downstream runners need them, expose only the necessary fixture under `cfg(any(test, feature = "test-utils"))` through test support, preserving private production internals. Reuse existing core store fakes where their semantics match; use real provider tests to cover provider-specific durability and consistency.

Inject location and device metadata as typed platform services or request extensions in a test-only runner. No public request header, production environment override or relaxed browser gate may activate these fixtures. Do not prepopulate C1's record, construct its EC context manually or bypass consent parsing. Check the supported metadata contract again after #1506 and #1600, since their APIs are moving.

### End-to-end evidence

Use complementary levels; report their results separately:

1. **Application conformance:** controlled platform inputs pass through the real router, identity lifecycle and finalization, with response and record assertions. This is mandatory and deterministic, including C2 failure injection.
2. **Local runtime integration:** real HTTP to current Fastly WASM under Viceroy and the native Axum executable, a minimal publisher origin and isolated stores. Assert document response, cookie persistence/replay and matching records wherever the runtime exposes the required trusted metadata. A fixture-specific binary may prove runtime lifecycle mechanics, but must be labeled as such and cannot certify the unmodified production ingress.
3. **Production ingress qualification:** fresh browser navigation to the unmodified deployed artifact, using trusted platform metadata and a supported record provider, followed by independent record inspection and a returning navigation. Required before declaring production ingress fully qualified. Deployment and credential use require separate authorization. Storage fault guarantees remain primarily established by controlled application tests, not destructive failure injection into a live service.

For Fastly, local positive creation through the unmodified artifact is blocked by the inspected Viceroy 0.21.0 JA4 hostcall implementation. Task 1 in the plan must determine whether the pinned simulator supports the required metadata. If not, application conformance plus seeded runtime tests may ship with an explicit gap, but cannot be reported as complete runtime or production qualification. For Axum, trusted proxy metadata must be supplied through the configured trusted-peer path, with a forged untrusted-peer negative control.

These tests cover the EC contract, not the entire provider, SharedID, mirror-cookie or CMP-script program. Those features and their browser tests retain their own tickets.

Application conformance ends when the finalized response is delivered. The Fastly runner does not run post-delivery work (queued telemetry and EC pull sync), and pull sync opens its own store through `require_identity_graph` rather than the recording fixture. The release-ordering assertions therefore cover writes made during finalization only. A post-delivery write or deletion that changes a returning reader's record is outside this suite; pull sync's partner writes keep their own tests.

### Architecture and CI

Keep a distinct SH-5 check for production adapter source calling EC generation, finalization or identity handlers directly. Implement a narrow syntax-aware check using existing repository tooling, or inspect imports and aliases as well as calls in a scoped script with regression fixtures. Exclude test modules and test targets without excluding production modules. Demonstrate it rejects both a direct call and an imported alias. No broad search for a single function spelling counts as proof of this requirement.

Before #1600, report existing violations explicitly and leave the requirement outstanding. After #1600, the check becomes blocking for all adapter crates, including unsupported adapters. Do not establish a permanent exception list for legacy calls.

Run the deterministic conformance jobs on pull requests. Run required runtime scenarios explicitly in `.github/workflows/integration-tests.yml`; an ignored Rust test needs an explicit CI invocation and result. Require the selected supported adapter's full applicable scenario set, not a runner that returns success for zero scenarios. Publish a concise adapter/scenario/evidence-level summary, with unexecuted obligations visible.

## Compatibility, privacy and exclusions

T5 adds no operator configuration and changes no cookie or consent semantics. Use fictional identifiers and example domains; never log identity values outside isolated fixture diagnostics. Keep production secrets out of fixtures. Do not add provider implementations, new platform capabilities, an Axum store, application extraction, provider fan-out, rate limits or deployment infrastructure here.

Preserve existing EC unit tests, seeded Fastly scenarios, withdrawal/tombstone behavior, cookie attributes and asset cache behavior. If tests expose a production defect, record it and resolve its ownership before extending this PR.

## Verification and completion

Demonstrate the suite rejects plausible broken behavior in an isolated disposable copy: suppress record persistence while issuing a cookie (C1), issue a cookie after the injected write failure (C2), expire or rotate on unknown geo (C3), accept unsupported startup (C4), and drop application finalization (C1). Setup or compilation failures are inconclusive, not successful negative controls.

The implementation runs target-matched checks plus the complete applicable gate list in `AGENTS.md` before handoff. Record exact revisions, tool versions, feature sets and which fixture or production artifact each runtime used. A suite-only delivery and complete issue acceptance are separate conclusions. This document is not runtime evidence.

Expected implementation size: medium, approximately 6–12 files and 500–1,000 lines for suite infrastructure if prerequisite test seams are available. Full runtime qualification or substantial missing seams may exceed that estimate and require a scope reassessment.

## Review log

- Round 1 (Codex): found that the no-consent scenario could incorrectly reject valid non-regulated behavior. Pinned creation/failure/consent fixtures to GDPR with parsed TCF Purpose-1 grant, denial and absent-signal variants. Clarified the present JA4 presence check and future ID-7/DEC-18 behavior.

- Round 2 (Codex, independent session): no blocking findings after checking the full revised specification and plan against source. Confirmed GDPR fixture reach, finalization/persistence assertions, future absent-JA4 policy, and explicit integration/qualification obligations. Review was source-only; no runtime behavior was verified.
