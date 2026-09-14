import { describe, expect, it, vi } from 'vitest';

import {
  createFirstDisplayBrowserRouteOwner,
  registerFirstDisplayBrowserRoute,
} from '../../src/first_display/leaf/browser_route_owner';

describe('first-display browser route owner', () => {
  it('shares one DOM interceptor across rules and releases each rule independently', () => {
    const nativeAppendChild = Element.prototype.appendChild;
    const releaseAlpha = registerFirstDisplayBrowserRoute({
      matches: (_kind, url) => url.includes('alpha.example'),
      rewrite: () => 'https://publisher.example/alpha',
    });
    const sharedAppendChild = Element.prototype.appendChild;
    const releaseBeta = registerFirstDisplayBrowserRoute({
      matches: (_kind, url) => url.includes('beta.example'),
      rewrite: () => 'https://publisher.example/beta',
    });
    const appendChildAfterSecondRegistration = Element.prototype.appendChild;

    releaseAlpha();
    const alpha = document.createElement('script');
    alpha.src = 'https://alpha.example/sdk.js';
    document.head.appendChild(alpha);
    const beta = document.createElement('script');
    beta.src = 'https://beta.example/sdk.js';
    document.head.appendChild(beta);
    releaseBeta();
    const appendChildAfterFinalRelease = Element.prototype.appendChild;
    alpha.remove();
    beta.remove();

    expect(appendChildAfterSecondRegistration).toBe(sharedAppendChild);
    expect(alpha.src).toBe('https://alpha.example/sdk.js');
    expect(beta.src).toBe('https://publisher.example/beta');
    expect(appendChildAfterFinalRelease).toBe(nativeAppendChild);
  });

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

  it('rewrites variadic and fragment insertion paths synchronously and compare-restores them', () => {
    const append = Element.prototype.append;
    const prepend = Element.prototype.prepend;
    const replaceChildren = Element.prototype.replaceChildren;
    const release = registerFirstDisplayBrowserRoute({
      matches: (kind, url) => kind === 'script' && url.includes('vendor.example'),
      rewrite: (url) => `/integrations/proxy?source=${encodeURIComponent(url)}`,
    });
    const appended = document.createElement('script');
    appended.src = 'https://vendor.example/append.js';
    const prepended = document.createElement('script');
    prepended.src = 'https://vendor.example/prepend.js';
    const nested = document.createElement('script');
    nested.src = 'https://vendor.example/fragment.js';
    const fragment = document.createDocumentFragment();
    fragment.appendChild(nested);

    document.head.append(appended);
    document.head.prepend(prepended);
    document.head.replaceChildren(fragment);
    const restoredAfterRelease = {
      append: Element.prototype.append,
      prepend: Element.prototype.prepend,
      replaceChildren: Element.prototype.replaceChildren,
    };
    release();
    nested.remove();

    expect(appended.getAttribute('src')).toContain('/integrations/proxy?source=');
    expect(prepended.getAttribute('src')).toContain('/integrations/proxy?source=');
    expect(nested.getAttribute('src')).toContain('/integrations/proxy?source=');
    expect(restoredAfterRelease.append).not.toBe(append);
    expect(Element.prototype.append).toBe(append);
    expect(Element.prototype.prepend).toBe(prepend);
    expect(Element.prototype.replaceChildren).toBe(replaceChildren);
  });

  it('owns matching beacon and fetch routes without changing publisher replacements on release', async () => {
    const originalBeacon = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const originalFetch = Object.getOwnPropertyDescriptor(window, 'fetch');
    const beacon = vi.fn(() => true);
    const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response());
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

  it('preserves Request identity when unmatched and metadata when rewriting its URL', async () => {
    const originalFetch = Object.getOwnPropertyDescriptor(window, 'fetch');
    const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response());
    Object.defineProperty(window, 'fetch', {
      configurable: true,
      value: fetch,
      writable: true,
    });
    const release = registerFirstDisplayBrowserRoute(
      {
        matches: (kind, url) => kind === 'fetch' && url.includes('analytics.example'),
        rewrite: () => 'https://publisher.example/integrations/analytics',
      },
      true
    );
    const unmatched = new Request('https://publisher.example/content', {
      body: 'unmatched-body',
      headers: { 'x-owner': 'publisher' },
      method: 'POST',
    });
    const matched = new Request('https://analytics.example/collect', {
      body: 'matched-body',
      headers: { 'x-owner': 'publisher' },
      method: 'POST',
    });

    await window.fetch(unmatched);
    await window.fetch(matched);
    const unmatchedInput = fetch.mock.calls[0]?.[0];
    const matchedInput = fetch.mock.calls[1]?.[0] as Request;
    release();
    if (originalFetch) Object.defineProperty(window, 'fetch', originalFetch);
    else Reflect.deleteProperty(window, 'fetch');

    expect(unmatchedInput).toBe(unmatched);
    expect(matchedInput).toBeInstanceOf(Request);
    expect(matchedInput.url).toBe('https://publisher.example/integrations/analytics');
    expect(matchedInput.method).toBe('POST');
    expect(matchedInput.headers.get('x-owner')).toBe('publisher');
    await expect(matchedInput.text()).resolves.toBe('matched-body');
  });

  it('continues restoring every owned surface when one browser property rejects restoration', () => {
    const appendChildDescriptor = Object.getOwnPropertyDescriptor(Element.prototype, 'appendChild');
    const insertBefore = Element.prototype.insertBefore;
    const append = Element.prototype.append;
    const prepend = Element.prototype.prepend;
    const replaceChildren = Element.prototype.replaceChildren;
    const beaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
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

    try {
      const owner = createFirstDisplayBrowserRouteOwner(document, window);
      const release = owner.register({ matches: () => false, rewrite: (url) => url }, true);
      const installedAppendChild = Element.prototype.appendChild;
      Object.defineProperty(Element.prototype, 'appendChild', {
        configurable: true,
        get: () => installedAppendChild,
        set: () => {
          throw new TypeError('hostile browser surface');
        },
      });

      expect(() => release()).not.toThrow();
      expect(Element.prototype.insertBefore).toBe(insertBefore);
      expect(Element.prototype.append).toBe(append);
      expect(Element.prototype.prepend).toBe(prepend);
      expect(Element.prototype.replaceChildren).toBe(replaceChildren);
      expect(navigator.sendBeacon).toBe(beacon);
      expect(window.fetch).toBe(fetch);
    } finally {
      if (appendChildDescriptor) {
        Object.defineProperty(Element.prototype, 'appendChild', appendChildDescriptor);
      }
      if (beaconDescriptor) Object.defineProperty(navigator, 'sendBeacon', beaconDescriptor);
      else Reflect.deleteProperty(navigator, 'sendBeacon');
      if (fetchDescriptor) Object.defineProperty(window, 'fetch', fetchDescriptor);
      else Reflect.deleteProperty(window, 'fetch');
    }
  });
});
