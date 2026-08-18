import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

import { createBeaconGuard } from '../../src/shared/beacon_guard';
import type { BeaconGuardConfig } from '../../src/shared/beacon_guard';

function hasHttpHostname(url: string, hostname: string): boolean {
  try {
    const parsed = new URL(url);
    return (
      (parsed.protocol === 'https:' || parsed.protocol === 'http:') && parsed.hostname === hostname
    );
  } catch {
    return false;
  }
}

function rewriteToProxy(url: string, proxyPath: string): string {
  const parsed = new URL(url);
  return `http://localhost${proxyPath}${parsed.pathname}${parsed.search}${parsed.hash}`;
}

describe('Beacon Guard', () => {
  let originalSendBeaconDescriptor: PropertyDescriptor | undefined;
  let originalFetchDescriptor: PropertyDescriptor | undefined;
  let sendBeaconSpy: ReturnType<typeof vi.fn>;
  let fetchSpy: ReturnType<typeof vi.fn>;
  let config: BeaconGuardConfig;

  beforeEach(() => {
    // Save originals
    originalSendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    originalFetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');

    // Create spies that simulate real sendBeacon/fetch behaviour
    sendBeaconSpy = vi.fn(() => true);
    navigator.sendBeacon = sendBeaconSpy as typeof navigator.sendBeacon;

    fetchSpy = vi.fn(() => Promise.resolve(new Response('', { status: 200 })));
    window.fetch = fetchSpy as typeof window.fetch;

    config = {
      name: 'Test',
      isTargetUrl: (url: string) => hasHttpHostname(url, 'analytics.example.com'),
      rewriteUrl: (url: string) => rewriteToProxy(url, '/proxy'),
    };
  });

  afterEach(() => {
    if (originalSendBeaconDescriptor) {
      Object.defineProperty(navigator, 'sendBeacon', originalSendBeaconDescriptor);
    } else {
      Reflect.deleteProperty(navigator, 'sendBeacon');
    }
    if (originalFetchDescriptor) {
      Object.defineProperty(window, 'fetch', originalFetchDescriptor);
    } else {
      Reflect.deleteProperty(window, 'fetch');
    }
  });

  describe('createBeaconGuard', () => {
    it('should return install/isInstalled/reset interface', () => {
      const guard = createBeaconGuard(config);
      expect(guard).toHaveProperty('install');
      expect(guard).toHaveProperty('isInstalled');
      expect(guard).toHaveProperty('reset');
    });

    it('should start as not installed', () => {
      const guard = createBeaconGuard(config);
      expect(guard.isInstalled()).toBe(false);
    });

    it('should mark as installed after install()', () => {
      const guard = createBeaconGuard(config);
      guard.install();
      expect(guard.isInstalled()).toBe(true);
    });

    it('should be idempotent', () => {
      const guard = createBeaconGuard(config);
      guard.install();
      const patchedSendBeacon = navigator.sendBeacon;
      guard.install(); // second install
      // Should not double-patch
      expect(navigator.sendBeacon).toBe(patchedSendBeacon);
    });
  });

  describe('sendBeacon interception', () => {
    it('should rewrite matching sendBeacon URLs', () => {
      const guard = createBeaconGuard(config);
      guard.install();

      navigator.sendBeacon('https://analytics.example.com/g/collect?v=2', '');

      expect(sendBeaconSpy).toHaveBeenCalledWith('http://localhost/proxy/g/collect?v=2', '');
    });

    it('should pass through non-matching sendBeacon URLs', () => {
      const guard = createBeaconGuard(config);
      guard.install();

      navigator.sendBeacon('https://other.example.com/track', 'data');

      expect(sendBeaconSpy).toHaveBeenCalledWith('https://other.example.com/track', 'data');
    });

    it.each([
      'https://analytics.example.com.evil.test/collect',
      'https://analytics.example.com@evil.test/collect',
    ])('should pass through an analytics hostname lookalike: %s', (url) => {
      const guard = createBeaconGuard(config);
      guard.install();

      navigator.sendBeacon(url, 'data');

      expect(sendBeaconSpy).toHaveBeenCalledWith(url, 'data');
    });

    it('should forward body data', () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const body = JSON.stringify({ event: 'page_view' });
      navigator.sendBeacon('https://analytics.example.com/collect', body);

      expect(sendBeaconSpy).toHaveBeenCalledWith('http://localhost/proxy/collect', body);
    });
  });

  describe('fetch interception', () => {
    it('should rewrite matching fetch URLs (string input)', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      await window.fetch('https://analytics.example.com/g/collect?v=2');

      expect(fetchSpy).toHaveBeenCalledWith('http://localhost/proxy/g/collect?v=2', undefined);
    });

    it('should pass through non-matching fetch URLs', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      await window.fetch('https://other.example.com/api');

      expect(fetchSpy).toHaveBeenCalledWith('https://other.example.com/api', undefined);
    });

    it('should forward RequestInit options', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const init: RequestInit = { method: 'POST', body: 'payload' };
      await window.fetch('https://analytics.example.com/collect', init);

      expect(fetchSpy).toHaveBeenCalledWith('http://localhost/proxy/collect', init);
    });

    it('should handle Request object input', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const request = new Request('https://analytics.example.com/g/collect?tid=G-TEST');
      await window.fetch(request);

      // The spy should receive a new Request with the rewritten URL
      const calledArg = fetchSpy.mock.calls[0]![0] as Request;
      expect(calledArg).toBeInstanceOf(Request);
      expect(calledArg.url).toContain('/proxy/g/collect?tid=G-TEST');
    });

    it('should handle URL object input', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const url = new URL('https://analytics.example.com/g/collect');
      await window.fetch(url);

      expect(fetchSpy).toHaveBeenCalledWith('http://localhost/proxy/g/collect', undefined);
    });
  });

  it('restores the exact publisher-owned descriptors on reset', () => {
    const sendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
    const guard = createBeaconGuard(config);

    guard.install();
    guard.reset();

    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconDescriptor);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchDescriptor);
  });

  it.each(['sendBeacon', 'fetch'] as const)(
    'leaves a publisher %s replacement intact while releasing the other wrapper',
    (replaced) => {
      const sendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
      const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
      const guard = createBeaconGuard(config);
      guard.install();
      const replacementSendBeacon = vi.fn(() => false) as typeof navigator.sendBeacon;
      const replacementFetch = vi.fn(() => Promise.resolve(new Response())) as typeof window.fetch;
      if (replaced === 'sendBeacon') navigator.sendBeacon = replacementSendBeacon;
      else window.fetch = replacementFetch;

      guard.reset();

      if (replaced === 'sendBeacon') {
        expect(navigator.sendBeacon).toBe(replacementSendBeacon);
        expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchDescriptor);
      } else {
        expect(window.fetch).toBe(replacementFetch);
        expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(
          sendBeaconDescriptor
        );
      }
    }
  );

  it('leaves descriptor-attribute changes to the installed wrappers intact', () => {
    const guard = createBeaconGuard(config);
    guard.install();
    const installedSendBeacon = navigator.sendBeacon;
    const installedFetch = window.fetch;
    const sendBeaconReplacement = {
      configurable: true,
      enumerable: false,
      value: installedSendBeacon,
      writable: true,
    } satisfies PropertyDescriptor;
    const fetchReplacement = {
      configurable: true,
      enumerable: false,
      value: installedFetch,
      writable: true,
    } satisfies PropertyDescriptor;
    Object.defineProperty(navigator, 'sendBeacon', sendBeaconReplacement);
    Object.defineProperty(window, 'fetch', fetchReplacement);

    guard.reset();

    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconReplacement);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchReplacement);
  });

  it('does not invoke or replace hostile publisher accessors during reset', () => {
    const guard = createBeaconGuard(config);
    guard.install();
    const sendBeaconGetter = vi.fn(() => {
      throw new Error('sendBeacon getter must remain inert');
    });
    const fetchGetter = vi.fn(() => {
      throw new Error('fetch getter must remain inert');
    });
    const sendBeaconReplacement = {
      configurable: true,
      enumerable: true,
      get: sendBeaconGetter,
    } satisfies PropertyDescriptor;
    const fetchReplacement = {
      configurable: true,
      enumerable: true,
      get: fetchGetter,
    } satisfies PropertyDescriptor;
    Object.defineProperty(navigator, 'sendBeacon', sendBeaconReplacement);
    Object.defineProperty(window, 'fetch', fetchReplacement);

    expect(() => guard.reset()).not.toThrow();

    expect(sendBeaconGetter).not.toHaveBeenCalled();
    expect(fetchGetter).not.toHaveBeenCalled();
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconReplacement);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchReplacement);
  });

  it('isolates hostile descriptor inspection and still releases the other wrapper', () => {
    const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
    const guard = createBeaconGuard(config);
    guard.install();
    const installedSendBeacon = navigator.sendBeacon;
    const nativeDescriptor = Object.getOwnPropertyDescriptor;
    const descriptor = vi
      .spyOn(Object, 'getOwnPropertyDescriptor')
      .mockImplementation((target, property) => {
        if (target === navigator && property === 'sendBeacon') {
          throw new Error('publisher descriptor inspection failed');
        }
        return nativeDescriptor(target, property);
      });

    expect(() => guard.reset()).not.toThrow();
    descriptor.mockRestore();

    expect(navigator.sendBeacon).toBe(installedSendBeacon);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchDescriptor);
  });

  it('releases an installed wrapper after a later patch assignment fails', () => {
    const sendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
    if (!fetchDescriptor || !('value' in fetchDescriptor)) {
      throw new Error('test requires an own fetch data descriptor');
    }
    const nonWritableFetchDescriptor = {
      ...fetchDescriptor,
      writable: false,
    } satisfies PropertyDescriptor;
    Object.defineProperty(window, 'fetch', nonWritableFetchDescriptor);
    const guard = createBeaconGuard(config);

    expect(() => guard.install()).toThrow(TypeError);
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).not.toEqual(
      sendBeaconDescriptor
    );

    expect(() => guard.reset()).not.toThrow();
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconDescriptor);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(nonWritableFetchDescriptor);
  });

  it('restores stacked guards in reverse order and remains idempotent', () => {
    const sendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const fetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
    const first = createBeaconGuard(config);
    const second = createBeaconGuard({
      ...config,
      name: 'Second',
    });
    first.install();
    const firstSendBeaconDescriptor = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
    const firstFetchDescriptor = Object.getOwnPropertyDescriptor(window, 'fetch');
    second.install();

    second.reset();
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(
      firstSendBeaconDescriptor
    );
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(firstFetchDescriptor);

    first.reset();
    first.reset();
    second.reset();
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconDescriptor);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchDescriptor);
  });

  describe('multiple guards', () => {
    it('should allow independent guards to coexist', () => {
      const config2: BeaconGuardConfig = {
        name: 'Other',
        isTargetUrl: (url: string) => hasHttpHostname(url, 'other-tracker.com'),
        rewriteUrl: (url: string) => rewriteToProxy(url, '/other-proxy'),
      };

      const guard1 = createBeaconGuard(config);
      const guard2 = createBeaconGuard(config2);

      guard1.install();
      guard2.install();

      const lookalikeUrl = 'https://other-tracker.com.evil.test/collect';
      navigator.sendBeacon(lookalikeUrl, 'data');

      expect(guard1.isInstalled()).toBe(true);
      expect(guard2.isInstalled()).toBe(true);
      expect(sendBeaconSpy).toHaveBeenCalledWith(lookalikeUrl, 'data');
    });
  });
});
