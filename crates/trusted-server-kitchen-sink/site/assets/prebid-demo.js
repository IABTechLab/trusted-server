(function () {
  'use strict';

  const {
    $,
    renderJson,
    setText,
    clearSlots,
    renderBidInSlot,
    flattenOpenRtbBids,
    appendLog,
  } = window.TSKitchen;

  const adSlots = [
    { code: 'header-banner', zone: 'header', sizes: [[728, 90], [970, 250]], checked: true },
    { code: 'in-content', zone: 'in_content', sizes: [[300, 250]], checked: true },
    { code: 'sticky-footer', zone: 'fixed_bottom', sizes: [[320, 50]], checked: false },
  ];

  const bidders = [
    { code: 'kargo', checked: true, params: { placementId: 'client-kargo-placeholder' } },
    { code: 'appnexus', checked: true, params: { placementId: 13144370 } },
    { code: 'openx', checked: false, params: { unit: 'static-openx-unit', delDomain: 'stackpop-d.openx.net' } },
    { code: 'mocktioneer', checked: false, params: { placementId: 'static-mocktioneer' } },
    { code: 'rubicon', checked: false, params: { accountId: 1001, siteId: 2002, zoneId: 3003 } },
  ];

  function renderCheckboxes() {
    $('#prebid-slot-controls').innerHTML = adSlots
      .map((slot) => `
        <label>
          <input type="checkbox" name="prebid-slot" value="${slot.code}" ${slot.checked ? 'checked' : ''}>
          ${slot.code} <small>(${slot.zone})</small>
        </label>
      `)
      .join('');

    $('#prebid-bidder-controls').innerHTML = bidders
      .map((bidder) => `
        <label>
          <input type="checkbox" name="prebid-bidder" value="${bidder.code}" ${bidder.checked ? 'checked' : ''}>
          ${bidder.code}
        </label>
      `)
      .join('');
  }

  function selectedValues(name) {
    return Array.from(document.querySelectorAll(`input[name="${name}"]:checked`)).map((input) => input.value);
  }

  function selectedAdUnits() {
    const selectedSlots = new Set(selectedValues('prebid-slot'));
    const selectedBidders = new Set(selectedValues('prebid-bidder'));
    return adSlots
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
            params: {
              ...bidder.params,
              zone: slot.zone,
              testPage: 'trusted-server-static-kitchen-sink',
            },
          })),
      }));
  }

  function updateConfigPreview() {
    const current = window.__tsjs_prebid;
    const preview = current || {
      note: 'No server-injected window.__tsjs_prebid found on this page yet.',
      expectedWhenProxied: {
        endpoint: '/auction',
        timeout: Number($('#prebid-timeout')?.value || 1500),
        debug: Boolean($('#prebid-debug')?.checked),
        clientSideBidders: ['rubicon'],
      },
    };
    $('#prebid-config-preview').value = JSON.stringify(preview, null, 2);
  }

  function safeCall(label, fn) {
    try {
      return fn();
    } catch (err) {
      return { error: `${label}: ${String(err)}` };
    }
  }

  function snapshotPrebidState() {
    const pbjs = window.pbjs;
    const state = {
      hasPbjs: Boolean(pbjs),
      isKitchenSinkStub: Boolean(pbjs?._trustedServerKitchenSinkStub),
      hasInjectedTsjsConfig: Boolean(window.__tsjs_prebid),
      injectedTsjsConfig: window.__tsjs_prebid || null,
      pbjsKeys: pbjs ? Object.keys(pbjs).sort().slice(0, 80) : [],
      queueLength: Array.isArray(pbjs?.que) ? pbjs.que.length : null,
      hasAddAdUnits: typeof pbjs?.addAdUnits === 'function',
      hasRequestBids: typeof pbjs?.requestBids === 'function',
      hasGetBidResponses: typeof pbjs?.getBidResponses === 'function',
      hasGetHighestCpmBids: typeof pbjs?.getHighestCpmBids === 'function',
      hasGetUserIdsAsEids: typeof pbjs?.getUserIdsAsEids === 'function',
    };

    if (pbjs?.getBidResponses) {
      state.bidResponses = safeCall('getBidResponses', () => pbjs.getBidResponses());
    }
    if (pbjs?.getHighestCpmBids) {
      state.highestCpmBids = safeCall('getHighestCpmBids', () => pbjs.getHighestCpmBids());
    }
    if (pbjs?.getUserIdsAsEids) {
      state.eids = safeCall('getUserIdsAsEids', () => pbjs.getUserIdsAsEids());
    }
    return state;
  }

  function refreshState() {
    updateConfigPreview();
    const state = snapshotPrebidState();
    renderJson('#prebid-state-json', state);
    return state;
  }

  function renderPrebidBids() {
    const pbjs = window.pbjs;
    if (typeof pbjs?.getHighestCpmBids !== 'function') return 0;
    const bids = safeCall('getHighestCpmBids', () => pbjs.getHighestCpmBids());
    if (!Array.isArray(bids)) return 0;
    let rendered = 0;
    for (const bid of bids) {
      if (renderBidInSlot({
        adUnitCode: bid.adUnitCode,
        ad: bid.ad,
        bidder: bid.bidder,
        cpm: bid.cpm,
        width: bid.width,
        height: bid.height,
      })) {
        rendered += 1;
      }
    }
    return rendered;
  }

  function installRequest(event) {
    event.preventDefault();
    const adUnits = selectedAdUnits();
    const timeout = Number($('#prebid-timeout').value) || 1500;
    const debug = Boolean($('#prebid-debug').checked);
    const pbjs = (window.pbjs = window.pbjs || {});
    pbjs.que = pbjs.que || [];

    renderJson('#prebid-units-json', { adUnits, timeout, debug });
    appendLog('#prebid-log', 'Generated Prebid ad units', adUnits);

    const execute = () => {
      const livePbjs = window.pbjs;
      if (typeof livePbjs?.addAdUnits !== 'function' || typeof livePbjs?.requestBids !== 'function') {
        setText('#prebid-status', 'Queued, but real pbjs API is not available yet');
        appendLog('#prebid-log', 'pbjs API unavailable; static-only visit likely using stub only', snapshotPrebidState());
        refreshState();
        return;
      }

      try {
        if (typeof livePbjs.setConfig === 'function') {
          livePbjs.setConfig({ debug, bidderTimeout: timeout });
        }
        if (typeof livePbjs.removeAdUnit === 'function') {
          for (const unit of adUnits) livePbjs.removeAdUnit(unit.code);
        }
        livePbjs.addAdUnits(adUnits);
        livePbjs.requestBids({
          timeout,
          bidsBackHandler: () => {
            const rendered = renderPrebidBids();
            const state = refreshState();
            setText('#prebid-status', `bidsBackHandler fired; rendered ${rendered} creative(s)`);
            appendLog('#prebid-log', 'Prebid bidsBackHandler fired', state);
          },
        });
        setText('#prebid-status', 'pbjs.requestBids called');
        appendLog('#prebid-log', 'pbjs.requestBids called');
      } catch (err) {
        setText('#prebid-status', 'Prebid request failed');
        appendLog('#prebid-log', 'Prebid request failed', String(err));
      }
    };

    if (typeof pbjs.requestBids === 'function') {
      execute();
    } else if (pbjs.que && typeof pbjs.que.push === 'function') {
      pbjs.que.push(execute);
      setText('#prebid-status', 'Queued callback on pbjs.que');
      appendLog('#prebid-log', 'Queued callback on pbjs.que. Waiting for real Prebid/TSJS bundle.');
      setTimeout(() => {
        const state = refreshState();
        if (!state.hasRequestBids) {
          setText('#prebid-status', 'Still queued; real Prebid API not present');
        }
      }, 1200);
    } else {
      execute();
    }

    refreshState();
  }

  function buildAuctionFallbackPayload() {
    return {
      adUnits: selectedAdUnits(),
      config: {
        debug: Boolean($('#prebid-debug').checked),
        source: 'trusted-server-static-kitchen-sink-prebid-fallback',
        page: window.location.href,
      },
    };
  }

  async function runAuctionFallback() {
    const endpoint = $('#prebid-fallback-endpoint').value.trim() || '/auction';
    const payload = buildAuctionFallbackPayload();
    renderJson('#prebid-units-json', payload);
    setText('#prebid-status', `Fallback POST ${endpoint} ...`);
    appendLog('#prebid-log', `Fallback POST ${endpoint}`, payload);

    try {
      const res = await fetch(endpoint, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify(payload),
      });
      const text = await res.text();
      const body = text.trim().startsWith('{') ? JSON.parse(text) : text;
      appendLog('#prebid-log', `Fallback response ${res.status}`, body);
      if (res.ok && body && typeof body === 'object') {
        clearSlots();
        let rendered = 0;
        for (const bid of flattenOpenRtbBids(body)) {
          if (renderBidInSlot(bid)) rendered += 1;
        }
        setText('#prebid-status', `Fallback rendered ${rendered} creative(s)`);
      } else {
        setText('#prebid-status', `Fallback response ${res.status}; no render`);
      }
      refreshState();
    } catch (err) {
      setText('#prebid-status', 'Fallback request failed');
      appendLog('#prebid-log', 'Fallback request failed', String(err));
    }
  }

  document.addEventListener('DOMContentLoaded', () => {
    renderCheckboxes();
    updateConfigPreview();
    refreshState();
    $('#prebid-form').addEventListener('submit', installRequest);
    $('#prebid-fallback-auction').addEventListener('click', runAuctionFallback);
    $('#prebid-refresh-state').addEventListener('click', refreshState);
    $('#prebid-clear-slots').addEventListener('click', clearSlots);
    $('#prebid-clear-log').addEventListener('click', () => {
      $('#prebid-log').innerHTML = '';
    });
  });
})();
