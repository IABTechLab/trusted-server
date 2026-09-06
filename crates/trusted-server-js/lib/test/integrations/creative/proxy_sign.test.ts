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

  it('rejects data, javascript, and same-origin URLs', () => {
    expect(shouldProxyExternalUrl('data:image/png;base64,AAAA')).toBe(false);
    expect(shouldProxyExternalUrl('javascript:alert(1)')).toBe(false);
    expect(shouldProxyExternalUrl('/first-party/proxy?foo=1')).toBe(false);
    expect(shouldProxyExternalUrl(`${location.origin}/first-party/proxy?foo=1`)).toBe(false);
  });

  it('posts to /first-party/sign and returns signed href', async () => {
    const signed =
      '/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example%2Fasset.js&tstoken=tok&tsexp=1';
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: signed }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    const result = await signProxyUrl('https://cdn.example/asset.js?cb=1');

    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining('/first-party/sign'),
      expect.objectContaining({
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'same-origin',
      })
    );
    expect(result).toEqual({ outcome: 'signed', href: signed });
  });

  it('returns fallback when fetch is unavailable', async () => {
    global.fetch = undefined as unknown as typeof fetch;
    const result = await signProxyUrl('https://cdn.example/asset.js');
    expect(result).toEqual({ outcome: 'fallback' });
  });

  it('returns blocked for a signing policy rejection', async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 403,
    }) as unknown as typeof fetch;

    const result = await signProxyUrl('https://blocked.example.com/asset.js');

    expect(result).toEqual({ outcome: 'blocked' });
  });

  it('returns fallback for a non-policy HTTP failure', async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
    }) as unknown as typeof fetch;

    const result = await signProxyUrl('https://cdn.example/asset.js');

    expect(result).toEqual({ outcome: 'fallback' });
  });

  it('returns fallback when a successful response lacks an href', async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({}),
    }) as unknown as typeof fetch;

    const result = await signProxyUrl('https://cdn.example/asset.js');

    expect(result).toEqual({ outcome: 'fallback' });
  });

  it('skips the doomed POST in an opaque origin and returns fallback', async () => {
    // A sandboxed srcdoc creative without `allow-same-origin` has origin
    // "null": the JSON POST would preflight and fail, so signing bails out
    // without issuing the request.
    const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
    Object.defineProperty(window, 'origin', { value: 'null', configurable: true });
    const fetchMock = vi.fn();
    global.fetch = fetchMock as unknown as typeof fetch;

    try {
      const result = await signProxyUrl('https://cdn.example/asset.js');
      expect(result).toEqual({ outcome: 'fallback' });
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      if (originDescriptor) {
        Object.defineProperty(window, 'origin', originDescriptor);
      } else {
        delete (window as { origin?: string }).origin;
      }
    }
  });
});
