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

  it('flags protocol-relative URLs with ports for proxying', () => {
    // Protocol-relative URL with custom port should be proxyable
    expect(shouldProxyExternalUrl('//local.example.com:9443/static/img/300x250.svg')).toBe(true);
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
    expect(result).toBe(signed);
  });

  it('returns null when fetch is unavailable', async () => {
    global.fetch = undefined as any;
    const result = await signProxyUrl('https://cdn.example/asset.js');
    expect(result).toBeNull();
  });

  it('preserves port in protocol-relative URL when signing', async () => {
    // When signing a protocol-relative URL with a custom port,
    // the port should be preserved in the absolute URL sent to the sign endpoint
    // Note: The scheme comes from location.href, which is http: in test env
    const signed =
      '/first-party/proxy?tsurl=http%3A%2F%2Flocal.example.com%3A9443%2Fstatic%2Fimg%2Ftest.svg&tstoken=tok';
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: signed }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    await signProxyUrl('//local.example.com:9443/static/img/test.svg');

    // Verify the absolute URL passed to fetch includes the port
    // In test env, location.href uses http, so the resolved URL uses http
    // The key thing is that :9443 port is preserved
    const bodyArg = JSON.parse((fetchMock.mock.calls[0][1] as RequestInit).body as string);
    expect(bodyArg.url).toContain('local.example.com:9443');
    expect(bodyArg.url).toBe('http://local.example.com:9443/static/img/test.svg');
  });
});
