// Trusted Server: minimal runtime shim for Prebid
// - Sets s2sConfig with first-party endpoints
// - Ensures 'smartadserver' is present on each bid request
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
      if (s2sBidders.indexOf('smartadserver') === -1) s2sBidders.push('smartadserver');

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

      // On each auction, ensure SAS present and strip non-S2S bidders
      var origRequestBids = typeof pbjs.requestBids === 'function' && pbjs.requestBids;
      if (origRequestBids) {
        pbjs.requestBids = function (opts) {
          try {
            // Hard-coded Smart AdServer params for testing
            var sasParams = {
              siteId: 686105,
              networkId: 5280,
              pageId: 2040327,
              formatId: 137675,
              target: 'testing=prebid',
              domain: (window.location && window.location.hostname) || ''
            };
            function transform(units) {
              if (!Array.isArray(units)) return units;
              return units.map(function (au) {
                if (!au || typeof au !== 'object') return au;
                if (!Array.isArray(au.bids)) au.bids = [];
                // Keep only approved S2S bidders
                au.bids = au.bids.filter(function (b) { return b && s2sBidders.indexOf(b.bidder) !== -1; });
                // Ensure Smart AdServer present
                var hasSAS = au.bids.some(function (b) { return b && b.bidder === 'smartadserver'; });
                if (!hasSAS) {
                  au.bids.push({ bidder: 'smartadserver', params: sasParams });
                }
                return au;
              });
            }
            if (opts && Array.isArray(opts.adUnits)) {
              opts.adUnits = transform(opts.adUnits);
            } else if (Array.isArray(pbjs.adUnits)) {
              pbjs.adUnits = transform(pbjs.adUnits);
            }
          } catch (e) {
            console.warn('[Trusted Server] requestBids augmentation failed', e);
          }
          return origRequestBids.apply(pbjs, arguments);
        };
      }

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
