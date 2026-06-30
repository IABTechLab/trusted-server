(function () {
  'use strict';

  const { $, renderJson, setText, clearSlots, renderBidInSlot } = window.TSKitchen;

  const adUnit = {
    code: 'header-banner',
    mediaTypes: { banner: { sizes: [[728, 90]], name: 'header' } },
    bids: [
      {
        bidder: 'kargo',
        params: { placementId: 'kitchen-sink-prebid-header', zone: 'header' },
      },
    ],
  };

  function snapshotState() {
    const pbjs = window.pbjs;
    return {
      hasPbjs: Boolean(pbjs),
      isKitchenSinkStub: Boolean(pbjs?._trustedServerKitchenSinkStub),
      hasInjectedTsjsConfig: Boolean(window.__tsjs_prebid),
      hasRequestBids: typeof pbjs?.requestBids === 'function',
      hasAddAdUnits: typeof pbjs?.addAdUnits === 'function',
      queueLength: Array.isArray(pbjs?.que) ? pbjs.que.length : null,
    };
  }

  function refreshState() {
    renderJson('#prebid-config-json', window.__tsjs_prebid || null);
    renderJson('#prebid-state-json', snapshotState());
  }

  function renderPrebidBids() {
    const pbjs = window.pbjs;
    if (typeof pbjs?.getHighestCpmBids !== 'function') return 0;
    const bids = pbjs.getHighestCpmBids();
    if (!Array.isArray(bids)) return 0;
    clearSlots();
    return bids.filter((bid) =>
      renderBidInSlot({
        adUnitCode: bid.adUnitCode,
        ad: bid.ad,
        bidder: bid.bidder,
        cpm: bid.cpm,
        width: bid.width,
        height: bid.height,
      }),
    ).length;
  }

  function queueRequest() {
    const pbjs = (window.pbjs = window.pbjs || {});
    pbjs.que = pbjs.que || [];

    const execute = () => {
      const livePbjs = window.pbjs;
      if (typeof livePbjs?.addAdUnits !== 'function' || typeof livePbjs?.requestBids !== 'function') {
        setText('#prebid-status', 'Queued; real Prebid API not present yet');
        refreshState();
        return;
      }

      livePbjs.addAdUnits([adUnit]);
      livePbjs.requestBids({
        timeout: 1500,
        trustedServer: { testMode: true },
        bidsBackHandler: () => {
          const rendered = renderPrebidBids();
          setText('#prebid-status', `bidsBackHandler fired; rendered ${rendered}`);
          refreshState();
        },
      });
      setText('#prebid-status', 'pbjs.requestBids called');
      refreshState();
    };

    if (typeof pbjs.requestBids === 'function') {
      execute();
    } else {
      pbjs.que.push(execute);
      setText('#prebid-status', 'Queued callback on pbjs.que');
      refreshState();
    }
  }

  document.addEventListener('DOMContentLoaded', () => {
    $('#queue-prebid').addEventListener('click', queueRequest);
    $('#refresh-prebid').addEventListener('click', refreshState);
    $('#clear-prebid').addEventListener('click', () => {
      clearSlots();
      setText('#prebid-status', 'Idle');
      refreshState();
    });
    refreshState();
  });
})();
