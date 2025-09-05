// Trusted Server: minimal runtime shim for Prebid
// - Sets s2sConfig with first-party endpoints using provided bidders
(function () {
  window.pbjs = window.pbjs || {};
  window.pbjs.que = window.pbjs.que || [];

  function initTrustedServerShim() {
    try {
      // Build S2S bidder list from config placeholders or runtime override
      var s2sBidders = (window.__TS_S2S_BIDDERS && Array.isArray(window.__TS_S2S_BIDDERS))
        ? window.__TS_S2S_BIDDERS.slice()
        : __BIDDERS__;
      if (!Array.isArray(s2sBidders)) s2sBidders = [];

      // Configure S2S with first-party endpoints
      pbjs.setConfig({
        s2sConfig: {
          accountId: '__ACCOUNT_ID__',
          enabled: true,
          bidders: s2sBidders,
          endpoint: '__SCHEME__://__HOST__/openrtb2/auction',
          syncEndpoint: '__SCHEME__://__HOST__/cookie_sync',
          timeout: __TIMEOUT__,
        },
        enabledBidders: s2sBidders,
        debug: __DEBUG__
      });

      console.log('[Trusted Server] Runtime shim active. s2s bidders:', s2sBidders);
    } catch (e) {
      console.error('[Trusted Server] Failed to initialize runtime shim', e);
    }
  }

  if (window.pbjs && typeof window.pbjs.setConfig === 'function') {
    // Prebid already loaded; run immediately
    initTrustedServerShim();
  } else {
    // Defer until Prebid loads
    window.pbjs.que.push(initTrustedServerShim);
  }
})();
