// Edge-injected GPT auction bootstrap.
//
// This is the minimal `window.__tsAdInit` that runs on first page load
// before the TSJS bundle has had a chance to install its richer
// idempotent implementation. The bundle in
// crates/js/lib/src/integrations/gpt/index.ts overwrites `__tsAdInit`
// once it loads.
//
// Contract with the bundle:
//   - Both implementations must set `window.__tsServicesEnabled = true`
//     after calling `enableSingleRequest()`/`enableServices()` so a
//     subsequent call from any source (the bundle's `__tsAdInit`, the
//     publisher's own GPT init code) becomes a no-op.
//   - `refresh()` is called only for the slots defined in this pass,
//     never the global slot list, so we never accidentally refresh
//     publisher-managed slots that we don't own.
//
// Only installed if `window.__tsAdInit` isn't already defined — that
// way the bundle (or anything else) can preempt this fallback by
// installing first.
(function () {
  if (typeof window === "undefined" || window.__tsAdInit) {
    return;
  }
  window.__tsAdInit = function () {
    var slots = window.__ts_ad_slots || [];
    var bids = window.__ts_bids || {};
    var divToSlotId = {};
    googletag.cmd.push(function () {
      var newSlots = [];
      slots.forEach(function (slot) {
        var s = googletag.defineSlot(
          slot.gam_unit_path,
          slot.formats,
          slot.div_id,
        );
        if (!s) return;
        s.addService(googletag.pubads());
        Object.entries(slot.targeting || {}).forEach(function (e) {
          s.setTargeting(e[0], e[1]);
        });
        var b = bids[slot.id] || {};
        ["hb_pb", "hb_bidder", "hb_adid"].forEach(function (k) {
          if (b[k]) s.setTargeting(k, b[k]);
        });
        s.setTargeting("ts_initial", "1");
        divToSlotId[slot.div_id] = slot.id;
        newSlots.push(s);
      });
      // Expose slot metadata on window so later calls (SPA navigation,
      // the bundle's __tsAdInit) can destroy stale slots and the render
      // listener can resolve slot IDs after navigation updates these maps.
      window.__tsPrevGptSlots = newSlots;
      window.__tsDivToSlotId = divToSlotId;
      // Guard the one-time-per-page setup so a follow-up call (e.g.
      // publisher's own init code or the bundle's `__tsAdInit` after
      // it overwrites this stub) doesn't double-enable services.
      if (!window.__tsServicesEnabled) {
        googletag.pubads().enableSingleRequest();
        googletag.enableServices();
        window.__tsServicesEnabled = true;
        googletag
          .pubads()
          .addEventListener("slotRenderEnded", function (ev) {
            var divId = ev.slot.getSlotElementId();
            // Read from window so SPA navigation updates are picked up;
            // early-return for slots not managed by Trusted Server.
            var slotId = (window.__tsDivToSlotId || {})[divId];
            if (!slotId) return;
            var b = (window.__ts_bids || {})[slotId] || {};
            // Prebid: verify the specific creative via hb_adid targeting.
            // APS: no hb_adid — fire if any TS bidder is present and slot is non-empty.
            var ourBidWon =
              !ev.isEmpty &&
              (b.hb_adid
                ? ev.slot.getTargeting("hb_adid")[0] === b.hb_adid
                : !!b.hb_bidder);
            if (ourBidWon) {
              if (b.nurl) navigator.sendBeacon(b.nurl);
              if (b.burl) navigator.sendBeacon(b.burl);
            }
          });
      }
      if (newSlots.length > 0) {
        googletag.pubads().refresh(newSlots);
      }
    });
  };
})();
