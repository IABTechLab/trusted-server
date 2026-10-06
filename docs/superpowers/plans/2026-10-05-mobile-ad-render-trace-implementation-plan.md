# Mobile Ad Rendering Trace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development or @superpowers:executing-plans to implement this plan task by task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the approved mobile trace journey from deliberate activation through real publisher reproduction to a bounded, redacted, browser-local report.

**Architecture:** Core owns early authenticated trace responses, request facts, and live auction evidence. The existing GPT recorder supplies browser observations and exact binding sidecars; a trace-owned browser projection supplies the same model to the mobile viewer and every export. Implement the four boundaries in spec section 17 as phases within this single plan, with optional network enrichment outside the v1 release gate.

**Tech Stack:** Rust 1.95.0/edition 2024, EdgeZero, Fastly Compute, Axum, Cloudflare Workers, Spin, TypeScript, the existing Vite/esbuild pipeline, Vitest, Prebid 10.26.0, Playwright, and native DOM/Web APIs.

**Runtime-boundary amendment:** This plan tracks the approved v1 amendment in
spec sections 8/9.2/16. Classify application-visible paths; inspect frozen
runtime-visible cookies with explicit ambiguity suppression; record runtime
rejection and adapter conversion failure separately. Original-target recovery
and original-wire reconstruction are not v1 prerequisites. Existing auth,
same-origin actions and response privacy remain required. Keep this one plan.

---

## Source and execution boundary

- Approved source: [Mobile ad-rendering trace design](../specs/2026-09-01-mobile-ad-render-trace-endpoint-design.md), especially sections 5, 8, 9, 12–17.
- Approval reference: [PR #1107](https://github.com/IABTechLab/trusted-server/pull/1107).
- Planning baseline: `20f4a0cc0139eac842d1d6ff1410a1af6ee8eba8` on `spec/mobile-ad-render-trace-endpoint`.
- The original spec and runtime-boundary amendment are approved. The user authorized implementation on 2026-10-05 after independent document review.
- Keep this branch and checkout. Do not create a worktree or branch. Implement incrementally against this single plan, recording verification and independent review evidence. Publish only the complete v1 feature after its release gates.
- Existing contributor rules apply to implementation: `error-stack`, `derive_more`, no local imports, no sensitive fixture data, documented public APIs, and sentence-case imperative commit messages.
- Proposed new filenames and function names below are implementation decisions, not claims that those symbols already exist. Locate existing code by symbol; line numbers will move.

## Execution order and dependency map

This is one implementation plan for one approved spec. The user requested a single plan; the four implementation boundaries in spec section 17 remain phases here rather than separate documents or mandatory separate PRs. All mandatory work stays on the existing branch and ships as one complete default-off v1 feature. Intermediate commits are review checkpoints, not independently published setup releases.

| Phase                       | Tasks | Deliverable                                                                                                                                      | Dependency                                                                    |
| --------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------- |
| 1. Route/privacy foundation | F0–F8 | Local authenticated application-visible routes on all four adapters, setup/session lifecycle, cookie/network facts and private publisher context | Reviewed immutable EdgeZero hook/metadata pin and runtime-boundary acceptance |
| 2. Live auction evidence    | E1–E7 | Bounded server records through all transports, API unit tokens and exact SSAT/SPA recorder sidecars                                              | Foundation gates, immutable request facts and private response policy         |
| 3. Browser handoff/viewer   | V1–V7 | Strict combined report, deterministic bounds, same-tab handoff, mobile presentation and equivalent local exports                                 | Foundation plus live collector/public GPT export                              |
| 4. Optional enrichment      | N1–N3 | SDK-verified optional protocol/POP/ASN facts only                                                                                                | Complete v1; not a release gate                                               |

Implementation may progress on pure F1–F3/F5–F6 and E1/V1 contract work while F0 is unavailable, but no adapter-ordering success may be claimed without F0. After each task, run its focused verification; checkpoint the completed phase against the acceptance matrix. Keep full release gates at the final handoff, and run the full repository gates before any earlier external PR handoff required by AGENTS.md.

No trace assets exist at the planning baseline. Build and evolve one unpublished v1 JS/CSS set through phases 1–3, keeping the committed digest/byte fixtures synchronized in each implementation commit. Freeze that complete set at first publication after V7; later changed bytes require a new asset-set URL/digest. Do not create a v2 set merely because the setup and viewer were implemented in different tasks.

## Staff review of implementation choices

Reuse the four adapter entry paths and existing authentication/cache mechanisms. Add focused trace modules rather than splitting the large publisher/orchestrator files or replacing the existing GPT attribution engine. The server projection reads live observations, never telemetry rows. The viewer imports the public GPT export contract, never the private store.

Pinned EdgeZero v0.0.8 (`567964158e4f8bd0d52321b9801de44966422e1b`) selects a method/path before middleware. Its concrete RouterService runner seams therefore require the upstream pre-dispatch hook and a reviewed immutable dependency pin. The EdgeZero feature branch now supplies that API, honest metadata and converter fixes. Verify method preservation for requests the runtime accepts, classify the SDK/runtime-visible pathname, and prove required cookie/control semantics at this boundary. Do not require an original-target SDK accessor or impossible original-wire reconstruction for v1. Fastly paths normalized outside trace retain their existing health/JA4 behavior; paths exposed within trace must be intercepted. Cloudflare's converter must preserve exposed extension tokens even though its tested wire parser rejects them before invocation. A finite method list or Fastly-only bypass does not satisfy application-visible adapter parity.

## Implementation invariants

1. Authenticate every reserved trace request before setup facts, body inspection, or mutation, including disabled routes and malformed paths; preserve first-match-wins handlers.
2. Trace dispatch creates no ordinary event/EC state, invokes no configured filters, performs no auction/EID processing, and makes no publisher or telemetry request.
3. Freeze runtime-visible CookieHealth before `gpt_diagnostics::prepare_request` sanitizes headers; retain no raw values. Base capture requires the flag plus exactly one valid inspectable session, with ambiguity suppressing it; document capture additionally requires the effective diagnostics navigation decision.
4. Preserve console-only diagnostics when trace is off. Query activation deliberately adopts the shared 1800-second lifetime even when trace is disabled.
5. Gate every new token, extension, listener, mapping, sidecar, and handoff action. TSJS requires the literal `window.__tsjs_trace_active === true`; the server independently evaluates each request.
6. Keep the three clocks and three evidence layers separate. Correlation failure never downgrades complete server capture. `/auction` has no GPT sidecars in v1.
7. Strictly allowlist both projection and ingestion. No page path, cookie value, identity, full IP, fingerprint, provider name, price, targeting, creative identity/payload, or internal request ID enters a trace artifact or trace-specific log.
8. Reuse terminal private/no-store protection, including Fastly's final guard after late effects. Only successful unprotected fixed assets can be public immutable.
9. Asset URLs are byte contracts. Freeze the complete v1 set at first publication; any later changed bytes require new URLs/digests while retaining published bytes/digests and exact routes.
10. All trace failures fail open for advertising. No diagnostic failure changes ordinary ad acceptance, ordering, targeting, rendering, or status.

## Acceptance evidence matrix

| Spec criterion                                      | Owning work                                      | Required evidence                                                                                                                                          |
| --------------------------------------------------- | ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1: off by default, ordinary traffic unchanged       | Foundation F1/F4/F7; auction E2/E5/E6; viewer V5 | Config validation, inactive gates, before/after bid fixtures, rollback reload                                                                              |
| 2–3: deliberate mobile setup and reproduction       | Foundation F2–F6                                 | Auth/method/path matrix, same-origin controls, GET read-only, observed-state verification, history/reload instructions                                     |
| 4: real publisher capture                           | Foundation F7; auction E1–E6                     | Traced reload, SSAT/SPA/API transports, unchanged ads                                                                                                      |
| 5: bounded same-tab snapshot                        | Viewer V1–V3/V5                                  | Strict validation, 512 KiB UTF-8 wrapper, deterministic truncation/floor rejection, successful write before navigation                                     |
| 6–7: separated evidence and equivalent export       | Viewer V4/V6/V7                                  | Semantic labels, exact joins, downloaded/copied/shared report equality                                                                                     |
| 8: public-safe fields only                          | All phases                                       | Distinct forbidden-data sentinels across HTML, transport, storage, logs, copy/share/export; hostile stored keys rejected                                   |
| 9: private/no-store                                 | Foundation F4/F7; auction E3/E4; viewer V7       | Hostile operator overrides, final Fastly effects, ESI/shared-template absence                                                                              |
| 10: honest degradation                              | Auction E5–E7; viewer V2–V7                      | Coverage truth table, transport/projection/storage failures, optional API absence                                                                          |
| 11: mobile/accessibility                            | Foundation F6; viewer V6/V7                      | 320 CSS px, 44 px controls, keyboard/focus/aria-live, iOS Safari and Android Chrome                                                                        |
| 12–13: exact schemas, provenance, hostile storage   | Viewer V1/V4/V6/V7                               | Source-member coverage/type gate, all unknown versions/keys rejected, CSP/XSS/depth/DOM-bound checks                                                       |
| 14: independent cleanup and verification            | Foundation F5/F6; viewer V3/V6/V7                | Offline end, failed deletion, mismatch and retry with separate statuses                                                                                    |
| 15: server entry point and honest joins             | Auction E1–E7; viewer V4/V6/V7                   | Zero-bid/failure records, SSAT/SPA exact joins, API independent, no inferred client winner                                                                 |
| 16: explicit runtime boundary and adapter parity    | Foundation F0/F2–F5/F8; viewer V7                | Separate pre-invocation rejection/conversion outcomes, visible path/auth/method tests, complete browser journey on all four adapters with suitable origins |
| 17: conservative Cookie ambiguity and public detail | Foundation F3/F4/F6; viewer V1–V3/V6/V7          | Per-Cookie fidelity-axis fixtures, cap/UTF-8/ambiguity precedence, inactive capture, exact reason/state validation and storage/export equality             |

## Shared verification commands

Run focused red/green tests in each task. A red run must fail for the intended behavior, not because a dependency is missing. Name new Rust regression functions/modules with the task filter shown in its command and assert each focused run actually selects tests; a zero-test run is not verification. Keep tests and the minimum implementation together in each planned commit. Use @superpowers:test-driven-development and @superpowers:verification-before-completion during execution. No implementation or runtime test pass is claimed by this plan.

Before the final implementation/PR handoff, run the complete repository gate list from the root unless a working directory is shown. Earlier internal task/phase checkpoints use focused tests and affected-target checks; if an earlier checkpoint is submitted as its own external PR, run the complete gates at that handoff as AGENTS.md requires:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
npm --prefix crates/trusted-server-js/lib run build
npm --prefix crates/trusted-server-js/lib run test
npm --prefix crates/trusted-server-js/lib run format
npm --prefix docs run format
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
git diff --check
```

Expected: every command exits 0; all suites have zero unexpected failures. Install locked npm dependencies with `npm ci` in the respective package only when missing. Use the pinned Rust/Node/runtime tools. Rust adapter aliases already select the proper targets; bare workspace `cargo test` is incorrect.

For runtime/build changes, also run `cargo build-fastly`, `cargo build-axum`, `cargo build-cloudflare`, and `cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release`. Verify changed public Rust documentation with `cargo doc -p trusted-server-core --no-deps --all-features --target wasm32-wasip1`; do not run the incompatible workspace all-feature command.

Browser changes additionally run `./scripts/integration-tests-browser.sh`, which builds artifacts, generates Viceroy config, builds the Docker fixtures, and executes Next.js and WordPress Playwright suites. It requires Docker, Viceroy, the WASM target, and Playwright Chromium. Manual mobile checks are required release evidence; an unavailable phone or Docker daemon is an explicitly pending check, never a pass.

## Execution checklist

### Execution evidence (2026-10-06)

Entries below record successive checkpoints. Later verification supersedes an
earlier checkpoint's pending status; the final status table records remaining
release work.

- User approved the amended contract and authorized implementation on the
  existing branch.
- F0: EdgeZero independently reviewed and published as PR #403, closing Task
  issue #402 through its PR-creator workflow. Trusted Server pins immutable
  revision `499d5c93d597f01b5e0497e332af82f2aa633277`. All four adapter check
  aliases pass. Independent probes using the resolved consumer versions pass
  47 hook/converter tests and all 54 freshly rebuilt raw-wire cases, with zero
  differences from committed observations. All reported upstream GitHub checks
  pass. Project-board association is pending token project permissions.
  Full F0 acceptance still requires F4 integration and V7 supported-origin
  browser workflows; converter evidence alone does not close that gate.
- F1: Default-off config, dependency validation on both startup paths and shared
  1800-second cookie policy implemented. Red tests demonstrated the missing
  dependency validation and literal missing cookie lifetime before implementation.
  `cargo test-fastly trace_config` selects two passing core tests;
  `cargo test-fastly gpt_diagnostics` selects fourteen passing core tests.
  Independent spec and quality reviews approved after corrections.
  Combined F1/F2 target-matched Fastly clippy passes; endpoint/query sequence
  proof follows F5.
- F2: Classifier, authentication and response preflight implemented. Spec review
  approved. Quality review identified combined separator/dot namespace erasure;
  a behavioral red regression and fix now pass all twelve route tests, with
  independent re-review approved. Twenty-seven existing auth tests pass.
  Successful shell/state/action/asset behavior depends on F3/F5/F6; no complete
  endpoint or runtime acceptance is claimed yet.
- F3: Runtime-visible cookie scanner and frozen request context implemented;
  ten cookie and seven context tests pass, with independent spec and quality
  approvals. Focused clippy, documentation build and formatting pass.
- F5: Deliberate action and frozen state handlers implemented. Fifteen focused
  action tests and forty-six combined trace tests pass. Both independent reviews
  approved; clippy, documentation build and formatting pass. Production callers
  and adapter body parity remain part of F4 integration.
- F6: Session helper, setup controls, strict request-context validator, escaped
  Rust shell, separate versioned assets and build verification are implemented
  and independently approved for their current unit scopes. Eighty-five browser
  foundation/asset tests and two Rust shell tests pass. Five compiled build-script
  probes verify missing/stale assets, stale source, unknown manifest fields and
  valid skipped builds. Complete report viewer and browser workflows remain pending.
- E1 browser contract slice: exact opaque tokens, bounded evidence and sidecar
  validators, exclusive transport types and immutable owned ingestion implemented.
  Independent review caught and corrected type exclusivity and caller-controlled
  array-method gaps; both re-reviews approved. One hundred and three focused tests
  pass; combined browser foundation and evidence tests total one hundred and
  eighty-eight. Scoped strict TypeScript and ESLint checks pass. The Rust contract
  also received independent spec and quality approvals: twenty-six focused native
  and Viceroy tests, nine documentation examples, all six adapter Clippy aliases
  and native core lint pass. Strict map/string decoding and required-fact validation
  before truncating tails are pinned by regressions.
- E2 private live carries received independent spec approval and architectural
  review. Forty-seven focused native and Viceroy tests pass, covering actual
  provider/mediator launch order, empty-plan versus split-dispatch semantics,
  checked tail counts, duplicate accepted instances and telemetry-independent
  cancellation. All four adapter suites, six adapter Clippy gates, formatting and
  committed assets pass; docs compile with the existing thirty-one warnings.
  Response slot-token adoption, final delivery disposition and public transports
  remain E3/E4 work.
- V1 public report contract is implemented and independently approved for its
  pure validation/type scope. Fifty-nine behavioral validation tests and source
  member classification/type tests pass. Validation and byte measurement consume
  the same owned descriptor snapshot; changing getters, serialization hooks,
  extra fields and unsupported schemas cannot enter accepted reports. The scoped
  TypeScript gate exposes only four pre-existing duplicate `w`/`h` declarations
  in `core/types.ts`, reproduced against the planning baseline; an in-memory
  future-source-field probe correctly breaks the exhaustive classification map.
- V2 projection and report builder are implemented, with separate spec and
  quality approvals. Seventeen projection and fifteen report tests pass, including
  checked omission overflow, stable capture-clock observations, dangling sidecar
  pruning, deterministic removal order and protected slot floors. The populated
  512 KiB fixture includes multibyte facts and exact six-counter/survivor checks.
  Collector integration and mobile capture responsiveness remain pending.
- V3 storage and export primitives are implemented and independently approved.
  Seventeen storage and eight export tests pass, covering exact origin/expiry,
  serialized UTF-8 bounds, safe integer capture time, unavailable storage,
  identical public-report formatting and independent deferred download cleanup.
  Actual branch trace tests now total three hundred and six across thirteen
  files. User action wiring, handoff recovery and browser acceptance remain pending.
- F4 integration received independent spec and quality approvals. The actual
  Fastly, Cloudflare and Spin runtime boundary suite passes all three selected
  tests, including each runtime's 198-case flag/auth matrix, normalized-in and
  normalized-out paths, encoded aliases, terminal headers and distinct runtime
  rejection outcomes. Native Axum parity and all six adapter lint gates pass.
  This closes the scoped hook/terminal integration; F0 browser acceptance still
  requires V7. No pre-invocation rejection is credited with an application response.
- The E1 owned-ingress correction, E5 pure collector and V4 pure correlation
  model received independent spec and quality approvals. Public validators now
  snapshot own data before validation, and collector imports do not pull in the
  report/GPT runtime graph. Actual trace tests total 336 across fifteen files.
  Live caller, sidecar and viewer integration remain separate tasks.
- F7 publisher bootstrap received independent spec and quality approvals. Six actual Viceroy
  tests cover the gate matrix, HTML-only injection, safe head ordering, failure
  privacy, ESI exclusion and distinct visitors using a warm template cache. An
  emitted-script Node test proves hostile-string roundtrip and deep freezing of
  all context containers. All four adapter suites and all six adapter lint gates
  pass; documentation retains the same 31 pre-existing warnings.
- E5 direct API wiring and V5 handoff received independent spec and quality
  approvals and are copied into the branch. Direct evidence is consumed before ordinary
  bids; supplied unreadable namespaces record a bounded validation issue. Handoff
  saves one validated report before same-tab navigation, keeps an explicit download
  after storage failure, and stops on destruction from callbacks. The actual branch
  passes 587 tests across thirty trace/asset/core/GPT files after rebuilding and
  refreshing draft source digests. Prebid hooks, live SSAT/SPA sidecars and V7
  browser acceptance remain separate tasks.
- V6 viewer and independent cleanup received independent spec and quality approvals
  and are copied into the branch. Sixty-one focused tests pass, including twelve local-delete
  × end-request × state-observation combinations, offline requests, independent
  retries, local deletion during a pending end request and listener destruction.
  Report facts and setup facts remain separate; exports use the same immutable
  public model. After rebuilding and refreshing draft assets, the actual branch
  passes 658 tests across thirty-seven selected trace/asset/core/GPT files.
  An independently reviewed browser-carried fixture passes the served viewer in
  actual Chromium against each publisher framework: 320 px layout, 44 px controls,
  text-only hostile content, real keyboard Clipboard and Blob download with equal
  report-only JSON, and independent local/server cleanup. This proves viewer
  behavior under its CSP; it does not substitute for live auction capture/handoff,
  physical devices or file-sharing acceptance.
- F8 uses two dedicated trace configurations alongside the unchanged default-off
  browser baseline, with explicit owned runtime cleanup in local and CI runners.
  Two process-cleanup regressions pass. Seven actual Chromium foundation tests
  pass against each of Next.js and WordPress with a freshly rebuilt Fastly artifact:
  explicit session activation/end, authenticated routes/assets, served asset digests,
  default-off behavior, cross-site mutation rejection and history/reload activation.
  Publisher reload exposed origin-304 reuse when the ad stack was disabled. Two
  meaningful failing regressions preceded a fix that strips validators/ranges for
  diagnostics document changes and rejects unexpected origin 304 responses with a
  private 502. Both regressions and eighteen existing cache-policy tests now pass.
  Browser TypeScript checks pass with the existing shared Node type definitions.
  Operator guide additions received scoped spec approval; full report workflows,
  other adapter browser workflows and physical mobile acceptance remain pending.
- E6 browser binding and the related real-snapshot V2 correction received
  independent spec and quality approvals and are copied into the branch. Accepted
  SSAT/SPA batches bind exact slot objects; sidecars emit only for the consumed
  opportunity and its actual GPT cycle. Zero-bid opportunities reuse the validated
  auction token, conflicting markers remain unknown, and oversized delivery lists
  keep server evidence without prefix joins. Optional own-data undefined members
  in the existing GPT snapshot are omitted during projection; other schema checks
  remain strict. Actual bootstrap, bundle, fallback and SPA snapshots pass report
  construction and exact correlation. The branch passes 990 selected tests across
  forty-four files, including committed asset verification. Live Rust transport
  wiring remains E3/E4 work.
- E7 Prebid transport is copied into the branch after independent spec and
  quality approvals. Review exposed sequential publisher bid-ID reuse; the
  corrected private association pairs the original bid ID with the actual SDK
  bidder-request ID. Late old timeout/error hooks cannot consume a newer request;
  ambiguous or missing associations still retain exact readable responses.
  Bounds, expiry and page lifecycle cleanup preserve ordinary bidding. Eleven
  actual registered SDK artifact tests pass; the branch passes 852 selected tests
  across twenty-seven files, including asset verification. The measured external
  Prebid shim is 52,030 characters with a narrowly adjusted 52,500 guard; strict
  source and changed-test checks introduce no diagnostics against the existing
  baseline.
- V7's local bidder fixture reaches the real Rust HTTP client through test-only
  Viceroy aliases derived from the production backend naming policy. Thirteen
  generator regressions pass, including rejection of non-loopback overrides and
  bounded timeout enumeration; helper Clippy passes. The fixture returns selected,
  empty and failed responses, counts actual invocations without retaining bodies,
  and rejects malformed/null controls with fixed 400 responses. A meaningful
  failing HTTP regression preceded that correction. Nine Chromium checks pass
  with the enabled Next.js configuration and eight shared checks pass against
  WordPress. A form-navigation race was corrected by waiting for the actual
  rejected navigation to commit. The live zero-bid journey proves two actual
  bidder calls before reaching its expected missing-transport failure; it is
  still incomplete until E4 delivers the live envelope and final assertions run.
- E3's typed `/auction` transport received independent spec and architecture
  approvals. The raw extension visitor tracks accepted conversion occurrences
  without changing public ad models or acceptance. Review caught oversized
  optional numeric values discarding unrelated valid references; a meaningful
  failing regression preceded borrowed per-unit isolation. Eleven focused cases
  pass on native and Fastly, all four full adapter suites and six adapter lint
  gates pass after the correction, and documentation/format/asset checks pass
  with the same pre-existing documentation warnings. API responses preserve
  ordinary ad content and add private evidence only under the frozen request gate.
  Publisher SSAT/SPA transport remains E4 work.
- V7's all-four browser harness reuses the Rust runtime launchers and owns its
  browser subprocess. Its source received independent review, TypeScript,
  discovery and baseline-skip checks pass, and a real hanging Chromium probe
  proves the owned Node/worker signal cleanup closes the observed detached
  browser processes locally. The normal browser workflows remain unexecuted
  pending fresh artifacts; this is not evidence of full adapter journey success.
- E4's request-scoped SSAT and both SPA transports received independent spec
  and architecture approvals. The document tail reuses the pre-dispatch auction
  identity and exact slot references, projects actual delivered dispositions,
  and keeps templates/ESI free of private transport. Skipped and empty auctions
  remain observable when the tail is reached; an unreached tail invents no
  evidence. Two meaningful failing regressions preceded implementation; six
  focused native and six focused Fastly cases pass. All four adapter suites,
  six adapter lint gates, formatting and asset checks pass. Final lint corrections
  reuse the ordinary inactive wrapper and replace a serializer panic with the
  exclusive unavailable marker and a fixed log message. Fresh production
  artifacts and complete live browser assertions remain pending.
- The integration workflow now gives raw runtime and normal browser trace
  acceptance a dedicated job with explicit Chromium, Wrangler and Spin
  prerequisites. Ordinary integration runs exclude these opt-in families.
  Independent review and six actual Bash 3.2 argument cases verify default,
  positive-filter and negative-skip behavior. The prepared CI job has not run
  remotely; its source is not credited as runtime acceptance evidence.
- Independent full mandatory-phase source/spec and architecture reviews found
  no actionable findings. Seventeen shared Chromium checks pass against the
  reviewed foundation artifact, including real CSP blocking, opener-cloned
  storage separation and simulated expiry, rejected report removal, blocked
  deletion with retained export, and offline cleanup followed by actual server
  retry/observation. These carried-report fixtures are not credited as live
  auction capture or a real browser restart. Three additional reviewed live
  fixtures cover a real failed-provider SSAT response and unchanged core API
  creatives after only the optional trace transport is removed or made invalid;
  their fresh-artifact execution remains pending.
- Final fresh production artifacts build on all four platforms. The complete
  repository browser runner passes 49 Next.js and 28 WordPress checks, with only
  framework-selection and separate-runtime skips. Actual SSAT zero-bid and
  failed-provider evidence, selected creative delivery through the real PUC
  bridge, both core API outcomes, the pinned real Prebid caller, both SPA aliases,
  and unchanged advertising after altered optional trace transport all pass.
  The first live handoff failure exposed a test locator that could not reach the
  intentionally closed diagnostics shadow root. A reviewed Chromium-only helper
  now finds the exact accessible button and sends a real pointer click; production
  shadow behavior and asset bytes are unchanged. The failing case and complete
  suites then pass.
- Fresh normal browser workflows pass separately on Axum, Cloudflare, Fastly and
  Spin: real Secure/HttpOnly cookie storage, separate observed state, publisher
  reload/capture, same-tab handoff, equivalent clipboard/download JSON, independent
  local deletion, server end and subsequent state verification. Auctions are
  deliberately disabled in this cross-runtime fixture; live bidder and creative
  transport proof comes from the dedicated Next.js/Viceroy suite above. Fresh raw
  boundary suites also pass against Cloudflare, Fastly and Spin, keeping runtime
  rejection separate from converted-path/header behavior. These are local runtime
  results, not deployed-platform or physical-device acceptance.
- Remaining repository gates pass: CLI and codegen lint, host CLI tests including
  actual Chrome fixtures, 21 cross-adapter parity cases, integration lint, JS build,
  pinned external Prebid build, 1,733 Vitest tests in 69 files with no type errors,
  JS/docs/Markdown format and browser TypeScript checks. Final current-source full
  adapter suites pass after the last E4 lint corrections: Fastly core 3,011,
  adapter 199, JS crate 4, OpenRTB 21 and doctests 23; Axum 44, Cloudflare 54 and
  Spin 89, with only existing ignored cases. Generator tests pass 13/0; core
  target-matched documentation and format checks pass with the same 31 existing
  documentation warnings. Physical mobile, actual session restoration and staging
  rollout remain pending release checks.
- Existing unpublished v1 draft bytes and source digests are deliberately
  refreshed during implementation. First complete publication remains gated on
  every mandatory phase and final verification.

- [x] Resolve and verify the EdgeZero prerequisite before foundation integration.
- [x] Execute and review foundation tasks F0–F8.
- [x] Execute and review live-evidence tasks E1–E7.
- [x] Execute and review browser implementation V1–V6 and automated V7 journeys.
- [ ] Complete V7 physical-device, actual session restoration and staging acceptance.
- [ ] Record all acceptance evidence, full CI gates, and manual mobile results.
- [ ] Enable only a controlled staging fixture; verify CDN/private responses and operator privacy acceptance.
- [x] Keep enrichment and future schemas separately scheduled.
- [x] Present the resulting implementation diff for independent review before external publishing.

| Scope                      | Current status                                                                          |
| -------------------------- | --------------------------------------------------------------------------------------- |
| Mandatory production code  | Implemented, independently reviewed; complete automated browser journeys pass           |
| Final repository gates     | All required automated gates pass                                                       |
| Physical mobile acceptance | iOS Safari and Android Chrome checks pending; see operator guide release checklist      |
| Real session restoration   | Manual check pending; actual opener cloning and simulated future-load expiry are tested |
| Staging and deployment     | Pending operator rollout/privacy/CDN acceptance; feature remains default off            |
| Optional phase 4           | Unscheduled; unavailable fields remain omitted rather than inferred                     |

## Independent plan review

Reviewed on 2026-10-05 by three independent read-only subagents against the approved spec and current source. Each reviewer assessed the consolidated document without relying on the earlier multi-document reviews; the findings were corrected centrally and the changed tasks were reviewed again.

| Reviewer                | Coverage                                                                                                                                                                             | Final result                     |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------- |
| Spec alignment          | All normative sections, acceptance criteria, activation, exact schemas, privacy, storage, failure behavior, rollout and optional scope                                               | Approved                         |
| Server and all adapters | Rust ownership, upstream API, runtime conversion, Fastly native shortcuts/finalization, authentication, cookies, controls, live outcomes, transports and parity                      | Approved after F0/F4 corrections |
| Browser, UI and build   | Both API callers, registered Prebid hooks, GPT binding, IIFE sharing, validation/truncation, local storage/export, CSP/mobile/accessibility, frozen assets and real fixture plumbing | Approved                         |

Baseline review corrections included early Fastly capture, actual converter/runtime probes, the browser-gated Cloudflare runner, successful empty-plan versus unsuccessful split-dispatch outcomes, throwing diagnostic-callback fail-open tests, dedicated fixture scoping and measured Prebid artifact-size handling. The runtime-boundary amendment supersedes its original-target/original-wire acceptance assumptions. File maps were checked against the checkout and consolidated phase paths.

This historical review approves the baseline plan's alignment and task ownership, not completed implementation or the subsequent amendment. Under the proposed runtime-boundary amendment, F0 still requires a reviewed upstream revision and actual resolved-runtime evidence, but original-path/header recovery is no longer a release gate. Full runtime/CI and supported-origin browser acceptance remain required during execution.

### Independent runtime-boundary amendment review

Two independent read-only subagents reviewed the amended spec and this single
plan together on 2026-10-05. The spec reviewer covered the complete normative
contract, path/auth precedence, runtime failure boundary, cookies and acceptance
criteria. The plan reviewer checked the actual EdgeZero ingress API and task
coverage across adapters, capture gates, lifecycle, schema validation, UI,
storage, export and size bounds.

The review found one status alignment gap: encoded namespace aliases needed an
explicit enabled `400` in the spec to match F2. That was corrected with the
existing authentication and disabled-feature precedence preserved. Document
cleanup added acceptance matrix rows 16–17, corrected the source/execution
boundary and stale raw-wire/first-PR wording, and clarified that unrelated-pair
tolerance applies after aggregate cookie checks. Both reviewers re-read the
corrections and approved with no remaining findings.

This is approval of document/API alignment. The user subsequently accepted the
amendment and authorized implementation on 2026-10-05; no implementation,
immutable dependency pin, runtime/browser acceptance or CI pass is claimed by
this review.

## Phase 1: Foundation

### Inputs, scope, and dependency

Read the shared sections above and approved [spec](../specs/2026-09-01-mobile-ad-render-trace-endpoint-design.md) sections 5.1–5.4, 6.1, 8, 9.1–9.2, 11–14.2, and 17.1. Execute in the current branch. This phase supplies setup/lifecycle behavior; the feature is released only after combined reports and the viewer are complete in phase 3.

Pinned EdgeZero v0.0.8 cannot satisfy method-independent pre-router dispatch. F0 is an actual prerequisite, not a suggested optimization. The EdgeZero feature branch implements the required hook and metadata API; a reviewed immutable upstream revision and resolved-runtime verification are still needed before adapter integration. If that revision is unavailable, finish the pure core/browser tasks and report adapter integration blocked. Original-target SDK recovery is not required. Never substitute ordinary middleware, enumerate only standard HTTP methods, edit the Cargo cache, or claim parity from Fastly alone.

### File map

| Action | Exact path                                                                                                                                                                                                                                                                                                                                                                 | Responsibility                                                                                                                             |
| ------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Modify | `Cargo.toml`, `Cargo.lock`                                                                                                                                                                                                                                                                                                                                                 | Reviewed EdgeZero revision supporting pre-dispatch, trusted origin and conservative request metadata                                       |
| Modify | `crates/trusted-server-core/src/lib.rs`                                                                                                                                                                                                                                                                                                                                    | Export only adapter-facing `trace` entry points                                                                                            |
| Create | `crates/trusted-server-core/src/trace/mod.rs`                                                                                                                                                                                                                                                                                                                              | Base/document gates, frozen runtime-visible cookie inspection, adapter-facing responder and typed terminal trace-response marker           |
| Create | `crates/trusted-server-core/src/trace/types.rs`                                                                                                                                                                                                                                                                                                                            | Versioned public network/context/cookie types; no request structs                                                                          |
| Create | `crates/trusted-server-core/src/trace/cookies.rs`                                                                                                                                                                                                                                                                                                                          | Read-only bounded exact-name scanner                                                                                                       |
| Create | `crates/trusted-server-core/src/trace/context.rs`                                                                                                                                                                                                                                                                                                                          | IP masking and bounded optional network projection                                                                                         |
| Create | `crates/trusted-server-core/src/trace/routes.rs`                                                                                                                                                                                                                                                                                                                           | Reserved-path classifier, POST controls/body validation, response hardening                                                                |
| Create | `crates/trusted-server-core/src/trace/shell.rs`                                                                                                                                                                                                                                                                                                                            | Escaped fixed shell/setup facts, data-URL favicon, external asset references                                                               |
| Modify | `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`                                                                                                                                                                                                                                                                                                           | Default-off option, shared set/clear cookie policy, effective document gate integration                                                    |
| Modify | `crates/trusted-server-core/src/config.rs`                                                                                                                                                                                                                                                                                                                                 | Raw-config validation on deploy/runtime paths                                                                                              |
| Modify | `crates/trusted-server-core/src/auth.rs`                                                                                                                                                                                                                                                                                                                                   | Bounded trace-auth failure logging without changing canonical first-match handler selection                                                |
| Modify | `crates/trusted-server-core/src/publisher.rs`                                                                                                                                                                                                                                                                                                                              | Request-scoped early trace bootstrap, never cached template/ESI data                                                                       |
| Modify | `crates/trusted-server-core/src/html_processor.rs`                                                                                                                                                                                                                                                                                                                         | Place request-scoped trace bootstrap before synchronous TSJS initialization                                                                |
| Modify | `crates/trusted-server-adapter-fastly/src/app.rs`, `crates/trusted-server-adapter-fastly/src/main.rs`                                                                                                                                                                                                                                                                      | Hook registration and native ingress snapshot capture before conversion                                                                    |
| Modify | `crates/trusted-server-adapter-axum/src/app.rs`, `crates/trusted-server-adapter-cloudflare/src/app.rs`, `crates/trusted-server-adapter-spin/src/app.rs`                                                                                                                                                                                                                    | Same hook for production and `routes_with_settings` seams                                                                                  |
| Modify | `crates/trusted-server-adapter-fastly/src/platform.rs`, `crates/trusted-server-adapter-axum/src/platform.rs`, `crates/trusted-server-adapter-cloudflare/src/platform.rs`, `crates/trusted-server-adapter-spin/src/platform.rs`                                                                                                                                             | Read-only existing client/geo sources, trusted scheme/authority adapter mapping                                                            |
| Create | `crates/trusted-server-js/lib/src/trace/types.ts`, `crates/trusted-server-js/lib/src/trace/lifecycle.ts`, `crates/trusted-server-js/lib/src/trace/viewer.ts`, `crates/trusted-server-js/lib/src/trace/viewer.css`                                                                                                                                                          | Setup/state/retry UI and shared lifecycle contracts, no report parser yet                                                                  |
| Create | `crates/trusted-server-js/lib/test/trace/lifecycle.test.ts`, `crates/trusted-server-js/lib/test/trace/setup.test.ts`                                                                                                                                                                                                                                                       | Setup actions and observed-state tests                                                                                                     |
| Modify | `crates/trusted-server-js/lib/build-all.mjs`, `crates/trusted-server-js/build.rs`, `crates/trusted-server-js/src/lib.rs`                                                                                                                                                                                                                                                   | Separate fixed trace build/embedding pipeline                                                                                              |
| Modify | `crates/trusted-server-js/Cargo.toml`                                                                                                                                                                                                                                                                                                                                      | Reuse workspace serde_json as a build dependency for the committed JSON asset manifest                                                     |
| Create | `crates/trusted-server-js/src/trace_assets.rs`, `crates/trusted-server-js/lib/trace-assets-manifest.json`, `crates/trusted-server-js/lib/test/trace-assets.test.mjs`                                                                                                                                                                                                       | Versioned immutable bytes, digest verification, asset lookup independent of integration discovery                                          |
| Modify | `crates/trusted-server-js/lib/.prettierignore`, `crates/trusted-server-js/lib/eslint.config.js`                                                                                                                                                                                                                                                                            | Exclude frozen generated asset bytes from source format/lint rewrites                                                                      |
| Create | `crates/trusted-server-js/lib/trace-assets/v1.js`, `crates/trusted-server-js/lib/trace-assets/v1.css`                                                                                                                                                                                                                                                                      | Frozen release bytes retained across later asset-set builds                                                                                |
| Create | `crates/trusted-server-integration-tests/browser/tests/shared/mobile-trace.spec.ts`, `crates/trusted-server-integration-tests/browser/helpers/trace-fixture.ts`, `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.trace.toml`                                                                                                                      | Dedicated setup endpoint/browser runtime, independent of shared disabled fixtures                                                          |
| Modify | `crates/trusted-server-integration-tests/browser/helpers/infra.ts`, `crates/trusted-server-integration-tests/browser/helpers/state.ts`, `crates/trusted-server-integration-tests/browser/global-setup.ts`, `crates/trusted-server-integration-tests/browser/global-teardown.ts`, `scripts/integration-tests-browser.sh`, `scripts/generate-integration-viceroy-configs.sh` | Launch and clean up dedicated enabled/disabled trace test runtime                                                                          |
| Modify | `crates/trusted-server-integration-tests/tests/parity.rs`                                                                                                                                                                                                                                                                                                                  | Common route/auth/order/body contract across native adapter seams                                                                          |
| Modify | `crates/trusted-server-integration-tests/tests/integration.rs`, `crates/trusted-server-integration-tests/tests/common/config.rs`                                                                                                                                                                                                                                           | Isolated real Workers transport/config regression using `crates/trusted-server-integration-tests/tests/environments/cloudflare.rs` harness |
| Modify | `trusted-server.example.toml`, `docs/guide/integrations/gpt-diagnostics.md`, `docs/guide/configuration.md`                                                                                                                                                                                                                                                                 | Default-off config, disclosure/transport/session/auth limits                                                                               |

Rust unit tests live beside each changed module. Do not add a new crate, UI framework, cookie parser dependency, or generic diagnostics subsystem.

### F0: Supply the missing EdgeZero pre-dispatch seam

**External files:** `crates/edgezero-core/src/router.rs`, `crates/edgezero-core/src/request.rs` or the existing request-metadata module, and `crates/edgezero-adapter-{fastly,axum,cloudflare,spin}/src/request.rs` in the upstream EdgeZero repository. These are not Trusted Server paths. Use a separate temporary source checkout only for dependency investigation; leave the Trusted Server branch intact.

- [ ] Confirm the existing pinned router with `cargo metadata --format-version 1 --no-deps` and `rg -n 'edgezero|v0.0.8' Cargo.toml Cargo.lock`. Expected: all EdgeZero packages resolve consistently to the current tag/revision before changes.
- [ ] Verify the reviewed upstream implementation of the hook API below. The EdgeZero feature branch now supplies `RouterBuilder::pre_dispatch_hook`, `PreDispatchHook` and bounded `RequestIngress` metadata; it has not yet supplied an immutable reviewed dependency revision. Retain `RouterService` as the concrete return type so existing adapter runners and test seams use it.

```rust
#[async_trait::async_trait(?Send)]
pub trait PreDispatchHook: Send + Sync + 'static {
    async fn handle(
        &self,
        request: &mut Request,
    ) -> Result<Option<Response>, EdgeError>;
}
```

- [ ] Verify upstream regressions for no hook, continuation mutation, early response, async empty stream inspection, shared hook across router clones, and an unregistered extension method. Both method and path must be inspected before `find_route`, state/introspection insertion, `RequestContext::new`, and ordinary middleware. Hook errors stop routing. Trace policy failures must instead return `Ok(Some(hardened_response))` to retain the exact response contract through the generic router.
- [ ] Verify `RouterBuilder::pre_dispatch_hook` stores `Arc<dyn PreDispatchHook>` and runs first in `RouterInner::dispatch`. `Some(response)` terminates; `None` preserves the possibly updated request for ordinary dispatch. Existing routers remain behavior-identical when no hook is installed.
- [ ] Verify runtime-visible method preservation in every converter. Cloudflare must use `req.inner().method()` and parse it with `http::Method::from_bytes`, not Workers `req.method()` (which maps unknown tokens to GET). Its browser contract constructs a `web_sys::Request` extension token and proves conversion. Application-seam tests require authenticated hardened 405 and Allow for that token on every exact trace route. Real Worker wire tests instead record pre-invocation rejection if the parser returns 501; never claim those two tests prove the same outcome.
- [ ] Consume `RequestIngress::{origin,header_fidelity}` without a parallel metadata type. Route decisions use the converted `Request::uri().path()`, not `CapturedTarget` or guessed original bytes. Original-target metadata may remain unavailable/transformed/over cap without rejecting an inspectable pathname. Canonical origin still requires adapter-owned `RequestIngress.origin()`, never `RequestInfo` forwarding-header fallbacks. Verify literal/encoded dot/separator and absolute/origin-form transport observations, including normalization into and out of trace.
- [ ] Verify Fastly's early snapshot captures origin before native mutations and is carried through `into_core_request_with_ingress`. Classify its SDK-visible pathname before shortcuts. Raw original target remains NotExposed and no SDK getter work is required for v1. Test `/_ts/trace/../../health` and `/_ts/trace/../debug/ja4` as ordinary paths when the runtime normalizes them outside trace; test normalizations into trace and visible reserved ambiguities against full local auth/hardening. Record existing health/JA4 auth limitations rather than asserting trace authentication on those ordinary requests.
- [ ] Verify runtime-visible cookie/control semantics against the resolved graph. Worker strings are UTF-8 and must copy via `HeaderValue::from_bytes(value.as_bytes())`; do not cast scalar values to bytes. Cookie-specific non-preserved octets plus U+FFFD, or non-preserved multiplicity plus comma, make all four states unavailable/runtime_header_ambiguous. Unknown/missing metadata alone permits marker-free sessions. Check exact visible reserved-name counts/duplicate precedence and visible byte bounds; global/same-name order and original-wire reconstruction are not prerequisites. Validate complete Origin/action/Fetch Metadata values and reject duplicates/folded values, never comma-split controls. Expose no raw bytes or parser messages.
- [ ] Run exact raw-byte Cookie/control tests: valid session plus original FF versus original valid EF BF BD, comma-coalesced repeated reserved cookies, visible duplicates, and visible header caps. Include actual isolated Workers transport beyond synthetic HeaderValue tests. Record runtime-rejected, conversion-failed and application-handled/transformed cases separately; only the latter can assert CookieHealth and hardened trace responses. Use Rust or existing native/shell tooling, with no Python shipped.
- [ ] In the upstream checkout run `cargo test -p edgezero-core pre_dispatch` and its adapter request-conversion tests. Expected: all new cases pass and old no-hook routing tests remain green. For Cloudflare’s browser-gated `tests/contract.rs`, reproduce the pinned upstream workflow: install `wasm-bindgen-cli` at the version resolved from its Cargo.lock, establish the required browser/WebDriver, then run `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test -p edgezero-adapter-cloudflare --features cloudflare --target wasm32-unknown-unknown --test contract`. A host-only run or a missing browser/runner is not conversion evidence; record it as pending.
- [ ] Obtain a reviewed immutable upstream release/revision exposing that API, update all workspace EdgeZero dependencies consistently in `Cargo.toml`, and regenerate `Cargo.lock` with `cargo update`. Do not invent a future tag/hash or ship a local Cargo-cache edit. Record the resolved commit and dependency graph. Current local upstream evidence uses worker 0.8.3 / wasm-bindgen 0.2.122; this consumer currently resolves 0.8.5 / 0.2.126. Repeat browser converter and real Worker ingress tests against the repinned graph; do not downgrade merely to match prior evidence.
- [ ] Run `cargo check-fastly`, `cargo check-axum`, `cargo check-cloudflare`, and `cargo check-spin`. Expected: all adapters compile against the same pin. Planned commit: `Add EdgeZero pre-dispatch support for reserved trace routes`.

**Amended capability gate:** Keep runtime rejection, adapter bootstrap/conversion failure and application-visible handling distinct. The proposed spec now accepts normalization outside trace as ordinary traffic and defines conservative Cookie ambiguity suppression. Original-target support remains documented external work, not a v1 blocker. F0 remains open until a reviewed immutable EdgeZero revision is adopted, converters and control/cookie semantics pass on the resolved graph, and safe normal requests work on every adapter. F4 additionally proves the trace terminal path bypasses ordinary lifecycle/finalizers. The hook does not precede adapter bootstrap/body buffering; no runtime rejection is falsely credited with a local response.

### F1: Validate default-off configuration and unify cookie policy

**Files:** `integrations/gpt_diagnostics.rs`, `config.rs`, `trusted-server.example.toml` under the paths in the file map.

- [ ] Write `trace_config_requires_enabled_on_both_validation_paths`: trace=true with enabled=false or omitted must fail both `validate_settings_for_deploy` and `validate_settings_for_runtime`. Add accepted flag=false/true cases and disabled unknown-field rejection.
- [ ] Run `cargo test-fastly trace_config`. Expected red: field rejected as unknown or the disabled-gate combination is not validated; inspect the actual failure.
- [ ] Add `#[serde(default)] pub trace_page_enabled: bool` to `GptDiagnosticsConfig`. Add a schema check for the dependency and a raw-config hook next to `validate_js_asset_proxy_config`; explicitly deserialize/validate even when `enabled` is false. Invoke it in both validation paths before enabled integration lookup.
- [ ] Write query/endpoint policy tests for enable→query enable, query enable→enable, repeated explicit activation, either clear surface, no ordinary-request refresh, and feature-disabled query activation. Expected set header: `__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=1800`; clear uses `Max-Age=0`, never Domain.
- [ ] Run `cargo test-fastly gpt_diagnostics`. Confirm a lifetime regression fails, then centralize the set/clear policy in that module for both writers. Keep the existing action enum/API where possible; update its obsolete browser-session docs.
- [ ] Re-run both filters and `cargo clippy-fastly`. Expected: green; config validation cannot silently log-and-disable invalid enabled settings. Planned commit: `Add validated trace configuration and bounded diagnostics cookie policy`.

### F2: Implement exact reserved classification and hardened responses

**Files:** `trace/routes.rs`, `trace/mod.rs`, `trace/shell.rs`, `lib.rs`; reuse `ec/admin.rs` and `response_privacy.rs`; make only the bounded trace-auth logging adjustment in `auth.rs`, without broad refactoring.

- [ ] Write table tests covering shell, state, enable, end, exact v1 assets, trailing slash, extra segments, lookalikes, repeated separators, encoded/repeatedly encoded separators, dot segments, decode-budget exhaustion, and unrelated paths. Reuse the four-round fixed-point decoding pattern in `deny_admin_diagnostic_fallback`.
- [ ] Run `cargo test-fastly trace::routes`. Expected red: missing classifier/responder, then specific status mismatches once compiled.
- [ ] Implement classification returning `NotTrace`, a supported route, or a reserved-path rejection from the application-visible `req.uri().path()`. Use bounded decoded variants only to reserve/reject aliases, never to serve them. Still-visible malformed encodings/dot ambiguities return 400 after auth/flag precedence; other reserved lookalikes return 404. Normalized-out paths continue as ordinary traffic; normalized-in exact trace paths receive full validation. Ignore original-target availability for decisions.
- [ ] Define the shared responder sequence: classify → `enforce_basic_auth` with the canonical request path/settings → harden challenge/error → flag check → reserved-path error → method check → route-specific handling. No body read or setup projection precedes authentication. Disabled requests are 404 after auth.
- [ ] Preserve existing first-match `Settings::handler_for_path(req.uri().path())` selection; neither decoded aliases nor original-target metadata creates a second auth-policy interpretation. Pin `/%5Fts/trace`: under `^/`, auth challenges before the local error; under a lone `^/_ts`, the unmatched alias is rejected without setup data/actions (400 when enabled, 404 when disabled). Set a diagnostic auth-log-policy extension before synchronous `enforce_basic_auth`; its wrong-credential log otherwise prints the full path. Emit a fixed trace-auth failure category while ordinary callers retain logging/authorization-marker behavior. Test a fictional secret-path sentinel never enters body/logs.
- [ ] Implement the complete local route table below. HEAD uses GET status/headers but always drops the body; HEAD on actions returns bodyless 405.

| Path                       | Methods   | Enabled response                           | Allow on 405 |
| -------------------------- | --------- | ------------------------------------------ | ------------ |
| `/_ts/trace`               | GET, HEAD | 200 escaped setup shell                    | `GET, HEAD`  |
| `/_ts/trace/state`         | GET, HEAD | 200 exact `{ "observed_active": boolean }` | `GET, HEAD`  |
| `/_ts/trace/enable`        | POST      | 200 bounded mutation-requested result      | `POST`       |
| `/_ts/trace/end`           | POST      | 200 bounded mutation-requested result      | `POST`       |
| `/_ts/trace/assets/v1.js`  | GET, HEAD | Fixed JS bytes                             | `GET, HEAD`  |
| `/_ts/trace/assets/v1.css` | GET, HEAD | Fixed CSS bytes                            | `GET, HEAD`  |

- [ ] Harden every local dynamic/error/challenge response, including assets errors: private/no-store, removal of shared/edge cache directives, nosniff, no-referrer, Permissions-Policy and exact spec CSP. Dynamic shell is HTML UTF-8; state/actions are JSON UTF-8. Build only bounded errors, no raw `Report` text in bodies/logs. Retain `WWW-Authenticate` on 401 and `Allow` on 405.
- [ ] Serve successful assets with correct JS/CSS MIME, nosniff, strong byte-derived ETag. If the selected handler has auth, use private/no-store even with accepted credentials; otherwise use public/max-age=31536000/immutable. Do not reflect query/report data into assets.
- [ ] Add regression tests for broad auth, earlier narrow-handler shadowing, disabled/auth order, protected assets, extension methods, HEAD bodylessness, and query nonreflection. Re-run filter and clippy. Planned commit: `Add authenticated local trace routing and response hardening`.

### F3: Inspect runtime-visible cookies and project request facts

**Files:** `trace/types.rs`, `trace/cookies.rs`, `trace/context.rs`, `trace/mod.rs`; canonical validators in `ec/generation.rs` and `ec/prebid_eids.rs` are reused.

- [ ] Write scanner tests for all four reserved names, absent/valid/invalid, duplicate precedence, multiple header fields, additional equals in values, unrelated malformed names, exact malformed reserved tokens, oversized values, exactly/over 16 KiB aggregate, and non-UTF-8 headers.
- [ ] Add a table covering each non-preserved status plus missing metadata: marker-free session and empty collection remain inspectable; comma anywhere with non-preserved multiplicity or U+FFFD anywhere with non-preserved octets makes all four unavailable/runtime_header_ambiguous. Test independent per-Cookie overrides, unrelated/quoted markers, exact visible duplicates, Preserved-axis marker semantics, valid other multibyte UTF-8 and original invalid bytes. Current adapters' common Unknown is not a reason to disable normal sessions.
- [ ] Run `cargo test-fastly trace::cookies`. Expected red; implement a read-only frozen visible-field scan, not CookieJar. Sum as_bytes lengths (including introduced separators); above 16,384 yields header_too_large, then actual invalid UTF-8 yields header_not_utf8, then markers yield runtime_header_ambiguous. Use str::from_utf8, not HeaderValue::to_str. Get per-Cookie axes from RequestIngress::header_fidelity(&COOKIE); missing metadata is unproved. Do not comma-split. Preserve exact names/count malformed reserved occurrences before canonical value parsing. Order does not affect counts. Test combined failures pin this precedence and suppress the capture gate.
- [ ] Enforce visible UTF-8 per-value byte caps 512 EC, 8192 EIDs, 16 tester/session. Reuse canonical validators and literal tester=true/session=1. Add runtime_header_ambiguous to the exact Rust detail enum, legal only with unavailable; emit source: request, no values/parser text/fidelity metadata. After aggregate checks, duplicate wins over individual validity; absent has no detail. These bounds/counts do not reconstruct discarded original wire information.
- [ ] Write `trace_context_masks_and_omits_forbidden_fields` using `192.0.2.129` and `2001:db8:1234:5678::1`: expect a deterministic /24 and /48 display mask, never the full addresses. Put distinct fictional sentinels in fingerprints, path/query, user/auction IDs, headers and geo city/coordinates; serialized output must exclude them.
- [ ] Run `cargo test-fastly trace::context`. Implement the exact section 9.1 schema with strict RFC3339 UTC capture time and only optional allowlisted network members. Country ≤2 ASCII; region/POP/protocol/cipher ≤32 UTF-8 bytes; edge hostname/region ≤128. Reject invalid Unicode/control/bidi values, omit bad optional fields with bounded categories; do not silently shorten or log values.
- [ ] Project existing `RuntimeServices.client_info` and read-only geo: Fastly IP/TLS/server metadata/country/region, Axum trusted IP, Cloudflare trusted IP/country, Spin trusted IP. ASN stays absent unless actually populated. No speculative HTTP/POP mapping in this increment.
- [ ] Re-run both filters. Tests use panic/counting KV/auction/filter/sink mocks to prove inspection has no identity or telemetry side effects. Planned commit: `Add bounded read-only trace request and cookie projections`.

### F4: Integrate the hook on every adapter before ordinary processing

**Files:** all four `src/app.rs`, Fastly `src/main.rs`, platform metadata mapping, adapter-local tests, `tests/parity.rs`.

- [ ] Write dispatch tests using each adapter's actual `routes_with_settings` or equivalent build seam with injected counting/panic services. Include exact/malformed/disabled routes, HEAD, extension methods, and auth failures. All ordinary lifecycle/filter/KV/auction/origin/sink counters must remain zero. Optional read-only geo is allowed only after auth.
- [ ] Run `cargo test-fastly trace_dispatch`, `cargo test-axum trace_dispatch`, `cargo test-cloudflare trace_dispatch`, and `cargo test-spin trace_dispatch`. Expected red until every route construction registers the hook.
- [ ] In Fastly main.rs capture RequestIngress before mutations and classify the SDK-visible pathname before native shortcuts. Only NotTrace may use health/JA4; runtime-normalized paths outside trace explicitly retain ordinary behavior. Carry the snapshot and consistent visible-path classification into the core responder. Real Viceroy raw-client tests cover normalizations out to health/JA4 and into trace, still-visible reserved ambiguity, exact routes, disabled/auth precedence and zero ordinary effects for trace. Do not wait for a raw-target SDK accessor or assert auth on normalized-out health/JA4.
- [ ] Register one hook capturing each adapter's `Arc<AppState>`, before EdgeZero method routing. Consume `RequestIngress` facts and a lazy read-only metadata supplier; canonical origin must not use `RequestInfo` forwarding-header fallbacks. Capture Fastly metadata before native mutation/shortcuts and pass the same snapshot through `into_core_request_with_ingress` into the existing direct `oneshot` seam. Return every trace policy rejection as `Ok(Some(hardened_response))`, not a propagated generic `EdgeError`. Do not construct the full EC/event pipeline for setup facts. Preserve harmless transport metadata needed for trusted IP resolution; sanitize trust-secret headers on continuing ordinary requests using existing policy.
- [ ] On a non-trace continuation only when trace config is enabled, freeze CookieHealth and incoming-session validity before prepare_request. Compute the base gate once from config plus runtime-visible inspection; unavailable/invalid/duplicate/absent gives false. After existing diagnostics decision, compute document eligibility without rereading sanitized cookies. Retain no raw values; suppress trace tokens/evidence/sidecars on ambiguity while ordinary advertising/console behavior remains unchanged.
- [ ] Mark all reserved local responses, including 401/disabled/path/method errors and successful assets, with a core `TraceTerminalResponse` extension. Fastly main must detect it before `apply_entry_point_finalize_headers` and bypass that ordinary finalizer: it performs geo lookup and applies operator headers that could otherwise replace CSP/MIME or add cookies after the early responder. Use only a trace-specific terminal pass to preserve the complete route header/cache/body contract through native conversion; do not insert `EcFinalizeState` or `RequestFilterEffects`. Add entry-point regressions with hostile CSP/Content-Type/Set-Cookie/cache overrides: auth failure performs zero geo lookups, GET/HEAD never gain Set-Cookie, action cookies remain exactly the accepted policy, and Allow/WWW-Authenticate/security headers survive. Preserve native adapter startup/runtime config validation and `routes_with_settings_and_services` seams.
- [ ] Add native parity tests exercising arbitrary methods, malformed paths, auth matrices, missing optional facts, and no-Content-Type streamed POSTs. Fastly is checked separately under Viceroy; the native parity test does not execute Fastly.
- [ ] Run all four trace_dispatch filters and `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity trace`. Add isolated actual-runtime tests in tests/integration.rs using CloudflareWorkers and CLOUDFLARE_WRANGLER_DIR; parameterize tests/common/config.rs without changing disabled baseline fixtures. Run the Cloudflare build.sh and `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test integration trace_runtime_boundary -- --ignored --test-threads=1`. A raw client preserves duplicate/invalid bytes where reqwest/fetch cannot. Pin extension-method wire rejection (local 501), FF replacement, comma joining and normal safe GET/POST flows separately from application-seam hardened 405 tests. Add equivalent Fastly/Spin evidence, including pre-component invalid-byte rejection and conversion-failure counters. Only successful conversion can assert local status/Allow/hardening/CookieHealth; preserve no trace-specific writes/capture for failures. Planned commit: `Intercept trace requests before all adapter lifecycles`.

### F5: Enforce deliberate actions and observe state separately

**Files:** `trace/routes.rs`, `trace/mod.rs`, adapter body-parity tests.

- [ ] Write same-origin action tests: missing/duplicate/conflicting action, Origin, Fetch Metadata, Host/authority/scheme; cross-site Origin; credentials/path/query/fragment in Origin; case/default-port canonicalization; forbidden action query; valid enable/end and repeated actions.
- [ ] Run `cargo test-fastly trace_actions`. Implement exact single control-header validation: correct `X-TS-Trace-Action`, exact `Sec-Fetch-Site: same-origin`, and one HTTP(S) Origin canonically equal to trusted inbound origin. Lowercase host/remove default ports for both; never trust arbitrary forwarding headers. Rejection is 403 with no mutation.
- [ ] Write body tests for missing/zero/invalid/positive Content-Length, any application-visible Transfer-Encoding, nonempty `Body::Once`, clean streamed EOF, empty chunks before EOF/nonempty chunk, and stream error. Assert 413 for invalid/positive length, encoding or actual body bytes, 400 for stream error, hardening and no Set-Cookie. Test identical zero lengths folded by the transport as one zero length plus an actually empty body; the spec does not require transport-level 413 for identical duplicates. Record conflicting-framing parser rejection separately rather than claiming the application returned a hardened response.
- [ ] Implement header prechecks, then explicitly match Body::Once/Stream. Only empty Once or clean stream EOF is accepted. Reject the first nonempty streamed chunk without continuing consumption. Do not use `into_bytes().unwrap_or_default()` or forward `into_bytes_bounded(0)` errors; those cannot implement the prescribed 413 behavior.
- [ ] For accepted action return the shared set/clear header independently of incoming CookieHealth; ambiguous cookies must not prevent a valid end action. State GET uses the shared frozen inspection and returns true only for exactly one present_valid/valid_diagnostics_value session. Absent/invalid/duplicate/unavailable is false, never cookie-absence proof. Test accepted enable/end with ambiguous cookies and separate unconfirmed activation observation. GET/HEAD never mutate cookies or identity; strict action/Origin/Fetch Metadata checks remain unchanged.
- [ ] Run `cargo test-fastly trace_actions`, all four adapter `trace_dispatch` filters, and parity. Expected: no mutation on any rejection, including bodiless fetch without Content-Type through Axum streaming. Planned commit: `Add same-origin trace actions and observed session state`.

No transport deadline/one-byte read/408 guarantee is added. Fastly, Cloudflare, and Spin pre-buffer; Axum streams non-JSON; body acceptance here cannot bound earlier transport allocation. Document that deployment limitation.

### F6: Build versioned setup assets and verified lifecycle UI

**Files:** `trace/shell.rs`, JS `src/trace/{types,lifecycle,viewer}.ts`, `viewer.css`, setup tests, asset build/embedding files from the map.

- [ ] Write lifecycle tests for activation success followed by active state, mismatch, failed verification, deactivation success/inactive, missing cookie, retry, rejected POST, and no added history entry. Assert POST is same-origin, bodyless, `credentials: 'same-origin'`, cache=no-store, with fixed action header; state GET is a distinct no-store request.
- [ ] Add setup/context rendering for unavailable/runtime_header_ambiguous with plain text explaining unreliable runtime-visible cookie inspection. An ambiguous follow-up state stays inactive and activation unconfirmed; successful end with inactive observation says no valid session observed, not cookie absent. Retain independent local cleanup/retry behavior.
- [ ] From `crates/trusted-server-js/lib` run `npx vitest run test/trace/lifecycle.test.ts test/trace/setup.test.ts`. Expected red. Implement `changeTraceSession(action: 'enable' | 'end')` returning mutation and observation results separately. Never equate POST success with confirmed state.
- [ ] Implement shell/setup UI from section 6.1: title, setup-request network/cookie facts, on/off state, reproduce-after-enable instructions, same-host/same-tab/history fallback, explicit reload guidance, 44 px enable/end/back controls and aria-live status. No previous-page URL inference, target parameter, automatic activation, or GET cookie mutation. Facts are escaped text nodes, not inline executable JSON; viewer reads fixed element attributes/text.
- [ ] Emit viewer JS as `dist/trace/v1.js` and CSS as `dist/trace/v1.css` separately from `tsjs-*.js`. Extend the existing Vite build with a standalone trace entry; no new integration registration or concatenation of viewer code into inactive publisher bundles.
- [ ] Add workspace `serde_json` to `crates/trusted-server-js/Cargo.toml` build-dependencies to parse the committed JSON manifest; this reuses an existing dependency rather than introducing another parser. Extend `build.rs` to embed assets independently, verify committed SHA-256 digests from `lib/trace-assets-manifest.json`, and fail closed for missing/stale assets under `TSJS_SKIP_BUILD=1`. Expose narrow static asset metadata through `src/trace_assets.rs`. Exclude only generated frozen `trace-assets/**` from JS Prettier and ESLint source rules via `lib/.prettierignore` and `lib/eslint.config.js`; digest tests govern those bytes and source TypeScript/CSS remain formatted/linted. Store published bytes in versioned source-controlled files `crates/trusted-server-js/lib/trace-assets/v1.js` and `v1.css`; manifest/digests alone cannot preserve old routes' bytes. Build output must match these frozen bytes at release, and future changes add a new version.
- [ ] Add a Vitest `test/trace-assets.test.mjs` with `// @vitest-environment node`, following the existing build-artifact tests, checking rebuilt bytes/digests, exact lookup URLs, no dynamic/request data and shell references. Run `node build-all.mjs`, `npx vitest run test/trace-assets.test.mjs`, setup Vitest tests, and `cargo test-fastly trace::routes`. Expected green. Planned commit: `Add versioned mobile trace setup assets and session controls`.

Keep v1 unpublished through all mandatory phases and update its draft digest/byte fixture with each source change. Freeze only the complete viewer set at first publication after V7. There is no planned interim setup publication or automatic v2 transition.

### F7: Inject the literal document gate and request context privately

**Files:** `publisher.rs`, `html_processor.rs`, `trace/mod.rs`, `integrations/gpt_diagnostics.rs`, publisher/unit cache regressions.

- [ ] Write gate matrix tests: valid incoming cookie + trace flag + active decision succeeds; missing cookie/query-enable only, query-disable, duplicate/invalid directives, prefetch, bot/ineligible navigation, trace flag=false, and nonboolean browser globals cannot activate document tracing.
- [ ] Run `cargo test-fastly trace_document`. Thread a request-scoped optional trace bootstrap through `ProcessResponseParams`, `OwnedProcessResponseParams` and `HtmlProcessorConfig`; use the existing head injection point in `html_processor.rs` beside the diagnostics bootstrap, before the unified TSJS tag. After the effective diagnostics decision, inject `window.__tsjs_trace_active=true` and the immutable redacted context before TSJS initializes, only on eligible documents. Expose context as `window.__tsjs_trace_request_context` with the `TraceRequestContextV1` contract; use the existing script-safe serializer/escaper, not raw executable concatenation. Missing/failed context projection leaves advertising intact and causes a bounded capture failure later.
- [ ] Keep tokens/context out of shared HTML templates and ESI fragments. Add them only in request-scoped injection/body seams. On inactive publisher responses, emit no trace assets/bootstrap/listeners/cache-policy change; existing explicit console activation retains its own behavior.
- [ ] Test projection/serialization failure, hostile `</script>` strings, unsupported metadata, shared-template reuse, and request-specific values for two users. Test hostile response_headers and late filter overrides; retain `late_filter_effects_cannot_make_an_assembled_response_public` and reuse `apply_response_headers_with_cache_privacy`/Fastly terminal guards.
- [ ] Run `cargo test-fastly trace_document`, `cargo test-fastly late_filter_effects_cannot_make_an_assembled_response_public`, all four adapter tests, and relevant clippy gates. Expected: private/no-store terminally, no shared bytes contain trace context, advertising preserved. Planned commit: `Inject eligible request-scoped trace context with terminal privacy`.

### F8: Document and verify the foundation review unit

**Files:** example TOML and operator docs in the map; all changed files for verification.

- [ ] Document default-off option beside `enabled`, same-origin script visibility of HttpOnly cookie health/opaque auction outcomes, potentially personal masked IP/coarse geo, and explicit operator disclosure acceptance.
- [ ] Document first-match-wins auth and public mobile deployment scope, cookie/session caveats, host-only origin scope, browser/service-worker trust limits, transport pre-buffering, local 400/401/403/404/405/413, and no timeout guarantee.
- [ ] Document the three runtime-boundary outcomes, normalized-out ordinary health/JA4 behavior, literal visible-path auth for rejected encoded aliases, visible cookie bounds and conservative comma/U+FFFD false negatives. Require a suitable same-origin test deployment for Secure host-only cookies; never weaken cookie attributes or infer HTTPS from untrusted headers. F0 no longer depends on external raw-target/header reconstruction work.
- [ ] Create a dedicated setup test runtime/helper and trace TOML without changing the shared disabled integration config. Parameterize the existing `scripts/generate-integration-viceroy-configs.sh` app-config input and generate isolated trace output with its existing `generate-viceroy-config --app-config` binary; preserve baseline input/output defaults. Track extra process IDs in browser state and extend both failed-setup cleanup and `global-teardown.ts` to stop every runtime/container; preserve baseline runtime teardown. Add foundation-specific browser tests to `crates/trusted-server-integration-tests/browser/tests/shared/mobile-trace.spec.ts` for read-only GET, cross-site actions, history, observed state, exact CSP/favicon, auth and inactive traffic. Run `./scripts/integration-tests-browser.sh tests/shared/mobile-trace.spec.ts` for this foundation checkpoint, plus affected adapter builds/clippy/docs and the trace parity suite. V7 owns the single complete release gate run. The later viewer phase extends these fixtures; foundation verification does not wait for the viewer.
- [ ] Review diff for accidental real data, raw logs, unrelated refactors, and unimplemented upstream promises. Planned commit: `Document and verify trace setup privacy and adapter parity`.
- [ ] Checkpoint the default-off setup phase with its test evidence and explicit dependency/mobile gaps, then continue E1; do not publish interim assets or declare the full feature complete.

## Phase 2: Live auction evidence

### Inputs and invariants

Depends on foundation tasks F0–F8. Read [spec](../specs/2026-09-01-mobile-ad-render-trace-endpoint-design.md) sections 5.2, 9.3–9.5, 10, 12, 13, and 14.1–14.4. Keep the existing branch and follow the shared verification's verification/commit rules.

Maintain the existing console-only `diagnostics_auction_id` behavior when trace is off; only the new trace carry/evidence/slot tokens are gated by trace. For active trace, mint the public auction token once before dispatch and use that same token in both public evidence and the existing GPT opportunity marker, including zero-bid/no-candidate paths.

### File map

| Action | Exact path                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Responsibility                                                                                                      |
| ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Create | `crates/trusted-server-core/src/trace/auction.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Typed opaque tokens, private observation carry, public evidence projection/bounds                                   |
| Modify | `crates/trusted-server-core/src/trace/mod.rs`, `crates/trusted-server-core/src/trace/types.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Export the narrow auction contracts/hooks                                                                           |
| Modify | `crates/trusted-server-core/src/auction/orchestrator.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Trace-only launch-order/terminal-fact carry through dispatch/collect/abandon                                        |
| Modify | `crates/trusted-server-core/src/auction/formats.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Permissive trace extension extraction along exact accepted-slot conversion; actual response conversion dispositions |
| Modify | `crates/trusted-server-core/src/auction/endpoints.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | API base gate, pre-dispatch identity, response evidence and privacy                                                 |
| Modify | `crates/trusted-server-core/src/openrtb.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Optional namespaced public response extension                                                                       |
| Modify | `crates/trusted-server-core/src/publisher.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | SSAT/SPA request-scoped carry, token-bearing slot definitions, held-tail scheduler transport, legacy alias          |
| Modify | `crates/trusted-server-js/lib/src/trace/types.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Evidence/transport/correlation types                                                                                |
| Create | `crates/trusted-server-js/lib/src/trace/validation.ts`, `crates/trusted-server-js/lib/src/trace/collector.ts`, `crates/trusted-server-js/lib/src/trace/runtime.ts`, `crates/trusted-server-js/lib/src/trace/pending.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Strict public evidence checks, one collector facade, API unit mapping and bounded pending lifecycle                 |
| Modify | `crates/trusted-server-js/lib/src/core/types.ts`, `crates/trusted-server-js/lib/src/core/global.d.ts`, `crates/trusted-server-js/lib/src/core/index.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Optional typed trace facade/slot extension and scheduler third argument                                             |
| Modify | `crates/trusted-server-js/lib/src/core/auction.ts`, `crates/trusted-server-js/lib/src/core/request.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Final grouped unit tokens, direct response collection before bid parsing                                            |
| Modify | `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Initial/SPA transport validation, slot identity to existing recorder                                                |
| Modify | `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/store.ts`, `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/api.ts`, `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Trace-only recorder input/callback at concrete cycle binding                                                        |
| Modify | `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Actual registered timeout/error hooks and request-associated evidence                                               |
| Create | `crates/trusted-server-js/lib/test/trace/evidence.test.ts`, `crates/trusted-server-js/lib/test/trace/collector.test.ts`, `crates/trusted-server-js/lib/test/trace/pending.test.ts`, `crates/trusted-server-js/lib/test/trace/tokens.test.ts` in the same directory                                                                                                                                                                                                                                                                                                                                                                                                          | Strict bounds, coverage truth table, token/mapping/pending behavior                                                 |
| Modify | `crates/trusted-server-js/lib/test/core/auction.test.ts`, `crates/trusted-server-js/lib/test/core/request.test.ts`; `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`, `crates/trusted-server-js/lib/test/integrations/gpt/spa_hook.test.ts`, `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`; `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/store.test.ts`, `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/api.test.ts`, `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/types.test.ts`; `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` | Behavior compatibility at existing seams                                                                            |
| Modify | `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Real pinned Prebid register/newBidder callback routing                                                              |
| Modify | `crates/trusted-server-integration-tests/browser/tests/shared/mobile-trace.spec.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Live transport/attribution regression fixtures added to foundation suite                                            |

New Rust tests stay in module `#[cfg(test)]` blocks. Do not serialize the telemetry observation, add fields to public GPT export, rename telemetry source vocabulary, or change provider protocols.

### E1: Define opaque tokens and the strict public auction contract

**Files:** `trace/auction.rs`, `trace/types.rs`, `trace/mod.rs`, JS trace types/validation, token/evidence tests.

- [ ] Add tests that accept exactly the existing unhyphenated lowercase auction UUID-v4 shape and new hyphenated lowercase slot UUID-v4 shape, enforce RFC variant, and reject uppercase, whitespace, wrong UUID versions, internal IDs, and normalized alternatives.
- [ ] Run `cargo test-fastly trace::auction` and, from `crates/trusted-server-js/lib`, `npx vitest run test/trace/tokens.test.ts test/trace/evidence.test.ts`. Expected red.
- [ ] Add newtypes `DiagnosticAuctionId` and `TraceSlotRef` with checked parsing and strong internal ownership. Generate auction UUID via `Uuid::new_v4().simple()` and slot UUID via canonical hyphenated UUID v4. Compare exact stored bytes. Browser token validators are:

```typescript
const auctionToken = /^ts-auc-[0-9a-f]{12}4[0-9a-f]{3}[89ab][0-9a-f]{15}$/
const slotToken =
  /^ts-slot-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
```

- [ ] Define the exact `TraceAuctionEvidenceV1`/`TraceAuctionTransportV1` types from spec 9.4/9.4.1. Explicit enums cover all source, terminal reason/status, provider role/status and candidate members. Transport is a discriminated one-of: schema_version=1 plus evidence OR unavailable_reason=`evidence_projection_failed`, never both/neither.
- [ ] Write table tests for every enum member and rejection, unknown keys at every boundary, provider ≤16, auction slots ≤64, requested sizes ≤16, token/string bounds, dimensions 1..100000, u16 ordinals/counts/counters, u32 durations, required positive ordinals and exact fixed coverage=`unavailable`.
- [ ] Implement strict validators that copy accepted values into trace-owned models, reject extra keys/unsafe numbers/invalid Unicode/control/bidi strings, and never modify a supplied ad object. Optional duration conversion failure omits/counts; required ordinal/count or checked u16 omission overflow rejects that evidence. Browser rejects invalid core-bounded inner evidence without truncating it again.
- [ ] Re-run tests and `cargo clippy-fastly`; expected green. Planned commit: `Define bounded public trace auction evidence and opaque tokens`.

### E2: Preserve live auction facts before dispatch and through terminal paths

**Files:** `trace/auction.rs`, `auction/orchestrator.rs`, publisher request/stream carries, endpoint carries.

- [ ] Write `trace_auction_identity_is_stable_across_terminal_outcomes` and `trace_auction_provider_numbers_follow_launch_order` with completed, zero-bid, skipped, dispatch-failed, execution-failed and abandoned cases. Randomize completion order and legacy HashMap insertion; output order must remain launch order.
- [ ] Run `cargo test-fastly trace_auction`. Expected red. Introduce a private optional `TraceAuctionCarry` owned by the request/dispatch carry with source, pre-dispatch token, exact accepted-slot token mapping, ordered provider observations and monotonic clock start. Instantiate only under the applicable server gate; do not use `AuctionRequest.id` or telemetry UUID.
- [ ] Extend the existing private dispatch/collect outcomes to preserve trace facts on failures. Observe calls at their actual launch point, including dispatch failures and mediator calls; never infer missing provider observations from a `Report`. Number providers in deterministic dispatch order, not completion/HashMap order; pass only redacted summaries into projection.
- [ ] Map terminal observations explicitly:

| Live condition                                    | Status                                | Reason                                            |
| ------------------------------------------------- | ------------------------------------- | ------------------------------------------------- |
| Successful execution, including zero bids         | completed                             | Omit unless an actual allowlisted reason is known |
| Consent/policy skip                               | skipped                               | policy_skipped                                    |
| Definitive converted slot list empty              | skipped                               | no_eligible_slots                                 |
| Unsuccessful split-path NotStarted without launch | dispatch_failed                       | no_provider_launched                              |
| Execution failed with provider failure known      | execution_failed                      | provider_execution_failed                         |
| Collection failure                                | execution_failed                      | collection_failed                                 |
| Dispatched but cannot be collected/delivered      | abandoned                             | unknown or a directly observed allowlisted reason |
| Other terminal failure                            | Corresponding observed failure status | unknown                                           |

- [ ] Distinguish path semantics before mapping `NotStarted`: `run_planned_auction` already returns successful `OrchestrationResult::no_bid()` for an enabled empty plan. Successful API/SPA empty-plan execution is completed with zero providers/no candidates; it is not a dispatch failure merely because internal dispatch was NotStarted. Add separate empty-plan API/SPA success and unsuccessful split-dispatch regressions, preserving ordinary response/status behavior and explicit consent/no-slot skips.
- [ ] Treat actual `DispatchAuctionOutcome::DispatchFailed` as dispatch_failed with provider_execution_failed when launch failures prove it, otherwise unknown; do not label NotStarted as a launch failure. Keep no-slot/policy-skipped trace observations independent of whether current telemetry constructs `AuctionObservationContext`. Do not create telemetry rows solely to get a trace identity. Abandoned telemetry's existing clamped duration and unordered lists are not a valid trace projection source; perform separate checked duration conversions.
- [ ] Derive provider statuses/counts and role from actual live responses/pending/disposition. Provider numbers never expose names. Compute per-slot bid counts from returned bids; no provider-to-slot no-bid attribution is inferred. Preserve final `provider_to_slot_no_bid: 'unavailable'`.
- [ ] Test that diagnostics disabled allocates no trace carry/tokens and launches exactly the same provider work. Re-run all four adapter suites after changing shared carries/constructors, plus focused filter/clippy. Planned commit: `Carry trace auction identity and live outcomes across dispatch`.

### E3: Preserve exact slot associations and API acceptance compatibility

**Files:** `auction/formats.rs`, `auction/endpoints.rs`, `openrtb.rs`, `trace/auction.rs`, `publisher.rs` request-scoped slot construction.

- [ ] Write converted-slot mapping tests for grouped multi-bidder units, duplicate codes, skipped non-banner units, filtered inputs, missing/invalid/duplicate token values, duplicate extension members and wrong scalar/object shapes. Ordinary AdRequest success/bids must equal the baseline for every trace-only malformed input.
- [ ] Run `cargo test-fastly trace_slot_conversion`. Extract only `adUnits[].ext.trusted_server.trace_slot_ref` permissively while retaining occurrence validity; do not add a strict serde string field that rejects requests whose unknown extension was previously ignored. Preserve duplicate-member detection with a narrow deserialize visitor if needed; do not reparse with `serde_json::Value` alone and silently lose duplicate evidence.
- [ ] Thread the accepted unit's token alongside the exact unit-to-`AuctionRequest.slots` conversion. One-based `slot_number` uses only definitive post-conversion order, never client response association. Invalid/missing/duplicate tokens get server-generated refs; ordinary input validity is unchanged.
- [ ] Keep trace association in a private sidecar rather than serialized `AdSlot`/provider input. Add bidder and mediator outbound JSON sentinel tests proving `trace_slot_ref`/diagnostic auction tokens never reach their requests, even if supplied as input extensions.
- [ ] For duplicate accepted routing keys preserve distinct instance ordinals/refs. Count actual returned records matching the ordinary key, documenting shared, non-disjoint counts rather than a unique-bid total. Since winner/delivery maps cannot prove an instance-specific disposition, emit `unknown`, omit selected size and never infer an instance/GPT join. Pin this with duplicate-code regressions and viewer wording.
- [ ] Use `OpenRtbResponseConversion` delivered-winner dispositions and actual bid-map inclusion to distinguish selected, selected_unrenderable, no_candidate and unknown. Selection alone is insufficient; malformed dimensions/creative rejection/missing price/cache fallback handling stay unchanged.
- [ ] Add `ext.trusted_server.trace_auction` to the response only under the base gate; preserve existing orchestrator extension, status, seats/bids and headers apart from deliberate private/no-store. Projection failure returns the exact unavailable envelope, no raw error.
- [ ] Run focused tests, `cargo test-fastly auction::endpoints`, native adapter tests, and clippy. Planned commit: `Associate trace slots through auction conversion without changing bids`.

### E4: Transport SSAT and SPA evidence at request-scoped seams

**Files:** `publisher.rs`, trace carry/projection, existing HTML processing; JS scheduler/SPA hooks in E6.

- [ ] Write initial-navigation and both canonical/legacy page-bids tests for matching slot tokens, single shared opportunity/evidence auction token, every terminal outcome, zero bids, context gate suppression, and projection failure with unchanged normal bids.
- [ ] Run `cargo test-fastly trace_transport`. Add tokens to exact request-scoped slot definitions while constructing the corresponding auction slots. Do not add random tokens inside generic `build_slot_json` when it is called for shared templates; use an optional request-scoped argument/map, leaving template output token-free.
- [ ] Extend `build_seam_script`/held-tail path to serialize the optional transport with the existing script-safe JSON method and call `scheduleInitialAdInit(bids, slots?, traceAuctionTransport?)`. Record available failure/skip evidence even when no winning bid exists. If tail injection never reaches the browser, do not fabricate a failure marker; collector remains not_observed absent other evidence.
- [ ] Add optional top-level `trace_auction` next to `slots` and `bids` in both `/_ts/page-bids` and `/__ts/page-bids`. Strip both envelope and slot extensions when gate=false; base gate applies without navigation eligibility. Initial documents additionally require effective navigation decision.
- [ ] Assert current GPT-only marker behavior when trace flag=false. When active, reuse the pre-dispatch trace token in the existing opportunity marker instead of minting another token at collection. No candidate/failed auctions still carry the same identity when deliverable.
- [ ] Mark every evidence-bearing response terminal private/no-store. Test late operator/cache overrides, ESI/shared-template absence, malformed HTML/serialization fail-open and old scheduler that ignores the third arg.
- [ ] Re-run focused Rust tests, all four adapters and cache guard regression; expected unchanged baseline bids/slots/render status except optional gated public members. Planned commit: `Carry live trace evidence through initial and SPA ad responses`.

### E5: Install one gated browser collector and instrument direct API requests

**Files:** JS trace runtime/collector/validation, core facade/globals, `core/auction.ts`, `core/request.ts`, collector/core tests.

- [ ] Write gate tests with missing/false/string/number globals while GPT diagnostics=true: no token generation, listeners, unit mappings, pending records, collector facade activation, or storage access. Test repeated integration IIFE installs share one active facade, not separate imported singletons.
- [ ] From `crates/trusted-server-js/lib`, run `npx vitest run test/trace/collector.test.ts test/core/auction.test.ts test/core/request.test.ts`. Expected red. Instantiate the collector once through a typed `window.tsjs` facade only behind the strict literal gate. Pure validation imports are allowed; import-time trace side effects are not.
- [ ] Accept immutable validated transport before normal bid parsing. Absent optional member adds no issue and never resets earlier records/issues; malformed supplied member adds evidence_validation_failed; supplied unavailable marker adds evidence_projection_failed. Direct non-OK/unreadable JSON/rejected fetch paths add evidence_transport_failed without retaining errors/bodies.
- [ ] Retain newest 16 server records/newest 128 sidecars, stable arrival/emission order and checked omission counters. Server eviction adds record_evicted; sidecar-only eviction adds correlation_unavailable. No sessionStorage writes during collection.
- [ ] Implement capture status independently of interpretation issues:

```typescript
type CaptureStatus = 'complete' | 'partial' | 'unavailable' | 'not_observed'

function captureStatus(
  recordCount: number,
  issues: readonly string[]
): CaptureStatus {
  const hasCaptureIssue = issues.some((issue) =>
    [
      'evidence_projection_failed',
      'evidence_transport_failed',
      'evidence_validation_failed',
      'record_evicted',
    ].includes(issue)
  )
  if (recordCount > 0) return hasCaptureIssue ? 'partial' : 'complete'
  return hasCaptureIssue ? 'unavailable' : 'not_observed'
}
```

- [ ] Deduplicate/sort the six exact issue enums in their documented order, max 16. `correlation_unavailable` and `external_client_side_unobservable` never change capture status. Mark external refresh limits only when the existing browser source actually observes them.
- [ ] Assign `crypto.randomUUID()` refs after `buildAdRequest` groups/deduplicates final units, then keep the exact request-scoped token-to-unit mapping through `sendAuction`. No Web Crypto means no client refs; ordinary request still runs. Validate echoed tokens/association without indices/raw unit codes in public evidence; record API evidence with correlation_unavailable and no GPT opportunity/sidecar.
- [ ] Add throwing token-generator and collector-callback fixtures for both actual `/auction` callers and the scheduler/SPA boundary. Assert the ordinary request, ad initialization, bid acceptance/order and targeting still complete unchanged; diagnostic failures remain bounded and never escape into advertising.
- [ ] Re-run focused tests; add unchanged bid-result assertions on absent, invalid and failed evidence. Planned commit: `Collect gated trace evidence without changing direct auction delivery`.

### E6: Emit sidecars only at the existing SSAT/SPA recorder binding

**Files:** `core/types.ts`, `integrations/gpt/index.ts`, diagnostics store/API/index, existing scheduler/SPA/recorder tests.

- [ ] Write initial scheduler third-argument and SPA parser tests: validate/record before adInit, absent optional member leaves old behavior, malformed transport still initializes ads, legacy retry preserves transport, stale/superseded SPA result does not join a current cycle.
- [ ] Run `npx vitest run test/integrations/gpt/schedule_initial_ad_init.test.ts test/integrations/gpt/spa_hook.test.ts test/integrations/gpt/ad_init.test.ts`. Add optional AuctionSlot.ext and the exact transport argument/member from spec; do not spread arbitrary extensions into trace objects.
- [ ] Validate a delivered slot ref occurs exactly once in both delivered slots and matching validated auction slots before associating it with the concrete slot object. Missing/malformed/duplicate/conflicting refs affect only correlation: preserve slot/bid order/content, add correlation_unavailable, and keep complete server capture. Absent optional envelope does not start token validation.
- [ ] Add an optional trace identity parameter to the existing opportunity forwarding and a trace-only store callback. Keep the current GPT export type unchanged. Pass trace identities only for validated SSAT/SPA opportunities; preserve existing attribution/source selection logic.
- [ ] Emit `TraceSlotCorrelationV1` inside `GptDiagnosticsStore.recordSlotRequested` after `consumeRequestIntent` selects evidence and concrete runtimeSlotNumber/requestNumber exist. It contains only schema/version, two tokens, two positive safe integers. No second attribution engine, timestamps, raw DOM/ad-unit IDs or best-effort joins.
- [ ] Write recorder tests for candidate/no-candidate/unrenderable, expired opportunities, competing sources, ambiguous/no concrete cycles, repeated callbacks, exact token equality and type assertions that GptDiagnosticsExportV1 has no new fields. Inject a throwing trace-sidecar callback and assert the recorder still completes ordinary binding and public GPT export unchanged. API evidence cannot call this binding.
- [ ] Run `npx vitest run test/integrations/gpt test/integrations/gpt_diagnostics test/trace/collector.test.ts`. Expected green, including source export type gate. Planned commit: `Record exact SSAT and SPA trace sidecars at GPT cycle binding`.

### E7: Consume Prebid transport exactly once through real registered hooks

**Files:** Prebid integration, `trace/pending.ts`, pending/Prebid tests and real artifact test.

- [ ] Write buildRequests/interpretResponse/onTimeout/onBidderError tests keyed by original bid IDs and final grouped unit refs, including multi-unit outcomes, concurrent requests, duplicate callbacks, absent response extension, malformed evidence and server-gate absence. Pending refs are request-local, not a global current request.
- [ ] Run `npx vitest run test/trace/pending.test.ts test/integrations/prebid/index.test.ts`. Add trace-only optional hooks to the actual spec passed to `pbjs.registerBidAdapter(undefined, ADAPTER_CODE, spec)` only when trace is active. Preserve all ordinary bidder spec behavior when inactive.
- [ ] Create a pending record after final buildRequests with maximum 128 retained entries. interpretResponse consumes before bids; `onTimeout(bidRequestsWithTimeout)` and `onBidderError({ error, bidderRequest })` consume the same matching pending record and add transport failure exactly once. Never store error text/XHR responses; capped/expired records without supported outcome hooks invent no failure.
- [ ] Implement and test this checked expiry helper; capture effective browser bidderTimeout once per record, unrelated to server `[auction]` timers:

```typescript
const MAX_CAPTURED_BIDDER_TIMEOUT = 2 ** 31 - 1 - 5000

function pendingExpiry(
  createdAtMs: number,
  configuredTimeout: unknown
): number | undefined {
  if (!Number.isSafeInteger(createdAtMs) || createdAtMs < 0) return undefined
  const timeout =
    typeof configuredTimeout === 'number' &&
    Number.isInteger(configuredTimeout) &&
    configuredTimeout >= 0 &&
    configuredTimeout <= MAX_CAPTURED_BIDDER_TIMEOUT
      ? configuredTimeout
      : 3000
  const expiresAtMs = createdAtMs + timeout + 5000
  return Number.isSafeInteger(expiresAtMs) ? expiresAtMs : undefined
}
```

- [ ] Use an integer wall-clock `Date.now()` (or its injected test clock) for record creation and read `pbjs.getConfig('bidderTimeout')` at creation. Configured `merged.timeout` already supplies that browser setting. Use a bounded timer delay of captured timeout+5000; future config changes do not alter existing expiry. Clock/checked-add failure declines only that pending attempt with no capture issue or reset; expiry alone removes the marker and remains not_observed.
- [ ] Cover configured 0/normal/max, missing/negative/fractional/NaN/infinite/string/over-max values, default 3000, safe-integer clock overflow, callback after expiry, cap eviction, timer cleanup on runtime destruction and repeated hooks. Use fake timers, not wall-clock waiting.
- [ ] Inject throwing token-generation/collector hooks in the registered Prebid path and prove bid callbacks/results remain unchanged. Measure the built shim against its existing 41,000-character guard in `test/prebid-artifact-integration.test.mjs`; if trace code requires an increase, record the measured before/after size and justify a narrow new bound while preserving the Prebid-free regression guard.
- [ ] Extend `test/prebid-artifact-integration.test.mjs` using its real external bundle+shim setup. Drive real adapterManager timeout/error callbacks and prove `registerBidAdapter` → `newBidder` → registered spec hooks. Do not stop at calling mock spec methods directly.
- [ ] Run `npx vitest run test/trace/pending.test.ts test/integrations/prebid/index.test.ts test/prebid-artifact-integration.test.mjs`, full JS tests/build/format, and affected Rust adapter checks. Verify API evidence stays independent and neither hook emits GPT sidecars or `trusted_server_direct`. Planned commit: `Capture Prebid trace transport through registered timeout and error hooks`.

### Review-unit verification and handoff

- [ ] Extend dedicated foundation browser fixtures with initial/SPA/API normal, zero-bid and failure results, token conversion filtering, old bundles, malformed members, legacy page-bids retry, disabled cookie-bearing traffic and fail-open ad assertions. The fixture must exercise actual built integration bundles, not fabricated evidence alone.
- [ ] Run `./scripts/integration-tests-browser.sh tests/shared/mobile-trace.spec.ts`, focused evidence tests and affected-target build/clippy/docs checks; V7 owns the single full release gate run. Expected: advertising assertions stay identical to baseline; all evidence/privacy/token cases pass.
- [ ] Search serialized trace fixture output for distinct forbidden sentinels covering IDs, names, prices, payloads, consent and raw errors. Assert no trace-specific logs contain sentinels or cookie/network values.
- [ ] Review deterministic provider numbering and every terminal path centrally; preserve existing telemetry tests without adopting its private fields or saturation rules. Planned commit: `Verify live trace transports and advertising compatibility`.
- [ ] Checkpoint evidence collection as memory-only, then continue V1. User snapshot/storage/viewer behavior remains phase 3; do not imply retained observations already survive navigation.

## Phase 3: Browser handoff and viewer

### Inputs and scope

Depends on foundation tasks F0–F8 and live-evidence tasks E1–E7. Read [spec](../specs/2026-09-01-mobile-ad-render-trace-endpoint-design.md) sections 6, 9.1–9.5, 10, 12, 13, 14.3–14.5, and 16. Follow the shared verification's same-branch and verification rules.

Use `window.tsjs.gptDiagnostics.snapshot()` (the public API in `integrations/gpt_diagnostics/api.ts`) as the only GPT input. No private-store export traversal, second GPT attribution engine, server upload, telemetry query, target URL, report ID, or new UI dependency.

### File map

| Action | Exact path                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Responsibility                                                                                  |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Modify | `crates/trusted-server-js/lib/src/trace/types.ts`, `crates/trusted-server-js/lib/src/trace/validation.ts`, `crates/trusted-server-js/lib/src/trace/runtime.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                | Complete trace/report/storage types and strict ingestion                                        |
| Create | `crates/trusted-server-js/lib/src/trace/projection.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | Exhaustive explicit GPT source-to-public projection                                             |
| Create | `crates/trusted-server-js/lib/src/trace/report.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Snapshot assembly, checked omissions and deterministic bounds                                   |
| Create | `crates/trusted-server-js/lib/src/trace/storage.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Single key, exact wrapper, age/size/origin validation and independent deletion                  |
| Create | `crates/trusted-server-js/lib/src/trace/export.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | One validated formatted artifact for copy/download/share                                        |
| Create | `crates/trusted-server-js/lib/src/trace/correlation.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Exact-token joins and explicit ambiguous/unmatched results                                      |
| Create | `crates/trusted-server-js/lib/src/trace/handoff.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Explicit capture → validate → write → same-tab navigation and recovery                          |
| Modify | `crates/trusted-server-js/lib/src/trace/viewer.ts`, `crates/trusted-server-js/lib/src/trace/viewer.css`, `crates/trusted-server-js/lib/src/trace/lifecycle.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                | Full report DOM/accessibility and independent cleanup/state verification                        |
| Modify | `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/index.ts`, `crates/trusted-server-js/lib/src/integrations/gpt_diagnostics/overlay.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Optional gated prominent handoff action; keep old GPT export intact                             |
| Create | `crates/trusted-server-js/lib/test/trace/fixtures.ts`, `crates/trusted-server-js/lib/test/trace/types.test.ts`, `crates/trusted-server-js/lib/test/trace/validation.test.ts`, `crates/trusted-server-js/lib/test/trace/projection.test.ts`, `crates/trusted-server-js/lib/test/trace/report.test.ts`, `crates/trusted-server-js/lib/test/trace/storage.test.ts`, `crates/trusted-server-js/lib/test/trace/export.test.ts`, `crates/trusted-server-js/lib/test/trace/correlation.test.ts`, `crates/trusted-server-js/lib/test/trace/handoff.test.ts`, `crates/trusted-server-js/lib/test/trace/viewer.test.ts` | Complete source and hostile/bounded fixtures, type/runtime/DOM/action tests                     |
| Modify | `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/overlay.test.ts`, `crates/trusted-server-js/lib/test/integrations/gpt_diagnostics/index.test.ts`                                                                                                                                                                                                                                                                                                                                                                                                                                              | Publisher action gating/presentation compatibility                                              |
| Modify | `crates/trusted-server-js/lib/trace-assets-manifest.json`, `crates/trusted-server-js/lib/test/trace-assets.test.mjs`, `crates/trusted-server-js/lib/build-all.mjs`; `crates/trusted-server-js/src/trace_assets.rs`; `crates/trusted-server-core/src/trace/routes.rs`, `crates/trusted-server-core/src/trace/shell.rs`                                                                                                                                                                                                                                                                                         | Complete unpublished v1 asset set/shell lookup; preserve byte contracts after first publication |
| Modify | `crates/trusted-server-js/lib/trace-assets/v1.js`, `crates/trusted-server-js/lib/trace-assets/v1.css`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Finalize the complete unpublished v1 bytes created in F6 before first release                   |
| Modify | `crates/trusted-server-integration-tests/browser/tests/shared/mobile-trace.spec.ts`, `crates/trusted-server-integration-tests/browser/helpers/gpt-stub.ts`, `crates/trusted-server-integration-tests/browser/helpers/state.ts`, `crates/trusted-server-integration-tests/browser/helpers/infra.ts`, `crates/trusted-server-integration-tests/browser/global-setup.ts`, `crates/trusted-server-integration-tests/browser/global-teardown.ts`, `crates/trusted-server-integration-tests/browser/playwright.config.ts`                                                                                           | Dedicated trace runtime and realistic server→GPT→creative fixtures                              |
| Modify | `crates/trusted-server-integration-tests/browser/helpers/trace-fixture.ts`, `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.trace.toml`                                                                                                                                                                                                                                                                                                                                                                                                                                              | Extend foundation runtime/settings with live auction cases                                      |
| Create | `crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/app/mobile-trace/page.tsx`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Real publisher slot fixture for the complete viewer journey                                     |
| Modify | `scripts/integration-tests-browser.sh`, `scripts/generate-integration-viceroy-configs.sh`, `.github/workflows/integration-tests.yml`                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Build/install any new trace fixture dependencies and browser projects consistently              |
| Modify | `docs/guide/integrations/gpt-diagnostics.md`, `docs/guide/configuration.md`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Full support journey, trust/privacy limits and rollout                                          |

`test/trace/types.test.ts` joins the existing Vitest typecheck include. Preserve its scoped source-error behavior; do not claim package-wide `tsc --noEmit` passes because Vitest passes.

### V1: Define exact report ingestion and classify every GPT source member

**Files:** trace types/validation/fixtures/type tests; source interface reference `src/core/types.ts`.

- [ ] Create a complete fictional source fixture exercising every current `GptDiagnosticsExportV1` property, every request-cycle optional value, all callback/attribution/coverage metadata and all relevant enums. Use distinct forbidden sentinels for every `adManager` member, previousCreativeId, slotElementId, adUnitPath, issue slotElementId and a secret-bearing example.com pathname.
- [ ] Write type-level member coverage using `satisfies Record<keyof GptDiagnosticsRequestCycle, 'copy' | 'exclude'>` and equivalent checks for the source root, slot, issues, coverage counters, metadata and nested durations/binding. All current keys must be classified and no later source key passes automatically. Source additions fail typecheck until classified.
- [ ] From `crates/trusted-server-js/lib`, run `npx vitest run test/trace/types.test.ts test/trace/validation.test.ts`. Expected red, including a real type assertion failure when a field is deliberately unclassified.
- [ ] Define only the compatibility matrix `TraceReportV1`, `TraceAuctionEvidenceV1`, `TraceSlotCorrelationV1`, `TraceGptDiagnosticsV1` and source GPT v1. Unknown versions at any boundary fail with an actionable bounded category. All stored objects have exact allowed/required keys, not optional passthrough fields.
- [ ] Implement request context/CookieHealth validators from foundation contracts. Validate masked addresses as the chosen /24 or /48 display representation, not arbitrary full IP strings; only canonical HTTP(S) origins equal to location.origin are accepted, with no credentials/path/query/fragment. Validate real UTC RFC3339 calendar times without permissive Date normalization.
- [ ] Propagate runtime_header_ambiguous through the exact TypeScript CookieHealthDetail union and setup/context/TraceReportV1 validation. It is legal only with unavailable; test wrong state/detail pairs and unknown reason rejection. Retain supported schema versions and bounds; expose no fidelity metadata or original header values.
- [ ] Implement exact GPT request-cycle allowlist below, with source optionality and enum membership from the current interfaces. Never accept adManager/previousCreativeId/slot or issue DOM IDs/ad-unit paths. The viewer accepts only pathname=`/[redacted]`.

```typescript
const TRACE_CYCLE_KEYS = [
  'requestNumber',
  'requestedAtMs',
  'responseAtMs',
  'renderAtMs',
  'loadAtMs',
  'viewableAtMs',
  'durations',
  'isEmpty',
  'requestedSlotSizes',
  'size',
  'observedSlotSize',
  'isBackfill',
  'slotContentChanged',
  'incompleteSequence',
  'responseClass',
  'requestPath',
  'requestIntentId',
  'trustedServerAuctionId',
  'opportunityToRequestMs',
  'replacedRequestNumber',
  'previousRenderToRequestMs',
  'creativeChanged',
  'loadObservedBeforeRender',
  'trustedServerOpportunity',
  'trustedServerCreativeRequestAtMs',
  'trustedServerCreativeResponseAtMs',
  'trustedServerCreativeFailures',
  'delivery',
] as const

const CALLBACK_REASONS = [
  'invalid_event_order',
  'missing_response_before_render',
  'invalid_visibility_percentage',
  'evicted_slot',
  'no_compatible_request_cycle',
  'overlapping_request_cycles',
] as const
```

- [ ] Validate remaining exact objects: root GPT schema/source version/capturedAt/page/slots/callbackIssues/attributionIssues/coverage/metadata; slot runtimeSlotNumber/binding/current+max visibility/requests; binding status/reason; five duration members; callback kind/runtimeSlotNumber/timestampMs/disposition/reason; attribution reason/timestampMs/runtimeSlotNumber; six callback coverage keys with observed/matched/unmatched/ambiguous; metadata droppedCallbacks/droppedAttributionIssues/evictedSlots/evictedRequestCycles.
- [ ] Apply all bounds: 64 GPT slots, ≤10 cycles/slot, 128 callback/attribution issues, ≤16 sizes/failure enums, origin ≤255 bytes, other allowed strings ≤128 bytes, exact tokens, finite nonnegative safe integer sequences/counters, browser timestamp/duration finite 0..MAX_SAFE_INTEGER, visibility 0..100, positive integer requested/selected/fill dimensions ≤100000; observed CSS dimensions integer 0..100000 including zeros. Outer/inner u16 omission counters retain their stricter bound.
- [ ] Count nested containers from report=1, not wrapper; objects and arrays each increment, max 10. Reject level 11, unknown properties/prototype keys, unpaired surrogates, C0/C1 and bidi override/isolate controls before any DOM/export. Validate wrapper separately as exactly stored_at_ms/report; precheck compact UTF-8 bytes before rendering/JSON parsing where input is serialized.
- [ ] Add one test per bound edge, forbidden member, enum/version/extra-key boundary, Unicode/control class, true complete-depth fixture, numeric overflow and zero observed dimensions. Re-run the filtered suites/type gate. Planned commit: `Define strict combined trace report validation`.

### V2: Project the public GPT model and implement deterministic report bounds

**Files:** projection/report/fixtures and their tests; collector interface from phase 2.

- [ ] Write field-by-field projection tests for the complete fixture. Expected root transforms: source `version`→source_schema_version=1, outer schema_version=1, source capturedAt retained, canonical origin retained, pathname replaced with literal. Every forbidden sentinel must be absent; no source object spreads/dynamic property inheritance.
- [ ] Run `npx vitest run test/trace/projection.test.ts`. Implement a new owned projection with explicit assignments for all V1-classified copied fields, checked source values and new nested objects/arrays. Invalid source values reject snapshot creation, not stringify/coerce/truncate strings. Mutating source after capture cannot mutate the projected report.
- [ ] Validate exact source container/key/own-data contracts and every value consumed by the projection. Never inspect excluded identity contents; their malformed contents cannot contaminate the report. Require the source pathname to be an own string, then replace it without reading its content or imposing a raw-path cap. Reject source arrays beyond their existing 64-slot/10-cycle/128-issue bounds; validate all supplied bounded sizes/failure enums before retaining the first sixteen and counting omissions.
- [ ] Write table tests for initial cardinality projection: newest 16 server records/128 sidecars, first 16 requested sizes/failure enums, ≤64 slots/10 cycles/128 issue records as defined by the source contract. Merge collector auction/sidecar evictions into outer omission counters exactly once; retain existing GPT metadata as its source meaning, not an extra outer omission count. Reject invalid inner server evidence rather than re-truncating it.
- [ ] Add the checked counter primitive and use it for every initial discard, later cycle/issue/auction/sidecar removal and optional invalid numeric discard from core:

```typescript
function addOmissions(current: number, added: number): number {
  if (
    !Number.isInteger(current) ||
    !Number.isInteger(added) ||
    current < 0 ||
    added < 0 ||
    current > 65535 ||
    added > 65535 - current
  ) {
    throw new Error('omission_counter_overflow')
  }
  return current + added
}
```

- [ ] Define stored_at_ms from the capture clock independently of source content. Outer report captured_at and GPT capturedAt must be within 60 seconds of that clock; request_context.captured_at may be older and is not overwritten by setup/time data. Validate the complete compact wrapper with `TextEncoder`, limit 512\*1024 bytes.
- [ ] Write truncation-order tests with deterministic IDs/timestamps: missing requestedAtMs sorts first; otherwise cycle order is requestedAtMs/runtimeSlotNumber/requestNumber. Callback/attribution order is timestampMs then original index. Array output retains original relative order after removals; server/provider/slot and sidecar observation orders remain stable.
- [ ] Implement the complete removal sequence, measuring the whole compact wrapper after each eligible removal:
  1. Protect each GPT slot's newest retained cycle for the entire algorithm.
  2. Remove globally oldest non-floor GPT cycles. Remove/count every associated sidecar in the same operation; never retain a claimed dangling join.
  3. Remove oldest callback issues, then oldest attribution issues.
  4. Remove oldest uncorrelated server auctions. Treat any remaining GPT-cycle auction-token reference as correlated even if no sidecar validates; do not orphan that cycle just because the join is unknown. Remove/count all referencing sidecars with the auction.
  5. Remove oldest eligible correlated server auctions together with all retained cycles that reference them and their sidecars. Skip any auction referenced by a protected floor cycle. Eligibility must preserve every GPT slot's newest cycle; no stage can empty a slot that started with cycles.
  6. Recompute capture_status/issues after each auction removal; auction omission adds record_evicted, sidecar-only omission adds correlation_unavailable. Deduplicate/sort issues and recalculate compact wrapper bytes including changed counters/issues.
  7. If still oversized after all eligible removals, fail with a bounded snapshot-size category. Do not remove the protected floor, return an empty GPT section, store, navigate, or offer a combined-report fallback.

- [ ] Add worst-case successful ≤512 KiB fixture with all source fields populated, multi-byte strings and exact six outer counters; counter-overflow fixture; full server eviction→unavailable/record_evicted; partial eviction→partial; sidecar-only eviction preserves complete; protected-floor+auction set >budget rejects. Use bounded valid model construction or a test-only injected smaller budget to exercise deterministic branches, plus an actual 512 KiB worst-case fixture. Do not weaken production v1 bounds.
- [ ] Include runtime_header_ambiguous in valid worst-case request-context/cookie fixtures and reassert exact encoded size, truncation/protected-floor behavior and existing depth/string bounds. Adding a valid reason does not authorize expanding the report envelope.
- [ ] Run `npx vitest run test/trace/projection.test.ts test/trace/report.test.ts test/trace/collector.test.ts`. Expected green and exact byte/counter assertions. Planned commit: `Project and deterministically bound combined trace snapshots`.

### V3: Store one validated report and export the identical model

**Files:** storage/export and unit tests.

- [ ] Write storage tests for one namespaced versioned key `trusted-server.trace.report.v1`; exact `{ stored_at_ms, report }` wrapper; replacement; absent/get/set/remove exceptions; malformed JSON; future, rollback and expiry; origin mismatch; hostile keys; oversized compact UTF-8 input. No storage write occurs during observation or viewer load.
- [ ] Run `npx vitest run test/trace/storage.test.ts`. Implement explicit read/write/delete results with bounded categories. Validate before setItem and after every getItem; remove rejected/expired entries best-effort, ignore them even when deletion fails. Enforce finite nonnegative safe stored_at_ms, max age 15 minutes, >60-second future skew rejection, capture-time consistency. Exactly 15 minutes remains within the age cap; older expires.
- [ ] Write export tests requiring the same TraceReportV1 bytes/model for download/copy/share/direct storage-failure recovery. Export the report, not private source objects or a page URL; use deterministic filename `trusted-server-trace-v1.json`, MIME `application/json`, formatted JSON and visible browser-carried/unverified labeling in the UI. Do not add an unknown provenance field to the approved schema.
- [ ] Pin unavailable/runtime_header_ambiguous through storage roundtrip, download, formatted copy, shared File and direct export. Wrong state/detail pairs reject on ingestion; every accepted output retains the same reason and no raw values.
- [ ] Implement one `formatTraceReport(report)` after validation used by all actions. Copy invokes clipboard only on explicit user action. Share supplies a `File` with the same formatted JSON only after `navigator.canShare({ files })` support and a tap; cancellation/rejection/absence leaves Copy and Download available with status. Never fall back to URL share/upload.
- [ ] Implement the download primitive with independent deferred URL cleanup:

```typescript
function downloadJson(json: string, filename: string): void {
  const url = URL.createObjectURL(
    new Blob([json], { type: 'application/json' })
  )
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  document.body.append(anchor)
  try {
    anchor.click()
  } finally {
    anchor.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
}
```

- [ ] Use fake timers/mocked URLs to assert click before scheduling, no synchronous revoke, each repeated download owns its URL, exact 1000 ms cleanup and accessible failure reporting. Do not change the pre-existing GPT-only export's synchronous cleanup in `api.ts`; that is outside this feature.
- [ ] Re-run storage/export tests. Expected: failure preserves visible valid report, no report upload/continuous storage. Planned commit: `Add validated trace storage and equivalent local exports`.

### V4: Join exact evidence without inventing winners or mixing clocks

**Files:** correlation, tests and renderer-facing result types.

- [ ] Write one valid SSAT and one SPA fixture joining exact diagnostic_auction_id/slot_ref and runtime_slot_number/request_number. Include no_candidate, unrenderable, empty/filled, creative bridge render/load/viewability and conflicting browser refresh paths.
- [ ] Run `npx vitest run test/trace/correlation.test.ts`. Implement joins only when one validated sidecar uniquely matches one server slot and one exported GPT cycle with consistent auction token. Runtime numbers/slot ordinals alone, timestamps, ad-unit/DOM paths and implicit array position never join.
- [ ] Add unmatched/duplicate/conflicting/forged/missing token and evicted-cycle fixtures. Keep structurally valid ambiguous records independent and show Correlation unknown; never choose a best match or assert no auction ran. Prune known dangling joins during builder truncation, not by fabricating replacement sidecars.
- [ ] Reject any stored sidecar referencing an auction_api record: unsupported in v1. API evidence displays independently with correlation_unavailable, retains complete server capture when otherwise valid, and never changes prebid_refresh to trusted_server_direct/competing.
- [ ] Map labels exactly: SSAT initial-page server auction, SPA Trusted Server page-refresh auction, API Trusted Server auction API; publisher/prebid_refresh=`Browser refresh observed; winner not determined`; competing/unattributed=`Multiple or unknown delivery paths`. Show `Trusted Server creative rendered` only when matched existing nonempty creative-bridge evidence proves participation, not candidate selection or GPT fill alone.
- [ ] Present server auction-local duration, request-relative milestones=`Unavailable in v1`, and browser/GPT timings in separate groups. Never subtract/add timestamps across clocks. Provider status remains auction-wide, not a per-slot no-bid reason.
- [ ] Re-run tests and assert source models are unchanged by joining. Planned commit: `Join exact trace evidence with explicit coverage and ambiguity`.

### V5: Add the explicit publisher handoff and storage-failure recovery

**Files:** handoff/runtime, diagnostics index/overlay, handoff/overlay/index tests.

- [ ] Write gated action tests: missing/false/nonboolean trace flag with console=true exposes no action, listeners, mappings, storage reads/writes; active=true supplies one prominent `View trace results` action with ≥44 px target. Existing GPT-only export remains separate.
- [ ] Run `npx vitest run test/trace/handoff.test.ts test/integrations/gpt_diagnostics/overlay.test.ts test/integrations/gpt_diagnostics/index.test.ts`. Add an optional overlay callback/options entry rather than rewriting the overlay or moving its internals into the trace viewer.
- [ ] On tap, obtain public GPT snapshot, immutable request context and collector snapshot; build/project/bound/validate the combined wrapper, then setItem, then `location.assign('/_ts/trace')`. No continuous persistence, new tab, URL payload, target URL, or extra diagnostic network request.
- [ ] If context/projection/validation/bounds fail, keep publisher page, announce bounded capture failure, do not navigate/write or offer a combined artifact. A GPT-only export is not an equivalent fallback.
- [ ] If only storage write fails after a valid report exists, keep publisher page, announce storage failure, retain that immutable combined report for an explicit direct Download action using V3. Navigation never happens after rejected storage. Destroy retry listeners/recovery state with the diagnostics runtime.
- [ ] Test exact action order, no navigation before successful write, snapshot source mutation isolation, serialization errors, quota/unavailable storage and direct-download equality. Re-run focused tests plus all GPT diagnostics tests. Planned commit: `Add explicit same-tab mobile trace handoff and recovery`.

### V6: Render the mobile report and independent cleanup outcomes

**Files:** viewer/styles/lifecycle, viewer/setup/export tests, Rust shell/routes and immutable asset metadata.

- [ ] Write DOM tests for no report→setup, rejected/expired report→reproduce message, valid report→summary/network/cookies/server/GPT/coverage/export/cleanup. Setup-request facts stay separately labeled and never fill missing publisher facts.
- [ ] Run `npx vitest run test/trace/viewer.test.ts test/trace/setup.test.ts test/trace/lifecycle.test.ts`. Render only the validated bounded model with `createElement`, safe attributes and textContent. No report innerHTML, inline style attributes/blocks or JS style writes under CSP; use classes/hidden.
- [ ] Add browser-carried/unverified heading and per-entry server-produced/copied-through-browser versus browser-observed provenance. All unavailable fields/states/ambiguities have plain text labels; no winner inference, exact page/path, or provider identity appears. Preserve zero observed box sizes without changing isEmpty.
- [ ] Render runtime_header_ambiguous as cookies unavailable for reliable inspection, never as actual invalid UTF-8 or absence. Cover all four affected cookie rows and retain export equality; no parsing detail or runtime fidelity metadata is displayed.
- [ ] Add formatted Copy, deterministic Download, and progressive file Share from V3. Status is aria-live; errors retain report. Disclosure says the selected share app receives the JSON. Keyboard focus and visible focus styles work independently of color/hover.
- [ ] Add `Clear report and end tracing` with explicit confirmation and distinct always-available `Delete local report`. After confirmation, attempt local deletion and end POST independently even if either fails; after POST attempt state verification independently. Successful deletion removes on-screen saved-report claims; failed deletion retains retry; failed/mismatched server observation uses unconfirmed/may remain active wording and independent retry. Never undo deletion because network failed.
- [ ] Cover the entire local-delete × mutation × state-verification outcome matrix with explicit UI assertions, including offline, successful mutation/unconfirmed observation, inactive-invalid/duplicate cookie, successful server end/failed deletion and retries. A false observed_active says no valid session observed, not cookie absent.
- [ ] Implement 320 px full-document layout with ≥44×44 primary controls, no horizontal page scroll, content-safe action placement, semantic headings/lists, zoom enabled, safe-area insets and readable long values. Tests check no content is covered by controls and report sections remain navigable by keyboard.
- [ ] Finalize the existing unpublished v1 JS/CSS bytes and committed digests with the complete viewer. Keep the exact v1 routes and shell references from F2/F6. At the V7 release checkpoint freeze this asset set; tests reject any later changed published bytes without a new URL/digest. No intermediate setup asset version is published as part of this plan.
- [ ] Run `node build-all.mjs`, `npx vitest run test/trace-assets.test.mjs`, `npx vitest run test/trace`, `cargo test-fastly trace::routes`, adapter parity and format checks. Expected: manifest pins every retained frozen version and current rebuilt bytes; shell references current URLs; altered bytes fail without a new version. Planned commit: `Add accessible consolidated mobile trace viewer and independent cleanup`.

### V7: Prove the complete journey with real endpoints and built bundles

**Files:** dedicated browser fixtures/helper/config/spec and build/CI paths in the file map; operator docs.

- [ ] Extend the foundation trace suite with a dedicated enabled config/runtime and real publisher slots; keep existing baseline config unchanged (it disables GPT/auction). Build images/artifacts through existing scripts/global setup. The runner executes both Next.js and WordPress: explicitly scope the Next.js-only publisher journey with the existing framework setting, or add equivalent WordPress content; run shared endpoint/setup scenarios against both. Generate the dedicated trace Viceroy config from the trace TOML through F8’s parameterized generator, never reuse the disabled baseline output. The existing observer-only GPT stub has no working defineSlot/refresh and cannot prove the chain; implement realistic callbacks in the dedicated fixture or extend the helper without changing existing test behavior.
- [ ] Write browser test: open publisher → navigate trace → assert read-only/no cookie → tap enable → separate state confirms → history Back → explicit real reload → collect SSAT and multiple GPT cycles → tap handoff → same-tab viewer → parse export and compare every displayed model section. Include SPA canonical/legacy page-bids and both actual `/auction` callers.
- [ ] Execute normal safe-cookie setup/enable/state/reload/capture/view/export/end/state across all four adapter fixtures on suitable same-origin deployments. Unknown fidelity alone must not suppress capture. Assert exact privacy/action/session contracts while documenting runtime rejection separately. Browser tests must prove cookie storage/observation, not inject a synthetic active-cookie header and call it a browser journey. Record unsuitable origins or unavailable runtime/browser environments as pending acceptance.
- [ ] Add raw-transport ambiguity fixtures alongside the browser workflow: valid session plus unrelated FF, original EF BF BD, comma-folded repeats and visible duplicates. For converted ambiguous requests assert state false, no trace tokens/evidence/sidecars, ordinary advertising unchanged, exact CookieHealth reason in setup/capture where observable, and valid end remains possible. Runtime-rejected requests produce no invented report. Normalization-in/out and encoded-alias auth tests follow F2/F4's actual visible path.
- [ ] Run `./scripts/integration-tests-browser.sh tests/shared/mobile-trace.spec.ts` from root. Expected red for missing journey fixtures/behavior, not artifact or Docker setup failure. Then fill the concrete fixture behavior and re-run after each scenario group.
- [ ] Add initial-navigation fixtures for selected, no-candidate, selected-unrenderable, skipped, dispatch-failed, execution-failed and abandoned evidence where delivery is possible; stopped/unreached tail remains not_observed. Test correlated SSAT/SPA chain, API independent, browser/client intent unknown, empty/ambiguous/unattributed states and malformed/absent transport with unchanged ads.
- [ ] Test genuine second-origin top-level GET, form and fetch activation/end attempts; configured auth; history unchanged by actions; BFCache guidance does not claim new capture; same-origin service-worker-forged exchange/report remains labeled unverified and real server endpoints still reject invalid requests.
- [ ] Test unexpired viewer reload, replacement, expired/hostile storage removal, removal failure, opener-cloned entry and expiry, restoration simulation, future-clock rollback, origin/hostname changes and same-host recovery instructions. Do not claim real browser restart coverage from a unit fixture; document any manual session-restore check still pending.
- [ ] Under the exact CSP test complete Blob JSON download with 1000 ms cleanup, repeated download, storage-failure direct export, clipboard/share absence/rejection/cancellation, no URL sharing, favicon without publisher `/favicon.ico`, framing/inline/third-party/report-derived injection blocked. Assert no upload/beacon or third-party trace request.
- [ ] Populate forbidden sentinels across the entire source fixture and assert absence in server trace HTML/JSON, stored wrapper, DOM, formatted copy, shared File and both downloads; hostile stored forbidden properties are rejected. Check output model/version equality, not just filename/download event.
- [ ] Add 320 px/mobile viewport and keyboard/focus/status tests to the existing Chromium runner. Keep real iOS Safari and Android Chrome manual acceptance as a separate checklist for zoom, safe areas, native file share/download, layperson instructions and retention of visible report after failures. Any optional automated WebKit project requires matching Playwright install changes in script and CI.
- [ ] Test rollback fresh loads with valid diagnostics cookie and console query activation retained: no trace gate/action/listeners/storage access/tokens/response evidence/new cache change. Already-running pages cannot be remotely revoked; later server requests immediately stop evidence. Previously cached static assets remain inert.
- [ ] Document reproduction/support/export instructions and browser-carried trust limits, no historic recovery, no client winner inference, host-only scope, independent cleanup retries and controlled staging rollout. Mark #1081/#1074/#1076 fields unavailable, not missing implementation.
- [ ] Run the full shared verification gates/builds/docs and the complete browser runner, plus manual mobile checklist. Review final diff and acceptance matrix. Planned commit: `Verify the mobile trace journey and document release acceptance`.
- [ ] Handoff v1 feature only after the first three phases meet all mandatory acceptance criteria; record pending environment/manual checks separately. Publishing/deployment is a subsequent explicit action.

## Approved review corrections — 2026-10-06

The user authorized this focused corrective pass after independent review of
Claude's findings. Keep the same feature branches, spec and plan.

- [x] Add a gated trace-only initial seam when ordinary ad-slot injection is
      withheld. Execute the emitted script and verify evidence is recorded once
      without invoking the scheduler, changing bids/slots or setting the ad-init
      latch; preserve queue initialization, generation guards and fail-open behavior.
- [x] Remove the redundant unconditional page-bids delivery observation. Retain
      the successful branch's definitive delivery observation and verify actual SPA
      success/fallback projections. The proposed partial-bid `Err` scenario is not
      reachable in current planned production execution; do not fabricate a
      legacy-test-only regression or claim a reproduced production truth defect.
- [x] Keep View usable after a navigation does not depart, and retain connected
      trace controls/live status across GPT data updates. Add retry/focus regressions.
- [x] Replace the five newly introduced ES2022 test APIs with ES2020-compatible
      access, without broadening the existing TypeScript target.
- [x] Correct EdgeZero's per-observation preservation metadata conservatively;
      document Fastly Content-Length folding; verify and pin the new upstream commit.
- [x] Clarify mediator-inclusive record counts, asset-manifest maintenance,
      publisher CSP compatibility, completed automation versus pending manual
      acceptance, and deployed HTTPS Enable/End checks.
- [x] Run target-matched checks and required full gates, independently review
      the corrections, and update both existing draft PRs without adding Python.

### Corrective-pass verification

Three independent reviewers approved the final corrections with no remaining
actionable findings: publisher/spec alignment, browser controls/types, and
EdgeZero metadata/architecture. EdgeZero commit
`75067d2c9a3cf865591665e88a736db4c8a13be0` replaces the previous reviewed pin
in all six workspace dependencies and eight lockfile sources, without unrelated
dependency changes. Its full workspace gates, adapter contracts and 54 raw
ingress cases pass; all checks on EdgeZero PR #403 pass.

Trusted Server's current-source format, all eight target-matched clippy gates,
four adapter test suites, host CLI tests and 21 parity cases pass against the new
pin. The host-only emitted-script regression executes real publisher output for
four skip conditions and buffered/streaming finalizers, each with five callback
modes; no scheduler or ad initialization runs and publisher-owned state remains
unchanged. The overlay regressions prove retained focus/live status and retry.
The normal JS and external Prebid builds, formatting, lint and all 1,736 Vitest
tests in 69 files pass. The five new ES2022 test errors are removed; the broader
JS TypeScript check retains unrelated baseline errors. Browser TypeScript
passes using the workspace's existing Node type declarations. Core documentation,
docs lint/format/build and Markdown formatting pass with existing warnings.

Fresh Fastly, Axum, Cloudflare worker-build and Spin artifacts pass the complete
repository browser runner: Next.js 49 passed with two expected skips; WordPress
28 passed with 23 expected skips. All four separate runtime browser workflows
pass, as do the three raw Cloudflare/Fastly/Spin boundary suites. These verify
local runtime behavior and do not replace physical-device, actual restoration or
deployed HTTPS/CDN/CSP acceptance. Both existing draft PRs receive the corrections;
neither feature branch contains Python files. Frozen standalone v1 asset bytes
remain unchanged by this corrective pass.

Claude's re-review found no new runtime defects and identified one automated
coverage gap: the ignored host-only emitted-script regression was not selected
by CI. The existing Node-equipped Rust job now runs that exact core library test
with an explicit Linux host target and `--ignored`; other ignored tests remain
excluded. The equivalent command on the macOS host selects one test and passes.
An independent reviewer approved the step with no findings. Workflow validation
passes; the nine pre-existing ShellCheck quoting notices are unchanged.

All four integration jobs on `b2930d7b9`, including framework browser tests and
four-runtime trace acceptance, have now passed remotely. The unrelated generated
Python CodeQL job fails because this feature branch contains no Python. The
separate CodeQL security gate reported test-fixture alerts; their resolution is
recorded below. Cloudflare's new-pin worker-build artifact and actual local
browser/raw-boundary suites use Trusted Server's resolved worker 0.8.5 and
wasm-bindgen 0.2.126; that consumer runtime check is complete. Deployed-platform
and physical-device acceptance remain separate release gates.

### CodeQL alert triage

On 2026-10-06, the user authorized fixing CodeQL findings or dismissing verified
false positives. Two independent read-only reviewers confirmed alerts #196–203
are confined to fictional authentication fixtures in `#[cfg(test)]` modules or
the parity integration-test target. Alerts #202–203 flag test-only passwords;
alerts #196–201 flag `expect()` calls on `Handler` deserialization. That
deserialization does not invoke the `Settings`/Tinybird secret validation named
in the alleged logging flow. No production secret reaches these fixtures.

All eight alerts were dismissed individually with GitHub's `used in tests`
reason and per-alert evidence. A fresh API check confirms no open alerts on
`refs/pull/1107/merge` and a successful CodeQL security gate. The earlier
explanation attributing the aggregate gate solely to Python extraction was
incomplete; the extraction error and these alerts are independent.

The generated `dynamic/github-code-quality/codeql` workflow still attempts Python
analysis on `refs/pull/1107/head` and exits 32 because that head contains no
Python. The default branch contains an unrelated Python browser helper, which
accounts for repository-wide language detection. This is an analysis failure,
not a dismissible vulnerability alert. Reading Code Quality setup returned HTTP
403 with the current maintainer account; configuration changes require a
repository admin. Any Python language-selection change is repository-wide and
must account for that default-branch helper. No scanner rules, source files or
production behavior were weakened to dismiss the test-only alerts.

Bundle splitting remains a separate load-order/performance design. Preserve
the build-input freshness guard and ordinary reserved-route/cache policy.
Physical mobile and deployed CDN/HTTPS acceptance remain release gates.

## Phase 4: Optional network enrichment

### Scheduling boundary

This is the optional fourth phase from approved [spec](../specs/2026-09-01-mobile-ad-render-trace-endpoint-design.md) sections 11 and 17.4. It does not block the first three phases or #1050 acceptance. Follow the shared sections above, keep the current branch while executing authorized work, and use the already strict optional field validators rather than expanding the report.

Do not adopt request-relative #1074/#1076 milestones, bidder/price/creative-number/winner disclosures from #1081, resolver facts, speed probes, raw IP or JA4/H2 here. Each later public contract change needs its own approved design/compatibility plan; this document schedules no speculative schema implementation.

### File map

| Action                         | Exact path                                                                                                                                                             | Responsibility                                                      |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| Modify if a source is verified | `crates/trusted-server-core/src/platform/types.rs`                                                                                                                     | Optional adapter-owned HTTP version/POP metadata, default None      |
| Modify                         | `crates/trusted-server-core/src/trace/context.rs`                                                                                                                      | Map only verified optional metadata through existing bounds         |
| Modify if needed               | `crates/trusted-server-adapter-fastly/src/main.rs`, `crates/trusted-server-adapter-fastly/src/platform.rs`                                                             | Native HTTP metadata before conversion, optional SDK ASN/POP source |
| Modify if a source is verified | `crates/trusted-server-adapter-axum/src/platform.rs`, `crates/trusted-server-adapter-cloudflare/src/platform.rs`, `crates/trusted-server-adapter-spin/src/platform.rs` | Explicit supported mapping, not fabricated cross-platform values    |
| Modify                         | `crates/trusted-server-integration-tests/tests/parity.rs`                                                                                                              | Common schema/omission expectations with differing supported fields |
| Modify                         | `crates/trusted-server-js/lib/test/trace/validation.test.ts`, `crates/trusted-server-js/lib/test/trace/viewer.test.ts`                                                 | Optional values accepted/bounded/rendered without default invention |
| Modify                         | `docs/guide/integrations/gpt-diagnostics.md`                                                                                                                           | Source/support matrix and privacy/provenance documentation          |

New Rust tests stay in the changed modules. Do not edit a dependency's Cargo-cache source. If upstream conversion must preserve a fact, release/pin that change through the foundation's dependency process.

### N1: Prove source availability against the actual SDK pin

- [ ] Record SDK revisions from `cargo metadata --format-version 1` and Cargo.lock and inspect the corresponding sources. Existing investigation found Fastly `Request::get_version()` and `Geo::as_number()` in the installed pinned SDK; verify again at execution, after the foundation EdgeZero upgrade.
- [ ] Establish the source table below with a concrete native accessor/type per newly enabled cell. Official runtime documentation must support environment-derived facts; an available header name is not a trusted source. Do not read arbitrary forwarded headers for HTTP/TLS/POP facts.

| Public field                  | Existing baseline                                             | Enrichment source to verify                                                                        | Failure behavior                          |
| ----------------------------- | ------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- | ----------------------------------------- |
| http_version/Fastly           | EdgeZero converter does not copy Fastly native version        | Capture native `req.get_version()` before conversion and carry adapter-owned fact                  | Omit unsupported/error value              |
| http_version/Axum             | Native inbound request version is available before conversion | Prove version is copied from inbound `http::Request` parts, not default-created downstream request | Omit if original version cannot be proved |
| http_version/Cloudflare, Spin | No current trace mapping                                      | Stable documented native runtime metadata only                                                     | Absent until verified                     |
| asn/Fastly                    | `geo_from_fastly` sets None                                   | Pinned `Geo::as_number()` with documented unknown/sentinel handling                                | Unknown/sentinel/error remains None       |
| asn/Cloudflare                | Geo adapter currently sets None                               | Typed documented Workers connection metadata with checked u32 conversion                           | Absent until verified                     |
| edge_pop/Fastly, Cloudflare   | No existing mapping                                           | Stable documented runtime POP field/environment variable                                           | Absent until verified                     |
| edge_pop/Axum, Spin           | Unavailable in v1                                             | No invented source                                                                                 | Absent                                    |

- [ ] Add red tests only for mappings with established sources. Test unavailable cells remain absent and do not inherit a default HTTP/1.1 value, hostname-derived POP, region-as-POP, fabricated ASN or full IP.
- [ ] Run adapter platform tests for selected mappings and `cargo test-fastly trace::context`. Expected red for supported new fields, existing omission tests remain green. If no source is established for a field, document it as unavailable and finish that field's discovery without runtime edits.

### N2: Add protocol/POP metadata without broadening disclosure

- [ ] For verified HTTP sources, add optional fields to `ClientInfo` (or the established adapter-owned inbound metadata extension) with defaults None. Preserve existing callers/constructors and do not infer protocol from a synthetic provider request or forwarding header.
- [ ] Use a closed protocol mapping for supported HTTP versions, bounded ≤32 bytes. Store it before the conversion point that would lose it; core receives only that normalized fact. Other adapters omit it until they can supply the same provenance.
- [ ] For a verified POP source, normalize printable Unicode/control rules and ≤32 UTF-8 bytes with the existing core helper. Invalid/unavailable/overlong values are omitted with only a bounded category; no source value enters logs.
- [ ] Test getter failures, absent metadata, values at/over bounds, control/bidi strings, and spoofed headers that cannot overwrite the adapter fact. Retain all masking/JA4/H2 exclusions and setup-vs-publisher labeling.
- [ ] Run `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`, and `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`. Expected: common schema, intentional availability differences, unchanged route/auth/cache behavior. Planned commit, only if a mapping was added: `Add verified optional trace protocol and POP facts`.

### N3: Populate verified ASN sources and verify the optional increment

- [ ] For Fastly's proven getter, add a test distinguishing a valid documented ASN from unknown/sentinel values, then populate `GeoInfo.asn` without copying city/coordinates or adding an active lookup beyond existing geo behavior. For Cloudflare, add a mapping only after N1 establishes the exact typed source; checked conversion failure omits it.
- [ ] Run selected geo tests to observe red, implement the minimal optional mapping and re-run. Update ordinary geo consumer tests if their intentionally observable GeoInfo now includes ASN; verify no identity/routing/consent behavior changes unintentionally.
- [ ] Test public network projection accepts only u32 ASN and emits no unsupported fallback. Viewer preserves unavailable labels and existing report validity/expiry/export equality. From JS lib run `npx vitest run test/trace/validation.test.ts test/trace/viewer.test.ts`.
- [ ] Document the actual resulting adapter support/provenance matrix and remaining unavailable cells. Verify masked IP/coarse geo privacy acceptance still applies and that optional field errors do not fail publisher delivery.
- [ ] Run all shared verification CI/build/doc gates and the trace browser suite. Re-run cache/auth/inactive gates because extra metadata crosses the shared platform type; feature-disabled behavior remains unchanged. Planned commit, only if ASN was added: `Add verified trace ASN projection and document platform support`.
- [ ] Handoff only implemented, verified optional facts. Future #1081/#1074/#1076 schemas remain separate work; do not mark them complete or add placeholders to the v1 report.
