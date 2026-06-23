(function () {
  'use strict';

  const { $, renderJson, setText, clearSlots, renderBidInSlot, flattenOpenRtbBids, appendLog } = window.TSKitchen;

  const slots = [
    { code: 'header-banner', zone: 'header', sizes: [[728, 90], [970, 250]], checked: true },
    { code: 'in-content', zone: 'in_content', sizes: [[300, 250]], checked: true },
    { code: 'sidebar-rail', zone: 'sidebar', sizes: [[300, 600]], checked: false },
    { code: 'sticky-footer', zone: 'fixed_bottom', sizes: [[320, 50]], checked: false },
  ];

  const bidders = [
    { code: 'kargo', checked: true, params: { placementId: 'static-kargo-placement' } },
    { code: 'appnexus', checked: true, params: { placementId: 13144370 } },
    { code: 'openx', checked: false, params: { unit: 'static-openx-unit', delDomain: 'stackpop-d.openx.net' } },
    { code: 'mocktioneer', checked: false, params: { placementId: 'static-mocktioneer' } },
  ];

  function renderCheckboxes() {
    const slotControls = $('#slot-controls');
    const bidderControls = $('#bidder-controls');

    slotControls.innerHTML = slots
      .map((slot) => `
        <label>
          <input type="checkbox" name="slot" value="${slot.code}" ${slot.checked ? 'checked' : ''}>
          ${slot.code} <small>(${slot.zone})</small>
        </label>
      `)
      .join('');

    bidderControls.innerHTML = bidders
      .map((bidder) => `
        <label>
          <input type="checkbox" name="bidder" value="${bidder.code}" ${bidder.checked ? 'checked' : ''}>
          ${bidder.code}
        </label>
      `)
      .join('');
  }

  function selectedValues(name) {
    return Array.from(document.querySelectorAll(`input[name="${name}"]:checked`)).map((input) => input.value);
  }

  function bidderParamsForSlot(bidder, slot) {
    return {
      ...bidder.params,
      zone: slot.zone,
      testPage: 'trusted-server-static-kitchen-sink',
    };
  }

  function buildRequestPayload() {
    const selectedSlots = new Set(selectedValues('slot'));
    const selectedBidders = new Set(selectedValues('bidder'));
    const units = slots
      .filter((slot) => selectedSlots.has(slot.code))
      .map((slot) => ({
        code: slot.code,
        mediaTypes: {
          banner: {
            sizes: slot.sizes,
            name: slot.zone,
          },
        },
        bids: bidders
          .filter((bidder) => selectedBidders.has(bidder.code))
          .map((bidder) => ({
            bidder: bidder.code,
            params: bidderParamsForSlot(bidder, slot),
          })),
      }));

    const payload = { adUnits: units };
    if ($('#auction-debug').checked) {
      payload.config = {
        debug: true,
        source: 'trusted-server-static-kitchen-sink',
        page: window.location.href,
        generatedAt: new Date().toISOString(),
      };
    }

    const eidsRaw = $('#auction-eids').value.trim();
    if (eidsRaw) {
      payload.eids = JSON.parse(eidsRaw);
    }

    return payload;
  }

  function fixtureResponse() {
    const selectedSlots = new Set(selectedValues('slot'));
    const bids = slots
      .filter((slot) => selectedSlots.has(slot.code))
      .map((slot, index) => ({
        id: `fixture-${slot.code}`,
        impid: slot.code,
        price: Number((1.25 + index * 0.42).toFixed(2)),
        adm: `
          <a href="https://example.com/click?slot=${encodeURIComponent(slot.code)}" style="display:grid;place-items:center;width:100%;height:100%;min-height:${slot.sizes[0][1]}px;text-decoration:none;background:linear-gradient(135deg,#dbeafe,#bfdbfe);color:#1e3a8a;border:2px solid #2563eb;font:700 18px system-ui;">
            Fixture creative · ${slot.code}
          </a>
        `,
        w: slot.sizes[0][0],
        h: slot.sizes[0][1],
        crid: `fixture-crid-${slot.code}`,
        adomain: ['example.com'],
      }));

    return {
      id: `fixture-${Date.now()}`,
      seatbid: [{ seat: 'fixture', bid: bids }],
      ext: {
        orchestrator: {
          strategy: 'local_fixture',
          bidders: 1,
          time_ms: 0,
        },
      },
    };
  }

  function renderResponse(body) {
    const bids = flattenOpenRtbBids(body);
    let rendered = 0;
    clearSlots();
    for (const bid of bids) {
      if (renderBidInSlot(bid)) rendered += 1;
    }
    setText('#auction-status', `Rendered ${rendered}/${bids.length} bids`);
    appendLog('#auction-log', `Rendered ${rendered} creative(s)`, { bids: bids.length });
  }

  async function runAuction(event) {
    event.preventDefault();
    const endpoint = $('#auction-endpoint').value.trim() || '/auction';
    const timeoutMs = Number($('#auction-timeout').value) || 2500;
    let payload;

    try {
      payload = buildRequestPayload();
    } catch (err) {
      setText('#auction-status', 'Invalid request payload');
      appendLog('#auction-log', 'Failed to build payload', String(err));
      return;
    }

    renderJson('#auction-request-json', payload);
    renderJson('#auction-response-json', { status: 'waiting' });
    setText('#auction-status', `POST ${endpoint} ...`);
    appendLog('#auction-log', `POST ${endpoint}`, payload);

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), timeoutMs);

    try {
      const started = performance.now();
      const res = await fetch(endpoint, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify(payload),
        signal: controller.signal,
      });
      const elapsedMs = Math.round(performance.now() - started);
      const text = await res.text();
      const contentType = res.headers.get('content-type') || '';
      let body = text;
      if (contentType.includes('json') || text.trim().startsWith('{')) {
        body = JSON.parse(text);
      }
      const responseSummary = {
        ok: res.ok,
        status: res.status,
        elapsedMs,
        contentType,
        body,
      };
      renderJson('#auction-response-json', responseSummary);
      appendLog('#auction-log', `Response ${res.status} in ${elapsedMs}ms`, responseSummary);
      if (res.ok && body && typeof body === 'object') {
        renderResponse(body);
      } else {
        setText('#auction-status', `Response ${res.status}; no OpenRTB render`);
      }
    } catch (err) {
      const message = err?.name === 'AbortError' ? `Timed out after ${timeoutMs}ms` : String(err);
      renderJson('#auction-response-json', { error: message });
      setText('#auction-status', message);
      appendLog('#auction-log', 'Auction request failed', message);
    } finally {
      clearTimeout(timeout);
    }
  }

  function renderFixture() {
    const payload = buildRequestPayload();
    const body = fixtureResponse();
    renderJson('#auction-request-json', payload);
    renderJson('#auction-response-json', body);
    appendLog('#auction-log', 'Rendered local fixture response', body);
    renderResponse(body);
  }

  document.addEventListener('DOMContentLoaded', () => {
    renderCheckboxes();
    $('#auction-form').addEventListener('submit', runAuction);
    $('#render-fixture').addEventListener('click', renderFixture);
    $('#clear-slots').addEventListener('click', () => {
      clearSlots();
      setText('#auction-status', 'Slots cleared');
    });
    $('#clear-log').addEventListener('click', () => {
      $('#auction-log').innerHTML = '';
    });
  });
})();
