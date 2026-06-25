(function () {
  'use strict';

  const { $, renderJson, setText, clearSlots, renderBidInSlot, flattenOpenRtbBids } = window.TSKitchen;

  function auctionPayload() {
    return {
      adUnits: [
        {
          code: 'header-banner',
          mediaTypes: { banner: { sizes: [[728, 90]], name: 'header' } },
          bids: [
            {
              bidder: 'kargo',
              params: { placementId: 'kitchen-sink-header', zone: 'header' },
            },
          ],
        },
      ],
      config: {
        debug: true,
        source: 'trusted-server-kitchen-sink-auction',
        page: window.location.href,
      },
    };
  }

  async function runAuction() {
    const payload = auctionPayload();
    renderJson('#auction-request-json', payload);
    renderJson('#auction-response-json', { status: 'waiting' });
    setText('#auction-status', 'POST /auction ...');
    clearSlots();

    try {
      const res = await fetch('/auction', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify(payload),
      });
      const text = await res.text();
      const contentType = res.headers.get('content-type') || '';
      const body = contentType.includes('json') || text.trim().startsWith('{') ? JSON.parse(text) : text;
      renderJson('#auction-response-json', { ok: res.ok, status: res.status, contentType, body });

      if (res.ok && body && typeof body === 'object') {
        const rendered = flattenOpenRtbBids(body).filter(renderBidInSlot).length;
        setText('#auction-status', `Rendered ${rendered} creative(s)`);
      } else {
        setText('#auction-status', `Response ${res.status}`);
      }
    } catch (err) {
      renderJson('#auction-response-json', { error: String(err) });
      setText('#auction-status', 'Request failed');
    }
  }

  document.addEventListener('DOMContentLoaded', () => {
    $('#run-auction').addEventListener('click', runAuction);
    $('#clear-auction').addEventListener('click', () => {
      clearSlots();
      setText('#auction-request-json', 'Not run yet.');
      setText('#auction-response-json', 'Not run yet.');
      setText('#auction-status', 'Idle');
    });
  });
})();
