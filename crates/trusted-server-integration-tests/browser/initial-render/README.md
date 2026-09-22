# Initial render browser regression

This standalone regression loads the production TSJS core, GPT and Prebid
bundles, a real external Prebid build, and Prebid Universal Creative in Chromium.
It checks whether an overlapping publisher auction can submit a second native
GPT request and replace the initial server creative.

From the repository root:

```sh
npm ci --prefix crates/trusted-server-js/lib
npm ci --prefix crates/trusted-server-integration-tests/browser
(cd crates/trusted-server-integration-tests/browser && npx playwright install chromium)
(cd crates/trusted-server-js/lib && node build-all.mjs)
node crates/trusted-server-integration-tests/browser/initial-render/run.cjs
```

The runner builds external Prebid in a temporary directory with the existing
builder. To reuse an artifact, supply `--prebid /path/to/artifact-directory`; that
directory must contain its `manifest.json` and corresponding bundle. The selected
Rubicon adapter and Shared ID module match the existing production artifact tests;
no real bidder endpoint is contacted during browser scenarios.

By default both runtime-only and bootstrap-before-runtime variants run. Select
one using `--variant runtime` or `--variant bootstrap`. The bootstrap variant loads
the actual injected bootstrap before the runtime, exercising the lifecycle
listeners that persist after runtime installation. It does not test bootstrap-only
operation when runtime fails to load.

| Publisher auction timing                     | Expected native requests | Visible creatives   |
| -------------------------------------------- | ------------------------ | ------------------- |
| Before TS claims the slot                    | 1                        | Client              |
| Before `slotRequested`                       | 1                        | Server              |
| After `slotRequested`, before initial render | 1                        | Server              |
| After initial render                         | 2                        | Server, then client |
| Independent refresh after initial render     | 2                        | Server, then client |

In the before-`slotRequested` control, the native request has already been
recorded; only its event delivery is held. In both overlap cases the publisher
auction starts before render and finishes after render. This exercises suppression
of a late callback without suppressing a genuinely subsequent auction.

Run against bundles built from the unfixed code with `--baseline` to assert the
original overlap behavior (two requests and a creative replacement). The bootstrap
source must also come from that revision if testing its baseline. Without this
flag, unfixed bundles must fail the overlap assertion.

Screenshots, Playwright traces, event/request records, and artifact hashes are
written to a temporary directory printed at startup. Use `--out /path/to/output`
to choose a directory outside the tracked source tree. Open a trace with the
browser package's `npx playwright show-trace /path/to/trace.zip`.

## Evidence boundaries

All browser requests are intercepted for fictional `example.com` origins. The
fixture supplies SSAT data and an OpenRTB client response. It deliberately controls
auction completion and GPT event delivery; there are no timing sleeps deciding
which auction wins.

GPT/GAM itself is simulated. The fixture records native requests without applying
ownership policy, then explicitly delivers the targeted creative through real
Universal Creative in a separate-origin iframe. Assertions inspect the actual
creative DOM as well as request counts. This demonstrates the admission and
replacement mechanism; it does not establish how a real GAM auction would choose
a winner or prove that a specific live site's flicker has the same cause. Rust
auction execution and HTML injection are outside this regression's scope.
