const assert = require("node:assert/strict");
const {
  assertRuntimeOrdering,
  assertCspRuntimeApis,
} = require("./evidence.cjs");
const { assertCriticalOutage } = require("./diagnostic.cjs");

function correctnessCases(regressionsOnly = false) {
  const mobile = { width: 390, height: 844 };
  const cases = [];
  const add = (name, mode = "slow", extra = {}) =>
    cases.push({ profile: "combined", name, viewport: mobile, mode, ...extra });
  add("registered", "renderer-absent");
  add("registered", "failed");
  add("csp");
  add("csp", "failed");
  if (regressionsOnly) return cases;
  add("inline", "slow", { control: "runtime-deferred" });
  add("inline", "slow", { control: "reservation-removed" });
  for (const mode of ["slow", "failed", "absent", "late-runtime"]) {
    add("inline", mode);
    add("registered", mode);
  }
  for (const name of [
    "shared",
    "absent-wrapper",
    "unmatched",
    "child-replacement",
    "unknown-height",
  ])
    add(name);
  add("shared", "slow", { viewport: { width: 1280, height: 1000 } });
  add("unknown-height", "slow", { profile: "loading" });
  for (const width of [1023, 1024])
    add("tall", "slow", { viewport: { width, height: 1000 } });
  add("inline", "slow", { noFill: true });
  return cases;
}

function assertCorrectnessSamples(samples, regressionsOnly = false) {
  const shape = ({
    profile,
    name,
    viewport,
    mode,
    noFill = false,
    control = null,
  }) => ({
    profile,
    name,
    viewport,
    mode,
    noFill,
    control,
  });
  assert.deepEqual(
    samples.filter((entry) => entry.stage === "correctness").map(shape),
    correctnessCases(regressionsOnly).map(shape),
    "Every declared correctness case must execute exactly once in order",
  );
}

function assertCspDocument(input) {
  assert.equal(
    input.enforcedCsp,
    input.name === "csp",
    "Candidate CSP case must enforce a real response policy; other cases must remain normal",
  );
}

function unavailableErrors(registered) {
  return [
    "tsjs.setConfig is not a function",
    ...(registered ? ["tsjs.registerPublisherSlot is not a function"] : []),
  ];
}

function assertUnavailableRuntime(raw, responses, runtimeUrl, mode) {
  assertCspRuntimeApis(raw, false);
  assertCriticalOutage(raw, responses, runtimeUrl, mode);
  assert.equal(
    raw.candidateAdId,
    "fixture-server-ad",
    "Unavailable renderer must not erase the admitted candidate data",
  );
  assert.deepEqual(
    raw.fixture.creatives,
    [],
    "No creative may render without the critical runtime",
  );
  assert.deepEqual(
    raw.fixture.trace.filter((entry) =>
      ["nativeDisplay", "nativeRefresh", "nativeRequest"].includes(entry.event),
    ),
    [],
    "Unavailable runtime must not display, refresh or request a TS slot",
  );
}

function assertDeferredRegression(raw, runtimeUrl, fcp, responseEnd, settings) {
  assert.equal(
    raw.early.core,
    false,
    "Deferring the sole runtime must break the direct early core call",
  );
  assert.equal(
    raw.early.queued,
    false,
    "Failed early publisher script must not enqueue its callback",
  );
  assertRuntimeOrdering(raw, runtimeUrl, true);
  assert.throws(
    () => assertRuntimeOrdering(raw, runtimeUrl, false),
    /before the parser reaches the article/,
    "The production synchronous ordering gate must reject this actual regression",
  );
  assertCspRuntimeApis(raw, true);
  assert.deepEqual(
    raw.fixture.requests,
    [],
    "Deferred runtime must reproduce the missed initial scheduling call",
  );
  assert.deepEqual(
    raw.fixture.creatives,
    [],
    "Deferred startup regression must not invent a creative",
  );
  assert.equal(
    raw.delivered,
    false,
    "Deferred startup regression must not report delivery",
  );
  assert.equal(
    raw.firstVisibleEstimateMs,
    null,
    "Deferred startup regression must not report a visible ad",
  );
  if (settings.cache === "cold" && settings.runtimeDelayMs === 750)
    assert(
      fcp < responseEnd,
      "Deferred regression must permit content paint during the 750ms runtime hold",
    );
}

module.exports = {
  correctnessCases,
  assertCorrectnessSamples,
  unavailableErrors,
  assertCspDocument,
  assertUnavailableRuntime,
  assertDeferredRegression,
};
