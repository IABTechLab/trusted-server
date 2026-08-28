import { describe, expect, it, vi } from 'vitest';

import { registerFirstDisplayBrowserRoute } from '../../src/first_display/leaf/browser_route_owner';

describe('first-display browser route owner', () => {
  it('rewrites matching scripts and preloads before native insertion and restores exactly once', () => {
    const appendChild = Element.prototype.appendChild;
    const insertBefore = Element.prototype.insertBefore;
    const release = registerFirstDisplayBrowserRoute({
      matches: (kind, url) =>
        (kind === 'script' || kind === 'preload') && url.includes('vendor.example'),
      rewrite: (url) => `/integrations/proxy?source=${encodeURIComponent(url)}`,
    });

    const script = document.createElement('script');
    script.src = 'https://vendor.example/sdk.js?v=1';
    document.head.appendChild(script);
    expect(script.getAttribute('src')).toContain('/integrations/proxy?source=');

    const preload = document.createElement('link');
    preload.rel = 'preload';
    preload.setAttribute('as', 'script');
    preload.href = 'https://vendor.example/later.js';
    document.head.insertBefore(preload, script);
    expect(preload.getAttribute('href')).toContain('/integrations/proxy?source=');

    release();
    release();
    expect(Element.prototype.appendChild).toBe(appendChild);
    expect(Element.prototype.insertBefore).toBe(insertBefore);

    const later = document.createElement('script');
    later.src = 'https://vendor.example/unowned.js';
    document.head.appendChild(later);
    expect(later.src).toBe('https://vendor.example/unowned.js');
    script.remove();
    preload.remove();
    later.remove();
  });

  it('owns matching beacon and fetch routes without changing publisher replacements on release', async () => {
    const originalBeacon = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const originalFetch = Object.getOwnPropertyDescriptor(window, 'fetch');
    const beacon = vi.fn(() => true);
    const fetch = vi.fn(async () => new Response());
    Object.defineProperty(navigator, 'sendBeacon', {
      configurable: true,
      value: beacon,
      writable: true,
    });
    Object.defineProperty(window, 'fetch', {
      configurable: true,
      value: fetch,
      writable: true,
    });
    const release = registerFirstDisplayBrowserRoute(
      {
        matches: (kind, url) =>
          (kind === 'beacon' || kind === 'fetch') && url.includes('analytics.example'),
        rewrite: () => 'https://publisher.example/integrations/analytics',
      },
      true
    );

    expect(navigator.sendBeacon('https://analytics.example/collect')).toBe(true);
    await window.fetch('https://analytics.example/fetch');
    expect(beacon).toHaveBeenCalledWith(
      'https://publisher.example/integrations/analytics',
      undefined
    );
    expect(fetch).toHaveBeenCalledWith(
      'https://publisher.example/integrations/analytics',
      undefined
    );

    const publisherFetch = vi.fn(async () => new Response());
    window.fetch = publisherFetch;
    release();
    expect(window.fetch).toBe(publisherFetch);
    if (originalBeacon) Object.defineProperty(navigator, 'sendBeacon', originalBeacon);
    else Reflect.deleteProperty(navigator, 'sendBeacon');
    if (originalFetch) Object.defineProperty(window, 'fetch', originalFetch);
    else Reflect.deleteProperty(window, 'fetch');
  });
});
