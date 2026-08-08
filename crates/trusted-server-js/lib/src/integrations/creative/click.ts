// Click guard runtime: detects mutated tracking URLs and rebuilds signed first-party clicks.
import { log } from '../../core/log';
import { creativeGlobal } from '../../shared/globals';
import { delay, queueTask } from '../../shared/async';
import { hasOpaqueOrigin, TRUSTED_BASE_URL } from '../../shared/origin';
import { createMutationScheduler } from '../../shared/scheduler';

import type { CreativeGuardHandle } from './startup';

type AnchorLike = HTMLAnchorElement | HTMLAreaElement;
type Canon = { base: string; params: Record<string, string> };
type Diff = { add: Record<string, string>; del: string[] };

// Rebuild URLs already written to an anchor's href by an earlier repair pass
// (the opaque-origin GET fallback). They are not `/first-party/click` URLs, so
// they cannot be canonicalized and deliberately never replace the canonical
// `data-tsclick`. Without remembering them, a later click would canonicalize
// the fallback against the original signed click, fail the base comparison, and
// navigate the pre-mutation URL — silently dropping the mutation the fallback
// exists to carry.
const pendingRebuilds = new WeakMap<AnchorLike, string>();

// Allow query/localStorage flag to crank logging when debugging creatives.
function enableDebugFromEnv(): void {
  try {
    const q = new URLSearchParams(location.search);
    const ls = creativeGlobal.localStorage;
    const flag = q.get('tsdebug') === '1' || (ls && ls.getItem && ls.getItem('tsdebug') === '1');
    if (flag) log.setLevel('debug');
  } catch (err) {
    log.debug('tsjs-creative:click: debug flag inspection failed', err);
  }
}

// Minimal querystring parser that tolerates malformed input.
function parseQuery(qs: string): Record<string, string> {
  const out: Record<string, string> = {};
  qs.replace(/^\?/, '')
    .split('&')
    .filter(Boolean)
    .forEach((kv) => {
      const [k, v = ''] = kv.split('=');
      if (k) out[decodeURIComponent(k)] = decodeURIComponent(v);
    });
  return out;
}

// Decode a signed /first-party/click URL back into its clear destination + params.
// URLs resolve against the pinned trusted base, not `location.href`: inside the
// sandboxed `srcdoc` creative iframe `location.href` is `about:srcdoc`, which
// `new URL` rejects as a base for the root-relative URLs the rewriter emits.
function canonFromFirstPartyClick(url: string): Canon | null {
  try {
    const u = new URL(url, TRUSTED_BASE_URL);
    if (!(u.pathname === '/first-party/click' || u.pathname.startsWith('/first-party/click')))
      return null;
    const p = parseQuery(u.search);
    const tsurl = p['tsurl'];
    if (!tsurl) return null;
    delete p['tstoken'];
    delete p['tsurl'];
    return { base: tsurl, params: p };
  } catch {
    return null;
  }
}

// Normalise arbitrary hrefs so we can compare them against the original click canon.
function canonFromAnyHref(href: string): Canon | null {
  const fp = canonFromFirstPartyClick(href);
  if (fp) return fp;
  try {
    const u = new URL(href, TRUSTED_BASE_URL);
    const params = parseQuery(u.search);
    u.search = '';
    u.hash = '';
    return { base: u.toString(), params };
  } catch {
    return null;
  }
}

// Compare two URLs but ignore http↔https differences that creatives often introduce.
function sameBaseIgnoreScheme(aBase: string, bBase: string): boolean {
  try {
    const au = new URL(aBase, TRUSTED_BASE_URL);
    const bu = new URL(bBase, TRUSTED_BASE_URL);
    return au.hostname === bu.hostname && au.pathname === bu.pathname;
  } catch {
    return aBase === bBase;
  }
}

// Exact canonical equality check covering base path and sorted query params.
function equalCanon(a: Canon, b: Canon): boolean {
  if (!sameBaseIgnoreScheme(a.base, b.base)) return false;
  const ak = Object.keys(a.params).sort();
  const bk = Object.keys(b.params).sort();
  if (ak.length !== bk.length) return false;
  for (let i = 0; i < ak.length; i++) {
    const k = ak[i]!;
    if (k !== bk[i] || a.params[k] !== b.params[k]) return false;
  }
  return true;
}

// Detect which query params were added/removed/changed while keeping base intact.
function diffParams(orig: Canon, mutated: Canon): Diff | null {
  if (!sameBaseIgnoreScheme(orig.base, mutated.base)) {
    return null;
  }

  const add: Record<string, string> = {};
  const del = new Set<string>();

  for (const key of Object.keys(orig.params)) {
    if (!(key in mutated.params)) {
      del.add(key);
    }
  }

  for (const [key, value] of Object.entries(mutated.params)) {
    if (!(key in orig.params)) {
      add[key] = value;
      continue;
    }
    if (orig.params[key] !== value) {
      del.add(key);
      add[key] = value;
    }
  }

  return { add, del: Array.from(del) };
}

// Traverse up from an event target to find the owning anchor or area element.
function closestAnchor(el: EventTarget | null): AnchorLike | null {
  let node = el as Node | null;
  while (node) {
    if (node.nodeType === 1) {
      const e = node as Element;
      if (e.tagName === 'A' || e.tagName === 'AREA') return e as AnchorLike;
    }
    node = (node as Element).parentElement;
  }
  return null;
}

// Construct fallback GET URL that asks the edge to rebuild the click on-demand.
function buildProxyRebuildUrl(tsClickStr: string, diff: Diff): string {
  const params = new URLSearchParams();
  params.set('tsclick', tsClickStr);
  if (Object.keys(diff.add).length > 0) {
    params.set('add', JSON.stringify(diff.add));
  }
  if (diff.del.length > 0) {
    params.set('del', JSON.stringify(diff.del));
  }
  return `/first-party/proxy-rebuild?${params.toString()}`;
}

// Call the proxy-rebuild endpoint so the edge can re-sign mutated click params.
// In an opaque origin (sandboxed srcdoc without `allow-same-origin`) the JSON
// POST is cross-origin (`Origin: null`), triggers a CORS preflight the edge
// does not answer, and always fails — so the guard skips it and recovers via
// the GET navigation fallback, which the edge answers with a 302 chain (no
// CORS applies to navigations).
async function rebuildClick(a: AnchorLike, tsClickStr: string, diff: Diff): Promise<string> {
  const addKeys = Object.keys(diff.add);
  const delKeys = diff.del;
  if (addKeys.length === 0 && delKeys.length === 0) {
    return tsClickStr;
  }

  const fallback = buildProxyRebuildUrl(tsClickStr, diff);

  if (typeof fetch !== 'function' || hasOpaqueOrigin()) {
    try {
      const el = a as Element;
      el.setAttribute('href', fallback);
    } catch (err) {
      log.debug('tsjs-creative:click: unable to set fallback href (no-fetch)', err);
    }
    return fallback;
  }

  const payload: Record<string, unknown> = { tsclick: tsClickStr };
  if (addKeys.length > 0) payload.add = diff.add;
  if (delKeys.length > 0) payload.del = delKeys;

  try {
    const resp = await fetch('/first-party/proxy-rebuild', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(payload),
      credentials: 'same-origin',
    });
    if (!resp.ok) {
      log.warn('tsjs-creative:click: proxy-rebuild HTTP error', resp.status);
      try {
        const el = a as Element;
        el.setAttribute('href', fallback);
      } catch (err) {
        log.debug('tsjs-creative:click: unable to set fallback href (http error)', err);
      }
      return fallback;
    }
    const data = (await resp.json()) as { href?: string; base?: string } | null;
    const href = data && typeof data.href === 'string' ? data.href : null;
    if (href) {
      persistRebuiltClick(a, href);
      log.info('tsjs-creative:click: rebuilt click', {
        added: addKeys,
        removed: delKeys,
      });
      return href;
    }
  } catch (err) {
    log.warn('tsjs-creative:click: proxy-rebuild request failed', err);
  }

  try {
    const el = a as Element;
    el.setAttribute('href', fallback);
  } catch (err) {
    log.debug('tsjs-creative:click: unable to apply fallback href', err);
  }
  return fallback;
}

// Work out the href we should navigate to after accounting for creative rewrites.
async function computeFinalUrl(a: AnchorLike, tsClickStr: string): Promise<string> {
  const orig = canonFromFirstPartyClick(tsClickStr);
  if (!orig) return tsClickStr;

  const rawHref = a.getAttribute && a.getAttribute('href');
  const currentHref = rawHref || a.href || '';
  if (!currentHref) return tsClickStr;

  // The href is a rebuild URL this guard wrote during an earlier repair pass and
  // the creative has not touched it since. It already carries that pass's
  // mutation; canonicalizing it against the original signed click would compare
  // two different bases, fail the diff, and navigate the pre-mutation URL.
  if (pendingRebuilds.get(a) === currentHref) return currentHref;

  const mutated = canonFromAnyHref(currentHref);
  if (!mutated) return tsClickStr;

  if (equalCanon(orig, mutated)) return tsClickStr;

  const diff = diffParams(orig, mutated);
  if (!diff) {
    log.warn('tsjs-creative:click: click base changed; keeping original', {
      original: orig.base,
      mutated: mutated.base,
    });
    return tsClickStr;
  }

  if (Object.keys(diff.add).length === 0 && diff.del.length === 0) {
    return tsClickStr;
  }

  log.debug('tsjs-creative:click: detected click rewrite', {
    add: Object.keys(diff.add),
    del: diff.del,
  });

  return rebuildClick(a, tsClickStr, diff);
}

// Resolve a click URL against the pinned trusted base and require an http(s)
// scheme. The inputs (anchor href / data-tsclick attributes) are
// creative-controlled DOM text, so anything else — javascript:, data:, vbscript:
// — must never reach a navigation sink or an href write. Returns null for
// unparseable or non-http(s) URLs so callers fail closed.
function resolveSafeNavigationUrl(url: string): string | null {
  try {
    const resolved = new URL(url, TRUSTED_BASE_URL);
    if (resolved.protocol === 'http:' || resolved.protocol === 'https:') {
      return resolved.toString();
    }
    log.warn('tsjs-creative:click: refusing non-http(s) navigation', resolved.protocol);
  } catch (err) {
    log.debug('tsjs-creative:click: could not resolve navigation URL', err);
  }
  return null;
}

// Send the user to the resolved URL while respecting middle clicks and targets.
// Root-relative URLs are absolutized against the pinned trusted base first: in
// the sandboxed srcdoc iframe there is no usable document URL for the
// navigation APIs to resolve them against.
function navigate(a: AnchorLike, url: string, isMiddle: boolean): void {
  const resolved = resolveSafeNavigationUrl(url);
  if (!resolved) return;
  const target = a.getAttribute('target') || (isMiddle ? '_blank' : '_self');
  if (target === '_blank' || isMiddle) {
    window.open(resolved, target, 'noopener,noreferrer');
  } else {
    location.href = resolved;
  }
}

// Persist a rebuilt click onto the anchor. `href` always takes the new value,
// but `data-tsclick` — the canonical signed click that future mutation diffs
// compare against — is only updated when the value is itself a signed
// /first-party/click URL. Writing the GET proxy-rebuild fallback there would
// make every later canonicalization fail and lose subsequent mutations.
function persistRebuiltClick(anchor: AnchorLike, finalUrl: string): void {
  // Persist the validated, absolutized URL — never the raw input. Beyond
  // enforcing the http(s) allowlist, an absolute URL keeps the anchor's
  // default navigation working inside the srcdoc iframe, where a relative
  // href would resolve against about:srcdoc.
  const resolved = resolveSafeNavigationUrl(finalUrl);
  if (!resolved) return;
  if (canonFromFirstPartyClick(resolved)) {
    pendingRebuilds.delete(anchor);
  } else {
    // Not a signed click (the GET rebuild fallback): remember it so a later
    // click navigates this repaired URL rather than the canonical original.
    pendingRebuilds.set(anchor, resolved);
  }
  // Writing an unchanged value still emits a mutation record, which would wake
  // the observer, recompute the same URL, and write again forever.
  if (anchor.getAttribute('href') === resolved) return;
  try {
    const el = anchor as Element;
    if (canonFromFirstPartyClick(resolved)) {
      el.setAttribute('data-tsclick', resolved);
    }
    el.setAttribute('href', resolved);
  } catch (err) {
    log.debug('tsjs-creative:click: failed to persist rebuilt href', err);
  }
}

// Give the creative one microtask to finish mutations before we lock in the href.
async function rebuildIfNeeded(anchor: AnchorLike, tsClickStr: string): Promise<string> {
  let finalUrl = await computeFinalUrl(anchor, tsClickStr);
  if (finalUrl === tsClickStr) {
    await delay();
    finalUrl = await computeFinalUrl(anchor, tsClickStr);
  }
  return finalUrl;
}

// Gate navigation until the click has been re-signed (or confirmed unchanged).
async function guardNavigation(
  anchor: AnchorLike,
  tsClickStr: string,
  isMiddle: boolean,
  isActive: () => boolean
): Promise<void> {
  const finalUrl = await rebuildIfNeeded(anchor, tsClickStr);
  if (!isActive()) return;
  if (finalUrl && finalUrl !== tsClickStr) {
    persistRebuiltClick(anchor, finalUrl);
  }
  navigate(anchor, finalUrl || tsClickStr, isMiddle);
}

// Entry point for click/auxclick handlers: prevent default and queue guarded nav.
function handleGuardedClick(ev: Event, isMiddle: boolean, isActive: () => boolean): void {
  const anchor = closestAnchor(ev.target);
  if (!anchor) return;

  const tsClickStr = anchor.getAttribute('data-tsclick') || '';
  if (!tsClickStr) return;

  ev.preventDefault();

  const runNavigation = () => {
    if (!isActive()) return;
    void guardNavigation(anchor, tsClickStr, isMiddle, isActive).catch((err) => {
      if (!isActive()) return;
      log.warn('tsjs-creative:click: failed to compute final URL', err);
      navigate(anchor, tsClickStr, isMiddle);
    });
  };

  queueTask(runNavigation);
}

// Observe href/data-tsclick mutations and repair anchors that third parties touch.
function monitorAnchorMutations(isActive: () => boolean): CreativeGuardHandle {
  if (typeof document === 'undefined' || typeof MutationObserver === 'undefined') {
    return Object.freeze({ dispose: () => undefined, scan: () => undefined });
  }

  const schedule = createMutationScheduler<AnchorLike>((anchor) => {
    if (!isActive()) return;
    const tsClickStr = anchor.getAttribute('data-tsclick') || '';
    if (!tsClickStr) return;
    void rebuildIfNeeded(anchor, tsClickStr)
      .then((finalUrl) => {
        if (!isActive()) return;
        if (finalUrl && finalUrl !== tsClickStr) {
          persistRebuiltClick(anchor, finalUrl);
        }
      })
      .catch((err) => {
        log.warn('tsjs-creative:click: failed to repair anchor', err);
      });
  });

  const scan = (): void => {
    if (!isActive()) return;
    const anchors = document.querySelectorAll<AnchorLike>('a[data-tsclick], area[data-tsclick]');
    anchors.forEach((anchor) => schedule(anchor));
  };

  const observer = new MutationObserver((records) => {
    if (!isActive()) return;
    for (const record of records) {
      if (record.type !== 'attributes') continue;
      const target = record.target;
      if (!(target instanceof Element)) continue;
      if (!target.matches('a[data-tsclick], area[data-tsclick]')) continue;
      schedule(target as AnchorLike);
    }
  });

  observer.observe(document, {
    subtree: true,
    attributes: true,
    attributeFilter: ['href', 'data-tsclick'],
  });

  let disposed = false;
  return Object.freeze({
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      observer.disconnect();
      schedule.dispose();
    },
    scan,
  });
}

// Wire up capture-phase click handlers + mutation observers to protect clicks.
export function installClickGuard(scanInitially = true): CreativeGuardHandle {
  if (log.getLevel && log.getLevel() === 'warn') {
    log.setLevel('info');
  }
  enableDebugFromEnv();
  log.info('tsjs-creative:click: installing click guard');

  let active = true;
  const isActive = (): boolean => active;
  const onClick = (ev: Event) => {
    if (!active) return;
    handleGuardedClick(ev, false, isActive);
  };

  const onAuxClick = (ev: MouseEvent) => {
    if (!active) return;
    if (ev.button !== 1) return;
    handleGuardedClick(ev, true, isActive);
  };

  document.addEventListener('click', onClick, true);
  document.addEventListener('auxclick', onAuxClick as EventListener, true);

  let mutations: CreativeGuardHandle | undefined;
  const dispose = (): void => {
    if (!active) return;
    active = false;
    document.removeEventListener('click', onClick, true);
    document.removeEventListener('auxclick', onAuxClick as EventListener, true);
    mutations?.dispose();
  };
  try {
    mutations = monitorAnchorMutations(isActive);
    const handle = Object.freeze({
      dispose,
      scan: (): void => mutations?.scan(),
    });
    if (scanInitially) handle.scan();
    return handle;
  } catch (error) {
    dispose();
    throw error;
  }
}
