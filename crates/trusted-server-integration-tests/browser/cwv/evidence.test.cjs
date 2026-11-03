const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  assertRuntimeOrdering,
  assertCacheUse,
  assertReservationControl,
  assertCspRuntimeApis,
  assertCandidateRendererVersion,
  assertVisibleCreative,
} = require("./evidence.cjs");
const url = "http://127.0.0.1/runtime.js";

test("native load/parser evidence works without a forced response delay", () => {
  for (const deferred of [true, false]) {
    const raw = {
      scriptLoads: [{ src: url, articlePresent: deferred, time: 2 }],
    };
    assertRuntimeOrdering(raw, url, deferred);
    assert.throws(() => assertRuntimeOrdering(raw, url, !deferred));
  }
  assert.throws(() => assertRuntimeOrdering({ scriptLoads: [] }, url, true));
});

test("warm cache evidence requires a real memory/disk hit, not a warm label or status 200", () => {
  assert.throws(() => assertCacheUse([{ url, status: 200 }], url, true));
  for (const hit of [{ servedFromCache: true }, { fromDiskCache: true }])
    assert.equal(assertCacheUse([{ url, ...hit }], url, true).length, 1);
  const other = [{ url: "http://127.0.0.1/hero.svg", servedFromCache: true }];
  assert.throws(() => assertCacheUse(other, url, true));
  assert.equal(assertCacheUse(other, url, false).length, 1);
  assert.throws(() =>
    assertCacheUse([{ url, fromServiceWorker: true }], url, true),
  );
});

test("reservation negative control requires both geometry and attributed observer shift", () => {
  const raw = {
    geometry: [
      { phase: "before-fill", article: { top: 100 } },
      { phase: "finished", article: { top: 350 } },
    ],
    shifts: [
      { value: 0.1, hadRecentInput: false, sources: [{ node: "article" }] },
    ],
  };
  assertReservationControl(raw, 0.1);
  assert.throws(() => assertReservationControl(raw, 0));
  assert.throws(() => assertReservationControl({ ...raw, shifts: [] }, 0.1));
  assert.throws(() =>
    assertReservationControl(
      {
        ...raw,
        shifts: [
          { value: 0.1, hadRecentInput: true, sources: [{ node: "article" }] },
        ],
      },
      0.1,
    ),
  );
  assert.throws(() =>
    assertReservationControl(
      { ...raw, geometry: [raw.geometry[0], raw.geometry[0]] },
      0.1,
    ),
  );
});

test("legacy arms need actual visible rendering, not the candidate-only renderer marker", () => {
  for (const profile of ["baseline", "reservation"]) {
    assertCandidateRendererVersion({}, profile);
    assertVisibleCreative([["Server result"]], 250, "Server result");
    assert.throws(() => assertVisibleCreative([], 250, "Server result"));
    assert.throws(() =>
      assertVisibleCreative([["Publisher result"]], 250, "Server result"),
    );
    assert.throws(() =>
      assertVisibleCreative([["Server result"]], null, "Server result"),
    );
  }
  for (const profile of ["loading", "combined"]) {
    assert.throws(() => assertCandidateRendererVersion({}, profile));
    assert.throws(() =>
      assertCandidateRendererVersion({ rendererVersion: 0 }, profile),
    );
    assertCandidateRendererVersion({ rendererVersion: 1 }, profile);
  }
});

test("CSP API proof distinguishes working compiled runtime from unavailable no-fallback boundary", () => {
  for (const available of [true, false]) {
    const raw = {
      apis: {
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
          ].map((name) => [name, available ? "function" : "undefined"]),
        ),
        shim: available ? "function" : "undefined",
        registrationVersion: available ? 1 : null,
        rendererVersion: available ? 1 : null,
      },
      early: { core: available, queued: available },
      fixture: { requests: [] },
      delivered: false,
      firstVisibleEstimateMs: null,
    };
    assertCspRuntimeApis(raw, available);
    assert.throws(() => assertCspRuntimeApis(raw, !available));
    if (available) {
      delete raw.apis.methods.registerPublisherSlot;
      assert.throws(() => assertCspRuntimeApis(raw, true));
    } else {
      raw.fixture.requests.push({ index: 0 });
      assert.throws(() => assertCspRuntimeApis(raw, false));
    }
  }
});

for (const status of ["blocked", "failed"])
  test(`setup ${status} replaces a previous successful outcome without launching a browser`, () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "cwv-outcome-"));
    try {
      const output = path.join(root, "out");
      const input = path.join(root, "input");
      fs.mkdirSync(output);
      fs.writeFileSync(
        path.join(output, "acceptance.json"),
        JSON.stringify({ status: "passed" }),
      );
      fs.writeFileSync(
        path.join(output, "artifacts.json"),
        JSON.stringify({ outcome: { status: "passed" } }),
      );
      if (status === "failed") {
        fs.mkdirSync(input);
        fs.writeFileSync(
          path.join(input, "manifest.json"),
          JSON.stringify({ schemaVersion: 999 }),
        );
      }
      const result = spawnSync(
        process.execPath,
        [
          path.join(__dirname, "run.cjs"),
          "--loading",
          input,
          "--combined",
          input,
          "--out",
          output,
          "--correctness-only",
        ],
        { encoding: "utf8" },
      );
      assert.equal(result.status, 1, result.stderr);
      const outcome = JSON.parse(
        fs.readFileSync(path.join(output, "acceptance.json")),
      );
      const artifacts = JSON.parse(
        fs.readFileSync(path.join(output, "artifacts.json")),
      );
      assert.equal(outcome.status, status, result.stderr);
      assert.deepEqual(artifacts.outcome, outcome);
      assert(outcome.error.message);
      assert(outcome.finishedAt);
      assert.equal(outcome.samples, 0);
      assert.equal(
        outcome.negativeControls["parser-blocking"].status,
        "pending",
      );
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
