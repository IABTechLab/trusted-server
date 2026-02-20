import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { createBeaconGuard, BeaconGuardConfig } from '../../src/shared/beacon_guard';

describe('Beacon Guard', () => {
  let originalSendBeacon: typeof navigator.sendBeacon;
  let originalFetch: typeof window.fetch;
  let sendBeaconSpy: ReturnType<typeof vi.fn>;
  let fetchSpy: ReturnType<typeof vi.fn>;
  let config: BeaconGuardConfig;

  beforeEach(() => {
    // Save originals
    originalSendBeacon = navigator.sendBeacon;
    originalFetch = window.fetch;

    // Create spies that simulate real sendBeacon/fetch behaviour
    sendBeaconSpy = vi.fn((_url: string | URL, _data?: BodyInit | null) => true);
    navigator.sendBeacon = sendBeaconSpy;

    fetchSpy = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) =>
      Promise.resolve(new Response('', { status: 204 }))
    );
    window.fetch = fetchSpy;

    config = {
      name: 'Test',
      isTargetUrl: (url: string) => url.includes('analytics.example.com'),
      rewriteUrl: (url: string) => url.replace(/https?:\/\/analytics\.example\.com/, '/proxy'),
    };
  });

  afterEach(() => {
    navigator.sendBeacon = originalSendBeacon;
    window.fetch = originalFetch;
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

      expect(sendBeaconSpy).toHaveBeenCalledWith('/proxy/g/collect?v=2', '');
    });

    it('should pass through non-matching sendBeacon URLs', () => {
      const guard = createBeaconGuard(config);
      guard.install();

      navigator.sendBeacon('https://other.example.com/track', 'data');

      expect(sendBeaconSpy).toHaveBeenCalledWith('https://other.example.com/track', 'data');
    });

    it('should forward body data', () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const body = JSON.stringify({ event: 'page_view' });
      navigator.sendBeacon('https://analytics.example.com/collect', body);

      expect(sendBeaconSpy).toHaveBeenCalledWith('/proxy/collect', body);
    });
  });

  describe('fetch interception', () => {
    it('should rewrite matching fetch URLs (string input)', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      await window.fetch('https://analytics.example.com/g/collect?v=2');

      expect(fetchSpy).toHaveBeenCalledWith('/proxy/g/collect?v=2', undefined);
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

      expect(fetchSpy).toHaveBeenCalledWith('/proxy/collect', init);
    });

    it('should handle Request object input', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const request = new Request('https://analytics.example.com/g/collect?tid=G-TEST');
      await window.fetch(request);

      // The spy should receive a new Request with the rewritten URL
      const calledArg = fetchSpy.mock.calls[0][0];
      expect(calledArg).toBeInstanceOf(Request);
      expect(calledArg.url).toContain('/proxy/g/collect?tid=G-TEST');
    });

    it('should handle URL object input', async () => {
      const guard = createBeaconGuard(config);
      guard.install();

      const url = new URL('https://analytics.example.com/g/collect');
      await window.fetch(url);

      expect(fetchSpy).toHaveBeenCalledWith('/proxy/g/collect', undefined);
    });
  });

  describe('multiple guards', () => {
    it('should allow independent guards to coexist', () => {
      const config2: BeaconGuardConfig = {
        name: 'Other',
        isTargetUrl: (url: string) => url.includes('other-tracker.com'),
        rewriteUrl: (url: string) => url.replace(/https?:\/\/other-tracker\.com/, '/other-proxy'),
      };

      const guard1 = createBeaconGuard(config);
      const guard2 = createBeaconGuard(config2);

      guard1.install();
      guard2.install();

      expect(guard1.isInstalled()).toBe(true);
      expect(guard2.isInstalled()).toBe(true);
    });
  });
});
