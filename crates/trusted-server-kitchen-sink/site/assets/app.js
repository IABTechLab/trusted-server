(function () {
  'use strict';

  const slotFrames = new Map();

  function $(selector, root = document) {
    return root.querySelector(selector);
  }

  function $$(selector, root = document) {
    return Array.from(root.querySelectorAll(selector));
  }

  function escapeHtml(value) {
    return String(value ?? '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }

  function pretty(value) {
    if (typeof value === 'string') {
      try {
        return JSON.stringify(JSON.parse(value), null, 2);
      } catch {
        return value;
      }
    }
    return JSON.stringify(value, null, 2);
  }

  function renderJson(selectorOrEl, value) {
    const el = typeof selectorOrEl === 'string' ? $(selectorOrEl) : selectorOrEl;
    if (!el) return;
    el.textContent = pretty(value);
  }

  function setText(selectorOrEl, value) {
    const el = typeof selectorOrEl === 'string' ? $(selectorOrEl) : selectorOrEl;
    if (!el) return;
    el.textContent = String(value ?? '');
  }

  function parseCookies(cookieString = document.cookie) {
    if (!cookieString) return {};
    return cookieString.split(';').reduce((acc, part) => {
      const [rawName, ...rawValue] = part.trim().split('=');
      if (!rawName) return acc;
      const value = rawValue.join('=');
      try {
        acc[rawName] = decodeURIComponent(value);
      } catch {
        acc[rawName] = value;
      }
      return acc;
    }, {});
  }

  function describeEnvironment() {
    const cookies = parseCookies();
    return {
      href: window.location.href,
      origin: window.location.origin,
      hostname: window.location.hostname,
      protocol: window.location.protocol,
      referrer: document.referrer || null,
      userAgent: navigator.userAgent,
      language: navigator.language,
      globalPrivacyControl: navigator.globalPrivacyControl ?? null,
      doNotTrack: navigator.doNotTrack ?? window.doNotTrack ?? null,
      hasTrustedServerCookie: Boolean(cookies['ts-ec']),
      hasEidCookie: Boolean(cookies['ts-eids']),
      hasTesterCookie: cookies['ts-tester'] === 'true',
      staticOnlyHint: !window.location.pathname.startsWith('/_ts/kitchen-sink'),
    };
  }

  function buildCreativeDocument(html) {
    return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<base target="_blank">
<style>
  html, body { margin: 0; padding: 0; width: 100%; min-height: 100%; }
  body { display: grid; place-items: center; background: #f8fafc; font-family: system-ui, sans-serif; }
  img, video, iframe { max-width: 100%; }
</style>
</head>
<body>${String(html ?? '')}</body>
</html>`;
  }

  function slotSelector(code) {
    return `[data-ad-slot="${String(code).replaceAll('"', '\\"')}"]`;
  }

  function findSlot(code) {
    return $(slotSelector(code));
  }

  function ensureSlotFrame(code, options = {}) {
    const slot = findSlot(code);
    if (!slot) return null;
    let frame = slotFrames.get(code);
    if (!frame || !frame.isConnected) {
      const mount = $('.ad-slot__mount', slot) || slot;
      mount.innerHTML = '';
      frame = document.createElement('iframe');
      frame.className = 'ad-frame';
      frame.title = `Ad creative for ${code}`;
      frame.loading = 'lazy';
      frame.sandbox = 'allow-scripts allow-popups allow-popups-to-escape-sandbox';
      mount.append(frame);
      slotFrames.set(code, frame);
    }
    const width = Number(options.width) || Number(slot.dataset.width) || 300;
    const height = Number(options.height) || Number(slot.dataset.height) || 250;
    frame.width = String(width);
    frame.height = String(height);
    frame.style.width = `${width}px`;
    frame.style.height = `${height}px`;
    return frame;
  }

  function clearSlots() {
    $$('[data-ad-slot]').forEach((slot) => {
      const mount = $('.ad-slot__mount', slot) || slot;
      mount.innerHTML = '<div class="ad-placeholder">No creative rendered yet</div>';
    });
    slotFrames.clear();
  }

  function renderBidInSlot(bid) {
    const code = bid?.impid || bid?.adUnitCode || bid?.code;
    if (!code) return false;
    const html = bid.adm || bid.ad || bid.html;
    if (!html) {
      const slot = findSlot(code);
      if (slot) {
        const mount = $('.ad-slot__mount', slot) || slot;
        mount.innerHTML = '<div class="ad-placeholder ad-placeholder--warning">Bid had no creative HTML</div>';
      }
      return false;
    }
    const frame = ensureSlotFrame(code, {
      width: bid.w || bid.width,
      height: bid.h || bid.height,
    });
    if (!frame) return false;
    frame.srcdoc = buildCreativeDocument(html);

    const slot = findSlot(code);
    const meta = $('.ad-slot__meta', slot);
    if (meta) {
      meta.textContent = `${bid.seat || bid.bidder || 'unknown'} · $${Number(bid.price || bid.cpm || 0).toFixed(2)} CPM`;
    }
    return true;
  }

  function flattenOpenRtbBids(body) {
    const bids = [];
    if (!body || !Array.isArray(body.seatbid)) return bids;
    for (const seatbid of body.seatbid) {
      for (const bid of seatbid.bid || []) {
        bids.push({
          ...bid,
          seat: seatbid.seat || bid.seat || 'unknown',
          width: bid.w,
          height: bid.h,
        });
      }
    }
    return bids;
  }

  async function copyToClipboard(text) {
    if (!navigator.clipboard) return false;
    await navigator.clipboard.writeText(String(text));
    return true;
  }

  function appendLog(selectorOrEl, message, detail) {
    const el = typeof selectorOrEl === 'string' ? $(selectorOrEl) : selectorOrEl;
    if (!el) return;
    const row = document.createElement('div');
    row.className = 'log-row';
    const time = new Date().toLocaleTimeString();
    row.innerHTML = `<strong>${escapeHtml(time)}</strong> ${escapeHtml(message)}`;
    if (detail !== undefined) {
      const pre = document.createElement('pre');
      pre.textContent = pretty(detail);
      row.append(pre);
    }
    el.prepend(row);
  }

  function initNav() {
    const page = document.body.dataset.page;
    if (!page) return;
    $$('[data-nav]').forEach((link) => {
      if (link.dataset.nav === page) link.setAttribute('aria-current', 'page');
    });
  }

  function initEnvironmentBlocks() {
    $$('[data-environment]').forEach((el) => {
      renderJson(el, describeEnvironment());
    });
  }

  function initCopyButtons() {
    $$('[data-copy-target]').forEach((button) => {
      button.addEventListener('click', async () => {
        const target = $(button.dataset.copyTarget);
        if (!target) return;
        const oldText = button.textContent;
        try {
          await copyToClipboard(target.textContent || '');
          button.textContent = 'Copied';
        } catch {
          button.textContent = 'Copy failed';
        } finally {
          setTimeout(() => {
            button.textContent = oldText;
          }, 1200);
        }
      });
    });
  }

  async function runSmokeAuction() {
    const output = $('#smoke-output');
    const payload = {
      adUnits: [
        {
          code: 'header-banner',
          mediaTypes: { banner: { sizes: [[728, 90]], name: 'header' } },
          bids: [{ bidder: 'kargo', params: { placementId: 'static-smoke-header' } }],
        },
      ],
      config: { debug: true, source: 'trusted-server-static-kitchen-sink' },
    };
    renderJson(output, { status: 'requesting /auction', payload });
    try {
      const res = await fetch('/auction', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify(payload),
      });
      const text = await res.text();
      renderJson(output, {
        ok: res.ok,
        status: res.status,
        contentType: res.headers.get('content-type'),
        body: text,
      });
    } catch (err) {
      renderJson(output, { error: String(err) });
    }
  }

  function initSmokeButton() {
    const button = $('#run-smoke-auction');
    if (!button) return;
    button.addEventListener('click', runSmokeAuction);
  }

  document.addEventListener('DOMContentLoaded', () => {
    initNav();
    initEnvironmentBlocks();
    initCopyButtons();
    initSmokeButton();
  });

  window.TSKitchen = {
    $,
    $$,
    escapeHtml,
    pretty,
    renderJson,
    setText,
    parseCookies,
    describeEnvironment,
    renderBidInSlot,
    flattenOpenRtbBids,
    clearSlots,
    appendLog,
    copyToClipboard,
  };
})();
