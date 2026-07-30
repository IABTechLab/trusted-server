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
//   - `refresh()` is called only for TS-defined slots in this pass and
//     publisher requests the initial gate held, never the global slot list.
//
// Only installed if `window.tsjs.adInit` isn't already defined.
(function () {
  if (typeof window === "undefined") return;
  var ts = (window.tsjs = window.tsjs || {});
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

  (window.googletag = window.googletag || { cmd: [] }).cmd.push(function () {
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

  function findSlotByElementId(pubads, elementId) {
    var slots = pubads.getSlots ? pubads.getSlots() : [];
    return (
      slots.find(function (slot) {
        return slot.getSlotElementId() === elementId;
      }) || null
    );
  }

  function elementIdForDisplayTarget(target) {
    if (typeof target === "string") return target;
    if (target && typeof target.getSlotElementId === "function") {
      return target.getSlotElementId() || null;
    }
    if (target && typeof target.id === "string") return target.id || null;
    return null;
  }

  function configuredSlotForElementId(elementId) {
    return (ts.adSlots || []).find(function (slot) {
      return (
        slot.div_id &&
        (elementId === slot.div_id || elementId.startsWith(slot.div_id)) &&
        !elementId.endsWith("-container")
      );
    });
  }

  function isElementVisible(element) {
    var style = window.getComputedStyle(element);
    return (
      style.display !== "none" &&
      style.visibility !== "hidden" &&
      style.visibility !== "collapse"
    );
  }

  function slotElementHasLayout(element) {
    if (!isElementVisible(element)) return false;
    var elementRect = element.getBoundingClientRect();
    if (elementRect.width > 0 && elementRect.height > 0) return true;

    var container = document.getElementById(element.id + "-container");
    if (!container || !isElementVisible(container)) return false;
    var containerRect = container.getBoundingClientRect();
    return containerRect.width > 0 && containerRect.height > 0;
  }

  function findSlotElementByDivId(divId) {
    if (!divId) return null;
    var exact = document.getElementById(divId);
    if (exact) return exact;

    var idElements = document.querySelectorAll("[id]");
    var prefixMatches = [];
    for (var i = 0; i < idElements.length; i++) {
      var candidate = idElements[i];
      if (
        divId &&
        candidate.id.startsWith(divId) &&
        !candidate.id.endsWith("-container")
      ) {
        prefixMatches.push(candidate);
      }
    }
    if (prefixMatches.length === 1) return prefixMatches[0];

    var activeMatches = prefixMatches.filter(slotElementHasLayout);
    if (activeMatches.length === 1) return activeMatches[0];

    if (
      prefixMatches.length > 1 &&
      ts.log &&
      typeof ts.log.warn === "function"
    ) {
      ts.log.warn("GPT slot prefix did not resolve to one active element", {
        divId: divId,
        prefixMatchCount: prefixMatches.length,
        activeMatchCount: activeMatches.length,
      });
    }
    return null;
  }

  function initialRequestGate() {
    if (!ts.gptInitialRequestGate) {
      ts.gptInitialRequestGate = {
        pendingDisplays: {},
        pendingRefreshes: {},
        released: false,
      };
    }
    return ts.gptInitialRequestGate;
  }

  function takeInitialPublisherRequests(pubads) {
    var gate = initialRequestGate();
    if (gate.released) return { displayIds: [], refreshSlots: [] };

    gate.released = true;
    var displayIds = Object.keys(gate.pendingDisplays);
    var refreshIds = Object.keys(gate.pendingRefreshes);
    gate.pendingDisplays = {};
    gate.pendingRefreshes = {};
    var slots = pubads.getSlots ? pubads.getSlots() : [];
    return {
      displayIds: displayIds,
      refreshSlots: slots.filter(function (slot) {
        return refreshIds.includes(slot.getSlotElementId());
      }),
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
  // publisher defineSlot() for that exact div to the same GPT slot.
  function installSlotHandoff() {
    window.googletag.cmd.push(function () {
      var tag = window.googletag;
      var pubads = tag.pubads && tag.pubads();
      if (!tag.defineSlot || !tag.display || !pubads) return;

      if (!tag.defineSlot.__tsSlotHandoffPatched) {
        var originalDefineSlot = tag.defineSlot.bind(tag);
        var patchedDefineSlot = function (adUnitPath, formats, elementId) {
          var handoff = ts.gptSlotHandoffs && ts.gptSlotHandoffs[elementId];
          if (!ts.gptSlotHandoffInternal && handoff) {
            var existingSlot = findSlotByElementId(pubads, elementId);
            if (existingSlot) {
              if (!handoff.publisherClaimed) {
                handoff.publisherClaimed = true;
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
                  JSON.stringify(handoff.formats) !== JSON.stringify(formats)
                ) {
                  ts.log &&
                    ts.log.warn &&
                    ts.log.warn(
                      "GPT slot handoff: publisher definition differs from TS configuration",
                      elementId,
                    );
                }
              }
              return existingSlot;
            }
          }
          return originalDefineSlot(adUnitPath, formats, elementId);
        };
        patchedDefineSlot.__tsSlotHandoffPatched = true;
        tag.defineSlot = patchedDefineSlot;
      }

      if (!tag.display.__tsSlotHandoffPatched) {
        var originalDisplay = tag.display.bind(tag);
        var patchedDisplay = function (target) {
          var elementId = elementIdForDisplayTarget(target);
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
          var gate = initialRequestGate();
          if (
            !ts.gptSlotHandoffInternal &&
            !gate.released &&
            elementId &&
            configuredSlotForElementId(elementId)
          ) {
            gate.pendingDisplays[elementId] = true;
            return;
          }
          originalDisplay(target);
        };
        patchedDisplay.__tsSlotHandoffPatched = true;
        tag.display = patchedDisplay;
      }

      if (!pubads.refresh.__tsSlotHandoffPatched) {
        var originalRefresh = pubads.refresh.bind(pubads);
        var patchedRefresh = function (requestedSlots) {
          if (ts.gptSlotHandoffInternal) {
            originalRefresh(requestedSlots);
            return;
          }
          var slots =
            requestedSlots || (pubads.getSlots ? pubads.getSlots() : null);
          if (!slots) {
            originalRefresh(requestedSlots);
            return;
          }
          var suppressed = false;
          var gate = initialRequestGate();
          var remainingSlots = slots.filter(function (slot) {
            var handoff =
              ts.gptSlotHandoffs && ts.gptSlotHandoffs[slot.getSlotElementId()];
            if (handoff && handoff.suppressPublisherRefresh) {
              handoff.suppressPublisherRefresh = false;
              suppressed = true;
              return false;
            }
            var elementId = slot.getSlotElementId();
            if (!gate.released && configuredSlotForElementId(elementId)) {
              gate.pendingRefreshes[elementId] = true;
              suppressed = true;
              return false;
            }
            return true;
          });
          if (!suppressed) {
            originalRefresh(requestedSlots);
          } else if (remainingSlots.length > 0) {
            originalRefresh(remainingSlots);
          }
        };
        patchedRefresh.__tsSlotHandoffPatched = true;
        pubads.refresh = patchedRefresh;
      }
    });
  }

  installSlotHandoff();

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
  ts.scheduleInitialAdInit = function (initialBids) {
    if ((ts.navGeneration || 0) !== 0) return;
    if (initialBids) ts.bids = initialBids;
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

  ts.adInit = function () {
    var slots = ts.adSlots || [];
    var bids = ts.bids || {};
    var divToSlotId = {};
    // Generation this invocation belongs to. The slot work below is queued on
    // googletag.cmd, which drains only when GPT loads; recheck first inside
    // the queued callback so a navigation committed in the gap cancels the
    // stale mutation — mirrors the bundle's adInit.
    var generation = ts.navGeneration || 0;

    googletag.cmd.push(function () {
      if ((ts.navGeneration || 0) !== generation) return;
      // Slots TS defined itself — tracked for SPA destroy. Publisher-owned
      // slots are reused but never destroyed by TS on navigation.
      var newSlots = [];
      // Publisher-owned slots can be refreshed on SPA navigation. On initial
      // load their first request is held until the targeting below is applied.
      var slotsToRefresh = [];
      var isInitialAdInit = !ts.gptInitialAdInitCompleted;
      // Element IDs of slots TS defined itself. GPT requires display() to
      // register/render a freshly-defined slot; refresh() alone no-ops for a
      // slot that was never displayed, so these are display()ed instead.
      var slotsToDisplay = [];
      var hasAppliedTargeting = false;
      slots.forEach(function (slot) {
        // Resolve actual div ID: exact match first, then the one active prefix
        // match. Responsive publishers may emit several mutually exclusive
        // siblings for one stable prefix, so document order is not sufficient.
        var el = findSlotElementByDivId(slot.div_id);
        if (!el) return;
        var actualDivId = el.id;
        var b = bids[slot.id] || {};

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
          if (!s) return;
          s.addService(googletag.pubads());
          tsOwned = true;
          ts.gptSlotHandoffs = ts.gptSlotHandoffs || {};
          ts.gptSlotHandoffs[actualDivId] = {
            gamUnitPath: slot.gam_unit_path,
            formats: slot.formats,
            publisherClaimed: false,
            suppressPublisherDisplay: false,
            suppressPublisherRefresh: false,
          };
        }

        Object.entries(slot.targeting || {}).forEach(function (e) {
          s.setTargeting(e[0], e[1]);
        });
        [
          "hb_pb",
          "hb_bidder",
          "hb_adid",
          "hb_cache_host",
          "hb_cache_path",
        ].forEach(function (k) {
          if (b[k]) s.setTargeting(k, b[k]);
        });
        // Keep in sync with TS_INITIAL_TARGETING_KEY in index.ts
        s.setTargeting("ts_initial", "1");
        hasAppliedTargeting = true;
        // Map the resolved inner div to the slot ID. This bootstrap fires no
        // beacons and registers no slotRenderEnded listener; the map is consumed
        // by the bundle's render bridge (index.ts) once it loads.
        divToSlotId[actualDivId] = slot.id;
        var slotElementId = s.getSlotElementId();
        if (slotElementId && slotElementId !== actualDivId) {
          divToSlotId[slotElementId] = slot.id;
        }
        if (tsOwned) {
          newSlots.push(s);
          var displayId = s.getSlotElementId() || actualDivId;
          slotsToDisplay.push(displayId);
        } else if (!isInitialAdInit) {
          slotsToRefresh.push(s);
        }
      });
      ts.prevGptSlots = newSlots;
      ts.divToSlotId = divToSlotId;
      var heldPublisherRequests = isInitialAdInit
        ? takeInitialPublisherRequests(googletag.pubads())
        : { displayIds: [], refreshSlots: [] };
      ts.gptInitialAdInitCompleted = true;
      if (!ts.servicesEnabled && (hasAppliedTargeting || heldPublisherRequests.displayIds.length > 0 || heldPublisherRequests.refreshSlots.length > 0)) {
        googletag.pubads().enableSingleRequest();
        googletag.enableServices();
        ts.servicesEnabled = true;
      }
      // Register/render TS-defined slots and replay publisher displays held
      // before server-side bids were available. The replay is the publisher's
      // one initial request, not a later TS refresh.
      heldPublisherRequests.displayIds.concat(slotsToDisplay).forEach(function (divId) {
        runHandoffInternal(function () {
          googletag.display(divId);
        });
      });
      // Replay held publisher refreshes after targeting. On SPA navigation TS
      // refreshes reused publisher slots as before; TS-defined slots need a
      // refresh only when effective GPT configuration disabled initial load.
      syncInitialLoadDisabled(window.googletag);
      var slotsNeedingRefresh = heldPublisherRequests.refreshSlots.concat(
        slotsToRefresh,
        ts.gptInitialLoadDisabled ? newSlots : [],
      );
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
