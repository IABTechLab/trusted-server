import { log } from '../core/log';
import type { CreativeWindow, TsCreativeApi } from '../shared/globals';
import { creativeGlobal, resolveWindow } from '../shared/globals';
import { delay, queueTask } from '../shared/async';
import { createMutationScheduler } from '../shared/scheduler';

type AnchorLike = HTMLAnchorElement | HTMLAreaElement;
type Canon = { base: string; params: Record<string, string> };
type Diff = { add: Record<string, string>; del: string[] };

function enableDebugFromEnv(): void {
  try {
    const q = new URLSearchParams(location.search);
    const ls = creativeGlobal.localStorage;
    const flag = q.get('tsdebug') === '1' || (ls && ls.getItem && ls.getItem('tsdebug') === '1');
    if (flag) log.setLevel('debug');
  } catch (err) {
    log.debug('tsjs-creative: debug flag inspection failed', err);
  }
}

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

function canonFromFirstPartyClick(url: string): Canon | null {
  try {
    const u = new URL(url, location.href);
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

function canonFromAnyHref(href: string): Canon | null {
  const fp = canonFromFirstPartyClick(href);
  if (fp) return fp;
  try {
    const u = new URL(href, location.href);
    const params = parseQuery(u.search);
    u.search = '';
    u.hash = '';
    return { base: u.toString(), params };
  } catch {
    return null;
  }
}

function sameBaseIgnoreScheme(aBase: string, bBase: string): boolean {
  try {
    const au = new URL(aBase, location.href);
    const bu = new URL(bBase, location.href);
    return au.hostname === bu.hostname && au.pathname === bu.pathname;
  } catch {
    return aBase === bBase;
  }
}

function equalCanon(a: Canon, b: Canon): boolean {
  if (!sameBaseIgnoreScheme(a.base, b.base)) return false;
  const ak = Object.keys(a.params).sort();
  const bk = Object.keys(b.params).sort();
  if (ak.length !== bk.length) return false;
  for (let i = 0; i < ak.length; i++) {
    const k = ak[i];
    if (k !== bk[i]) return false;
    if (a.params[k] !== b.params[k]) return false;
  }
  return true;
}

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

async function rebuildClick(a: AnchorLike, tsClickStr: string, diff: Diff): Promise<string> {
  const addKeys = Object.keys(diff.add);
  const delKeys = diff.del;
  if (addKeys.length === 0 && delKeys.length === 0) {
    return tsClickStr;
  }

  const fallback = buildProxyRebuildUrl(tsClickStr, diff);

  if (typeof fetch !== 'function') {
    try {
      const el = a as Element;
      el.setAttribute('href', fallback);
    } catch (err) {
      log.debug('tsjs-creative: unable to set fallback href (no-fetch)', err);
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
      log.warn('tsjs-creative: proxy-rebuild HTTP error', resp.status);
      try {
        const el = a as Element;
        el.setAttribute('href', fallback);
      } catch (err) {
        log.debug('tsjs-creative: unable to set fallback href (http error)', err);
      }
      return fallback;
    }
    const data = (await resp.json()) as { href?: string; base?: string } | null;
    const href = data && typeof data.href === 'string' ? data.href : null;
    if (href) {
      const el = a as Element;
      try {
        el.setAttribute('data-tsclick', href);
        el.setAttribute('href', href);
      } catch (err) {
        log.debug('tsjs-creative: failed to update anchor attributes', err);
      }
      log.info('tsjs-creative: rebuilt click', {
        added: addKeys,
        removed: delKeys,
      });
      return href;
    }
  } catch (err) {
    log.warn('tsjs-creative: proxy-rebuild request failed', err);
  }

  try {
    const el = a as Element;
    el.setAttribute('href', fallback);
  } catch (err) {
    log.debug('tsjs-creative: unable to apply fallback href', err);
  }
  return fallback;
}

async function computeFinalUrl(a: AnchorLike, tsClickStr: string): Promise<string> {
  const orig = canonFromFirstPartyClick(tsClickStr);
  if (!orig) return tsClickStr;

  const rawHref = a.getAttribute && a.getAttribute('href');
  const currentHref = rawHref || a.href || '';
  if (!currentHref) return tsClickStr;

  const mutated = canonFromAnyHref(currentHref);
  if (!mutated) return tsClickStr;

  if (equalCanon(orig, mutated)) return tsClickStr;

  const diff = diffParams(orig, mutated);
  if (!diff) {
    log.warn('tsjs-creative: click base changed; keeping original', {
      original: orig.base,
      mutated: mutated.base,
    });
    return tsClickStr;
  }

  if (Object.keys(diff.add).length === 0 && diff.del.length === 0) {
    return tsClickStr;
  }

  log.debug('tsjs-creative: detected click rewrite', {
    add: Object.keys(diff.add),
    del: diff.del,
  });

  return rebuildClick(a, tsClickStr, diff);
}

function navigate(a: AnchorLike, url: string, isMiddle: boolean): void {
  const target = a.getAttribute('target') || (isMiddle ? '_blank' : '_self');
  if (target === '_blank' || isMiddle) {
    window.open(url, target, 'noopener,noreferrer');
  } else {
    location.href = url;
  }
}

async function rebuildIfNeeded(anchor: AnchorLike, tsClickStr: string): Promise<string> {
  let finalUrl = await computeFinalUrl(anchor, tsClickStr);
  if (finalUrl === tsClickStr) {
    await delay();
    finalUrl = await computeFinalUrl(anchor, tsClickStr);
  }
  return finalUrl;
}

async function guardNavigation(
  anchor: AnchorLike,
  tsClickStr: string,
  isMiddle: boolean
): Promise<void> {
  const finalUrl = await rebuildIfNeeded(anchor, tsClickStr);
  if (finalUrl && finalUrl !== tsClickStr) {
    try {
      const el = anchor as Element;
      el.setAttribute('data-tsclick', finalUrl);
      el.setAttribute('href', finalUrl);
    } catch (err) {
      log.debug('tsjs-creative: failed to persist rebuilt href before navigation', err);
    }
  }
  navigate(anchor, finalUrl || tsClickStr, isMiddle);
}

function handleGuardedClick(ev: Event, isMiddle: boolean): void {
  const anchor = closestAnchor(ev.target);
  if (!anchor) return;

  const tsClickStr = anchor.getAttribute('data-tsclick') || '';
  if (!tsClickStr) return;

  ev.preventDefault();

  const runNavigation = () => {
    void guardNavigation(anchor, tsClickStr, isMiddle).catch((err) => {
      log.warn('tsjs-creative: failed to compute final URL', err);
      navigate(anchor, tsClickStr, isMiddle);
    });
  };

  queueTask(runNavigation);
}

export function installProxyClickGuard(): void {
  try {
    log.setLevel('debug');
  } catch (err) {
    log.debug('tsjs-creative: unable to set log level', err);
  }
  enableDebugFromEnv();
  log.info('tsjs-creative: installing click guard');

  const onClick = (ev: Event) => {
    handleGuardedClick(ev, false);
  };

  const onAuxClick = (ev: MouseEvent) => {
    if (ev.button !== 1) return;
    handleGuardedClick(ev, true);
  };

  document.addEventListener('click', onClick, true);
  document.addEventListener('auxclick', onAuxClick as EventListener, true);

  monitorAnchorMutations();
}

export const tsCreative: TsCreativeApi = { installProxyClickGuard };

try {
  creativeGlobal.tscreative = tsCreative;
} catch (err) {
  log.debug('tsjs-creative: failed to expose global tscreative', err);
}

export default tsCreative;

(function auto() {
  const maybeWindow = resolveWindow();
  if (maybeWindow && typeof document !== 'undefined') {
    const win = maybeWindow as CreativeWindow;
    if (win.__ts_creative_installed) return;
    win.__ts_creative_installed = true;
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', () => installProxyClickGuard());
    } else {
      installProxyClickGuard();
    }
  }
})();

function monitorAnchorMutations(): void {
  if (typeof document === 'undefined' || typeof MutationObserver === 'undefined') return;

  const schedule = createMutationScheduler<AnchorLike>((anchor) => {
    const tsClickStr = anchor.getAttribute('data-tsclick') || '';
    if (!tsClickStr) return;
    void rebuildIfNeeded(anchor, tsClickStr)
      .then((finalUrl) => {
        if (finalUrl && finalUrl !== tsClickStr) {
          try {
            const el = anchor as Element;
            el.setAttribute('data-tsclick', finalUrl);
            el.setAttribute('href', finalUrl);
          } catch (err) {
            log.debug('tsjs-creative: failed to persist rebuilt href during mutation flush', err);
          }
        }
      })
      .catch((err) => {
        log.warn('tsjs-creative: failed to repair anchor', err);
      });
  });

  const scan = () => {
    const anchors = document.querySelectorAll<AnchorLike>('a[data-tsclick], area[data-tsclick]');
    anchors.forEach((anchor) => schedule(anchor));
  };

  scan();

  const observer = new MutationObserver((records) => {
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
}
