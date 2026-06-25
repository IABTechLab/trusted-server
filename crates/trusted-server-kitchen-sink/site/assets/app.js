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

  function renderJson(selectorOrEl, value) {
    const el = typeof selectorOrEl === 'string' ? $(selectorOrEl) : selectorOrEl;
    if (!el) return;
    el.textContent = JSON.stringify(value, null, 2);
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

  function buildCreativeDocument(html) {
    return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<base target="_blank">
<style>
  html, body { margin: 0; width: 100%; min-height: 100%; }
  body { display: grid; place-items: center; font-family: system-ui, sans-serif; }
</style>
</head>
<body>${String(html ?? '')}</body>
</html>`;
  }

  function findSlot(code) {
    return $(`[data-ad-slot="${String(code).replaceAll('"', '\\"')}"]`);
  }

  function clearSlots() {
    $$('[data-ad-slot]').forEach((slot) => {
      const mount = $('.ad-slot__mount', slot) || slot;
      mount.innerHTML = '<div class="ad-placeholder">No creative rendered</div>';
      const meta = $('.ad-slot__meta', slot);
      if (meta) meta.textContent = `${slot.dataset.width || 0}×${slot.dataset.height || 0}`;
    });
    slotFrames.clear();
  }

  function renderBidInSlot(bid) {
    const code = bid?.impid || bid?.adUnitCode || bid?.code;
    const html = bid?.adm || bid?.ad || bid?.html;
    if (!code || !html) return false;

    const slot = findSlot(code);
    if (!slot) return false;

    const mount = $('.ad-slot__mount', slot) || slot;
    mount.innerHTML = '';
    const frame = document.createElement('iframe');
    frame.className = 'ad-frame';
    frame.title = `Ad creative for ${code}`;
    frame.sandbox = 'allow-scripts allow-popups allow-popups-to-escape-sandbox';
    frame.width = String(bid.w || bid.width || slot.dataset.width || 300);
    frame.height = String(bid.h || bid.height || slot.dataset.height || 250);
    frame.srcdoc = buildCreativeDocument(html);
    mount.append(frame);
    slotFrames.set(code, frame);

    const meta = $('.ad-slot__meta', slot);
    if (meta) {
      const price = Number(bid.price || bid.cpm || 0).toFixed(2);
      meta.textContent = `${bid.seat || bid.bidder || 'bid'} · $${price}`;
    }
    return true;
  }

  function flattenOpenRtbBids(body) {
    if (!body || !Array.isArray(body.seatbid)) return [];
    return body.seatbid.flatMap((seatbid) =>
      (seatbid.bid || []).map((bid) => ({
        ...bid,
        seat: seatbid.seat || bid.seat || 'unknown',
        width: bid.w,
        height: bid.h,
      })),
    );
  }

  function initNav() {
    const page = document.body.dataset.page;
    if (!page) return;
    $$('[data-nav]').forEach((link) => {
      if (link.dataset.nav === page) link.setAttribute('aria-current', 'page');
    });
  }

  document.addEventListener('DOMContentLoaded', initNav);

  window.TSKitchen = {
    $,
    $$,
    escapeHtml,
    renderJson,
    setText,
    parseCookies,
    renderBidInSlot,
    flattenOpenRtbBids,
    clearSlots,
  };
})();
