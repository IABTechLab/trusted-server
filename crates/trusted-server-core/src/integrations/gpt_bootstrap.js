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
  if (ts.adInit) return;

  // Track whether the publisher disabled GPT initial load. GPT exposes no
  // getter for this, so wrap pubads().disableInitialLoad() to record it. With
  // initial load disabled, display() only registers a slot and the ad request
  // must come from a later refresh(); adInit() reads this to refresh its own
  // freshly defined slots so they are not left blank. Pushed onto the command
  // queue so it runs before the publisher's own disableInitialLoad() call.
  (window.googletag = window.googletag || { cmd: [] }).cmd.push(function () {
    var pubads = googletag.pubads && googletag.pubads();
    if (
      !pubads ||
      typeof pubads.disableInitialLoad !== "function" ||
      pubads.__tsInitialLoadHooked
    ) {
      return;
    }
    var original = pubads.disableInitialLoad.bind(pubads);
    pubads.disableInitialLoad = function () {
      ts.gptInitialLoadDisabled = true;
      return original();
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

  function matchingHandoff(pubads, adUnitPath, formats, elementId) {
    var exact = ts.gptSlotHandoffs && ts.gptSlotHandoffs[elementId];
    if (exact) return exact;

    var candidates = Object.values(ts.gptSlotHandoffs || {}).filter(
      function (handoff, index, allHandoffs) {
        return (
          allHandoffs.indexOf(handoff) === index &&
          !handoff.publisherClaimed &&
          elementId.startsWith(handoff.divIdPrefix) &&
          handoff.gamUnitPath === adUnitPath &&
          JSON.stringify(handoff.formats) === JSON.stringify(formats) &&
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
        candidate.id.startsWith(divId) &&
        !candidate.id.endsWith("-container")
      ) {
        prefixMatches.push(candidate);
      }
    }
    // A unique prefix match may be a lazy slot that has not been sized yet.
    // Geometry is only needed to disambiguate multiple responsive siblings.
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
          var handoff = matchingHandoff(pubads, adUnitPath, formats, elementId);
          if (!ts.gptSlotHandoffInternal && handoff) {
            var existingSlot = findSlotByElementId(pubads, handoff.slotElementId);
            if (existingSlot) {
              if (!handoff.publisherClaimed) {
                ts.gptSlotHandoffs[elementId] = handoff;
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

  ts.adInit = function () {
    var slots = ts.adSlots || [];
    var bids = ts.bids || {};
    var divToSlotId = {};

    googletag.cmd.push(function () {
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
            divIdPrefix: slot.div_id,
            slotElementId: actualDivId,
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
        } else {
          slotsToRefresh.push(s);
        }
      });
      ts.prevGptSlots = newSlots;
      ts.divToSlotId = divToSlotId;
      if (!ts.servicesEnabled) {
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
