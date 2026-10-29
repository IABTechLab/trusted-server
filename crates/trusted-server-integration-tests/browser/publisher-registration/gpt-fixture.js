// Transport only. Never read TS claims to decide whether a native call requests.
(() => {
  const options = window.publisherFixtureOptions ?? {};
  const trace = [];
  const requests = [];
  const creatives = [];
  const nativeSlots = [];
  const elements = [];
  const listeners = new Map();
  const displayed = new Set();
  const fetched = new Set();
  const destroyed = new Set();
  const queue = [];
  let disabled = options.initialLoad !== "enabled";
  let sra = false;
  let enabled = false;
  let batch = 0;
  let deferCommands = !!options.deferCommands;
  const record = (event, details = {}) =>
    trace.push({ event, time: Math.round(performance.now()), ...details });
  const elementIndex = (element) => {
    if (!element) return null;
    if (!elements.includes(element)) elements.push(element);
    return elements.indexOf(element);
  };
  const emit = (event, slot, details = {}) => {
    record(event, { native: nativeSlots.indexOf(slot), ...details });
    for (const callback of listeners.get(event) ?? [])
      callback({ slot, ...details });
  };
  const snapshot = (slot) => ({
    native: nativeSlots.indexOf(slot),
    id: slot.getSlotElementId(),
    path: slot.getAdUnitPath(),
    element: elementIndex(document.getElementById(slot.getSlotElementId())),
    targeting: Object.fromEntries(
      slot.getTargetingKeys().map((key) => [key, [...slot.getTargeting(key)]]),
    ),
    sizes: slot
      .getSizes()
      .map((size) =>
        size === "fluid" ? size : [size.getWidth(), size.getHeight()],
      ),
  });
  const request = (slots, source, refreshOptions) => {
    const batchId = batch++;
    for (const slot of slots) {
      if (destroyed.has(slot)) continue;
      const entry = {
        index: requests.length,
        batch: batchId,
        source,
        ...snapshot(slot),
        options: refreshOptions,
      };
      requests.push(entry);
      fetched.add(slot);
      record("nativeRequest", entry);
      emit("slotRequested", slot);
    }
  };
  const serviceTargeting = new Map();
  const service = {
    getSlots: () => nativeSlots.filter((slot) => !destroyed.has(slot)),
    getTargeting: (key) => serviceTargeting.get(key) ?? [],
    getTargetingKeys: () => [...serviceTargeting.keys()],
    setTargeting(key, value) {
      serviceTargeting.set(key, [String(value)]);
      return service;
    },
    clearTargeting(key) {
      if (key === undefined) serviceTargeting.clear();
      else serviceTargeting.delete(key);
      return service;
    },
    enableSingleRequest() {
      sra = true;
      record("enableSingleRequest");
      return true;
    },
    disableInitialLoad() {
      disabled = true;
      record("disableInitialLoad", { enabled });
    },
    addEventListener(event, callback) {
      const callbacks = listeners.get(event) ?? [];
      callbacks.push(callback);
      listeners.set(event, callbacks);
      return service;
    },
    removeEventListener(event, callback) {
      listeners.set(
        event,
        (listeners.get(event) ?? []).filter((fn) => fn !== callback),
      );
    },
    refresh(slots, refreshOptions) {
      const selected = slots ?? service.getSlots();
      record("nativeRefresh", {
        natives: selected.map((slot) => nativeSlots.indexOf(slot)),
        options: refreshOptions,
        scoped: slots !== undefined,
      });
      request(
        selected.filter((slot) => displayed.has(slot)),
        "refresh",
        refreshOptions,
      );
    },
  };
  if (options.initialLoad !== "unknown")
    service.isInitialLoadDisabled = () => disabled;
  const googletag = {
    apiReady: true,
    pubadsReady: true,
    cmd: {
      push(...callbacks) {
        for (const callback of callbacks) {
          if (deferCommands) queue.push(callback);
          else callback();
        }
        return queue.length;
      },
    },
    pubads: () => service,
    defineSlot(path, sizes, id) {
      const targeting = new Map();
      const formats =
        sizes.length === 2 && sizes.every((size) => typeof size === "number")
          ? [sizes]
          : sizes;
      const slot = {
        getSlotElementId: () => id,
        getAdUnitPath: () => path,
        getSizes: () =>
          formats.map((size) =>
            size === "fluid"
              ? size
              : { getWidth: () => size[0], getHeight: () => size[1] },
          ),
        getTargeting: (key) => targeting.get(key) ?? [],
        getTargetingKeys: () => [...targeting.keys()],
        setTargeting(key, value) {
          targeting.set(
            key,
            (Array.isArray(value) ? value : [value]).map(String),
          );
          record("targetingSet", {
            native: nativeSlots.indexOf(slot),
            key,
            value: [...targeting.get(key)],
          });
          return slot;
        },
        clearTargeting(key) {
          if (key === undefined) targeting.clear();
          else targeting.delete(key);
          record("targetingCleared", {
            native: nativeSlots.indexOf(slot),
            key,
          });
          return slot;
        },
        updateTargetingFromMap(map) {
          for (const [key, value] of Object.entries(map)) {
            if (value === null) slot.clearTargeting(key);
            else slot.setTargeting(key, value);
          }
          return slot;
        },
        addService: () => slot,
        setConfig: () => slot,
        defineSizeMapping: () => slot,
      };
      nativeSlots.push(slot);
      record("nativeDefine", snapshot(slot));
      return slot;
    },
    destroySlots(slots = service.getSlots()) {
      for (const slot of slots) {
        record("nativeDestroy", {
          ...snapshot(slot),
          connected: !!document.getElementById(slot.getSlotElementId())
            ?.isConnected,
        });
        destroyed.add(slot);
      }
      return true;
    },
    enableServices() {
      enabled = true;
      record("enableServices");
    },
    display(target) {
      const id =
        typeof target === "string"
          ? target
          : (target.getSlotElementId?.() ?? target.id);
      const slot = service
        .getSlots()
        .find((candidate) => candidate.getSlotElementId() === id);
      record("nativeDisplay", {
        id,
        native: nativeSlots.indexOf(slot),
        disabled,
        sra,
      });
      if (!slot) throw new Error("display requires a defined slot: " + id);
      if (!document.getElementById(id))
        throw new Error("display requires a connected element: " + id);
      displayed.add(slot);
      if (!disabled)
        request(
          sra
            ? service.getSlots().filter((candidate) => !fetched.has(candidate))
            : [slot],
          "display",
        );
    },
    setConfig(config) {
      if (config.disableInitialLoad !== undefined)
        disabled = !!config.disableInitialLoad;
      if (config.singleRequest !== undefined) sra = !!config.singleRequest;
      record("setConfig", { config });
    },
  };
  if (options.initialLoad !== "unknown")
    googletag.getConfig = () => ({
      disableInitialLoad: disabled,
      singleRequest: sra,
    });
  window.googletag = googletag;
  window.fixture = {
    trace,
    requests,
    creatives,
    nativeSlots,
    elements,
    record,
    snapshot,
    emit,
    flushCommands() {
      deferCommands = false;
      while (queue.length) queue.shift()();
    },
    setPolicy(value) {
      disabled = value;
      record("fixturePolicy", { disabled });
    },
    render(index, { noFill = false } = {}) {
      const entry = requests[index];
      if (!entry) throw new Error("Missing native request " + index);
      const slot = nativeSlots[entry.native];
      const root = document.getElementById(entry.id);
      if (!root)
        throw new Error("Cannot render into absent native element " + entry.id);
      if (noFill) {
        root.replaceChildren();
        emit("slotRenderEnded", slot, { isEmpty: true });
        return;
      }
      const adId = entry.targeting.hb_adid?.[0];
      const frame = document.createElement("iframe");
      frame.width = "300";
      frame.height = "250";
      // Model the creative box, not the browser\'s default iframe border and baseline gap.
      frame.style.display = "block";
      frame.style.border = "0";
      frame.dataset.request = String(index);
      frame.src = adId
        ? "https://creative.example.com/fixture-puc?adId=" +
          encodeURIComponent(adId)
        : "https://creative.example.com/fixture-publisher";
      root.replaceChildren(frame);
      record("fixtureRender", {
        index,
        element: elementIndex(root),
        candidate: !!adId,
      });
      emit("slotResponseReceived", slot);
      emit("slotRenderEnded", slot, { isEmpty: false, size: [300, 250] });
    },
  };
  const containsWindow = (view, target) => {
    if (view === target) return true;
    for (let index = 0; index < view.length; index++) {
      if (containsWindow(view.frames[index], target)) return true;
    }
    return false;
  };
  window.addEventListener("message", (event) => {
    let data = event.data;
    if (typeof data === "string") {
      try {
        data = JSON.parse(data);
      } catch {
        return;
      }
    }
    if (data?.message === "Prebid Request") {
      record("creativeBridgeRequest", { adId: data.adId });
    }
    if (data?.type !== "fixture-creative-ready") return;
    const frame = [...document.querySelectorAll("iframe")].find((frame) =>
      containsWindow(frame.contentWindow, event.source),
    );
    if (!frame) return;
    creatives.push({
      label: data.label,
      index: Number(frame.dataset.request),
      element: elementIndex(frame.parentElement),
    });
    record("creativeReady", creatives.at(-1));
  });
  window.pbjs ??= { que: [], cmd: [] };
  window.__tsjs_gpt_enabled = true;
  window.__tsjs_prebid = {
    timeout: 15000,
    debug: false,
    serverSideBidders: [],
    clientSideBidders: [],
  };
})();
