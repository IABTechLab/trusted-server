import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createPermutiveIntegrationRegistration,
  createPermutiveRuntime,
} from '../../../src/integrations/permutive/module';
import type { RuntimeCapabilityV1 } from '../../../src/kernel/runtime';

const RELEASE_ID = 'a'.repeat(64);

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

  it('uses the adopted parser-time segments for the first persistent auction', () => {
    localStorage.removeItem('permutive-app');
    let contributor: (() => Readonly<Record<string, unknown>> | undefined) | undefined;
    const runtime = Object.freeze({
      registerAuctionContext: (_id: string, candidate: typeof contributor) => {
        contributor = candidate;
        return vi.fn();
      },
    }) as unknown as RuntimeCapabilityV1;
    const preparationDisposers: Array<() => void> = [];
    const activationDisposers: Array<() => void> = [];
    const controller = new AbortController();
    const registration = createPermutiveIntegrationRegistration(RELEASE_ID);
    if (registration.phase !== 'takeover') throw new TypeError('Expected takeover registration');
    const prepared = registration.prepareSync({
      config: Object.freeze({}),
      interfaces: Object.freeze({ 'runtime.v1': runtime }),
      signal: controller.signal,
      onDispose: (callback: () => void) => preparationDisposers.push(callback),
    });
    const adoption = Object.freeze({
      version: 1 as const,
      adoptInitialDisplay: true as const,
      handoff: Object.freeze({
        slices: Object.freeze(['first_display', 'permutive_initial']),
        parserState: Object.freeze([
          Object.freeze({
            sliceId: 'permutive_initial',
            observations: Object.freeze(['segments']),
            values: Object.freeze([
              Object.freeze(['segments', JSON.stringify(['initial-one', 'initial-two'])] as const),
            ]),
          }),
        ]),
      }),
      identities: Object.freeze([]),
    });

    prepared.activate({
      adoption,
      afterCommit: vi.fn(),
      signal: controller.signal,
      onDispose: (callback: () => void) => activationDisposers.push(callback),
    });

    expect(contributor?.()).toEqual({
      permutive_segments: ['initial-one', 'initial-two'],
    });
    expect(contributor?.()).toBeUndefined();

    activationDisposers.reverse().forEach((release) => release());
    preparationDisposers.reverse().forEach((release) => release());
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
