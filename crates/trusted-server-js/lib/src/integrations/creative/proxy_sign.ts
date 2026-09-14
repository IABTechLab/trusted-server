/** @file Validates and signs dynamic external creative resource URLs. */

import { log } from '../../core/log';
import { hasOpaqueOrigin } from '../../shared/origin';

const PROXY_PREFIX = '/first-party/proxy';

/** Return whether a URL is external, HTTP(S), and not already proxied. */
export function shouldProxyExternalUrl(raw: string): boolean {
  const value = String(raw || '').trim();
  if (!value) return false;
  if (/^(data:|javascript:|blob:|about:)/i.test(value)) return false;
  if (value.startsWith(PROXY_PREFIX)) return false;
  try {
    const url = new URL(value, location.href);
    if (url.origin === location.origin) {
      if (url.pathname.startsWith(PROXY_PREFIX)) return false;
      return false;
    }
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

/** Fail-closed result of requesting a first-party proxy signature. */
export type ProxySignOutcome =
  | { outcome: 'signed'; href: string }
  | { outcome: 'fallback' }
  | { outcome: 'blocked' };

const FALLBACK: ProxySignOutcome = { outcome: 'fallback' };

/**
 * Request a signed first-party proxy URL.
 *
 * Opaque creative origins and operational failures return `fallback`; policy
 * denials return `blocked` so callers do not restore the rejected raw URL.
 */
export async function signProxyUrl(raw: string): Promise<ProxySignOutcome> {
  if (typeof fetch !== 'function') return FALLBACK;
  // A sandboxed srcdoc creative without `allow-same-origin` has an opaque
  // origin: this JSON POST would preflight with `Origin: null` and fail, so
  // skip the doomed request and leave the resource URL unsigned. Dynamic
  // signing from opaque-origin creatives needs a same-origin parent
  // postMessage broker — tracked in
  // https://github.com/IABTechLab/trusted-server/issues/982. Until then,
  // dynamically inserted resources degrade to loading directly, which the
  // sandbox still isolates from the publisher origin.
  if (hasOpaqueOrigin()) return FALLBACK;
  let absolute: string;
  try {
    absolute = new URL(raw, location.href).toString();
  } catch {
    return FALLBACK;
  }

  let endpoint = '/first-party/sign';
  try {
    endpoint = new URL('/first-party/sign', location.href).toString();
  } catch {
    /* fall back to relative path */
  }

  try {
    const resp = await fetch(endpoint, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify({ url: absolute }),
    });
    if (!resp.ok) {
      log.warn('tsjs-creative: sign HTTP error', resp.status);
      return resp.status === 403 ? { outcome: 'blocked' } : FALLBACK;
    }
    const data = (await resp.json()) as { href?: string } | null;
    const href = data && typeof data.href === 'string' ? data.href : '';
    return href ? { outcome: 'signed', href } : FALLBACK;
  } catch (err) {
    log.warn('tsjs-creative: sign request failed', err);
    return FALLBACK;
  }
}
