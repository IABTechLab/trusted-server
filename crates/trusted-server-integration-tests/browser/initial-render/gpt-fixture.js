(() => {
  const trace = [];
  const requests = [];
  const creatives = [];
  const listeners = new Map();
  const targeting = new Map();
  const record = (event, details = {}) =>
    trace.push({ event, time: Math.round(performance.now()), ...details });
  const slot = {
    getSlotElementId: () => "example-slot",
    getAdUnitPath: () => "/123/example-placement",
    getSizes: () => [{ getWidth: () => 300, getHeight: () => 250 }],
    getTargeting: (key) => targeting.get(key) ?? [],
    getTargetingKeys: () => [...targeting.keys()],
    setTargeting(key, value) {
      targeting.set(
        key,
        Array.isArray(value) ? value.map(String) : [String(value)],
      );
      record("targetingSet", { key, value: targeting.get(key) });
      return slot;
    },
    clearTargeting(key) {
      if (key === undefined) targeting.clear();
      else targeting.delete(key);
      record("targetingCleared", { key });
      return slot;
    },
    updateTargetingFromMap(map) {
      record("targetingMap", { map });
      for (const [key, value] of Object.entries(map)) {
        if (value === null) targeting.delete(key);
        else
          targeting.set(
            key,
            Array.isArray(value) ? value.map(String) : [String(value)],
          );
      }
      return slot;
    },
    addService: () => slot,
    setConfig: () => slot,
  };
  const snapshot = (label) => {
    const claim = window.tsjs?.firstImpression?.slots["example-slot"];
    record("claimSnapshot", {
      label,
      phase: claim?.phase,
      publisherRegistrationClosed: !!claim?.publisherRegistrationClosed,
      publisherTokenCount: Object.keys(claim?.publisherAuctions ?? {}).length,
    });
  };
  const emit = (event, extra = {}) => {
    snapshot("before:" + event);
    record(event, { slot: slot.getSlotElementId(), ...extra });
    for (const fn of listeners.get(event) ?? []) fn({ slot, ...extra });
    snapshot("after:" + event);
  };
  const service = {
    getSlots: () => [slot],
    getTargeting: () => [],
    getTargetingKeys: () => [],
    setTargeting: () => service,
    clearTargeting: () => service,
    enableSingleRequest() {},
    disableInitialLoad() {},
    isInitialLoadDisabled: () => false,
    addEventListener(event, fn) {
      const entries = listeners.get(event) ?? [];
      entries.push(fn);
      listeners.set(event, entries);
      return service;
    },
    refresh(slots) {
      if (slots && !slots.includes(slot)) return;
      const request = {
        index: requests.length,
        targeting: Object.fromEntries(targeting),
        owner: window.tsjs?.firstImpression?.slots["example-slot"]?.owner,
      };
      requests.push(request);
      record("nativeRequest", request);
      if (!window.fixture.holdRequestEvent) emit("slotRequested");
    },
  };
  window.googletag = {
    apiReady: true,
    pubadsReady: true,
    cmd: {
      push(...callbacks) {
        callbacks.forEach((fn) => fn());
        return callbacks.length;
      },
    },
    pubads: () => service,
    defineSlot: () => slot,
    destroySlots: () => true,
    enableServices() {},
    display() {
      service.refresh([slot]);
    },
    getConfig: () => ({ disableInitialLoad: false }),
    setConfig() {},
  };
  window.fixture = {
    trace,
    requests,
    creatives,
    slot,
    record,
    emit,
    snapshot,
    holdRequestEvent: false,
    render(index) {
      const request = requests[index];
      if (!request) throw new Error("Missing recorded native request " + index);
      const adId = request.targeting.hb_adid?.[0];
      if (!adId) throw new Error("Recorded request has no creative ad ID");
      const frame = document.createElement("iframe");
      frame.width = "300";
      frame.height = "250";
      frame.dataset.request = String(index);
      frame.src =
        "https://creative.example.com/fixture-puc?adId=" +
        encodeURIComponent(adId);
      const root = document.getElementById("example-slot");
      const previous = root.querySelector("iframe");
      root.replaceChildren(frame);
      record("fixtureGptRender", {
        request: index,
        adId,
        replaced: !!previous,
      });
      emit("slotResponseReceived");
      emit("slotRenderEnded", { isEmpty: false, size: [300, 250] });
    },
  };
  window.addEventListener("message", (event) => {
    if (event.data?.type === "fixture-creative-ready") {
      creatives.push(event.data.label);
      record("creativeReady", { label: event.data.label });
    }
  });
  window.pbjs = { que: [], cmd: [] };
  window.__tsjs_gpt_enabled = true;
  window.__tsjs_prebid = {
    timeout: 15000,
    debug: false,
    serverSideBidders: [],
    clientSideBidders: [],
  };
})();
