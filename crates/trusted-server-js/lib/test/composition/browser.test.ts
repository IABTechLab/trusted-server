import { afterEach, describe, expect, it, vi } from 'vitest';

import type { GoogletagAdapter } from '../../src/adapters/googletag';
import type { CaptureMessageListener, MessagingAdapter } from '../../src/adapters/messaging';
import type { PrebidAdapter } from '../../src/adapters/prebid';
import {
  createBrowserComposition,
  createNoopBrowserComposition,
  createTestBrowserRuntimeComposition,
} from '../../src/composition/browser';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';

function createTarget() {
  return {
    googletag: undefined as unknown,
    pbjs: undefined as unknown,
    addEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
    removeEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
  };
}

describe('browser composition', () => {
  afterEach(() => vi.useRealTimers());

  it('constructs live adapters without changing production globals', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    expect(target.addEventListener).not.toHaveBeenCalled();

    target.googletag = {};
    target.pbjs = {};
    expect(composition.adapters.googletag.bindingStatus()).toBe('present');
    expect(composition.adapters.prebid.bindingStatus()).toBe('present');

    target.googletag = 1;
    target.pbjs = 'not-prebid';
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');
  });

  it('installs the capture-phase message listener synchronously and disposes once', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });
    const listener = vi.fn();

    const dispose = composition.adapters.messaging.installCaptureListener(listener);

    expect(target.addEventListener).toHaveBeenCalledTimes(1);
    expect(target.addEventListener).toHaveBeenCalledWith('message', listener, true);

    dispose();
    dispose();
    expect(target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(target.removeEventListener).toHaveBeenCalledWith('message', listener, true);
  });

  it('uses exact injected fakes without constructing concrete adapters', () => {
    const googletag: GoogletagAdapter = {
      bindingStatus: () => 'present',
    };
    const prebid: PrebidAdapter = {
      bindingStatus: () => 'incompatible',
    };
    const messaging: MessagingAdapter = {
      installCaptureListener: () => vi.fn(),
    };

    const composition = createBrowserComposition({
      adapters: { googletag, messaging, prebid },
    });

    expect(composition.adapters).toEqual({ googletag, messaging, prebid });
    expect(Object.isFrozen(composition.adapters)).toBe(true);
    expect(Object.isFrozen(composition)).toBe(true);
  });

  it('provides a side-effect-free no-op composition for kernel and service tests', () => {
    const composition = createNoopBrowserComposition();
    const listener = vi.fn();

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    expect(() => composition.adapters.messaging.installCaptureListener(listener)()).not.toThrow();
    expect(listener).not.toHaveBeenCalled();
  });

  it('activates reversible core effects in exact order and disposes them in reverse', async () => {
    const target = {};
    const order: string[] = [];
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId: 'a'.repeat(64),
        manifest: {
          version: 1,
          releaseId: 'a'.repeat(64),
          integrations: [{ id: 'test', required: true }],
        },
        knownIntegrationIds: Object.freeze(['test']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'boot', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: { bindingStatus: () => 'pending' },
          prebid: { bindingStatus: () => 'pending' },
          messaging: { installCaptureListener: () => vi.fn() },
        },
        coreActivations: {
          bridgeRecognizer: ({ onDispose }, adapters) => {
            expect(Object.isFrozen(adapters)).toBe(true);
            onDispose(() => order.push('dispose-bridge'));
            order.push('bridge');
          },
          correctnessGptListeners: ({ onDispose }, adapters) => {
            expect(Object.isFrozen(adapters)).toBe(true);
            onDispose(() => order.push('dispose-gpt'));
            order.push('gpt');
          },
        },
      }
    );

    expect(composition.runtime.state).toBe('unclaimed');
    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration({
        id: 'test',
        release: 'a'.repeat(64),
        prepare: ({ onDispose }: { onDispose(callback: () => void): void }) => {
          onDispose(() => order.push('dispose-module'));
          return { activate: () => order.push('module') };
        },
      })
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['bridge', 'gpt', 'module']);

    composition.runtime.dispose();
    expect(order).toEqual([
      'bridge',
      'gpt',
      'module',
      'dispose-module',
      'dispose-gpt',
      'dispose-bridge',
    ]);
    expect(Object.isFrozen(composition)).toBe(true);
    expect(Object.isFrozen(composition.runtime)).toBe(true);
  });

  it('constructs one session lazily from accepted boot and keeps it across SPA replacement', async () => {
    const projection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial',
        results: [{ slot: 'initial-slot', outcome: 'no_bid' }],
      },
      bids: [],
    };
    let prefix = 0;
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: { bindingStatus: () => 'pending' },
          prebid: { bindingStatus: () => 'pending' },
          messaging: { installCaptureListener: () => vi.fn() },
        },
        coreActivations: {
          bridgeRecognizer: vi.fn(),
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );

    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    const session = composition.runtimeSessionForTest();
    expect(session).toBeDefined();
    expect(composition.runtimeSessionForTest()).toBe(session);
    expect(session?.currentNavigation?.currentAuctionProjection).toEqual(projection);
    expect(Object.isFrozen(session?.currentNavigation?.currentAuctionProjection)).toBe(true);

    projection.auction.auctionId = 'publisher-mutated';
    expect(
      (
        session?.currentNavigation?.currentAuctionProjection as {
          auction: { auctionId: string };
        }
      ).auction.auctionId
    ).toBe('initial');
    const replacement = session?.replaceNavigation();
    expect(replacement).toMatchObject({ ok: true });
    if (!replacement?.ok) throw new Error('Expected SPA navigation');
    expect(replacement.value.currentAuctionProjection).toBeUndefined();
    expect(composition.runtimeSessionForTest()).toBe(session);

    const pageBids = composition.pageBidsControllerForTest();
    expect(
      pageBids?.commit({
        version: 1,
        auction: {
          version: 1,
          auctionId: 'spa',
          results: [{ slot: 'spa-slot', outcome: 'no_bid' }],
        },
        bids: [],
      })
    ).toEqual({ status: 'committed' });
    expect(composition.projectionSlotsForTest()).toEqual(['spa-slot']);

    composition.runtime.dispose();
    expect(session?.disposed).toBe(true);
  });

  it('unwinds a lazily-created session when navigation identity generation fails', async () => {
    const bridge = vi.fn();
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        coreActivations: {
          bridgeRecognizer: bridge,
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => ({
          ok: false,
          reason: 'identity_generation_failed',
        }),
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
    expect(bridge).not.toHaveBeenCalled();
  });

  it('releases initial programmatic slots before admitting a replacement SPA projection', async () => {
    let prefix = 0;
    const programmaticSlots = Object.freeze(
      Array.from({ length: 256 }, (_, index) => `programmatic-${index}`)
    );
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: programmaticSlots,
        coreActivations: {
          bridgeRecognizer: vi.fn(),
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(composition.projectionSlotsForTest()).toEqual(programmaticSlots);
    const replacement = composition.runtimeSessionForTest()?.replaceNavigation();
    expect(replacement).toMatchObject({ ok: true });
    expect(composition.projectionSlotsForTest()).toEqual([]);

    expect(
      composition.pageBidsControllerForTest()?.commit({
        version: 1,
        auction: {
          version: 1,
          auctionId: 'spa',
          results: [{ slot: 'spa-slot', outcome: 'no_bid' }],
        },
        bids: [],
      })
    ).toEqual({ status: 'committed' });
    expect(composition.projectionSlotsForTest()).toEqual(['spa-slot']);
  });

  it('fails closed when admitted programmatic input contains duplicate slot ids', async () => {
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: Object.freeze(['duplicate', 'duplicate']),
        coreActivations: {
          bridgeRecognizer: vi.fn(),
          correctnessGptListeners: vi.fn(),
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
  });

  it('owns an immutable copy of admitted programmatic slot input for navigation cleanup', async () => {
    const programmaticSlots = ['programmatic-one', 'programmatic-two'];
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: programmaticSlots,
        coreActivations: {
          bridgeRecognizer: vi.fn(),
          correctnessGptListeners: vi.fn(),
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    programmaticSlots[0] = 'publisher-mutated';
    programmaticSlots.length = 1;

    expect(composition.runtimeSessionForTest()?.replaceNavigation()).toMatchObject({ ok: true });
    expect(composition.projectionSlotsForTest()).toEqual([]);
  });

  it('constructs or activates nothing after a terminal fallback', async () => {
    vi.useFakeTimers();
    const serviceConstruction = vi.fn(() => ({
      config: Object.freeze({}),
      interfaces: Object.freeze({}),
    }));
    const adapterActivation = vi.fn(() => 'pending' as const);
    const listenerActivation = vi.fn(() => vi.fn());
    const timerActivation = vi.fn(() => setTimeout(vi.fn(), 1));
    const latePreparation = vi.fn();
    const target = {};
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId: 'a'.repeat(64),
        manifest: {
          version: 1,
          releaseId: 'a'.repeat(64),
          integrations: [{ id: 'missing', required: true }],
        },
        knownIntegrationIds: Object.freeze(['missing']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'boot', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: serviceConstruction,
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: { bindingStatus: adapterActivation },
          prebid: { bindingStatus: adapterActivation },
          messaging: { installCaptureListener: listenerActivation },
        },
        coreActivations: {
          bridgeRecognizer: timerActivation,
          correctnessGptListeners: adapterActivation,
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);

    expect(
      (target as { _registerIntegration(value: unknown): boolean })._registerIntegration({
        id: 'missing',
        release: 'a'.repeat(64),
        prepare: latePreparation,
      })
    ).toBe(false);
    await vi.runAllTimersAsync();

    expect(serviceConstruction).not.toHaveBeenCalled();
    expect(adapterActivation).not.toHaveBeenCalled();
    expect(listenerActivation).not.toHaveBeenCalled();
    expect(timerActivation).not.toHaveBeenCalled();
    expect(latePreparation).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });
});
