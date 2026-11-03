const test = require("node:test");
const assert = require("node:assert/strict");
const {
  correctnessCases,
  assertCorrectnessSamples,
  unavailableErrors,
  assertCspDocument,
  assertUnavailableRuntime,
  assertDeferredRegression,
} = require("./contract.cjs");
const runtimeUrl = "http://127.0.0.1/static/tsjs=tsjs-unified.min.js?v=example";
const methods = [
  "setConfig",
  "getConfig",
  "requestAds",
  "addAdUnits",
  "renderAdUnit",
  "renderAllAdUnits",
  "adInit",
  "scheduleInitialAdInit",
  "registerPublisherSlot",
];
const apis = (available) => ({
  methods: Object.fromEntries(
    methods.map((name) => [name, available ? "function" : "undefined"]),
  ),
  shim: available ? "function" : "undefined",
  registrationVersion: available ? 1 : null,
  rendererVersion: available ? 1 : null,
});

for (const regressionsOnly of [false, true])
  test(`full correctness case completeness rejects missing, duplicate and reordered cases, regressionsOnly=${regressionsOnly}`, () => {
    const cases = correctnessCases(regressionsOnly);
    assert.equal(cases.length, regressionsOnly ? 4 : 24);
    const samples = cases.map((entry) => ({ ...entry, stage: "correctness" }));
    assertCorrectnessSamples(samples, regressionsOnly);
    for (const changed of [
      [],
      samples.slice(1),
      [...samples, samples[0]],
      [...samples].reverse(),
    ])
      assert.throws(() => assertCorrectnessSamples(changed, regressionsOnly));
    assert.throws(() =>
      assertCorrectnessSamples(
        samples.map((entry) => ({ ...entry, stage: "measurement" })),
        regressionsOnly,
      ),
    );
    if (!regressionsOnly) {
      assert.deepEqual(
        cases.filter((entry) => entry.control).map((entry) => entry.control),
        ["runtime-deferred", "reservation-removed"],
      );
      for (const name of ["inline", "registered"])
        for (const mode of ["failed", "absent", "late-runtime"])
          assert(
            cases.some((entry) => entry.name === name && entry.mode === mode),
          );
      for (const width of [1023, 1024])
        assert(
          cases.some(
            (entry) => entry.name === "tall" && entry.viewport.width === width,
          ),
        );
    }
  });

test("CSP cases must actually enforce policy, including full scopes rather than only diagnostics", () => {
  assertCspDocument({ name: "csp", enforcedCsp: true });
  assertCspDocument({ name: "inline", enforcedCsp: false });
  for (const changed of [
    { name: "csp", enforcedCsp: false },
    { name: "csp" },
    { name: "inline", enforcedCsp: true },
    { name: "registered" },
  ])
    assert.throws(() => assertCspDocument(changed));
});

test("deferred negative control must reproduce the broken early API and parser contract, not just carry a control label", () => {
  const raw = () => ({
    early: { core: false, queued: false },
    scriptLoads: [{ src: runtimeUrl, articlePresent: true }],
    apis: apis(true),
    fixture: { requests: [], creatives: [] },
    delivered: false,
    firstVisibleEstimateMs: null,
  });
  for (const settings of [
    { cache: "cold", runtimeDelayMs: 0 },
    { cache: "warm", runtimeDelayMs: 750 },
    { cache: "cold", runtimeDelayMs: 750 },
  ])
    assertDeferredRegression(raw(), runtimeUrl, 100, 800, settings);
  for (const change of [
    (raw) => {
      raw.early.core = true;
    },
    (raw) => {
      raw.early.queued = true;
    },
    (raw) => {
      raw.scriptLoads[0].articlePresent = false;
    },
    (raw) => {
      raw.scriptLoads = [];
    },
    (raw) => {
      raw.apis.methods.registerPublisherSlot = "undefined";
    },
    (raw) => raw.fixture.requests.push({}),
    (raw) => raw.fixture.creatives.push({}),
    (raw) => {
      raw.delivered = true;
    },
    (raw) => {
      raw.firstVisibleEstimateMs = 1;
    },
  ]) {
    const changed = raw();
    change(changed);
    assert.throws(() =>
      assertDeferredRegression(changed, runtimeUrl, 100, 800, {
        cache: "cold",
        runtimeDelayMs: 0,
      }),
    );
  }
  assert.throws(() =>
    assertDeferredRegression(raw(), runtimeUrl, 900, 800, {
      cache: "cold",
      runtimeDelayMs: 750,
    }),
  );
});

test("failed, absent and renderer-absent sole runtimes require no APIs, queue drain, requests or creatives", () => {
  const raw = () => ({
    early: { core: false, queued: false },
    apis: apis(false),
    candidateAdId: "fixture-server-ad",
    preCoreQueue: { callbacks: [], runtimeLoads: [] },
    scriptLoads: [],
    navigation: [{ domContentLoadedEventEnd: 1 }],
    geometry: [{ article: { top: 1 } }],
    fixture: { requests: [], trace: [], creatives: [] },
    delivered: false,
    firstVisibleEstimateMs: null,
  });
  for (const mode of ["failed", "absent", "renderer-absent", "late-runtime"]) {
    const responses =
      mode === "absent" ? [] : [{ url: runtimeUrl, status: 404 }];
    assertUnavailableRuntime(raw(), responses, runtimeUrl, mode);
    for (const change of [
      (raw) => {
        raw.apis.methods.setConfig = "function";
      },
      (raw) => {
        raw.apis.rendererVersion = 1;
      },
      (raw) => raw.preCoreQueue.callbacks.push({}),
      (raw) => raw.preCoreQueue.runtimeLoads.push({}),
      (raw) => raw.fixture.requests.push({}),
      (raw) => raw.fixture.creatives.push({ label: "Server result" }),
      (raw) => raw.fixture.trace.push({ event: "nativeDisplay" }),
      (raw) => raw.fixture.trace.push({ event: "nativeRefresh" }),
      (raw) => {
        raw.delivered = true;
      },
      (raw) => {
        raw.firstVisibleEstimateMs = 1;
      },
      (raw) => {
        raw.candidateAdId = undefined;
      },
    ]) {
      const changed = raw();
      change(changed);
      assert.throws(() =>
        assertUnavailableRuntime(changed, responses, runtimeUrl, mode),
      );
    }
    assert.throws(() =>
      assertUnavailableRuntime(
        raw(),
        [...responses, { url: runtimeUrl, status: 200 }],
        runtimeUrl,
        mode,
      ),
    );
  }
  assert.deepEqual(unavailableErrors(false), [
    "tsjs.setConfig is not a function",
  ]);
  assert.deepEqual(unavailableErrors(true), [
    "tsjs.setConfig is not a function",
    "tsjs.registerPublisherSlot is not a function",
  ]);
});
