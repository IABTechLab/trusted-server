const assert = require("node:assert/strict");
const { JSDOM } = require("jsdom");
const { assertCspRuntimeApis } = require("./evidence.cjs");

const baselineRevision = "51730bdaf841e4d7bb96c2258e5e98169bdff9a7";
const expectedArchitecture = {
  baseline: "pinned-original-main-synchronous-unified",
  candidate: "synchronous-versioned-unified-without-inline-bootstrap",
};

function assertScreenDocument(html, profile) {
  const candidate = ["loading", "combined"].includes(profile);
  const dom = new JSDOM(html);
  try {
    const document = dom.window.document;
    const runtimes = document.querySelectorAll("script#trustedserver-js");
    assert.equal(runtimes.length, 1, "Screen requires one production runtime");
    const runtime = runtimes[0];
    assert(
      !runtime.hasAttribute("defer"),
      "Screen expects synchronous runtime",
    );
    assert(!runtime.hasAttribute("async"), "Screen expects parser blocking");
    assert(
      !runtime.type || runtime.type === "text/javascript",
      "Screen expects a classic runtime",
    );
    const url = new URL(
      runtime.getAttribute("src"),
      "https://publisher.example.com",
    );
    assert.equal(url.pathname, "/static/tsjs=tsjs-unified.min.js");
    if (candidate) {
      assert(url.searchParams.get("v"), "Candidate runtime must be versioned");
      assert(
        !document.querySelector("#trustedserver-bootstrap"),
        "Candidate must not embed the compiled early bootstrap",
      );
      assert(
        !html.includes("// Edge-injected GPT auction bootstrap."),
        "Candidate must not embed the provisional raw bootstrap",
      );
      const scripts = [...document.querySelectorAll("script")];
      const config = scripts.find(
        (script) =>
          !script.src &&
          script.textContent.includes("window.__tsjs_gpt_enabled=true"),
      );
      assert(config, "Candidate must retain GPT configuration");
      assert(
        scripts.indexOf(config) < scripts.indexOf(runtime),
        "Candidate GPT configuration must precede runtime execution",
      );
    }
  } finally {
    dom.window.close();
  }
}

// Test-only publisher queue, installed before any parser-created production script.
// Core drains it before the GPT IIFE installs registration and rendering methods.
function installEarlyProbe() {
  const methods = () => ({
    methods: Object.fromEntries(
      [
        "setConfig",
        "getConfig",
        "requestAds",
        "addAdUnits",
        "renderAdUnit",
        "renderAllAdUnits",
        "adInit",
        "scheduleInitialAdInit",
        "registerPublisherSlot",
      ].map((name) => [name, typeof window.tsjs?.[name]]),
    ),
    shim: typeof window.__tsjs_installGptShim,
    registrationVersion: window.tsjs?.publisherSlotRegistrationVersion ?? null,
    rendererVersion: window.tsjs?.tsRenderBridgeVersion ?? null,
  });
  const state = (window.cwv.preCoreQueue = { callbacks: [], runtimeLoads: [] });
  window.tsjs ??= {};
  window.tsjs.que ??= [];
  window.tsjs.que.push(() => {
    state.callbacks.push({
      time: performance.now(),
      articlePresent: !!document.getElementById("article"),
      apis: methods(),
      coreCallSucceeded: typeof window.tsjs.getConfig() === "object",
    });
  });
  document.addEventListener(
    "load",
    (event) => {
      if (
        !["trustedserver-js", "trustedserver-js-recovery"].includes(
          event.target?.id,
        )
      )
        return;
      state.runtimeLoads.push({ time: performance.now(), apis: methods() });
    },
    true,
  );
}

function assertEarlyProbe(raw, profile, articlePresent = false) {
  const probe = raw.preCoreQueue;
  assert.equal(
    probe?.callbacks.length,
    1,
    "Pre-core callback must drain exactly once",
  );
  const callback = probe.callbacks[0];
  assert.equal(
    callback.articlePresent,
    articlePresent,
    articlePresent
      ? "Deferred control or explicit late recovery must drain the queue after article parsing"
      : "Queue must drain before body parsing",
  );
  assert.equal(
    callback.coreCallSucceeded,
    true,
    "Queued callback must call real core API",
  );
  for (const name of [
    "setConfig",
    "getConfig",
    "requestAds",
    "addAdUnits",
    "renderAdUnit",
    "renderAllAdUnits",
  ])
    assert.equal(
      callback.apis.methods[name],
      "function",
      `Queued core API ${name}`,
    );
  // Do not require GPT methods at the earlier core queue drain.
  assert.equal(probe.runtimeLoads.length, 1, "Capture one native runtime load");
  assert(
    callback.time <= probe.runtimeLoads[0].time,
    "Queue must drain before runtime load",
  );
  if (["loading", "combined"].includes(profile))
    assertCspRuntimeApis(probe.runtimeLoads[0], true);
}

function screenCases(runs) {
  const cases = [];
  for (const viewport of [
    { width: 390, height: 844 },
    { width: 1280, height: 1000 },
  ])
    for (let run = 0; run < runs; run++) {
      const order = ["baseline", "loading", "reservation", "combined"];
      if (run % 2) order.reverse();
      for (const profile of order)
        cases.push({ profile, name: "inline", viewport });
    }
  return cases;
}

function assertScreenSamples(samples, runs) {
  const expected = screenCases(runs);
  assert(expected.length > 0, "Diagnostic must execute samples");
  assert.deepEqual(
    samples.map(({ profile, name, viewport, stage, mode, control }) => ({
      profile,
      name,
      viewport,
      stage,
      mode,
      control,
    })),
    expected.map((entry) => ({
      ...entry,
      stage: "measurement",
      mode: "slow",
      control: null,
    })),
    "Diagnostic must execute every declared case in alternating order without skips",
  );
}

function outageCases() {
  return ["inline", "csp"].flatMap((name) =>
    ["slow", "failed"].map((mode) => ({
      profile: "combined",
      name,
      mode,
      viewport: { width: 390, height: 844 },
    })),
  );
}

function assertOutageSamples(samples) {
  assert.deepEqual(
    samples.map(({ profile, name, viewport, stage, mode, control }) => ({
      profile,
      name,
      viewport,
      stage,
      mode,
      control,
    })),
    outageCases().map((entry) => ({
      ...entry,
      stage: "correctness",
      control: null,
    })),
    "Outage diagnostic must execute all four normal/CSP success/failure cases without skips",
  );
}

function assertCriticalOutage(raw, responses, runtimeUrl, mode = "failed") {
  assert.deepEqual(
    raw.preCoreQueue.callbacks,
    [],
    "Failed critical runtime must not drain the pre-core queue",
  );
  assert.deepEqual(
    raw.preCoreQueue.runtimeLoads,
    [],
    "Failed critical runtime must not report native load",
  );
  assert.deepEqual(
    raw.scriptLoads.filter((entry) => entry.src === runtimeUrl),
    [],
    "No successful runtime load may be observed during outage",
  );
  assert.deepEqual(
    responses
      .filter((entry) => entry.url === runtimeUrl)
      .map((entry) => entry.status),
    mode === "absent" ? [] : [404],
    mode === "absent"
      ? "Absent runtime tag must issue no runtime request"
      : "Observe exactly one failed critical runtime response, not a cached runtime or retry",
  );
  assert(
    raw.navigation[0].domContentLoadedEventEnd > 0,
    "Parser must reach DOMContentLoaded after critical-script failure",
  );
  assert(
    raw.geometry.at(-1)?.article,
    "Parser must reach the article despite the failed critical runtime",
  );
}

module.exports = {
  baselineRevision,
  expectedArchitecture,
  assertScreenDocument,
  installEarlyProbe,
  assertEarlyProbe,
  screenCases,
  assertScreenSamples,
  outageCases,
  assertOutageSamples,
  assertCriticalOutage,
};
