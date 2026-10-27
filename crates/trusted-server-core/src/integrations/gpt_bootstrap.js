// Edge-injected GPT auction bootstrap.
//
// This is the minimal `window.tsjs.adInit` that runs on first page load
// before the TSJS bundle has had a chance to install its richer
// idempotent implementation. The bundle in
// crates/trusted-server-js/lib/src/integrations/gpt/index.ts overwrites `tsjs.adInit`
// once it loads.
//
// Contract with the bundle:
//   - Both implementations must set `window.tsjs.servicesEnabled = true`
//     after calling `enableSingleRequest()`/`enableServices()` so a
//     subsequent call becomes a no-op.
//   - `refresh()` is called only for the slots defined in this pass,
//     never the global slot list.
//
// Only installed if `window.tsjs.adInit` isn't already defined.
(function () {
  if (typeof window === "undefined") return;
  var ts = (window.tsjs = window.tsjs || {});
  var tag;

  if (window.__tsjs_gam_attribution_enabled === true) {
    tag = window.googletag = window.googletag || { cmd: [] };
    tag.cmd = tag.cmd || [];
    tag.cmd.push(function () {
      try {
        var gpt = window.googletag;
        if (gpt && typeof gpt.setConfig === "function") {
          // "ts" is the fixed GAM key, not the local window.tsjs alias.
          gpt.setConfig({ targeting: { ts: "true" } });
        }
      } catch (error) {
        // Attribution must not interrupt the existing bootstrap queue.
        ts.log &&
          ts.log.warn &&
          ts.log.warn("GAM attribution targeting failed", error);
      }
    });
  }

  if (ts.adInit) return;

  // Track whether the publisher disabled GPT initial load. Read the effective
  // googletag.getConfig() value when available, and wrap googletag.setConfig()
  // and the legacy pubads().disableInitialLoad() method so changes are
  // synchronized immediately and still detected when getConfig() is
  // unavailable. With initial load disabled, display() only registers a slot
  // and the ad request must come from a later refresh(); adInit() reads this to
  // refresh its own freshly defined
  // slots so they are not left blank. Pushed onto the command queue so it runs
  // before the publisher's own GPT configuration.
  function syncInitialLoadDisabled(gpt) {
    if (typeof gpt.getConfig !== "function") return false;
    var config = gpt.getConfig("disableInitialLoad");
    if (!config || typeof config.disableInitialLoad === "undefined") {
      return false;
    }
    ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
    return true;
  }

  tag = tag || (window.googletag = window.googletag || { cmd: [] });
  tag.cmd = tag.cmd || [];
  tag.cmd.push(function () {
    var gpt = window.googletag;
    syncInitialLoadDisabled(gpt);
    if (
      typeof gpt.setConfig === "function" &&
      !gpt.__tsInitialLoadConfigHooked
    ) {
      var originalSetConfig = gpt.setConfig.bind(gpt);
      gpt.setConfig = function (config) {
        var result = originalSetConfig.apply(gpt, arguments);
        if (
          !syncInitialLoadDisabled(gpt) &&
          config &&
          "disableInitialLoad" in config
        ) {
          ts.gptInitialLoadDisabled = config.disableInitialLoad === true;
        }
        return result;
      };
      gpt.__tsInitialLoadConfigHooked = true;
    }

    var pubads = gpt.pubads && gpt.pubads();
    if (
      !pubads ||
      typeof pubads.disableInitialLoad !== "function" ||
      pubads.__tsInitialLoadHooked
    ) {
      return;
    }
    var originalDisableInitialLoad = pubads.disableInitialLoad.bind(pubads);
    pubads.disableInitialLoad = function () {
      var result = originalDisableInitialLoad.apply(pubads, arguments);
      if (!syncInitialLoadDisabled(gpt)) {
        ts.gptInitialLoadDisabled = true;
      }
      return result;
    };
    pubads.__tsInitialLoadHooked = true;
  });

  var FIRST_IMPRESSION_LEASE_MS = 5000;
  var MAX_FIRST_IMPRESSION_SLOTS = 256;

  function firstImpressionState(now) {
    var generation = ts.navGeneration || 0;
    if (!ts.firstImpression || ts.firstImpression.generation !== generation) {
      ts.firstImpression = {
        generation: generation,
        nextToken: 0,
        slots: Object.create(null),
        fallbackSlots: Object.create(null),
      };
    }
    var state = ts.firstImpression;
    state.slots = Object.assign(Object.create(null), state.slots);
    state.fallbackSlots = Object.assign(
      Object.create(null),
      state.fallbackSlots,
    );
    Object.keys(state.slots).forEach(function (elementId) {
      var claim = state.slots[elementId];
      if (
        claim.generation !== generation ||
        claim.slotElementId !== elementId ||
        claim.element.ownerDocument !== document ||
        claim.element !== document.getElementById(elementId) ||
        !claim.element.isConnected
      ) {
        delete state.slots[elementId];
        return;
      }
      var hasReservedFallback =
        claim.owner === "publisher" &&
        (claim.phase === "auctioning" || claim.phase === "delivery_pending") &&
        state.fallbackSlots[elementId] === claim.element;
      Object.keys(claim.publisherAuctions || {}).forEach(function (token) {
        var auction = claim.publisherAuctions[token];
        if (
          auction.expiresAt <= now &&
          !hasReservedFallback &&
          !(claim.owner === "trusted_server" && auction.suppressDelivery)
        ) {
          delete claim.publisherAuctions[token];
        }
      });
      if (
        claim.owner === "publisher" &&
        (claim.phase === "auctioning" || claim.phase === "delivery_pending") &&
        Object.keys(claim.publisherAuctions || {}).length === 0 &&
        claim.expiresAt <= now &&
        !hasReservedFallback
      ) {
        delete state.slots[elementId];
      }
    });
    Object.keys(state.fallbackSlots).forEach(function (elementId) {
      var element = state.fallbackSlots[elementId];
      if (
        !element.isConnected ||
        element.id !== elementId ||
        document.getElementById(elementId) !== element
      ) {
        delete state.fallbackSlots[elementId];
      }
    });
    return state;
  }

  function firstImpressionClaim(element) {
    return firstImpressionState(Date.now()).slots[element.id];
  }

  function storeFirstImpressionClaim(state, claim) {
    if (
      !state.slots[claim.slotElementId] &&
      Object.keys(state.slots).length >= MAX_FIRST_IMPRESSION_SLOTS
    ) {
      return false;
    }
    state.slots[claim.slotElementId] = claim;
    return true;
  }

  function schedulePendingRenderDiagnostic(claim) {
    var startedAt = Date.now();
    window.setTimeout(function () {
      if (
        !ts.firstImpression ||
        ts.firstImpression.slots[claim.slotElementId] !== claim ||
        claim.generation !== (ts.navGeneration || 0) ||
        !claim.element.isConnected ||
        document.getElementById(claim.slotElementId) !== claim.element ||
        claim.owner !== "trusted_server" ||
        claim.phase === "rendered"
      )
        return;
      var diagnostic = { phase: claim.phase, ageMs: Date.now() - startedAt };
      claim.pendingRenderDiagnostic = diagnostic;
      try {
        if (ts.log && ts.log.debug) {
          ts.log.debug("[tsjs-gpt] initial render remains pending", {
            slot: claim.slotElementId,
            generation: claim.generation,
            phase: diagnostic.phase,
            ageMs: diagnostic.ageMs,
          });
        }
      } catch (error) {
        // Optional logging must never affect initial ownership or delivery.
      }
    }, FIRST_IMPRESSION_LEASE_MS);
  }

  function claimFirstImpressionForTrustedServer(element) {
    var now = Date.now();
    var state = firstImpressionState(now);
    var existing = state.slots[element.id];
    if (existing) {
      var canTransitionPublisherFallback =
        existing.owner === "publisher" &&
        existing.phase !== "requested" &&
        existing.phase !== "rendered" &&
        existing.expiresAt <= now &&
        state.fallbackSlots[element.id] === element;
      if (!canTransitionPublisherFallback) return null;
      existing.owner = "trusted_server";
      existing.phase = "delivery_pending";
      // Deliberately unbounded, matching first_impression.ts: only render,
      // navigation, or element replacement ends TS initial ownership.
      existing.expiresAt = Number.POSITIVE_INFINITY;
      Object.keys(existing.publisherAuctions || {}).forEach(function (token) {
        existing.publisherAuctions[token].suppressDelivery = true;
      });
      schedulePendingRenderDiagnostic(existing);
      return existing;
    }
    var claim = {
      generation: state.generation,
      slotElementId: element.id,
      element: element,
      owner: "trusted_server",
      phase: "delivery_pending",
      // Unbounded by contract; see the fallback transition above.
      expiresAt: Number.POSITIVE_INFINITY,
      publisherAuctions: Object.create(null),
    };
    if (!storeFirstImpressionClaim(state, claim)) return null;
    schedulePendingRenderDiagnostic(claim);
    return claim;
  }

  function releaseTrustedServerFirstImpressionClaim(element, claim) {
    var state = firstImpressionState(Date.now());
    if (
      state.slots[element.id] === claim &&
      claim.owner === "trusted_server" &&
      claim.phase === "delivery_pending"
    ) {
      delete state.slots[element.id];
      if (state.fallbackSlots[element.id] === element) {
        delete state.fallbackSlots[element.id];
      }
    }
  }

  function installFirstImpressionListeners() {
    if (ts.firstImpressionListenersInstalled) return;
    tag.cmd.push(function () {
      if (ts.firstImpressionListenersInstalled) return;
      var pubads = window.googletag.pubads();
      if (!pubads || typeof pubads.addEventListener !== "function") return;
      var observe = function (phase) {
        return function (event) {
          var elementId =
            event.slot && event.slot.getSlotElementId
              ? event.slot.getSlotElementId()
              : "";
          var element = elementId && document.getElementById(elementId);
          if (!element) return;
          if (phase === "rendered") observePublisherSlotRender(ts, event.slot);
          var state = firstImpressionState(Date.now());
          var claim = state.slots[elementId];
          if (!claim) {
            storeFirstImpressionClaim(state, {
              generation: state.generation,
              slotElementId: elementId,
              element: element,
              owner: "publisher",
              phase: phase,
              expiresAt: Number.POSITIVE_INFINITY,
              publisherAuctions: Object.create(null),
            });
            return;
          }
          if (claim.phase !== "rendered") claim.phase = phase;
          if (claim.owner === "publisher") {
            claim.expiresAt = Number.POSITIVE_INFINITY;
          } else if (phase === "rendered") {
            claim.publisherRegistrationClosed = true;
          }
        };
      };
      pubads.addEventListener("slotRequested", observe("requested"));
      pubads.addEventListener("slotRenderEnded", observe("rendered"));
      ts.firstImpressionListenersInstalled = true;
    });
  }

  installFirstImpressionListeners();

  // Protocol v1 deliberately mirrors publisher_registration.ts; the shared executable contract checks both.
  const CANDIDATE_KEYS = [
    "hb_pb",
    "hb_bidder",
    "hb_adid",
    "hb_cache_host",
    "hb_cache_path",
  ];
  function registrationTag() {
    return window.googletag;
  }
  function invalidatePublisherSlotRegistrations(ts) {
    var _a, _b, _c;
    const previous = ts.publisherSlotRegistrationState;
    for (const record of Object.values(
      (_a = previous == null ? void 0 : previous.records) != null ? _a : {},
    )) {
      record.status = { status: "cancelled", reason: "navigation" };
      record.element = void 0;
      record.gptSlot = void 0;
    }
    ts.publisherSlotRegistrationState = {
      generation: (_b = ts.navGeneration) != null ? _b : 0,
      nextToken:
        (_c = previous == null ? void 0 : previous.nextToken) != null ? _c : 0,
      records: /* @__PURE__ */ Object.create(null),
    };
  }
  function registrationState(ts) {
    var _a, _b;
    if (
      ((_a = ts.publisherSlotRegistrationState) == null
        ? void 0
        : _a.generation) !== ((_b = ts.navGeneration) != null ? _b : 0)
    ) {
      invalidatePublisherSlotRegistrations(ts);
    }
    return ts.publisherSlotRegistrationState;
  }
  function settlePublisherSlotPayload(ts, generation, slots, bids) {
    var _a, _b;
    if (((_a = ts.navGeneration) != null ? _a : 0) !== generation) return;
    registrationState(ts).payload = { generation, slots, bids };
    (_b = ts.publisherSlotRegistrationDispatch) == null ? void 0 : _b.call(ts);
  }
  function retainPublisherSlotMappings(ts, mapping, targetingKeys) {
    const state = registrationState(ts);
    for (const record of Object.values(state.records)) {
      if (!record.consumed || !record.targetingKeys) continue;
      mapping[record.elementId] = record.slotId;
      targetingKeys[record.elementId] = record.targetingKeys;
    }
  }
  function registeredCandidateAllowed(ts, slotId, bid) {
    var _a, _b, _c, _d, _e, _f, _g, _h;
    const record =
      ((_a = ts.publisherSlotRegistrationState) == null
        ? void 0
        : _a.generation) === ((_b = ts.navGeneration) != null ? _b : 0)
        ? ts.publisherSlotRegistrationState.records[slotId]
        : void 0;
    if (record) {
      if (
        !record.consumed ||
        record.selectedCandidate !== bid ||
        !((_c = record.element) == null ? void 0 : _c.isConnected) ||
        record.element.id !== record.elementId ||
        document.getElementById(record.elementId) !== record.element
      )
        return false;
      try {
        return (
          !!record.gptSlot &&
          !!((_g =
            (_e = (_d = registrationTag()) == null ? void 0 : _d.pubads) == null
              ? void 0
              : (_f = _e.call(_d)).getSlots) == null
            ? void 0
            : _g.call(_f).includes(record.gptSlot))
        );
      } catch (e) {
        return false;
      }
    }
    return !((_h = ts.adSlots) == null
      ? void 0
      : _h.some(
          (slot) =>
            slot.id === slotId &&
            slot.initialization === "publisher_registered",
        ));
  }
  function observePublisherSlotRender(ts, native) {
    var _a, _b;
    const state = ts.publisherSlotRegistrationState;
    if (
      (state == null ? void 0 : state.generation) !==
      ((_a = ts.navGeneration) != null ? _a : 0)
    )
      return;
    for (const record of Object.values(state.records)) {
      if (
        record.gptSlot === native &&
        record.consumed &&
        ((_b = record.element) == null ? void 0 : _b.isConnected) &&
        record.element.id === record.elementId &&
        document.getElementById(record.elementId) === record.element &&
        (record.status.status === "requested" ||
          record.status.status === "dispatching")
      ) {
        record.status = { status: "rendered" };
      }
    }
  }
  function finiteSize(size) {
    return (
      Array.isArray(size) &&
      size.length === 2 &&
      size.every(
        (value) =>
          typeof value === "number" && Number.isInteger(value) && value > 0,
      )
    );
  }
  function normalizedFormats(formats) {
    return [
      ...new Map(
        formats.map((size) => [size.join("x"), [size[0], size[1]]]),
      ).values(),
    ].sort((left, right) => left[0] - right[0] || left[1] - right[1]);
  }
  function loadPolicy(ts, tag) {
    var _a, _b;
    try {
      const effective =
        (_b =
          (_a = tag == null ? void 0 : tag.getConfig) == null
            ? void 0
            : _a.call(tag, "disableInitialLoad")) == null
          ? void 0
          : _b.disableInitialLoad;
      if (effective !== void 0) ts.gptInitialLoadDisabled = effective === true;
      if (
        effective === true ||
        (effective === void 0 && ts.gptInitialLoadDisabled === true)
      )
        return;
      return {
        status: "rejected",
        reason:
          effective === false || ts.gptInitialLoadDisabled === false
            ? "initial_load_enabled"
            : "initial_load_unknown",
      };
    } catch (e) {
      return { status: "rejected", reason: "initial_load_unknown" };
    }
  }
  function exactElement(id) {
    const element = document.getElementById(id);
    if (!id || !(element == null ? void 0 : element.isConnected))
      return { error: { status: "rejected", reason: "missing_element" } };
    if (
      Array.from(document.querySelectorAll("[id]")).filter(
        (candidate) => candidate.id === id,
      ).length !== 1
    ) {
      return { error: { status: "rejected", reason: "duplicate_dom_id" } };
    }
    return { element };
  }
  function registrationHandle(ts, record) {
    return {
      getStatus: () => {
        var _a;
        if (
          record.generation !== ((_a = ts.navGeneration) != null ? _a : 0) &&
          !record.consumed
        ) {
          record.status = { status: "cancelled", reason: "navigation" };
        }
        return { ...record.status };
      },
      cancel: () => {
        if (
          record.status.status === "rejected" ||
          record.status.status === "cancelled"
        )
          return;
        record.status = { status: "cancelled", reason: "explicit" };
        if (!record.consumed) {
          record.element = void 0;
          record.gptSlot = void 0;
        }
      },
    };
  }
  function installPublisherSlotRegistration(ts, claim) {
    registrationState(ts);
    ts.publisherSlotRegistrationVersion = 1;
    ts.registerPublisherSlot = (options) => {
      var _a, _b, _c, _d, _e;
      const state = registrationState(ts);
      const rejected = (status) => ({
        getStatus: () => ({ ...status }),
        cancel: () => {},
      });
      if (
        !options ||
        typeof options.slotId !== "string" ||
        !options.slotId ||
        !Number.isInteger(options.navigationGeneration) ||
        !Array.isArray(options.eligibleFormats) ||
        !options.eligibleFormats.every(finiteSize) ||
        !options.gptSlot ||
        ![
          "getSlotElementId",
          "getAdUnitPath",
          "setTargeting",
          "clearTargeting",
        ].every((key) => typeof options.gptSlot[key] === "function")
      ) {
        return rejected({ status: "rejected", reason: "invalid_arguments" });
      }
      if (options.navigationGeneration !== state.generation)
        return rejected({ status: "rejected", reason: "stale_generation" });
      const tag = registrationTag();
      const policy = loadPolicy(ts, tag);
      if (policy) return rejected(policy);
      let elementId;
      let adUnitPath;
      try {
        elementId = options.gptSlot.getSlotElementId();
        adUnitPath = options.gptSlot.getAdUnitPath();
        if (typeof elementId !== "string" || typeof adUnitPath !== "string")
          throw new Error("invalid slot identity");
        if (
          !((_c =
            (_a = tag == null ? void 0 : tag.pubads) == null
              ? void 0
              : (_b = _a.call(tag)).getSlots) == null
            ? void 0
            : _c.call(_b).includes(options.gptSlot))
        )
          return rejected({
            status: "rejected",
            reason: "native_slot_missing",
          });
      } catch (e) {
        return rejected({ status: "rejected", reason: "invalid_arguments" });
      }
      const { element, error } = exactElement(elementId);
      if (error || !element) return rejected(error);
      const configured =
        (_d = state.payload) == null
          ? void 0
          : _d.slots.find((slot) => slot.id === options.slotId);
      if (configured && configured.gam_unit_path !== adUnitPath) {
        return rejected({ status: "rejected", reason: "path_mismatch" });
      }
      const eligibleFormats = normalizedFormats(options.eligibleFormats);
      const previous = state.records[options.slotId];
      if (
        previous &&
        (previous.consumed || previous.status.status !== "cancelled")
      ) {
        if (
          previous.gptSlot === options.gptSlot &&
          previous.element === element &&
          JSON.stringify(previous.eligibleFormats) ===
            JSON.stringify(eligibleFormats)
        )
          return registrationHandle(ts, previous);
        return rejected({
          status: "rejected",
          reason: "conflicting_registration",
        });
      }
      const record = {
        generation: state.generation,
        slotId: options.slotId,
        token: ++state.nextToken,
        gptSlot: options.gptSlot,
        element,
        elementId,
        adUnitPath,
        eligibleFormats,
        status: { status: "waiting_payload" },
        consumed: false,
        queued: false,
      };
      state.records[record.slotId] = record;
      (_e = ts.publisherSlotRegistrationDispatch) == null
        ? void 0
        : _e.call(ts);
      return registrationHandle(ts, record);
    };
    ts.publisherSlotRegistrationDispatch = (readyRecord) => {
      const state = registrationState(ts);
      const current = (record) =>
        state.records[record.slotId] === record &&
        record.generation === state.generation &&
        !record.consumed &&
        (record.status.status === "waiting_payload" ||
          record.status.status === "waiting_delivery");
      const dispatch = (record) => {
        var _a, _b, _c, _d, _e;
        if (!current(record)) return;
        record.queued = false;
        const payload = state.payload;
        if (!payload || payload.generation !== state.generation) return;
        const slot = payload.slots.find(
          (candidate) => candidate.id === record.slotId,
        );
        if (!slot || slot.initialization !== "publisher_registered") {
          record.status = {
            status: "inactive",
            reason: slot ? "not_registered_mode" : "not_admitted",
          };
          return;
        }
        const tag = registrationTag();
        const policy = loadPolicy(ts, tag);
        if (policy) {
          record.status = policy;
          return;
        }
        const identity = exactElement(record.elementId);
        if (
          !record.element ||
          identity.element !== record.element ||
          record.element.id !== record.elementId
        ) {
          record.status = { status: "cancelled", reason: "element_changed" };
          return;
        }
        const native = record.gptSlot;
        const service =
          (_a = tag == null ? void 0 : tag.pubads) == null
            ? void 0
            : _a.call(tag);
        if (
          !native ||
          !((_b = service == null ? void 0 : service.getSlots) == null
            ? void 0
            : _b.call(service).includes(native)) ||
          native.getSlotElementId() !== record.elementId
        ) {
          record.status = {
            status: "cancelled",
            reason: "native_slot_missing",
          };
          return;
        }
        if (
          native.getAdUnitPath() !== record.adUnitPath ||
          record.adUnitPath !== slot.gam_unit_path
        ) {
          record.status = { status: "rejected", reason: "path_mismatch" };
          return;
        }
        if (document.hidden) {
          queue(record);
          return;
        }
        const first = claim(record.element);
        if (!first) {
          record.status = { status: "publisher_owned", reason: "prior_claim" };
          return;
        }
        record.consumed = true;
        record.status = { status: "dispatching" };
        const wasInternal = ts.gptSlotHandoffInternal;
        const wasRefreshing = ts.adInitRefreshInProgress;
        try {
          const bid = payload.bids[record.slotId];
          const fits =
            bid &&
            finiteSize([bid.w, bid.h]) &&
            slot.formats.some(
              (size) => size[0] === bid.w && size[1] === bid.h,
            ) &&
            record.eligibleFormats.some(
              (size) => size[0] === bid.w && size[1] === bid.h,
            );
          const candidate =
            ts.tsRenderBridgeVersion === 1 && fits ? bid : void 0;
          record.selectedCandidate = candidate;
          if (!candidate)
            record.noCandidateReason = !bid
              ? "no_bid"
              : ts.tsRenderBridgeVersion !== 1
                ? "renderer_unavailable"
                : "incompatible_dimensions";
          const previousKeys =
            (_d =
              (_c = ts.prevSlotTargetingKeys) == null
                ? void 0
                : _c[record.elementId]) != null
              ? _d
              : [];
          for (const key of /* @__PURE__ */ new Set([
            ...CANDIDATE_KEYS,
            "ts_initial",
            ...previousKeys,
          ]))
            native.clearTargeting(key);
          const targeting = { ...slot.targeting, ts_initial: "1" };
          for (const key of CANDIDATE_KEYS) {
            delete targeting[key];
            if (candidate == null ? void 0 : candidate[key])
              targeting[key] = String(candidate[key]);
          }
          record.targetingKeys = Object.keys(targeting);
          for (const [key, value] of Object.entries(targeting))
            native.setTargeting(key, value);
          first.targeting = targeting;
          ts.divToSlotId = Object.assign(
            /* @__PURE__ */ Object.create(null),
            ts.divToSlotId,
          );
          ts.prevSlotTargetingKeys = Object.assign(
            /* @__PURE__ */ Object.create(null),
            ts.prevSlotTargetingKeys,
          );
          retainPublisherSlotMappings(
            ts,
            ts.divToSlotId,
            ts.prevSlotTargetingKeys,
          );
          try {
            (_e = ts.gptDiagnosticsRecorder) == null
              ? void 0
              : _e.recordTrustedServerOpportunity(
                  native,
                  slot.id,
                  (candidate == null ? void 0 : candidate.hb_adid)
                    ? candidate.adm || candidate.hb_cache_host
                      ? "renderable_candidate"
                      : "unrenderable_candidate"
                    : "no_candidate",
                  candidate == null ? void 0 : candidate.hb_auction_id,
                  record.eligibleFormats,
                );
          } catch (e) {}
          ts.gptSlotHandoffInternal = true;
          ts.adInitRefreshInProgress = true;
          tag.display(record.elementId);
          service.refresh([native]);
          if (record.status.status === "dispatching")
            record.status = { status: "requested" };
        } catch (e) {
          record.status = {
            status: "failed_after_dispatch",
            reason: "delivery_exception",
          };
        } finally {
          ts.gptSlotHandoffInternal = wasInternal;
          ts.adInitRefreshInProgress = wasRefreshing;
        }
      };
      const queue = (record) => {
        if (!current(record) || record.queued) return;
        record.queued = true;
        record.status = { status: "waiting_delivery" };
        const foreground = () => {
          if (!current(record)) return;
          if (document.hidden) {
            document.addEventListener("visibilitychange", foreground, {
              once: true,
            });
            return;
          }
          window.requestAnimationFrame(() => {
            var _a, _b;
            if (!current(record)) return;
            if (document.hidden) {
              foreground();
              return;
            }
            (_b = (_a = registrationTag()) == null ? void 0 : _a.cmd) == null
              ? void 0
              : _b.push(() => {
                  var _a2;
                  return (_a2 = ts.publisherSlotRegistrationDispatch) == null
                    ? void 0
                    : _a2.call(ts, record);
                });
          });
        };
        foreground();
      };
      if (readyRecord) {
        dispatch(readyRecord);
        return;
      }
      for (const record of Object.values(state.records)) {
        if (!current(record)) continue;
        if (!state.payload) {
          record.status = { status: "waiting_payload" };
          continue;
        }
        const admitted = state.payload.slots.find(
          (slot) => slot.id === record.slotId,
        );
        if (!admitted || admitted.initialization !== "publisher_registered") {
          record.status = {
            status: "inactive",
            reason: admitted ? "not_registered_mode" : "not_admitted",
          };
          continue;
        }
        queue(record);
      }
    };
  }

  installPublisherSlotRegistration(ts, claimFirstImpressionForTrustedServer);

  // Snapshot exact old native identities before retiring their registration records.
  // This mirrors runtime navigation cleanup, without adding a bootstrap auction client.
  function clearBootstrapNavigationTargeting() {
    var gpt = window.googletag;
    var previousKeys = Object.assign(
      Object.create(null),
      ts.prevSlotTargetingKeys,
    );
    var registeredRecords = Object.values(
      (ts.publisherSlotRegistrationState &&
        ts.publisherSlotRegistrationState.records) ||
        {},
    ).filter(function (record) {
      return record.consumed && record.targetingKeys;
    });
    var registeredIds = new Set();
    var snapshot = new Map();
    var baseKeys = CANDIDATE_KEYS.concat(["ts_initial"]);
    registeredRecords.forEach(function (record) {
      registeredIds.add(record.elementId);
      previousKeys[record.elementId] = record.targetingKeys;
      if (record.gptSlot)
        snapshot.set(record.gptSlot, baseKeys.concat(record.targetingKeys));
    });
    var touchedIds = new Set(
      Object.keys(previousKeys).concat(Object.keys(ts.divToSlotId || {})),
    );
    var pubads = gpt.pubads && gpt.pubads();
    var live = pubads && pubads.getSlots ? pubads.getSlots() : [];
    live.concat(ts.prevGptSlots || []).forEach(function (slot) {
      var id = slot.getSlotElementId();
      if (touchedIds.has(id) && !registeredIds.has(id)) {
        snapshot.set(slot, baseKeys.concat(previousKeys[id] || []));
      }
    });
    var clearSnapshot = function () {
      var current = window.googletag.pubads && window.googletag.pubads();
      if (!current) return false;
      var currentSlots = current.getSlots ? current.getSlots() : [];
      snapshot.forEach(function (keys, slot) {
        if (
          !currentSlots.includes(slot) ||
          typeof slot.clearTargeting !== "function"
        )
          return;
        new Set(keys).forEach(function (key) {
          slot.clearTargeting(key);
        });
      });
      return true;
    };
    if (snapshot.size > 0 && !clearSnapshot()) tag.cmd.push(clearSnapshot);
    ts.prevSlotTargetingKeys = Object.create(null);
    ts.divToSlotId = Object.create(null);
  }

  // One observer resolves a replaceable handler, so runtime takeover preserves
  // framework wrappers and transfers counter ownership instead of nesting writers.
  function installBootstrapNavigationBridge() {
    if (ts.gptNavigationBridge) return;
    var currentPath = location.pathname;
    ts.gptNavigationBridge = {
      onNavigate: function (path) {
        if (path === currentPath) return;
        currentPath = path;
        clearBootstrapNavigationTargeting();
        ts.navGeneration = (ts.navGeneration || 0) + 1;
        invalidatePublisherSlotRegistrations(ts);
        delete ts.firstImpression;
        Object.entries(ts.gptSlotHandoffs || {}).forEach(function (entry) {
          if (!entry[1].publisherClaimed) delete ts.gptSlotHandoffs[entry[0]];
        });
        ts.gptNavigationBridge.pendingGeneration = ts.navGeneration;
      },
    };
    var observe = function () {
      ts.gptNavigationBridge.onNavigate(location.pathname);
    };
    ["pushState", "replaceState"].forEach(function (method) {
      var original = history[method].bind(history);
      history[method] = function (state, unused, url) {
        original(state, unused, url);
        observe();
      };
    });
    window.addEventListener("popstate", observe, true);
  }
  installBootstrapNavigationBridge();

  // Minimal fallback for tsjs.scheduleInitialAdInit, mirroring the bundle's
  // hydration-safe scheduler in
  // crates/trusted-server-js/lib/src/integrations/gpt/index.ts: the </body>
  // bids script hands the SSR bids payload to this scheduler, which applies
  // it and runs adInit only while the page is still on navigation
  // generation 0 (the SSR document), after window load plus a double
  // requestAnimationFrame so the call lands outside React's hydration
  // window. Keeps initial server-side ads working when the main TSJS bundle
  // fails to load; the bundle overwrites this with the full implementation.
  //
  // Hidden documents: rAF is not serviced while the document is hidden, so a
  // background-tab load holds the initial adInit until first view. Intended,
  // and deliberately identical to the bundle scheduler — the impression is
  // spent on a viewed tab, and the post-hydration guarantee holds whenever
  // the request is actually issued.
  ts.scheduleInitialAdInit = function (initialBids, initialSlots) {
    // The bundle may replace this scheduler after the fallback claims the initial
    // pass. Keep the latch on the shared document API so replacement cannot reset it.
    if ((ts.navGeneration || 0) !== 0 || ts.initialAdInitScheduled) return;
    ts.initialAdInitScheduled = true;
    // Slots are generation-guarded for the same reason the bids are: the
    // shared-template seam sends both, and an assignment made before this call
    // would overwrite a committed SPA navigation's slots.
    if (initialSlots !== undefined) ts.adSlots = initialSlots;
    if (initialBids !== undefined) ts.bids = initialBids;
    if (initialBids !== undefined || initialSlots !== undefined) {
      settlePublisherSlotPayload(ts, 0, ts.adSlots || [], ts.bids || {});
    }
    var fire = function () {
      if ((ts.navGeneration || 0) !== 0) return;
      if (typeof ts.adInit === "function") ts.adInit();
    };
    var afterFrames = function () {
      window.requestAnimationFrame(function () {
        window.requestAnimationFrame(fire);
      });
    };
    if (document.readyState === "complete") afterFrames();
    else window.addEventListener("load", afterFrames, { once: true });
  };

  function findSlotByElementId(pubads, elementId) {
    var slots = pubads.getSlots ? pubads.getSlots() : [];
    return (
      slots.find(function (slot) {
        return slot.getSlotElementId() === elementId;
      }) || null
    );
  }

  function normalizedGptFormats(formats) {
    return formats.length === 2 &&
      formats.every(function (format) {
        return typeof format === "number";
      })
      ? [formats]
      : formats;
  }

  function handoffFormatsMatch(handoff, formats) {
    return (
      JSON.stringify(handoff.formats) ===
      JSON.stringify(normalizedGptFormats(formats))
    );
  }

  function matchingHandoff(pubads, adUnitPath, formats, elementId) {
    var exact = ts.gptSlotHandoffs && ts.gptSlotHandoffs[elementId];
    if (exact) return exact.publisherClaimed ? null : exact;

    var candidates = Object.values(ts.gptSlotHandoffs || {}).filter(
      function (handoff, index, allHandoffs) {
        return (
          allHandoffs.indexOf(handoff) === index &&
          !handoff.publisherClaimed &&
          !document.getElementById(handoff.slotElementId) &&
          elementId.startsWith(handoff.divIdPrefix) &&
          handoff.gamUnitPath === adUnitPath &&
          handoffFormatsMatch(handoff, formats) &&
          findSlotByElementId(pubads, handoff.slotElementId)
        );
      },
    );
    return candidates.length === 1 ? candidates[0] : null;
  }

  function displayTargetElementId(target) {
    if (typeof target === "string") return target;
    if (target && typeof target.getSlotElementId === "function") {
      return target.getSlotElementId();
    }
    return target && target.id ? target.id : null;
  }

  function isElementVisible(element) {
    if (typeof element.checkVisibility === "function") {
      return element.checkVisibility({
        checkVisibilityCSS: true,
        visibilityProperty: true,
      });
    }

    for (var current = element; current; current = current.parentElement) {
      var style = window.getComputedStyle(current);
      if (
        style.display === "none" ||
        style.visibility === "hidden" ||
        style.visibility === "collapse"
      ) {
        return false;
      }
    }
    return true;
  }

  function slotElementHasLayout(element) {
    if (!isElementVisible(element)) return false;
    var elementRect = element.getBoundingClientRect();
    if (elementRect.width > 0 && elementRect.height > 0) return true;

    var container = document.getElementById(element.id + "-container");
    if (!container || !isElementVisible(container)) return false;
    var containerRect = container.getBoundingClientRect();
    return containerRect.width > 0;
  }

  function resolveSlotElementByDivId(divId) {
    if (!divId) {
      return { element: null, prefixMatchCount: 0, activeMatchCount: 0 };
    }
    // Exact-id matches intentionally skip the visibility tiers below: a
    // configured literal id is unambiguous, so a hidden match is still the
    // right element. Prefix matches go through the tiers because a prefix can
    // match several candidates and only visibility/layout disambiguates them —
    // so a hidden exact-id match resolves while a hidden prefix match does not.
    var exact = document.getElementById(divId);
    if (exact) {
      return { element: exact, prefixMatchCount: 1, activeMatchCount: 1 };
    }

    var idElements = document.querySelectorAll("[id]");
    var prefixMatches = [];
    for (var i = 0; i < idElements.length; i++) {
      var candidate = idElements[i];
      if (
        candidate.id.startsWith(divId) &&
        !candidate.id.endsWith("-container")
      ) {
        prefixMatches.push(candidate);
      }
    }
    // A unique prefix match may be a lazy slot that has not been sized yet,
    // but it must still be visible through its ancestor containers.
    if (prefixMatches.length === 1 && isElementVisible(prefixMatches[0])) {
      return {
        element: prefixMatches[0],
        prefixMatchCount: 1,
        activeMatchCount: 1,
      };
    }

    var visibleMatches = prefixMatches.filter(isElementVisible);
    if (visibleMatches.length === 1) {
      return {
        element: visibleMatches[0],
        prefixMatchCount: prefixMatches.length,
        activeMatchCount: 1,
      };
    }

    var activeMatches = visibleMatches.filter(slotElementHasLayout);
    return {
      element: activeMatches.length === 1 ? activeMatches[0] : null,
      prefixMatchCount: prefixMatches.length,
      activeMatchCount: activeMatches.length,
    };
  }

  function runHandoffInternal(callback) {
    var wasInternal = ts.gptSlotHandoffInternal;
    ts.gptSlotHandoffInternal = true;
    try {
      return callback();
    } finally {
      ts.gptSlotHandoffInternal = wasInternal;
    }
  }

  // TS cannot wait an arbitrary amount of time for a framework to define a
  // slot: publishers that never define one would render blank. Instead, TS
  // defines its fallback on the actual inner div and aliases only a later
  // publisher defineSlot() for that exact div, or a hydration-renamed replacement
  // after the original div is gone, to the same GPT slot.
  function installSlotHandoff() {
    window.googletag.cmd.push(function () {
      var tag = window.googletag;
      var pubads = tag.pubads && tag.pubads();
      if (!tag.defineSlot || !tag.display || !pubads) return;

      if (!tag.defineSlot.__tsSlotHandoffPatched) {
        var originalDefineSlot = tag.defineSlot.bind(tag);
        var patchedDefineSlot = function (adUnitPath, formats, elementId) {
          if (!ts.gptSlotHandoffInternal && typeof elementId === "string") {
            var handoff = matchingHandoff(
              pubads,
              adUnitPath,
              formats,
              elementId,
            );
            if (handoff) {
              var existingSlot = findSlotByElementId(
                pubads,
                handoff.slotElementId,
              );
              if (existingSlot) {
                ts.gptSlotHandoffs[elementId] = handoff;
                handoff.publisherClaimed = true;
                // The supported publisher lifecycle is defineSlot → addService → display.
                // Intentionally wait for that display instead of applying a time heuristic.
                handoff.suppressPublisherDisplay = true;
                handoff.suppressPublisherRefresh =
                  ts.gptInitialLoadDisabled === true;
                ts.prevGptSlots = (ts.prevGptSlots || []).filter(
                  function (ownedSlot) {
                    return ownedSlot !== existingSlot;
                  },
                );
                if (
                  handoff.gamUnitPath !== adUnitPath ||
                  !handoffFormatsMatch(handoff, formats)
                ) {
                  ts.log &&
                    ts.log.warn &&
                    ts.log.warn(
                      "GPT slot handoff: publisher definition differs from TS configuration",
                      elementId,
                    );
                }
                return existingSlot;
              }
            }
          }
          return elementId === undefined
            ? originalDefineSlot(adUnitPath, formats)
            : originalDefineSlot(adUnitPath, formats, elementId);
        };
        patchedDefineSlot.__tsSlotHandoffPatched = true;
        tag.defineSlot = patchedDefineSlot;
      }

      if (!tag.display.__tsSlotHandoffPatched) {
        var originalDisplay = tag.display.bind(tag);
        var patchedDisplay = function (target) {
          var elementId = displayTargetElementId(target);
          var handoff =
            elementId && ts.gptSlotHandoffs && ts.gptSlotHandoffs[elementId];
          if (
            !ts.gptSlotHandoffInternal &&
            handoff &&
            handoff.suppressPublisherDisplay
          ) {
            handoff.suppressPublisherDisplay = false;
            return;
          }
          originalDisplay(target);
        };
        patchedDisplay.__tsSlotHandoffPatched = true;
        tag.display = patchedDisplay;
      }

      if (!pubads.refresh.__tsSlotHandoffPatched) {
        var originalRefresh = pubads.refresh.bind(pubads);
        var callRefresh = function (slots, options) {
          if (options === undefined) {
            originalRefresh(slots);
          } else {
            originalRefresh(slots, options);
          }
        };
        var patchedRefresh = function (requestedSlots, options) {
          if (ts.gptSlotHandoffInternal) {
            callRefresh(requestedSlots, options);
            return;
          }
          var slots =
            requestedSlots || (pubads.getSlots ? pubads.getSlots() : null);
          if (!slots) {
            callRefresh(requestedSlots, options);
            return;
          }
          var suppressed = false;
          var remainingSlots = slots.filter(function (slot) {
            var handoff =
              ts.gptSlotHandoffs && ts.gptSlotHandoffs[slot.getSlotElementId()];
            if (!handoff || !handoff.suppressPublisherRefresh) return true;
            handoff.suppressPublisherRefresh = false;
            suppressed = true;
            return false;
          });
          if (!suppressed) {
            callRefresh(requestedSlots, options);
          } else if (remainingSlots.length > 0) {
            callRefresh(remainingSlots, options);
          }
        };
        patchedRefresh.__tsSlotHandoffPatched = true;
        pubads.refresh = patchedRefresh;
      }
    });
  }

  installSlotHandoff();

  function bootstrapTargeting(slot, bid) {
    var targeting = Object.assign({}, slot.targeting || {});
    ["hb_pb", "hb_bidder", "hb_adid", "hb_cache_host", "hb_cache_path"].forEach(
      function (key) {
        if (bid[key]) targeting[key] = String(bid[key]);
      },
    );
    targeting.ts_initial = "1";
    return targeting;
  }

  function scheduleFirstImpressionFallback(slot, bid, element, generation) {
    var state = firstImpressionState(Date.now());
    if (state.fallbackSlots[element.id]) return;
    state.fallbackSlots[element.id] = element;

    var retry = function () {
      if (
        (ts.navGeneration || 0) !== generation ||
        !element.isConnected ||
        document.getElementById(element.id) !== element
      ) {
        return;
      }
      var claim = firstImpressionClaim(element);
      if (claim) {
        if (
          claim.owner !== "publisher" ||
          claim.phase === "requested" ||
          claim.phase === "rendered"
        ) {
          return;
        }
        var delay = Math.max(0, claim.expiresAt - Date.now());
        if (delay > 0) {
          window.setTimeout(retry, delay + 1);
          return;
        }
      }

      tag.cmd.push(function () {
        if (
          (ts.navGeneration || 0) !== generation ||
          !element.isConnected ||
          document.getElementById(element.id) !== element
        ) {
          return;
        }
        var fallbackClaim = claimFirstImpressionForTrustedServer(element);
        if (!fallbackClaim) return;
        var pubads = window.googletag.pubads();
        var existingSlots = pubads.getSlots ? pubads.getSlots() : [];
        var gptSlot =
          existingSlots.find(function (candidate) {
            return candidate.getSlotElementId() === element.id;
          }) || null;
        var tsOwned = false;
        if (!gptSlot) {
          gptSlot = runHandoffInternal(function () {
            return window.googletag.defineSlot(
              slot.gam_unit_path,
              slot.formats,
              element.id,
            );
          });
          if (!gptSlot) {
            releaseTrustedServerFirstImpressionClaim(element, fallbackClaim);
            return;
          }
          gptSlot.addService(pubads);
          tsOwned = true;
          ts.gptSlotHandoffs = ts.gptSlotHandoffs || {};
          ts.gptSlotHandoffs[element.id] = {
            gamUnitPath: slot.gam_unit_path,
            formats: slot.formats,
            divIdPrefix: slot.div_id,
            slotElementId: element.id,
            publisherClaimed: false,
            suppressPublisherDisplay: false,
            suppressPublisherRefresh: false,
          };
        }

        var targeting = bootstrapTargeting(slot, bid);
        Object.entries(targeting).forEach(function (entry) {
          gptSlot.setTargeting(entry[0], entry[1]);
        });
        fallbackClaim.targeting = targeting;
        var slotElementId = gptSlot.getSlotElementId() || element.id;
        ts.divToSlotId = ts.divToSlotId || {};
        ts.divToSlotId[element.id] = slot.id;
        ts.divToSlotId[slotElementId] = slot.id;
        ts.prevSlotTargetingKeys = ts.prevSlotTargetingKeys || {};
        var targetingKeys = Object.keys(slot.targeting || {});
        ts.prevSlotTargetingKeys[element.id] = targetingKeys;
        ts.prevSlotTargetingKeys[slotElementId] = targetingKeys;
        if (tsOwned) {
          ts.prevGptSlots = ts.prevGptSlots || [];
          ts.prevGptSlots.push(gptSlot);
        }
        if (!ts.servicesEnabled) {
          pubads.enableSingleRequest();
          window.googletag.enableServices();
          ts.servicesEnabled = true;
        }
        if (tsOwned) {
          runHandoffInternal(function () {
            window.googletag.display(slotElementId);
          });
        }
        syncInitialLoadDisabled(window.googletag);
        if (!tsOwned || ts.gptInitialLoadDisabled) {
          ts.adInitRefreshInProgress = true;
          try {
            runHandoffInternal(function () {
              pubads.refresh([gptSlot]);
            });
          } finally {
            ts.adInitRefreshInProgress = false;
          }
        }
      });
    };

    retry();
  }

  ts.adInit = function () {
    var slots = (ts.adSlots || []).filter(function (slot) {
      return slot.initialization !== "publisher_registered";
    });
    var bids = ts.bids || {};
    var divToSlotId = Object.create(null);
    var nextSlotTargetingKeys = Object.create(null);
    // Generation this invocation belongs to. The slot work below is queued on
    // googletag.cmd, which drains only when GPT loads; recheck first inside
    // the queued callback so a navigation committed in the gap cancels the
    // stale mutation — mirrors the bundle's adInit.
    var generation = ts.navGeneration || 0;
    var warnedResolutionFailures = Object.create(null);

    googletag.cmd.push(function () {
      if ((ts.navGeneration || 0) !== generation) return;
      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
      var newSlots = [];
      // Publisher-owned slots TS reused — refreshed to pick up server-side
      // targeting. The publisher already display()ed these.
      var slotsToRefresh = [];
      // Element IDs of slots TS defined itself. GPT requires display() to
      // register/render a freshly-defined slot; refresh() alone no-ops for a
      // slot that was never displayed, so these are display()ed instead.
      var slotsToDisplay = [];
      slots.forEach(function (slot) {
        // Resolve actual div ID: exact match first, then the visibility and
        // geometry tiers for prefix matches. Responsive publishers may emit
        // several mutually exclusive siblings for one stable prefix, so
        // document order is not sufficient.
        var resolution = resolveSlotElementByDivId(slot.div_id);
        var el = resolution.element;
        if (!el) {
          if (!warnedResolutionFailures[slot.div_id]) {
            if (resolution.prefixMatchCount > 1) {
              warnedResolutionFailures[slot.div_id] = true;
              if (ts.log && typeof ts.log.warn === "function") {
                ts.log.warn(
                  "GPT slot prefix did not resolve to one active element",
                  {
                    divId: slot.div_id,
                    prefixMatchCount: resolution.prefixMatchCount,
                    activeMatchCount: resolution.activeMatchCount,
                  },
                );
              }
            } else if (
              resolution.prefixMatchCount === 1 &&
              resolution.activeMatchCount === 0
            ) {
              // The common breakpoint-hidden config: the prefix matched one
              // element but it is hidden, so the slot is skipped. Logged so a
              // blank placement is diagnosable without stepping the resolver.
              warnedResolutionFailures[slot.div_id] = true;
              if (ts.log && typeof ts.log.debug === "function") {
                ts.log.debug(
                  "GPT slot prefix matched only a hidden element; skipping slot",
                  { divId: slot.div_id },
                );
              }
            }
          }
          return;
        }
        var actualDivId = el.id;
        var b = bids[slot.id] || {};
        var tsClaim = claimFirstImpressionForTrustedServer(el);
        if (!tsClaim) {
          var currentClaim = firstImpressionClaim(el);
          if (currentClaim && currentClaim.owner === "publisher") {
            scheduleFirstImpressionFallback(slot, b, el, generation);
          }
          return;
        }

        var existingSlots = googletag.pubads().getSlots();
        var s =
          existingSlots.find(function (gs) {
            return gs.getSlotElementId() === actualDivId;
          }) || null;
        var tsOwned = false;
        if (!s) {
          // Define TS's fallback on the publisher's actual div. The scoped
          // handoff wrapper returns this slot if the publisher defines it later.
          s = runHandoffInternal(function () {
            return googletag.defineSlot(
              slot.gam_unit_path,
              slot.formats,
              actualDivId,
            );
          });
          if (!s) {
            releaseTrustedServerFirstImpressionClaim(el, tsClaim);
            return;
          }
          s.addService(googletag.pubads());
          tsOwned = true;
          ts.gptSlotHandoffs = ts.gptSlotHandoffs || {};
          ts.gptSlotHandoffs[actualDivId] = {
            gamUnitPath: slot.gam_unit_path,
            formats: slot.formats,
            divIdPrefix: slot.div_id,
            slotElementId: actualDivId,
            publisherClaimed: false,
            suppressPublisherDisplay: false,
            suppressPublisherRefresh: false,
          };
        }

        var targeting = bootstrapTargeting(slot, b);
        Object.entries(targeting).forEach(function (entry) {
          s.setTargeting(entry[0], entry[1]);
        });
        tsClaim.targeting = targeting;
        // Map the resolved inner div to the slot ID. This bootstrap fires no
        // beacons and registers no slotRenderEnded listener; the map is consumed
        // by the bundle's render bridge (index.ts) once it loads.
        divToSlotId[actualDivId] = slot.id;
        var slotElementId = s.getSlotElementId();
        var targetingKeys = Object.keys(slot.targeting || {});
        nextSlotTargetingKeys[actualDivId] = targetingKeys;
        if (slotElementId && slotElementId !== actualDivId) {
          divToSlotId[slotElementId] = slot.id;
          nextSlotTargetingKeys[slotElementId] = targetingKeys;
        }
        if (tsOwned) {
          newSlots.push(s);
          var displayId = s.getSlotElementId() || actualDivId;
          slotsToDisplay.push(displayId);
        } else {
          slotsToRefresh.push(s);
        }
      });
      ts.prevGptSlots = newSlots;
      retainPublisherSlotMappings(ts, divToSlotId, nextSlotTargetingKeys);
      ts.divToSlotId = divToSlotId;
      ts.prevSlotTargetingKeys = nextSlotTargetingKeys;
      var hasRenderableWork =
        slotsToDisplay.length > 0 || slotsToRefresh.length > 0;
      if (!ts.servicesEnabled && hasRenderableWork) {
        googletag.pubads().enableSingleRequest();
        googletag.enableServices();
        ts.servicesEnabled = true;
      }
      // Register and render TS-defined slots. GPT requires display() for a
      // freshly-defined slot; without it the slot no-ops and misses its
      // impression. Runs after enableServices(); on SPA navigation services are
      // already enabled, so this runs unconditionally for new slots.
      slotsToDisplay.forEach(function (divId) {
        runHandoffInternal(function () {
          googletag.display(divId);
        });
      });
      syncInitialLoadDisabled(window.googletag);

      // Reused publisher-owned slots always need a refresh to pick up the
      // server-side targeting. TS-defined slots are fetched by display() above
      // unless the publisher disabled initial load, in which case display() only
      // registers them and refresh() must request the ad — otherwise they render
      // blank. Only add them in that case to avoid double-requesting.
      var slotsNeedingRefresh = ts.gptInitialLoadDisabled
        ? slotsToRefresh.concat(newSlots)
        : slotsToRefresh;

      if (slotsNeedingRefresh.length > 0) {
        // One-shot bypass: this internal refresh delivers the just-applied
        // server-side targeting to GAM. If slim-Prebid has already wrapped
        // refresh(), it must pass this call straight through — not clear the
        // targeting and run a duplicate client-side auction. Mirrors the
        // bundle's adInit() in crates/trusted-server-js/lib/src/integrations/gpt/index.ts.
        ts.adInitRefreshInProgress = true;
        try {
          runHandoffInternal(function () {
            googletag.pubads().refresh(slotsNeedingRefresh);
          });
        } finally {
          ts.adInitRefreshInProgress = false;
        }
      }
    });
  };
})();
