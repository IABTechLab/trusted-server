(function () {
  'use strict';

  const { $, renderJson, parseCookies, describeEnvironment, appendLog, escapeHtml } = window.TSKitchen;

  function summarize(cookies, environment, eids) {
    return {
      'Current host': environment.hostname,
      'Likely static-only visit': environment.staticOnlyHint ? 'yes' : 'no',
      'ts-ec cookie': cookies['ts-ec'] ? 'present' : 'missing',
      'ts-eids cookie': cookies['ts-eids'] ? 'present' : 'missing',
      'ts-tester cookie': cookies['ts-tester'] || 'missing',
      'Global Privacy Control': String(environment.globalPrivacyControl),
      'Do Not Track': environment.doNotTrack || 'unset',
      'Prebid EIDs visible': Array.isArray(eids) ? `${eids.length} source(s)` : 'not available',
    };
  }

  function renderSummary(summary) {
    $('#identity-summary').innerHTML = Object.entries(summary)
      .map(([key, value]) => `<div><dt>${escapeHtml(key)}</dt><dd>${escapeHtml(value)}</dd></div>`)
      .join('');
  }

  function readPrebidEids() {
    const pbjs = window.pbjs;
    if (typeof pbjs?.getUserIdsAsEids !== 'function') {
      return {
        available: false,
        reason: 'window.pbjs.getUserIdsAsEids is not available on this page',
      };
    }
    try {
      return {
        available: true,
        eids: pbjs.getUserIdsAsEids(),
      };
    } catch (err) {
      return {
        available: false,
        reason: String(err),
      };
    }
  }

  function refreshDiagnostics() {
    const cookies = parseCookies();
    const environment = describeEnvironment();
    const eidResult = readPrebidEids();
    const eids = eidResult.available ? eidResult.eids : null;
    renderSummary(summarize(cookies, environment, eids));
    renderJson('#identity-cookies-json', cookies);
    renderJson('#identity-eids-json', eidResult);
    renderJson('#identity-environment-json', environment);
    appendLog('#identity-log', 'Refreshed diagnostics', {
      cookieNames: Object.keys(cookies),
      eidAvailable: eidResult.available,
    });
  }

  async function setTesterCookie() {
    appendLog('#identity-log', 'Requesting /_ts/set-tester');
    try {
      const res = await fetch('/_ts/set-tester', {
        method: 'GET',
        credentials: 'same-origin',
        cache: 'no-store',
      });
      appendLog('#identity-log', `/_ts/set-tester response ${res.status}`, {
        ok: res.ok,
        status: res.status,
        cacheControl: res.headers.get('cache-control'),
      });
    } catch (err) {
      appendLog('#identity-log', '/_ts/set-tester request failed', String(err));
    } finally {
      setTimeout(refreshDiagnostics, 250);
    }
  }

  function clearLocalTestData() {
    for (const key of Object.keys(localStorage)) {
      if (key.startsWith('ts-kitchen') || key.startsWith('trusted-server')) {
        localStorage.removeItem(key);
      }
    }
    appendLog('#identity-log', 'Cleared local test data keys');
    refreshDiagnostics();
  }

  document.addEventListener('DOMContentLoaded', () => {
    $('#refresh-diagnostics').addEventListener('click', refreshDiagnostics);
    $('#set-tester-cookie').addEventListener('click', setTesterCookie);
    $('#clear-local-test-data').addEventListener('click', clearLocalTestData);
    $('#clear-identity-log').addEventListener('click', () => {
      $('#identity-log').innerHTML = '';
    });
    refreshDiagnostics();
  });
})();
