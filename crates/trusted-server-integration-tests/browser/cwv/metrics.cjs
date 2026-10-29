// Chromium layout-shift entries use navigation-relative milliseconds.
function sessionCls(entries) {
  let maximum = 0;
  let value = 0;
  let first = 0;
  let previous = 0;
  for (const entry of entries) {
    if (entry.hadRecentInput) continue;
    if (
      value &&
      entry.startTime - previous < 1000 &&
      entry.startTime - first < 5000
    ) {
      value += entry.value;
    } else {
      value = entry.value;
      first = entry.startTime;
    }
    previous = entry.startTime;
    maximum = Math.max(maximum, value);
  }
  return maximum;
}

function distribution(values) {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!sorted.length) return { count: 0, median: null, min: null, max: null };
  const middle = Math.floor(sorted.length / 2);
  return {
    count: sorted.length,
    median:
      sorted.length % 2
        ? sorted[middle]
        : (sorted[middle - 1] + sorted[middle]) / 2,
    min: sorted[0],
    max: sorted.at(-1),
  };
}

function observePage() {
  const state = (window.cwv = {
    supported: PerformanceObserver.supportedEntryTypes,
    paint: [],
    lcp: [],
    shifts: [],
    resources: [],
    geometry: [],
    firstVisibleEstimateMs: null,
    delivered: false,
    errors: [],
    cspViolations: [],
  });
  const observers = [];
  const identifier = (node) => node?.id || node?.tagName || null;
  const record = (type, entries) => {
    for (const entry of entries) {
      if (type === "paint") state.paint.push(entry.toJSON());
      if (type === "resource") state.resources.push(entry.toJSON());
      if (type === "largest-contentful-paint") {
        state.lcp.push({
          ...entry.toJSON(),
          element: identifier(entry.element),
        });
      }
      if (type === "layout-shift") {
        state.shifts.push({
          startTime: entry.startTime,
          value: entry.value,
          hadRecentInput: entry.hadRecentInput,
          sources: (entry.sources || []).map((source) => ({
            node: identifier(source.node),
            previousRect: source.previousRect.toJSON(),
            currentRect: source.currentRect.toJSON(),
          })),
        });
      }
    }
  };
  for (const type of [
    "paint",
    "largest-contentful-paint",
    "layout-shift",
    "resource",
  ]) {
    if (!state.supported.includes(type)) continue;
    const observer = new PerformanceObserver((list) =>
      record(type, list.getEntries()),
    );
    observer.observe({ type, buffered: true });
    observers.push({ type, observer });
  }
  state.geometrySnapshot = (phase) => {
    const wrapper = document.getElementById("fixture-ad-space");
    const article = document.getElementById("article");
    state.geometry.push({
      phase,
      time: performance.now(),
      wrapper: wrapper?.getBoundingClientRect().toJSON() ?? null,
      minHeight: wrapper ? getComputedStyle(wrapper).minHeight : null,
      article: article?.getBoundingClientRect().toJSON() ?? null,
    });
  };
  state.finish = () => {
    for (const { type, observer } of observers)
      record(type, observer.takeRecords());
    state.geometrySnapshot("finished");
    return {
      ...state,
      navigation: performance
        .getEntriesByType("navigation")
        .map((entry) => entry.toJSON()),
      observationEndMs: performance.now(),
      fixture: window.fixture && {
        requests: fixture.requests,
        trace: fixture.trace,
        creatives: fixture.creatives,
      },
      early: window.fixtureEarly,
      registration: window.fixtureHandle?.getStatus(),
      rendererVersion: window.tsjs?.tsRenderBridgeVersion,
      candidateAdId: window.tsjs?.bids?.["fixture-header"]?.hb_adid,
      registeredNative: window.fixturePublisherSlot
        ? fixture.nativeSlots.indexOf(window.fixturePublisherSlot)
        : null,
    };
  };
  window.addEventListener("securitypolicyviolation", (event) => {
    state.cspViolations.push({
      blockedURI: event.blockedURI,
      violatedDirective: event.violatedDirective,
      disposition: event.disposition,
    });
  });
  window.addEventListener("error", (event) => {
    if (event.message) state.errors.push(event.message);
  });
  document.addEventListener("DOMContentLoaded", () => {
    state.geometrySnapshot("parsed");
    requestAnimationFrame(() =>
      requestAnimationFrame(() => state.geometrySnapshot("first-frames")),
    );
  });
}

module.exports = { sessionCls, distribution, observePage };
