(function () {
  'use strict';

  const { $, renderJson, setText, clearSlots, renderBidInSlot, flattenOpenRtbBids } = window.TSKitchen;

  const bidders = [
    { code: 'mocktioneer', checked: true, params: { placementId: 'kitchen-sink-mocktioneer' } },
    { code: 'kargo', checked: false, params: { placementId: 'kitchen-sink-kargo' } },
    { code: 'appnexus', checked: false, params: { placementId: 13144370 } },
    { code: 'openx', checked: false, params: { unit: 'kitchen-sink-openx', delDomain: 'example.com' } },
  ];

  function renderBidderControls() {
    $('#bidder-controls').innerHTML = bidders
      .map(
        (bidder) => `
          <label>
            <input type="checkbox" name="bidder" value="${bidder.code}" ${bidder.checked ? 'checked' : ''}>
            ${bidder.code}
          </label>
        `,
      )
      .join('');
  }

  function selectedBidders() {
    const checkedInputs = Array.from(document.querySelectorAll('input[name="bidder"]:checked'));
    const selectedCodes = new Set(checkedInputs.map((input) => input.value));
    return bidders.filter((bidder) => selectedCodes.has(bidder.code));
  }

  function auctionPayload() {
    const zone = 'header';
    return {
      adUnits: [
        {
          code: 'header-banner',
          mediaTypes: { banner: { sizes: [[728, 90]], name: zone } },
          bids: selectedBidders().map((bidder) => ({
            bidder: bidder.code,
            params: {
              ...bidder.params,
              zone,
            },
          })),
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
    renderBidderControls();
    $('#run-auction').addEventListener('click', runAuction);
    $('#clear-auction').addEventListener('click', () => {
      clearSlots();
      setText('#auction-request-json', 'Not run yet.');
      setText('#auction-response-json', 'Not run yet.');
      setText('#auction-status', 'Idle');
    });
  });
})();
