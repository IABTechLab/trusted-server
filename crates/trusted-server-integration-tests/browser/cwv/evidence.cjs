const assert = require("node:assert/strict");

function assertRuntimeOrdering(raw, runtimeUrl, deferred) {
  const load = raw.scriptLoads.find((entry) => entry.src === runtimeUrl);
  assert(
    load,
    "Successful runtime must have a captured native script load event",
  );
  assert.equal(
    load.articlePresent,
    deferred,
    deferred
      ? "Deferred runtime must load after the parser reaches the article"
      : "Parser-blocking runtime must load before the parser reaches the article",
  );
  return load;
}

function assertCacheUse(requests, runtimeUrl, requireRuntime) {
  const hits = requests.filter(
    (entry) => entry.servedFromCache || entry.fromDiskCache,
  );
  assert(
    hits.length > 0,
    "Warm measurement must demonstrate actual HTTP cache reuse",
  );
  if (requireRuntime)
    assert(
      hits.some((entry) => entry.url === runtimeUrl),
      "Warm successful runtime must be served from HTTP cache",
    );
  return hits;
}

function assertReservationControl(raw, cls) {
  const before = raw.geometry.find((entry) => entry.phase === "before-fill");
  const after = raw.geometry.at(-1);
  assert(before && after, "Reservation control requires before/after geometry");
  assert(
    after.article.top > before.article.top,
    "Removing reservation must move the article during fill",
  );
  assert(cls > 0, "Removing reservation must produce observer-reported CLS");
  assert(
    raw.shifts.some(
      (entry) =>
        !entry.hadRecentInput &&
        entry.value > 0 &&
        entry.sources.some(
          (source) => source.node === "article" || source.withinArticle,
        ),
    ),
    "Layout-shift observer must attribute reservation-control movement to the article",
  );
}

function assertCandidateRendererVersion(raw, profile) {
  // Both isolation arms use original-main JS, which predates this protocol marker.
  if (profile === "baseline" || profile === "reservation") return;
  assert.equal(
    raw.rendererVersion,
    1,
    "Available candidate unified runtime must install the real renderer",
  );
}

function assertVisibleCreative(text, firstVisibleEstimateMs, expectedLabel) {
  assert(
    text.flat().includes(expectedLabel),
    "Actual creative iframe DOM must contain the expected creative",
  );
  assert(
    Number.isFinite(firstVisibleEstimateMs),
    "Visible ad estimate must be observed",
  );
}

function assertCspRuntimeApis(raw, available) {
  for (const name of [
    "setConfig",
    "getConfig",
    "requestAds",
    "addAdUnits",
    "renderAdUnit",
    "renderAllAdUnits",
    "adInit",
    "scheduleInitialAdInit",
    "registerPublisherSlot",
  ])
    assert.equal(
      raw.apis.methods[name],
      available ? "function" : "undefined",
      `CSP runtime API ${name}`,
    );
  assert.equal(
    raw.apis.shim,
    available ? "function" : "undefined",
    "CSP runtime shim API",
  );
  assert.equal(
    raw.apis.registrationVersion,
    available ? 1 : null,
    "CSP registration protocol availability",
  );
  assert.equal(
    raw.apis.rendererVersion,
    available ? 1 : null,
    "CSP renderer availability",
  );
  if (!available) {
    assert.equal(
      raw.early.core,
      false,
      "Failed synchronous runtime must leave core unavailable",
    );
    assert.equal(
      raw.early.queued,
      false,
      "Core-dependent fixture callback must not execute",
    );
    assert.deepEqual(
      raw.fixture.requests,
      [],
      "Unavailable CSP runtime must not initiate TS delivery",
    );
    assert.equal(
      raw.delivered,
      false,
      "Unavailable CSP runtime must not report delivery",
    );
    assert.equal(
      raw.firstVisibleEstimateMs,
      null,
      "Unavailable CSP runtime must not report a visible ad",
    );
  }
}

module.exports = {
  assertRuntimeOrdering,
  assertCspRuntimeApis,
  assertCandidateRendererVersion,
  assertVisibleCreative,
  assertCacheUse,
  assertReservationControl,
};
