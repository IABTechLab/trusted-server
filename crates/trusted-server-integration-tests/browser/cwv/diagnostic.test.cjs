const test = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  assertScreenDocument,
  installEarlyProbe,
  assertEarlyProbe,
  screenCases,
  assertScreenSamples,
  outageCases,
  assertOutageSamples,
  assertCriticalOutage,
} = require("./diagnostic.cjs");

const runtime =
  '<script id="trustedserver-js" src="/static/tsjs=tsjs-unified.min.js?v=example"></script>';
const config = "<script>window.__tsjs_gpt_enabled=true;</script>";
const html = `<!doctype html><head>${config}${runtime}</head><body><main id="article"></main></body>`;

test("screen independently requires synchronous versioned unified with config before it", () => {
  for (const profile of ["baseline", "loading", "reservation", "combined"])
    assertScreenDocument(html, profile);
  for (const changed of [
    html.replace('id="trustedserver-js"', 'id="trustedserver-js" defer'),
    html.replace('id="trustedserver-js"', 'id="trustedserver-js" async'),
    html.replace(
      'id="trustedserver-js"',
      'id="trustedserver-js" type="module"',
    ),
    html.replace("?v=example", ""),
    html.replace("tsjs-unified", "tsjs-gpt"),
    html.replace(config + runtime, runtime + config),
    html.replace(config, ""),
    html.replace(runtime, runtime + runtime),
    html.replace(
      config,
      config + '<script id="trustedserver-bootstrap">compiled();</script>',
    ),
    html.replace(
      config,
      config +
        "<script>// Edge-injected GPT auction bootstrap.\nraw();</script>",
    ),
  ])
    assert.throws(() => assertScreenDocument(changed, "combined"));
});

test("original-main allows its actual raw bootstrap instead of using candidate HTML", () => {
  const legacy = html.replace(
    config,
    "<script>// Edge-injected GPT auction bootstrap.\nraw();</script>",
  );
  assertScreenDocument(legacy, "baseline");
  assertScreenDocument(legacy, "reservation");
  assert.throws(() => assertScreenDocument(legacy, "loading"));
});

function probeFixture({
  articlePresent = false,
  runtimeId = "trustedserver-js",
} = {}) {
  const listeners = {};
  const context = vm.createContext({
    window: { cwv: {} },
    performance: { now: () => 1 },
    document: {
      getElementById: () => (articlePresent ? {} : null),
      addEventListener: (name, callback) => {
        listeners[name] = callback;
      },
    },
  });
  vm.runInContext(`(${installEarlyProbe.toString()})();`, context);
  const { window } = context;
  const callback = window.tsjs.que[0];
  for (const name of [
    "setConfig",
    "getConfig",
    "requestAds",
    "addAdUnits",
    "renderAdUnit",
    "renderAllAdUnits",
  ])
    window.tsjs[name] = () => ({});
  callback();
  for (const name of [
    "adInit",
    "scheduleInitialAdInit",
    "registerPublisherSlot",
  ])
    window.tsjs[name] = () => {};
  window.__tsjs_installGptShim = () => {};
  window.tsjs.publisherSlotRegistrationVersion = 1;
  window.tsjs.tsRenderBridgeVersion = 1;
  listeners.load({ target: { id: runtimeId } });
  return JSON.parse(JSON.stringify(window.cwv));
}

test("pre-core queue drain observes core methods without assuming GPT has initialized", () => {
  const raw = probeFixture();
  assert.equal(
    raw.preCoreQueue.callbacks[0].apis.methods.registerPublisherSlot,
    "undefined",
  );
  assert.equal(
    raw.preCoreQueue.runtimeLoads[0].apis.methods.registerPublisherSlot,
    "function",
  );
  assertEarlyProbe(raw, "combined");
  assertEarlyProbe(raw, "baseline");
});

test("explicit dynamic recovery captures native load and late queue drain without satisfying normal early ordering", () => {
  const raw = probeFixture({
    articlePresent: true,
    runtimeId: "trustedserver-js-recovery",
  });
  assertEarlyProbe(raw, "combined", true);
  assert.throws(() => assertEarlyProbe(raw, "combined"), /before body parsing/);
});

test("early API evidence fails closed for dropped/duplicate queue callbacks and missing GPT APIs", () => {
  for (const change of [
    (raw) => {
      raw.preCoreQueue.callbacks = [];
    },
    (raw) => {
      raw.preCoreQueue.callbacks.push(raw.preCoreQueue.callbacks[0]);
    },
    (raw) => {
      raw.preCoreQueue.callbacks[0].coreCallSucceeded = false;
    },
    (raw) => {
      raw.preCoreQueue.callbacks[0].articlePresent = true;
    },
    (raw) => {
      raw.preCoreQueue.runtimeLoads = [];
    },
    (raw) => {
      raw.preCoreQueue.runtimeLoads[0].apis.methods.registerPublisherSlot =
        "undefined";
    },
  ]) {
    const raw = probeFixture();
    change(raw);
    assert.throws(() => assertEarlyProbe(raw, "combined"));
  }
});

test("screen executes precisely eight times runs with alternating four-way order", () => {
  const cases = screenCases(3);
  assert.equal(cases.length, 24);
  assert.deepEqual(
    cases.slice(0, 8).map((entry) => entry.profile),
    [
      "baseline",
      "loading",
      "reservation",
      "combined",
      "combined",
      "reservation",
      "loading",
      "baseline",
    ],
  );
  assert.deepEqual(
    [...new Set(cases.map((entry) => entry.viewport.width))],
    [390, 1280],
  );
  const samples = cases.map((entry) => ({
    ...entry,
    stage: "measurement",
    mode: "slow",
    control: null,
  }));
  assertScreenSamples(samples, 3);
  assert.throws(() => assertScreenSamples([], 3));
  assert.throws(() => assertScreenSamples(samples.slice(1), 3));
  assert.throws(() => assertScreenSamples([...samples, samples[0]], 3));
  assert.throws(() => assertScreenSamples([...samples].reverse(), 3));
});

test("outage diagnostic declares exactly four complete normal/CSP success/failure cases", () => {
  const cases = outageCases();
  assert.deepEqual(
    cases.map(({ name, mode }) => [name, mode]),
    [
      ["inline", "slow"],
      ["inline", "failed"],
      ["csp", "slow"],
      ["csp", "failed"],
    ],
  );
  const samples = cases.map((entry) => ({
    ...entry,
    stage: "correctness",
    control: null,
  }));
  assertOutageSamples(samples);
  assert.throws(() => assertOutageSamples([]));
  assert.throws(() => assertOutageSamples(samples.slice(1)));
  assert.throws(() => assertOutageSamples([...samples, samples[0]]));
  assert.throws(() => assertOutageSamples([...samples].reverse()));
});

test("critical outage evidence requires failed transport, undrained queue and completed parser", () => {
  const runtimeUrl =
    "https://publisher.example.com/static/tsjs=tsjs-unified.min.js?v=example";
  const raw = () => ({
    preCoreQueue: { callbacks: [], runtimeLoads: [] },
    scriptLoads: [],
    navigation: [{ domContentLoadedEventEnd: 1 }],
    geometry: [{ article: { top: 1 } }],
  });
  const responses = () => [{ url: runtimeUrl, status: 404 }];
  assertCriticalOutage(raw(), responses(), runtimeUrl);
  for (const change of [
    (raw) => raw.preCoreQueue.callbacks.push({}),
    (raw) => raw.preCoreQueue.runtimeLoads.push({}),
    (raw) => raw.scriptLoads.push({ src: runtimeUrl }),
    (raw) => {
      raw.navigation[0].domContentLoadedEventEnd = 0;
    },
    (raw) => {
      raw.geometry[0].article = null;
    },
  ]) {
    const changed = raw();
    change(changed);
    assert.throws(() => assertCriticalOutage(changed, responses(), runtimeUrl));
  }
  for (const changed of [
    [],
    [{ url: runtimeUrl, status: 200 }],
    [...responses(), ...responses()],
  ])
    assert.throws(() => assertCriticalOutage(raw(), changed, runtimeUrl));
});

test("public full acceptance verifier rejects both diagnostic scopes", async () => {
  const { pathToFileURL } = require("node:url");
  const { validateMeasurement, conditions } = await import(
    pathToFileURL(
      path.resolve(
        __dirname,
        "../../../../scripts/test-client-cwv-acceptance.mjs",
      ),
    ).href
  );
  for (const scope of ["diagnostic-screen", "diagnostic-outages"])
    assert.throws(
      () =>
        validateMeasurement(
          { schemaVersion: 1, status: "passed", scope },
          conditions[0],
          1,
        ),
      (error) =>
        error.actual === scope &&
        error.expected === "measurement-and-correctness",
    );
});

for (const scope of ["diagnostic-screen", "diagnostic-outages"])
  test(`missing ${scope} exports invalidate stale passing outcome before browser startup`, () => {
    const out = fs.mkdtempSync(path.join(os.tmpdir(), "cwv-diagnostic-"));
    try {
      fs.writeFileSync(
        path.join(out, "acceptance.json"),
        JSON.stringify({
          status: "passed",
          scope: "measurement-and-correctness",
        }),
      );
      const missing = path.join(out, "missing");
      const result = spawnSync(
        process.execPath,
        [
          path.join(__dirname, "run.cjs"),
          `--${scope}`,
          "--runtime-delay-ms",
          "0",
          "--baseline",
          missing,
          "--loading",
          missing,
          "--combined",
          missing,
          "--out",
          out,
        ],
        { encoding: "utf8" },
      );
      assert.equal(result.status, 1, result.stderr);
      const outcome = JSON.parse(
        fs.readFileSync(path.join(out, "acceptance.json")),
      );
      assert.equal(outcome.status, "blocked");
      assert.equal(outcome.scope, scope);
      assert.equal(outcome.samples, 0);
      assert.equal(outcome.diagnostic.fullAcceptancePassed, false);
      assert.equal(
        outcome.diagnostic.expectedSamples,
        scope === "diagnostic-screen" ? 40 : 4,
      );
      if (scope === "diagnostic-outages")
        assert.equal(outcome.diagnostic.changedOutageBehaviorApproved, true);
      assert.match(outcome.error.message, /ENOENT/);
      for (const control of Object.values(outcome.negativeControls)) {
        assert.equal(control.status, "not-requested");
        assert.match(
          control.reason,
          scope === "diagnostic-screen"
            ? /outside this screen/
            : /Full acceptance remains unexecuted/,
        );
      }
      assert.deepEqual(
        JSON.parse(fs.readFileSync(path.join(out, "artifacts.json"))).outcome,
        outcome,
      );
    } finally {
      fs.rmSync(out, { recursive: true, force: true });
    }
  });
