# Publisher registration implementation validation

Status: Implementation in progress. Task 0 transport preflight passed; opt-in delivery implementation and combined verification are underway.
No production deployment, activation or real-GPT staging is authorized.

[Plan](../plans/2026-10-27-854-publisher-registered-initial-delivery-plan.md) ·
[Design](../specs/2026-10-27-854-publisher-registered-initial-delivery-design.md)

## Environment and evidence

Base: `5bc38862512c5c71085b5052bc3e528ee9b9987d`.
Worktree: `issue-854-publisher-slot-registration`.
Local logs and generated artifacts: `/tmp/ts-854-implementation/`.

The user authorized implementation and a draft PR on 2026-10-27. Initial dependency
installation failed with `EDQUOT` because the tmpfs user quota was exhausted.
With subsequent user permission, the coordinator removed two inactive Cargo
caches, recovering about 8.2 GB. Their source snapshots and saved evidence were
retained. Process arguments, environments and memory maps had no references to
the selected caches.

Dependencies were then installed from this worktree's package manifests into
isolated temporary directories. Ignored local `node_modules` symlinks point there.
No dependencies or experimental source were copied from another checkout.

Cargo has one command owner and uses `/tmp/ts-854-implementation/target`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`,
and `CARGO_BUILD_JOBS=2`. Frozen baseline JS is at
`/tmp/ts-854-implementation/baseline-dist`. Artifact reuse must match the source
and bundle fingerprints, not merely an existing filename.

## Clean-base checks

All runtime source remained unchanged during these checks. The complete baseline
source listing is `evidence/baseline-source.sha256`; its SHA-256 is
`94c788d8d7604968a7e3e55907ebd1f4b4358408859618b6e9f6627d60499f19`.
Paths below are relative to the temporary evidence directory. JS commands run in
`crates/trusted-server-js/lib` unless stated otherwise.

| Check                                                                                                                                                           | Result                                                                                                          | Time                                  | Log                                                       |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------- | --------------------------------------------------------- |
| `npm ci` for JS and browser dependencies; Playwright Chromium install                                                                                           | Passed after quota cleanup                                                                                      | Not separately timed                  | `npm-js.log`, `npm-browser.log`, `chromium-install.log`   |
| `node build-all.mjs --out-dir /tmp/ts-854-implementation/baseline-dist`                                                                                         | Passed                                                                                                          | Not separately timed                  | `baseline-build.log`                                      |
| Focused GPT `ad_init`, `gpt_bootstrap`, `schedule_initial_ad_init`, `spa_hook` Vitest suites, `--maxWorkers=2`                                                  | 215 passed; no type errors                                                                                      | 5.09 s                                | `baseline-gpt.log`                                        |
| `npx vitest run --maxWorkers=2`                                                                                                                                 | 1,211 tests in 50 files passed; no type errors                                                                  | 79.53 s                               | `baseline-js-full.log`                                    |
| Repository-root `node --test crates/trusted-server-integration-tests/browser/initial-render/pages.test.cjs`                                                     | 2 passed                                                                                                        | 0.99 s                                | `baseline-pages.log`                                      |
| Repository-root `node crates/trusted-server-integration-tests/browser/initial-render/run.cjs --out /tmp/ts-854-implementation/evidence/baseline-initial-render` | All 10 runtime/bootstrap scenarios passed, with real Prebid v10.26.0 and visible server/client creative content | 8.8 s including external Prebid build | `baseline-initial-render.log`, `baseline-initial-render/` |

The prior checkout's broad-suite failures were not reproduced on this clean base.
They are not exclusions for this implementation.

## Task 0 and regression preparation

The native mapping helper passed all three tests on the explicit
`x86_64-unknown-linux-gnu` target in 4m45.4s. Command, input hashes and log are under
`evidence/task0/native-helper*`. The source-labeled Next.js fixture image built
in 50.5s. These do not establish a provider-transport pass.

The first transport launcher built the baseline WASM in 6m20s, then stopped during
application configuration validation because the isolated fixture lacked the
required admin-protection handler. The next attempt reached Viceroy, but the
fixture's epoch-zero TC timestamps correctly failed production consent expiry
checks. Both inputs were fixed only in the test fixture. Failed attempts remain
at `evidence/transport-config-failure/` and `evidence/transport-consent-failure/`.

The final `--transport-smoke` passed in 15.010s with Viceroy 0.17.0. Each budget
case received two actual Rust OpenRTB POSTs at `/api/fixture-auction`, admitted the
known 300x250 winner and accepted legal no-bid with an empty bid map. Full budget
reused the mapped 1000 ms backend; reduced budget reused the mapped 250 ms backend.
All observed provider names matched the eight shared-policy-generated mappings.
The application endpoint stayed HTTPS and certificate checking stayed enabled.

The coordinator inspected both `admission.json` records, actual route exchanges,
Viceroy logs, exit-zero `outcome.txt` and final owned-file hashes. Exact commands,
versions and timings are in `evidence/task0/handoff.md`; runtime evidence is under
`evidence/transport/{full,reduced}/`.

- Baseline WASM SHA-256: `e8435647841fe54d0413ab41253249f182ee18f70f0a84ebc339ca29821833fa`.
- WASM source fingerprint: `e8a77df4c24c8835366cc125006b431aaa544d67354f3a25f798184956e51112`.
- Frozen bundle-set fingerprint: `056f0fd185cf5577a58583a12343e07f0ab3147e9e111057fe3c0208a1d284cc`.

This is the Task 0 baseline transport pass, not verification of the feature or its
final combined artifacts. The framework run must use updated feature assets and
must disable debug ADM replacement to prove actual Universal Creative rendering.

The new offline fixture passed eight pure tests, including repeated native call
counting, enabled-load SRA exposure, disabled-load isolation, targeting snapshots,
escaping and refresh registration. Its explicit baseline runner passed all 18
runtime/bootstrap/handoff controls in 8.658s total case time, retaining traces and
screenshots under `evidence/offline/baseline-refresh-registration/`. Reordered and
narrowed sizes cause a second request. Identical-size aliasing after subtree
replacement has one request but no creative in the final element. That is asserted
as a visibility failure, not accepted as correct delivery.

The default new-mode runner failed because protocol version 1 is absent, with
failure artifacts at `evidence/offline/pre-feature/`. It does not allow expected
failures to become a fixed-contract pass. The earlier disabled-load exploratory
control failure is retained too; it included a legitimate later Prebid refresh
and was replaced by the intended enabled-load automatic controls.

## V3 implementation checks and additional history defect

The coordinator verified the five frozen behavior-source hashes before and after
building all 13 feature bundles and the paired Fastly WASM. The full JS suite
passed 1,415 tests in 51 files with no type errors in 60.37s. Library lint and
formatting, full documentation formatting, outside-docs Markdown formatting and
Actionlint with ShellCheck passed.

The offline suite passed all 105 fixed cases, with actual Universal Creative,
ordinary publisher creatives, real Prebid first-refresh bypass and later auctions,
identity, size changes, hidden/cancelled work, mapping ownership and stale callback
checks. The existing initial-render suite passed all ten scenarios on the same V3
artifact hashes. The offline worker's exact commands, hashes and red fixture
correction are recorded under `evidence/offline/`; the final run is
`fixed-v3-adrequest/`. No runtime change was needed for that fixture correction.

The real Rust/Next.js launcher passed 13 inline and 13 genuine shared-template
cases, with no skips or unexpected/flaky results. Current provider transport passed
again at both timeout budgets, with debug ADM injection disabled. Actual served
JS bytes matched the selected distribution. Evidence is `evidence/framework/` and
`evidence/task8/handoff.md`. The final combined run took 58.498s. The shared request
transport removes routing's synthetic no-cache semantics and fulfills the actual
Rust response unchanged; the strict shared seam and authorized template storage
were observed, not replaced with handwritten browser bids.

A coordinator probe then found a missing case: with only the raw bootstrap loaded,
a real pathname change after A admission left generation zero unchanged. A fresh
B registration made one ordinary GAM request with A's targeting. The executable
probe and exact result are `bootstrap-history-probe.cjs` and
`evidence/bootstrap-history-probe.json`. The bounded independent architecture
review confirmed this violates current-navigation admission, rather than a
publisher precondition. Its report is `bootstrap-navigation-architecture.md`.

V4 is authorized to add provisional bootstrap navigation observation, transfer
one counter owner to the runtime, and reuse the existing page-bids path for
late-runtime catch-up without retiring current-generation waiting registrations.
No fetch loop belongs in the bootstrap. V3 passing cases remain useful neighboring
proof but do not clear this newly reproduced defect.

The full Rust gate matrix ran. Two initial failures were the same introduced test
locator error: `.find("</body>")` matched a comment inside the included head script.
The cfg(test)-only correction uses the actual closing body location. Native GPT,
full Fastly tests, formatting and relevant lint were re-executed successfully;
other passing gates are recorded at `evidence/rust-final/REPORT.md`. Updated
combined-artifact verification remains pending V4.

## Final V4 source and browser proof

The final five-source build manifest is
`evidence/registration-source-stable-v4-final.sha256`. Earlier V4 manifests are
historical. Before rebuilding, a second coordinator check found that failed
catch-up followed by a same-path retry incremented the generation and cancelled
valid waiting registrations. Six strengthened assertions reproduced it. The
bounded fix reuses the captured catch-up path and generation through the existing
fetch/apply function, without another transition, cleanup or controller engine.
Eleven targeted checks then passed. Runtime-only legacy retry policy is unchanged.

All seven focused suites passed 794 tests with zero skips in 34.89s. The common
contract has 68 scenarios across four implementations, plus three bootstrap-start
instances of the returned-A failure case. Nonapplicable runtime-only catch-up is
not declared as a skipped test. The complete current JS suite passed 1,490 tests
in 51 files, with no type errors, in 108.30s. Library lint and formatting passed.
Logs are `registration-v4-retry-focused.log` and `final-v4-js-all.log` under
`evidence/`.

The original raw-bootstrap history probe now reports generation one,
`waiting_payload` and zero native requests on B. Its V3 source/result remain
preserved; the new result is `evidence/bootstrap-history-probe-v4.json`.
All 109 current offline browser cases passed in 130.301s. They include actual
push/replace/popstate and A-to-B-to-A history, same-token/runtime catch-up,
a forced late B response after C, and a controlled 500 followed by a same-path
retry preserving native, element, token and generation. No page errors occurred.
The one console error is the intentional 500 response. The previous ten-scenario
initial-render suite also passed on the same four loaded V4 artifact hashes,
with no page errors, in 4.928s. Evidence is `evidence/offline/final-v4/`,
`evidence/legacy-final-v4/` and their sibling logs.

The coordinator checked all five source hashes before and after building 13 new
modules into `feature-dist-v4`. A real release build with `TSJS_PREBUILT_DIR`
unset rebuilt and embedded all 13 modules through `build.rs`; it passed in
5m19.009s. Its immutable WASM is
`final-v4/trusted-server-adapter-fastly.wasm`, SHA-256
`9935a545f27a22cf63bf81108ed624f604b2ec0c1a0ee12d4e9012b68dae354f`.
Source checks and full bundle hashes are in `evidence/feature-v4-wasm-source-check.log`
and `evidence/final-v4-artifacts.sha256`. The coordinator independently confirmed
that the WASM contains all 13 exact independently built module byte sequences;
`evidence/final-v4-build-rs-byte-parity.json` records every comparison.

Four independent read-only reviewers share the exact 32-file implementation,
fixture, CI and operator-guide target, including all untracked files. Its fingerprint
is `669bfc753a5b4360f856adfa0a38dbecb7c097c49846048f13c3dee58916ae42`.
Historical planning/review progress records are excluded so this ledger can be
updated. Correctness, architecture and style report no findings. The coordinator
verified the code-smell reviewer's only candidate, a nonblocking duplicated
Prebid byte-verification helper opportunity, and left it unchanged. The
[combined implementation review](2026-10-27-854-publisher-registered-initial-delivery-implementation-review.md)
records coverage, R1's disposition and proof limits.

## Final V4 framework, CI preparation and Rust checks

Actual preparation passed in 35.201s with Viceroy absent from the restricted PATH
and `TSJS_PREBUILT_DIR` unset. The log records both conditions and the actual
build.rs rebuild of 13 modules. The normal integration configuration was also
generated from the unchanged normal fixture/template with the current prepared
native generator. Evidence is `evidence/task8-final-v4-preparation-command.log`.

The final strict artifact-reuse run passed all 26 real Rust/Next.js cases,
13 inline and 13 shared, with zero skipped, unexpected or flaky cases. It passed
fresh full/reduced provider transport and actual served-module equality checks.
The first run passed in 60.896s; an instrumentation-only argv separator correction
then captured every tool call in a second passing run of 67.033s. The final scoped
spy records exactly one `cargo --version`, ten Docker runtime/inspect calls,
zero Cargo build/run and zero Docker build calls. No source change or runtime
fix was needed. Evidence is `evidence/task8-final-v4-no-build-summary.json`,
`task8-final-v4-framework-v2-command.log` and the paired framework evidence.

All 19 final Rust commands passed freshly on V4 in 491.948s total command wall
time. These include native GPT 49/49, full Fastly 3,220 passing with ten existing
ignored tests, Fastly reuse 221, Axum 48, Cloudflare 54, Spin 88, parity 17 and
template digest ten. All required Clippy gates and the additional native backend
example Clippy check passed. All 39 whole-target snapshots matched the reviewed
fingerprint, and the five behavior sources, immutable WASM and 13 selected bundle
hashes stayed unchanged. `evidence/rust-final-v4/REPORT.md` records exact commands,
per-gate environment, durations, exits and hashes. V3 evidence remains separate.

The full docs format check, outside-docs Markdown formatting, WASM clock lint and
relative-link checks passed. Later progress-note edits receive focused formatting
checks. The nine changed Markdown files had 16 checked relative links and no
missing targets. Independent style review also passed Actionlint, ShellCheck,
fixture formatting and untracked whitespace checks.

## Final normal browser matrix

Both standard framework runs exited zero with their unchanged application config,
normal logging, timeout/retry policy and one worker. Next.js passed 21 tests with
14 expected skips in 28.787s command wall time. WordPress passed 11 tests with
24 expected skips in 29.571s. Those skips are the dedicated registration guard and
the other framework's tests, not excluded failures. The dedicated registration
cases already passed separately with zero skips.

The ordinary CI Prebid command omits a user-ID override and therefore selects
nine default modules. Its exact separate artifact built in 3.196s, with SHA-256
`166d18ff74d4e9d023a01468ee433d4f5b75b9fd7c7ab60639f0396a74d9072b`.
The dedicated registration profile remains Rubicon plus SharedId, matching its
own CI preparation command. No mismatched subset artifact was used for normal
matrix claims. Only ignored build outputs changed during this preparation.

Normal Next.js printed two nonfatal pinned-Viceroy task panics at
`session/downstream.rs:121`, `response receiver is open: ()`. All tests passed and
the server continued. The exact stderr remains in its command log. No clean-base
reproduction was performed, so these warnings are unclassified, not declared
unrelated or silently omitted. Normal WordPress completed without a test failure.
Evidence is `evidence/task8-final-v4-normal-{nextjs,wordpress}/` and sibling
normal artifact/config logs. Managed teardown stopped the owned runtime and
containers. Root free space fell to about 235 MB; no further heavy work or root
cleanup was authorized.

## Remaining external gates

- Hosted GitHub CI has not yet run for this branch. Local preparation/reuse
  execution does not claim a hosted workflow pass.
- Real publisher adoption and separately approved real-GPT staging remain external
  deployment gates. Local fixtures do not prove a live-site fix or CWV improvement.
