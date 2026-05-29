// Edge-injected GPT auction bootstrap.
//
// This is the minimal `window._ts.adInit` that runs on first page load
// before the TSJS bundle has had a chance to install its richer
// idempotent implementation. The bundle in
// crates/js/lib/src/integrations/gpt/index.ts overwrites `_ts.adInit`
// once it loads.
//
// Contract with the bundle:
//   - Both implementations must set `window._ts.servicesEnabled = true`
//     after calling `enableSingleRequest()`/`enableServices()` so a
//     subsequent call becomes a no-op.
//   - `refresh()` is called only for the slots defined in this pass,
//     never the global slot list.
//
// Only installed if `window._ts.adInit` isn't already defined.
(function () {
  if (typeof window === "undefined") return;
  var ts = (window._ts = window._ts || {});
  if (ts.adInit) return;

  ts.adInit = function () {
    var slots = ts.adSlots || [];
    var bids = ts.bids || {};
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
        // Keep in sync with TS_INITIAL_TARGETING_KEY in index.ts
        s.setTargeting("ts_initial", "1");
        divToSlotId[slot.div_id] = slot.id;
        newSlots.push(s);
      });
      ts.prevGptSlots = newSlots;
      ts.divToSlotId = divToSlotId;
      if (!ts.servicesEnabled) {
        googletag.pubads().enableSingleRequest();
        googletag.enableServices();
        ts.servicesEnabled = true;
        googletag
          .pubads()
          .addEventListener("slotRenderEnded", function (ev) {
            var divId = ev.slot.getSlotElementId();
            var slotId = (ts.divToSlotId || {})[divId];
            if (!slotId) return;
            var b = (ts.bids || {})[slotId] || {};
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
