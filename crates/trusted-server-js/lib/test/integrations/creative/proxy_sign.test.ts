import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  shouldProxyExternalUrl,
  signProxyUrl,
} from '../../../src/integrations/creative/proxy_sign';

const ORIGINAL_FETCH = global.fetch;

describe('creative/proxy_sign.ts', () => {
  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
  });

  it('flags external http/https URLs for proxying', () => {
    expect(shouldProxyExternalUrl('https://cdn.example/ad.js')).toBe(true);
    expect(shouldProxyExternalUrl('http://cdn.example/pixel.gif')).toBe(true);
  });

  it('rejects data, javascript, same-origin URLs, and only the exact trusted proxy path', () => {
    expect(shouldProxyExternalUrl('data:image/png;base64,AAAA')).toBe(false);
    expect(shouldProxyExternalUrl('javascript:alert(1)')).toBe(false);
    expect(shouldProxyExternalUrl('/first-party/proxy?foo=1')).toBe(false);
    expect(shouldProxyExternalUrl(`${location.origin}/first-party/proxy?foo=1`)).toBe(false);
    expect(shouldProxyExternalUrl('https://foreign.example/first-party/proxy?foo=1')).toBe(true);
  });

  it('trusts only the captured origin and exact proxy endpoint', async () => {
    const descriptor = Object.getOwnPropertyDescriptor(document, 'currentScript');
    const script = Object.assign(document.createElement('script'), {
      src: 'https://ads.example.com/static/tsjs=tsjs-unified.min.js?v=hash',
    });
    Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
    vi.resetModules();

    try {
      const { shouldProxyExternalUrl: shouldProxy } =
        await import('../../../src/integrations/creative/proxy_sign');
      expect(shouldProxy('https://ads.example.com/first-party/proxy?token=1')).toBe(false);
      expect(shouldProxy('https://ads.example.com/first-party/proxy-extra?token=1')).toBe(true);
      expect(shouldProxy('https://foreign.example/first-party/proxy?token=1')).toBe(true);
    } finally {
      if (descriptor) Object.defineProperty(document, 'currentScript', descriptor);
      else Reflect.deleteProperty(document, 'currentScript');
    }
  });

  it('posts to /first-party/sign and returns signed href', async () => {
    const signed =
      'https://ads.example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example%2Fasset.js&tstoken=tok&tsexp=1';
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: signed }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    const result = await signProxyUrl('https://cdn.example/asset.js?cb=1');

    expect(fetchMock).toHaveBeenCalledWith(
      new URL('/first-party/sign', location.href).toString(),
      expect.objectContaining({
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
      })
    );
    expect(result).toBe(signed);
  });

  it('returns null when fetch is unavailable', async () => {
    global.fetch = undefined as any;
    const result = await signProxyUrl('https://cdn.example/asset.js');
    expect(result).toBeNull();
  });
});
