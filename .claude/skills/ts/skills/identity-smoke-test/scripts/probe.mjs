// Read Trusted Server identity state from the current publisher page.
//
// Usage:
//   node probe.mjs --host <publisher-host> [--cdp http://127.0.0.1:9222] <state|eids|gpp|pbjs>
//
//   state  injected tsjs config, tsjs scripts, CMP APIs and readable cookie names
//   eids   EID sources in __Host-ts-eids-mirror and ts-eids (IDs reduced to lengths)
//   gpp    the CMP's own GPP state from __gpp('ping'), next to the mirrored __gpp cookie
//   pbjs   EID sources each Prebid auction carried, with auction start times
//   links  up to ten same-site article-like links, to pick a second page view
import { connect, evaluate, parseArgs } from './cdp-lib.mjs';

const { options, positionals } = parseArgs(process.argv.slice(2));
if (!options.host) throw new Error('--host <publisher-host> is required');
const probe = positionals[0] ?? 'state';

const PROBES = {
  state: `(() => ({
    url: location.href,
    publicOrigin: document.getElementById('trustedserver-js')?.dataset.tsPublicOrigin ?? null,
    tsjsVersion: window.tsjs?.version ?? null,
    tsjsPrebid: window.__tsjs_prebid ?? null,
    tsjsSourcepoint: window.__tsjs_sourcepoint ?? null,
    tsjsScripts: [...document.scripts].map((s) => s.src).filter((src) => /\\/static\\/tsjs=|\\/integrations\\//.test(src)),
    pbjsVersion: window.pbjs?.version ?? null,
    cmp: { tcfapi: typeof window.__tcfapi, gpp: typeof window.__gpp, uspapi: typeof window.__uspapi },
    readableCookies: document.cookie.split('; ').filter(Boolean).map((c) => c.split('=')[0]).sort(),
  }))()`,
  eids: `(() => {
    const read = (name) => {
      const hit = document.cookie.split('; ').find((c) => c.startsWith(name + '='));
      return hit ? decodeURIComponent(hit.slice(name.length + 1)) : null;
    };
    const decode = (value) => {
      try {
        const b64 = value.replace(/-/g, '+').replace(/_/g, '/');
        return JSON.parse(atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4)));
      } catch (error) { return { decodeError: String(error) }; }
    };
    const sources = (payload) => {
      const eids = Array.isArray(payload) ? payload : payload?.eids;
      if (!Array.isArray(eids)) return payload;
      const now = Date.now() / 1000;
      return eids.map((e) => ({ source: e.source, uids: (e.uids ?? []).length,
        expiresInDays: (e.uids ?? []).map((u) => (u.exp ? Math.round((u.exp - now) / 86400) : null)) }));
    };
    const mirror = read('__Host-ts-eids-mirror');
    const eids = read('ts-eids');
    return { mirror: mirror && sources(decode(mirror)), tsEids: eids && sources(decode(eids)) };
  })()`,
  gpp: `new Promise((resolve) => {
    const cookie = decodeURIComponent((document.cookie.split('; ').find((c) => c.startsWith('__gpp=')) ?? '').slice(6));
    if (typeof window.__gpp !== 'function') { resolve({ gppApi: false, cookie }); return; }
    setTimeout(() => resolve({ timeout: true, cookie }), 4000);
    window.__gpp('ping', (data) => resolve({ signalStatus: data?.signalStatus, cmpStatus: data?.cmpStatus,
      applicableSections: data?.applicableSections, gppString: data?.gppString, cookie,
      parsedSections: data?.parsedSections }));
  })`,
  links: `(() => [...new Set([...document.querySelectorAll('a[href]')].map((a) => {
      try { const u = new URL(a.href); u.hash = ''; return u; } catch { return null; }
    }).filter((u) => u && u.host === location.host && u.pathname.split('/').filter(Boolean).length >= 2
      && !/\\.(jpg|png|gif|svg|pdf)$/i.test(u.pathname)).map((u) => u.href))].slice(0, 10))()`,
  pbjs: `(() => {
    const events = window.pbjs?.getEvents ? window.pbjs.getEvents() : [];
    const sources = (eids) => (eids ?? []).map((e) => e.source).sort();
    return {
      pbjs: Boolean(window.pbjs),
      userIdSources: window.pbjs?.getUserIdsAsEids ? sources(window.pbjs.getUserIdsAsEids()) : null,
      auctions: events.filter((e) => e.eventType === 'auctionInit').map((e) => ({
        startedAtMs: e.args?.timestamp,
        bidders: (e.args?.bidderRequests ?? []).map((b) => b.bidderCode),
        ortb2EidSources: sources(e.args?.bidderRequests?.[0]?.ortb2?.user?.ext?.eids),
      })),
    };
  })()`,
};

if (!(probe in PROBES)) throw new Error(`unknown probe ${probe}; use one of ${Object.keys(PROBES).join(', ')}`);
const session = await connect({ cdp: options.cdp ?? 'http://127.0.0.1:9222', host: options.host });
console.log(JSON.stringify(await evaluate(session, PROBES[probe]), null, 1));
session.close();
