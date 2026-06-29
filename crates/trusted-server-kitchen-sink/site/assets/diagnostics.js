(function () {
  'use strict';

  const { $, escapeHtml, renderJson, parseCookies, setText } = window.TSKitchen;

  function renderSummary(cookies) {
    const rows = {
      'ts-ec': cookies['ts-ec'] ? 'present' : 'missing',
      'ts-eids': cookies['ts-eids'] ? 'present' : 'missing',
      'ts-tester': cookies['ts-tester'] || 'missing',
      'Global Privacy Control': String(navigator.globalPrivacyControl ?? 'unset'),
      'Do Not Track': navigator.doNotTrack ?? window.doNotTrack ?? 'unset',
    };
    $('#identity-summary').innerHTML = Object.entries(rows)
      .map(([key, value]) => `<div><dt>${escapeHtml(key)}</dt><dd>${escapeHtml(value)}</dd></div>`)
      .join('');
  }

  function refreshDiagnostics() {
    const cookies = parseCookies();
    renderSummary(cookies);
    renderJson('#identity-cookies-json', cookies);
    setText('#identity-status', 'Refreshed');
  }

  async function setTesterCookie() {
    setText('#identity-status', 'GET /_ts/set-tester ...');
    try {
      const res = await fetch('/_ts/set-tester', {
        method: 'GET',
        credentials: 'same-origin',
        cache: 'no-store',
      });
      setText('#identity-status', `Tester endpoint returned ${res.status}`);
    } catch (err) {
      setText('#identity-status', `Tester endpoint failed: ${String(err)}`);
    } finally {
      setTimeout(refreshDiagnostics, 250);
    }
  }

  document.addEventListener('DOMContentLoaded', () => {
    $('#refresh-diagnostics').addEventListener('click', refreshDiagnostics);
    $('#set-tester-cookie').addEventListener('click', setTesterCookie);
    refreshDiagnostics();
  });
})();
