# Issue 854 client CWV validation

Status: Implementation and local validation complete. Hosted checks pending after push. No deployment or production activation performed.

[Spec](../specs/2026-10-29-854-client-cwv-completion-design.md) · [Plan](../plans/2026-10-29-854-client-cwv-completion-plan.md)

## Baseline and environment

Baseline commit: `c36c82df43c2f62a59444290f863f736b579ed33`, clean before this extension. Node 24.12.0 and Rust 1.95.0. Local Viceroy is 0.17.0, matching this branch's `.tool-versions`. The hosted PR merge currently uses newer main-branch pins, including Viceroy 0.21.0.

Temporary logs, frozen artifacts and raw browser evidence are under `/tmp/ts-854-cwv/`. Baseline source was frozen with `git archive HEAD`, not copied from another working tree. Native fixture access to private production payload builders and the exporter are added only to that archive for measurement. The baseline exporter omits the new reservation path setter because baseline production code has no reservation feature.

One coordinator owns all Cargo commands, using `CARGO_TARGET_DIR=/tmp/ts-854-cwv/target`, `CARGO_BUILD_JOBS=2`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, and `CARGO_INCREMENTAL=0`. Builds explicitly select baseline or current bundles; private Rust build.rs output is never accepted solely by filename.

## Baseline and preflight checks

| Check                                                        | Tested state             | Result                                                                                                                  | Evidence                                                    |
| ------------------------------------------------------------ | ------------------------ | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| `npm ci` in JS, browser and docs roots                       | Baseline lockfiles       | Passed                                                                                                                  | `evidence/npm-{js,browser,docs}.log`                        |
| `node build-all.mjs --out-dir /tmp/ts-854-cwv/baseline-dist` | Baseline JS              | Passed, 13 modules                                                                                                      | `evidence/baseline-build.log`                               |
| `npx vitest run --maxWorkers=2`                              | Baseline                 | 1,490 tests across 51 files passed, no type errors, 40.03 s                                                             | `evidence/baseline-js.log`                                  |
| Launcher runtime preflight tests                             | Dynamic checkout pin fix | Both 0.17.0 and 0.21.0 accepted only when matching their checkout pin; mismatches rejected; preparation bypass retained | `browser/publisher-registration/launcher.test.cjs`          |
| `bash -n scripts/test-publisher-registration.sh`             | Dynamic checkout pin fix | Passed                                                                                                                  | Coordinator command                                         |
| `node --test browser/cwv/metrics.test.cjs`                   | New metrics code         | Five CLS/session-window/distribution tests passed                                                                       | Committed tests; combined with launcher, seven tests passed |

Paths beginning `browser/` above are under `crates/trusted-server-integration-tests/`.

## Retained iteration failures

The first native fixture compile used `IntegrationRegistry::new`, which exists only in unit-test builds. The fixture now uses the production `with_plan` constructor and compiled auction plan. This was fixture code, not a runtime defect. Log: `evidence/baseline-export-first-failure.log`.

The next fixture execution exposed missing required request-signing store identifiers in its fictional configuration. The fixture was corrected, then baseline and current exports completed successfully. No failed export is represented as a measurement pass.

## Existing hosted failure

Original failing job: https://github.com/IABTechLab/trusted-server/actions/runs/40604604795/job/122545660578

The shared setup action reads `.tool-versions` and restored Viceroy 0.21.0. The publisher-registration launcher then required a literal 0.17.0 and stopped before framework execution. The fix reads the authoritative pin from the checkout, preserving exact validation rather than weakening it. Local 0.17.0 success alone did not catch this merge-base drift. The separate original CodeQL check reported three high-severity "Bad HTML filtering regexp" annotations, all in test-only script counting/extraction. The regexes did not count uppercase script tags. Tests now use case-insensitive extraction and uppercase injection inputs. This strengthens the fixture assertions; no runtime injection defect is claimed, and the hosted CodeQL rerun is still required.

## Combined JavaScript and native checks

Full current Vitest passed 1,497 tests across 52 files in 35.95 s, with no errors in its configured type-test scope. Full JS ESLint and Prettier checks passed. Standalone `npx tsc --noEmit` is a separate failing baseline check: archived baseline produced 366 diagnostic lines, current produced 360 after test cleanup. No new-file diagnostics remain. This is not a claim that package-wide TypeScript compilation passes.

The native core suite exposed two different issues. One attribution test searched for an attribute name anywhere in HTML and found the new inline program's attribute-reading code. It now checks the actual attribute value. Eleven Settings loader failures are pre-existing Cargo environment pollution: Cargo injects `TRUSTED_SERVER__GIT_VERSION` from build.rs into test execution, and the runtime Settings environment loader treats it as an unknown configuration field. The archived baseline reproduces `settings::tests::test_set_env` failing. Running the exact current test executables without that injected runtime variable passed 2,983 core tests plus both ten-test native integration suites. Logs: `evidence/current-core-native.log`, `baseline-core-env-failure.log`, `current-core-native-clean-env.log`, `current-core-native-final-clean-env.log` and `current-core-executables.jsonl`.

Focused native checks also passed 14 reservation-filter tests, 54 TSJS-filter tests and the supported-profile/active-diagnostics HTML checks. The first broader adapter test pass completed Fastly, reusable Fastly, Axum, Cloudflare, Spin, parity, CLI and build-digest gates. Clippy then found a missing panic doc on the new fixture access wrapper. Those pre-CSP checks are retained under `evidence/gates-pre-csp`; affected gates were rerun after the compatibility fix rather than silently reusing the earlier source state; final results are below.

The current distribution has 14 modules. Final normalized-transport publisher-registration browser contracts passed all 109 cases in 85 s; the unchanged initial-render comparison passed all ten scenarios in 9.5 s. Final fixture/launcher tests passed 14 cases. Both browser runners used the same current core/GPT/Prebid bundle bytes and real external Prebid v10.26.0. External Prebid SHA-256: `32a7f07131047948134510d57c7f4eeb832e1ae0c75d8b61d6fd38aab2be780c`.

## Controlled before/after measurement

Command:

```sh
node crates/trusted-server-integration-tests/browser/cwv/run.cjs \
  --baseline /tmp/ts-854-cwv/baseline-export \
  --loading /tmp/ts-854-cwv/final-unreserved \
  --combined /tmp/ts-854-cwv/final-reserved \
  --runs 5 --out /tmp/ts-854-cwv/evidence/cwv-final-normalized
```

Final normalized-transport run passed 61 cases in 135 s: 40 alternating repeated measurements and 21 functional cases, including all three review regressions. Every timing group contains exactly five measurement samples; correctness cases are separate groups. Browser 145.0.7632.6, Node 24.12.0, fresh contexts, loopback HTTP and a deliberately held 750 ms runtime response. CPU was unthrottled on a shared host while Rust checks ran. First-visible is an animation-frame/DOM estimate. Large paint gains demonstrate removing that controlled parser-blocking wait, not a forecast of production improvement.

| Viewport    | Arm              | FCP/LCP median and range, ms | CLS median | Native request median, ms | Visible estimate median, ms |
| ----------- | ---------------- | ---------------------------- | ---------- | ------------------------- | --------------------------- |
| 390 × 844   | Baseline         | 856, 828 to 872              | 0.207065   | 845                       | 1098.7                      |
| 390 × 844   | Loading only     | 100, 96 to 144               | 0.207065   | 843                       | 1098.7                      |
| 390 × 844   | Reservation only | 896, 840 to 1012             | 0          | 907                       | 1143.0                      |
| 390 × 844   | Combined         | 104, 48 to 164               | 0          | 824                       | 1068.7                      |
| 1280 × 1000 | Baseline         | 844, 820 to 860              | 0.111133   | 861                       | 1093.6                      |
| 1280 × 1000 | Loading only     | 64, 44 to 96                 | 0.111133   | 805                       | 1020.0                      |
| 1280 × 1000 | Reservation only | 832, 804 to 916              | 0          | 845                       | 1064.2                      |
| 1280 × 1000 | Combined         | 72, 52 to 196                | 0          | 805                       | 1045.8                      |

The reservation-only arm is explicitly a CSS-only isolation derivative: baseline production HTML and assets plus the exact Rust-generated reservation style from current output. It is not described as a baseline feature. Raw derived bytes, manifests, observer entries, targeting, screenshots, traces and distributions are retained in `evidence/cwv-final-normalized/`. The original 58-case `evidence/cwv-final/` is retained as a superseded transport-fixture version, not the final input state. Lockfiles were retained after the run with an explicit provenance sidecar, not represented as regenerated output.

Functional cases covered inline/shared payloads, automatic/registered slots, delayed/failed/absent/late runtime, actual creative DOM through Universal Creative, child replacement, absent wrapper, unmatched path, unknown dimensions, breakpoint boundaries and no-fill. Each matched case made one native request; unmatched paths made none. A 420 px creative exceeding the 250 px minimum still shifted. A 600 px creative shifted at width 1023 and did not shift at width 1024. No-fill retained 250 px, and desktop 250 px fill left the explicitly configured 350 px whitespace.

### Transfer cost

This is not a size-reduction result. Raw inline JavaScript grew from 57,550 to 112,674 bytes. The retained raw GPT bootstrap is 56,429 bytes, gzip 12,953 and Brotli 11,113. The compiled early entry adds 55,123 bytes, gzip 18,072 and Brotli 16,001. Fetched runtime shrank from 91,321 to 56,932 bytes. Estimated combined HTML-plus-runtime gzip bytes grew from 42,104 to 49,617, an extra 7,513 bytes. These are compression estimates, not a throttled real-network transfer measurement. The extra early code preserves APIs, guards and rendering when runtime is delayed or absent.

### Measurement iteration failure

The first CSS-only derivative selected every style-looking substring and accidentally copied an iframe-template style string from inline JavaScript. That iteration is retained but superseded. The runner now selects only the exact `data-ts-slot-reservations` production tag, with a regression test. Seven CWV unit tests passed before the final 61-case comparison.

## Internal review and fix loop

Four lanes reviewed fingerprint `10b37d03ab8a17fb5055716bb9feeeb4adb44c16ee23050f5593eb23a80dac00`. Correctness and architecture were fresh workers; style and catalogued code-smell inspection reused implementation workers and did not claim independent authorship. No catalogued smells were found.

- C1/A1 reproduced a default compatibility regression in Chromium. A response policy allowing same-origin external scripts and all old inline hashes blocked only the new compiled bootstrap. Baseline core APIs worked; current core APIs stayed undefined after load and early publisher code threw. Fixed with conservative synchronous fallback for enforced response CSP, including configured response headers, not merely a deployment warning. Six focused Rust CSP checks passed, including actual-origin and shared cold/warm paths.
- C2 found the actual framework's renderer-absence control still blocked only the old unified URL. The new path installed the renderer and never exercised the intended absence. The control now removes the compiled early entry and blocks the actual runtime. A separate failed-runtime control retains early rendering. Both pass parser-driven real-creative checks and the final actual Next.js inline/shared framework gate.
- T1/T2 final validation-document and launcher-test formatting were corrected. The final launcher/fixture test command passes all 14 cases.

A deferred-only runtime entry was considered but not required in this patch. It needs another lifecycle entry and changes standalone recovery behavior; measured savings are not established. Extra bytes remain disclosed rather than relabeled as improvement.

### Independent fix verification

Correctness and architecture re-reviewed code/test/fixture/CI fingerprint `cde724a8e04098c81fb914d1d2d8bebb229c2c7dba73c181fa4002459f5afacb`, excluding documentation being finalized. Both reported no remaining findings. Independent Chromium checks used actual baseline inline hashes plus same-origin external permission, not hashes selected from the changed page. Core APIs and immediate queue behavior now work, with zero CSP violations. Genuine renderer absence yields one ordinary fill without candidate targeting; failure of only the deferred runtime preserves the early bridge and candidate fill. Twenty focused Node tests passed.

The reviewed fingerprint included a Playwright `--list` discovery artifact with 20 empty/skipped result records. It was removed rather than committed or counted as execution. Source-equivalent cleanup fingerprint: `3e26f0810225b31e84ee128b583589222e303e1c20ffa2604d177bd1983c7964`.

Final unreserved/reserved exporter manifests were checked against all 12 source/toolchain/lockfile fingerprints and embedded exporter text. All prior ordinary generated HTML documents and served JS assets compare byte-for-byte equal to the final exports. Those results remained applicable after the CSP fix. The transport fixture was then normalized as described below, and all 61 browser cases plus all 109 offline contracts were rerun for that exact final fixture hash. Three new focused regressions passed in the worker run and an independent correctness run. Their exact command uses `--baseline /tmp/ts-854-cwv/baseline-export --loading /tmp/ts-854-cwv/final-unreserved --combined /tmp/ts-854-cwv/final-reserved --regressions-only`.

Final manifest hashes:

- Unreserved: `d42aa228b5609dce03bed88f7633b931c10018d56d04f3c815a95fe4be4ef9ef`.
- Reserved: `722d560435a534b51d73bbf8384300028a1d0ece38610cd8544878e01a150972`.
- Baseline: `e903286285cab8ca79d0b334e6012a136f7d76975a813dc00eeb76d54664b5fe`.

## Final framework, artifact and backend gates

### Real build and framework

With `TSJS_PREBUILT_DIR` unset, `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1` passed in 11m28s. Its real build.rs generated all 14 modules; every file exactly matches the independently built distribution, and every complete module string is embedded byte-for-byte in the final WASM.

WASM SHA-256: `8477c69f44e01318321a31fbd881bf0cf6c610260561d4d00e6e52c85cb48e2a`.

The source-labelled Next.js image `test-nextjs:publisher-registration-25b7eefeb9c2f758` built successfully. Docker printed two pre-existing warnings about unset default build-argument values, but the actual build used the pinned Node argument. Full/reduced actual-provider transport preflights each observed two requests, admitted the intended winner and accepted legal no-bid. The selected backend timers were 1000 ms and 250 ms; certificate checking remained enabled.

The first framework run passed 18 of 20 inline cases. Two new narrow-fill geometry assertions correctly failed: the simulated native iframe declared 300 by 250 but its browser-default border made its actual box 304 by 254, with another four pixels of inline baseline spacing expanding the wrapper to 258. This was fixture chrome, not reservation policy. The transport now sets only its own outer iframe to block layout with zero border. Production CSS and the strict 250 px assertions were not changed. The real Chromium geometry matrix confirms the old default, old lab-CSS normalization, new default and new lab-CSS cases.

The final `scripts/test-publisher-registration.sh` run used `--bundle-mode build-rs --reuse-artifacts`, the same verified WASM/image/configs, and the normalized fixture. It passed all 40 actual Rust/Next.js cases: 20 inline in 27.4 s and 20 shared-template in 34.1 s. These include all six pre-hydration responsive/fill/no-fill cases, real registration/navigation lifecycle, true renderer absence, independent runtime failure, visible actual creative content and served-byte equality. No Cargo or Docker build was needed for that reuse run. Evidence: `evidence/framework-normalized/`; the prior failed run remains in `evidence/framework/`.

Because the shared transport bytes changed, the entire 61-case lab and 109-case offline suite were rerun, rather than treating the older 58-case input state as current. CLS is identical in all eight repeated viewport/arm groups before/after the transport correction; timing ranges are wider on the concurrently loaded host. No ad-startup improvement is inferred from those noisy timing differences.

Final transport SHA-256: `a9921ac1863db659094edbb079283458c7270d417e0043357f01a7f705b8a12e`.
Final normalized sample SHA-256: `a2f6940891d2021168ef5e9de4e1c739e5b277f13e00ace219d0222649021d8e`.
Final reviewed code-only fingerprint: `917beeb91a6709a9194417e57cfa175be4dcd5ad9738edb5d3331daacfa6afd5`. Independent correctness review accepted the fixture-only delta and its JSDOM regression without findings.

### Final backend gates

All 21 post-fix commands below passed. Commands used the coordinator environment described above and `TSJS_PREBUILT_DIR=/tmp/ts-854-cwv/current-dist`; the separate release build above deliberately did not use prebuilt JS. Per-command logs and the exact ledger are in `evidence/gates/`. Passing suites retain their existing ignored tests where the command's normal policy excludes them; no new skip was introduced.

| Gate                                 | Result | Seconds | Command                                                                                                                                       |
| ------------------------------------ | ------ | ------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| test-fastly                          | Passed | 84      | `cargo test-fastly`                                                                                                                           |
| test-fastly-reuse                    | Passed | 34      | `cargo test-fastly-reuse`                                                                                                                     |
| test-axum                            | Passed | 46      | `cargo test-axum`                                                                                                                             |
| test-cloudflare                      | Passed | 33      | `cargo test-cloudflare`                                                                                                                       |
| test-spin                            | Passed | 5       | `cargo test-spin`                                                                                                                             |
| parity                               | Passed | 19      | `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --target x86_64-unknown-linux-gnu --test parity`               |
| test-cli                             | Passed | 45      | `cargo test --package trusted-server-cli --target x86_64-unknown-linux-gnu`                                                                   |
| template-build-digest                | Passed | 15      | `cargo test -p trusted-server-core --target x86_64-unknown-linux-gnu --test template_build_digest`                                            |
| clippy-fastly                        | Passed | 48      | `cargo clippy-fastly`                                                                                                                         |
| clippy-axum                          | Passed | 50      | `cargo clippy-axum`                                                                                                                           |
| clippy-cloudflare                    | Passed | 30      | `cargo clippy-cloudflare`                                                                                                                     |
| clippy-cloudflare-wasm               | Passed | 57      | `cargo clippy-cloudflare-wasm`                                                                                                                |
| clippy-spin-native                   | Passed | 1       | `cargo clippy-spin-native`                                                                                                                    |
| clippy-spin-wasm                     | Passed | 36      | `cargo clippy-spin-wasm`                                                                                                                      |
| clippy-cli                           | Passed | 107     | `cargo clippy-cli`                                                                                                                            |
| clippy-codegen                       | Passed | 9       | `cargo clippy-codegen`                                                                                                                        |
| clippy-template-build-digest         | Passed | 51      | `cargo clippy -p trusted-server-core --target x86_64-unknown-linux-gnu --test template_build_digest -- -D warnings`                           |
| clippy-cwv-example                   | Passed | 50      | `cargo clippy -p trusted-server-integration-tests --target x86_64-unknown-linux-gnu --example cwv_html -- -D warnings`                        |
| clippy-publisher-registration-helper | Passed | 1       | `cargo clippy -p trusted-server-integration-tests --target x86_64-unknown-linux-gnu --example publisher_registration_backends -- -D warnings` |
| doc-core-js                          | Passed | 17      | `cargo doc -p trusted-server-core -p trusted-server-js --target x86_64-unknown-linux-gnu --all-features --no-deps`                            |
| wasm-clock                           | Passed | 10      | `scripts/lint-wasm-clock.sh`                                                                                                                  |

Full Rust formatting, JS lint/format, docs lint/format and Markdown formatting outside docs passed. VitePress built successfully, with existing VCL-highlighting and chunk-size warnings. The final build output is retained in `evidence/docs-build-final.log`. The integration workflow parsed successfully through the repository's YAML formatter. ShellCheck/actionlint were unavailable, so their checks are not claimed. The new fixture/launcher and CWV unit commands passed 14 and seven cases respectively.

The standalone full TypeScript and Cargo-injected native Settings failures described above remain disclosed baseline limitations. The final clean-environment native core result is 2,983 plus both ten-test integration suites, not a claim that the unmodified failing Cargo invocation passed.

## Evidence limits

No real GPT/GAM staging, customer-page measurement, production activation or field CWV validation has run. The controlled browser fixtures use fictional bids and simulated GPT transport. A first-visible timestamp is a frame/DOM estimate, not hardware presentation or a GAM impression timestamp. Initial-document reservation does not install new CSS for subsequent client-only SPA routes.
