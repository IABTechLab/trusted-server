(function () {
  'use strict';

  const { $, $$, renderJson, clearSlots, renderBidInSlot, appendLog } = window.TSKitchen;

  function currentUrlForProbe(el) {
    if (el.tagName === 'IMG') return el.currentSrc || el.src;
    if (el.tagName === 'A') return el.href;
    if (el.tagName === 'SCRIPT') return el.src;
    return el.getAttribute('src') || el.getAttribute('href') || '';
  }

  function looksProxied(url) {
    return /\/first-party\/proxy|[?&]tsurl=|[?&]tstoken=|\/integrations\//.test(url);
  }

  function inspectProbes() {
    const probes = $$('[data-proxy-probe]').map((el) => {
      const currentUrl = currentUrlForProbe(el);
      return {
        kind: el.dataset.proxyProbe,
        tagName: el.tagName.toLowerCase(),
        originalUrl: el.dataset.originalUrl || null,
        currentUrl,
        changedFromOriginal: Boolean(el.dataset.originalUrl && currentUrl !== el.dataset.originalUrl),
        looksProxied: looksProxied(currentUrl),
      };
    });
    const result = {
      inspectedAt: new Date().toISOString(),
      href: window.location.href,
      probes,
      summary: {
        total: probes.length,
        changed: probes.filter((probe) => probe.changedFromOriginal).length,
        looksProxied: probes.filter((probe) => probe.looksProxied).length,
      },
    };
    renderJson('#proxy-probe-json', result);
    appendLog('#creative-log', 'Inspected origin HTML probes', result.summary);
    return result;
  }

  function imageCreativeHtml() {
    return `
      <a href="https://example.com/landing?creative=image-fixture" style="display:block;width:300px;height:250px;position:relative;text-decoration:none;color:white;background:#0f172a;overflow:hidden;">
        <img src="https://placehold.co/300x250/png?text=Creative+Asset" alt="Creative asset" width="300" height="250" style="display:block;width:300px;height:250px;object-fit:cover;">
        <span style="position:absolute;left:10px;bottom:10px;padding:6px 8px;border-radius:999px;background:rgba(15,23,42,.82);font:700 13px system-ui;">external image + click URL</span>
      </a>
    `;
  }

  function scriptCreativeHtml() {
    return `
      <div id="creative-root" style="width:300px;height:250px;display:grid;place-items:center;background:linear-gradient(135deg,#fef3c7,#fed7aa);color:#7c2d12;font:800 18px system-ui;text-align:center;padding:16px;">
        Script creative fixture<br><small id="script-status">script pending</small>
      </div>
      <script>
        document.getElementById('script-status').textContent = 'inline script ran in sandbox';
      <\/script>
    `;
  }

  function renderFixture(kind) {
    const html = kind === 'script' ? scriptCreativeHtml() : imageCreativeHtml();
    clearSlots();
    const rendered = renderBidInSlot({
      impid: 'creative-proxy-fixture',
      seat: 'fixture',
      price: 0,
      w: 300,
      h: 250,
      adm: html,
    });
    appendLog('#creative-log', `Rendered ${kind} fixture creative`, { rendered });
  }

  document.addEventListener('DOMContentLoaded', () => {
    $('#inspect-probes').addEventListener('click', inspectProbes);
    $('#render-image-creative').addEventListener('click', () => renderFixture('image'));
    $('#render-script-creative').addEventListener('click', () => renderFixture('script'));
    $('#clear-creative-slot').addEventListener('click', clearSlots);
    $('#clear-creative-log').addEventListener('click', () => {
      $('#creative-log').innerHTML = '';
    });
    inspectProbes();
  });
})();
