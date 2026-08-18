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

      var bidId = detail.seatBidId;
      if (bidId.indexOf("silent-") === 0) return;
      if (bidId.indexOf("reject-") === 0) {
        detail.reject(new Error("fictional_rejection"));
        return;
      }
      if (bidId.indexOf("nested-") === 0) {
        var frame = document.createElement("iframe");
        frame.setAttribute("sandbox", "allow-scripts");
        frame.srcdoc =
          "<script>parent.postMessage({message:'fictional-nested-ready'},'*')<\/script>";
        frame.onload = function () {
          detail.resolve();
        };
        document.body.appendChild(frame);
        return;
      }
      detail.resolve();
      if (bidId.indexOf("duplicate-") === 0) {
        detail.resolve();
        detail.reject(new Error("fictional_duplicate_rejection"));
      }
    });
  });
})();
