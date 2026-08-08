import { afterEach, describe, expect, it, vi } from 'vitest';

import { createPermutiveRuntime } from '../../../src/integrations/permutive/module';

describe('transactional Permutive integration module', () => {
  afterEach(() => vi.useRealTimers());

  it('registers one disposable auction-context contributor during activation', () => {
    const order: string[] = [];
    let contributor: (() => Readonly<Record<string, unknown>> | undefined) | undefined;
    const runtime = createPermutiveRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => undefined,
      getSegments: () => ['11', '22'],
      installGuard: () => order.push('guard:install'),
      location: { host: 'news.example', protocol: 'https:' },
      registerContext: (candidate) => {
        contributor = candidate;
        order.push('context:register');
        return () => order.push('context:release');
      },
      resetGuard: () => order.push('guard:reset'),
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      started: vi.fn(),
      timedOut: vi.fn(),
    });

    const release = runtime.activate(undefined);

    expect(contributor?.()).toEqual({ permutive_segments: ['11', '22'] });
    expect(order).toEqual(['guard:install', 'context:register']);
    release();
    release();
    expect(order).toEqual(['guard:install', 'context:register', 'context:release', 'guard:reset']);
  });

  it('bounds a context-service segment snapshot even when an injected reader overproduces', () => {
    let contributor: (() => Readonly<Record<string, unknown>> | undefined) | undefined;
    const runtime = createPermutiveRuntime({
      getSegments: () => Array.from({ length: 101 }, (_, index) => `${index}`),
      installGuard: vi.fn(),
      registerContext: (candidate) => {
        contributor = candidate;
        return vi.fn();
      },
      resetGuard: vi.fn(),
    });

    const release = runtime.activate(undefined);
    const snapshot = contributor?.() as { readonly permutive_segments?: readonly string[] };

    expect(snapshot.permutive_segments).toHaveLength(100);
    expect(Object.isFrozen(snapshot.permutive_segments)).toBe(true);
    release();
  });

  it('rewrites a later SDK config and compare-restores every owned field', async () => {
    vi.useFakeTimers();
    const config = {
      apiHost: 'api.permutive.com',
      apiProtocol: 'https',
      cdnBaseUrl: 'cdn.permutive.com',
      cdnProtocol: 'https',
      secureSignalsApiHost: 'signals.permutive.com',
      segmentSyncApiHost: 'sync.permutive.com',
    };
    let available = false;
    const runtime = createPermutiveRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => (available ? { config } : undefined),
      getSegments: () => [],
      installGuard: vi.fn(),
      location: { host: 'news.example', protocol: 'https:' },
      registerContext: () => vi.fn(),
      resetGuard: vi.fn(),
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      started: vi.fn(),
      timedOut: vi.fn(),
    });
    const release = runtime.activate(undefined);
    runtime.start(undefined);
    available = true;
    await vi.advanceTimersByTimeAsync(50);

    expect(config).toEqual({
      apiHost: 'news.example/integrations/permutive/api',
      apiProtocol: 'https',
      cdnBaseUrl: 'news.example/integrations/permutive/cdn',
      cdnProtocol: 'https',
      secureSignalsApiHost: 'news.example/integrations/permutive/secure-signal',
      segmentSyncApiHost: 'news.example/integrations/permutive/sync',
    });
    config.apiHost = 'publisher.example/replacement';
    release();
    expect(config).toEqual({
      apiHost: 'publisher.example/replacement',
      apiProtocol: 'https',
      cdnBaseUrl: 'cdn.permutive.com',
      cdnProtocol: 'https',
      secureSignalsApiHost: 'signals.permutive.com',
      segmentSyncApiHost: 'sync.permutive.com',
    });
  });

  it('rolls back the guard when context registration is refused', () => {
    const resetGuard = vi.fn();
    const runtime = createPermutiveRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => undefined,
      getSegments: () => [],
      installGuard: vi.fn(),
      location: { host: 'news.example', protocol: 'https:' },
      registerContext: () => undefined,
      resetGuard,
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      started: vi.fn(),
      timedOut: vi.fn(),
    });

    expect(() => runtime.activate(undefined)).toThrowError('Permutive context registration failed');
    expect(resetGuard).toHaveBeenCalledOnce();
  });
});
