// Fictional hermetic APS runner fixture.
// This implements only Trusted Server's documented queue/callback test shape.
// It is not copied, transformed, or derived from APS runner bytes.
(function () {
  "use strict";

  if (!(window._aps instanceof Map)) return;
  window._aps.forEach(function (account) {
    if (!account || !Array.isArray(account.queue)) return;
    account.queue.splice(0).forEach(function (event) {
      var detail = event && event.detail;
      var keys = detail && Object.getOwnPropertyNames(detail).sort();
      if (
        !detail ||
        JSON.stringify(keys) !==
          JSON.stringify([
            "aaxResponse",
            "reject",
            "resolve",
            "seatBidId",
            "source",
          ]) ||
        detail.source !== "internal" ||
        typeof detail.resolve !== "function" ||
        typeof detail.reject !== "function"
      ) {
        if (detail && typeof detail.reject === "function") {
          detail.reject(new Error("fictional_detail_invalid"));
        }
        return;
      }

      window.setTimeout(function () {
        var bidId = detail.seatBidId;
        if (bidId.indexOf("silent-") === 0) return;
        if (bidId.indexOf("reject-") === 0) {
          detail.reject(new Error("fictional_rejection"));
          return;
        }
        if (bidId.indexOf("deferred-") === 0) {
          window.__fictionalApsResolve = function () {
            delete window.__fictionalApsResolve;
            detail.resolve();
          };
          return;
        }
        var decoded;
        var bid;
        try {
          decoded = JSON.parse(atob(detail.aaxResponse));
          bid = decoded.seatbid[0].bid[0];
        } catch (_error) {
          detail.reject(new Error("fictional_envelope_invalid"));
          return;
        }
        if (bidId.indexOf("nested-") === 0) {
          var frame = document.createElement("iframe");
          frame.setAttribute("data-fictional-creative", "nested");
          frame.setAttribute("sandbox", "allow-scripts");
          frame.setAttribute("width", String(bid.w));
          frame.setAttribute("height", String(bid.h));
          frame.style.border = "0";
          frame.style.display = "block";
          frame.style.width = String(bid.w) + "px";
          frame.style.height = String(bid.h) + "px";
          frame.style.margin = "0";
          frame.style.overflow = "hidden";
          frame.srcdoc =
            "<!doctype html><style>html,body{border:0;height:100%;margin:0;overflow:hidden;padding:0;width:100%}</style><script>parent.postMessage({message:'fictional-nested-ready'},'*')<\/script>";
          frame.onload = function () {
            detail.resolve();
          };
          document.body.appendChild(frame);
          return;
        }
        if (bidId.indexOf("creative-") === 0) {
          var creative = document.createElement(bid.ext.tagtype);
          creative.setAttribute("data-fictional-creative", bid.ext.tagtype);
          creative.onload = function () {
            detail.resolve();
          };
          creative.onerror = function () {
            detail.reject(new Error("fictional_creative_load_failed"));
          };
          if (bid.ext.tagtype === "iframe") {
            creative.setAttribute("width", String(bid.w));
            creative.setAttribute("height", String(bid.h));
            creative.style.border = "0";
            creative.style.display = "block";
            creative.style.width = String(bid.w) + "px";
            creative.style.height = String(bid.h) + "px";
            creative.style.margin = "0";
            creative.style.overflow = "hidden";
          }
          creative.src = bid.ext.creativeurl;
          document.body.appendChild(creative);
          return;
        }
        detail.resolve();
        if (bidId.indexOf("duplicate-") === 0) {
          detail.resolve();
          detail.reject(new Error("fictional_duplicate_rejection"));
        }
      }, 50);
    });
  });
})();
