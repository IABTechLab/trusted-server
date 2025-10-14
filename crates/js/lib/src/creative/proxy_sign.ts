import { log } from '../core/log';

const PROXY_PREFIX = '/first-party/proxy';

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

export async function signProxyUrl(raw: string): Promise<string | null> {
  if (typeof fetch !== 'function') return null;
  let absolute: string;
  try {
    absolute = new URL(raw, location.href).toString();
  } catch {
    return null;
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
      return null;
    }
    const data = (await resp.json()) as { href?: string } | null;
    const href = data && typeof data.href === 'string' ? data.href : null;
    return href;
  } catch (err) {
    log.warn('tsjs-creative: sign request failed', err);
    return null;
  }
}
