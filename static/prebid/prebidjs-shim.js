(function (w) {
  var p = (w.pbjs = w.pbjs || {});
  p.que = p.que || [];

  // Provide no-op APIs to keep pages from breaking before tsjs loads
  if (!p.setConfig) {
    p.setConfig = function () {};
  }
  if (!p.getConfig) {
    p.getConfig = function () {
      return {};
    };
  }
  if (!p.requestBids) {
    p.requestBids = function (o) {
      try {
        o && o.bidsBackHandler && o.bidsBackHandler();
      } catch (e) {}
    };
  }
  if (!p.getHighestCpmBids) {
    p.getHighestCpmBids = function () {
      return [];
    };
  }

  if (w.console && console.info) {
    console.info('[tsjs] Loaded Prebid shim; tsjs will be injected separately');
  }
})(window);
